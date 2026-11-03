//! Layers created, destroyed and reordered by scripts after load (SceneScript
//! `thisScene.createLayer`, `destroyLayer`, `sortLayer`): their GPU side.
//!
//! - A created object gets its passes through [`RenderScene::extend`]: the same preparation
//!   as the scene's own objects (`prepare_image`, `particles::prepare`, `text::prepare`), with
//!   the scene's shader provider, texture cache, compiled shaders, pipelines and bind group
//!   layouts. Only what the object needs alone is created: uniform blocks, quad vertices,
//!   composites, bind groups (and a particle stream).
//! - `destroyLayer` parks the object's runtime state (image, animated passes, particle
//!   system, text); the next layer of the same configuration takes it back, so a wallpaper
//!   that keeps creating and destroying layers creates nothing once its pool is warm.
//! - The frame operations are rebuilt from the scripts' layer order when it changes: one
//!   `RenderScene::set_ops`, bundles re-recorded once before the next frame.
//!
//! Nothing here runs for a scene whose scripts do not use these functions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lumenwall_assets::Vfs;
use lumenwall_render::targets::ScopeId;
use lumenwall_render::{
    GpuContext, ImageHandle, ImagePass, Op, RtId, SceneBuilder, add_image_with_puppet,
};
use lumenwall_scene::{ObjectIndex, ObjectKind, ParticleSource, Project};
use serde_json::{Map, Value as Json};

use super::{
    AnimatedPass, ImageRuntime, LiveUniform, PendingImage, SceneOptions, SceneRuntime, align, node,
    prepare_image, properties_json, value_floats, visibility_chain, world_transform,
};
use crate::particles::{self, ParticleRuntime};
use crate::script::SceneScripts;
use crate::shaders::ShaderProvider;
use crate::text;
use crate::texture::TextureCache;

/// Runtime state of a parked object (destroyed layer), kept for reuse.
#[derive(Default)]
struct Parked {
    image: Option<ImageRuntime>,
    animated: Vec<AnimatedPass>,
    particle: Option<ParticleRuntime>,
    text: Option<text::TextRuntime>,
}

/// What the scene keeps to set layers up after load.
pub(crate) struct RuntimeLayers {
    vfs: Arc<Vfs>,
    shaders: Box<dyn ShaderProvider>,
    textures: TextureCache,
    opts: SceneOptions,
    root: ScopeId,
    full: RtId,
    /// Frame operations before the first object (the scene clear).
    prefix: Vec<Op>,
    /// Frame operations of each drawn object.
    object_ops: HashMap<ObjectIndex, Vec<Op>>,
    /// Drawn objects scripts do not list (the bloom), kept last in load order.
    tail: Vec<ObjectIndex>,
    parked: HashMap<ObjectIndex, Parked>,
    /// Current id of the text runtime of each created text object (it follows the layer id).
    text_ids: HashMap<ObjectIndex, i32>,
    /// Objects set up on the GPU after load, and the time it took.
    pub(crate) created: u32,
    pub(crate) create_time: Duration,
    /// Draw-order rebuilds (frame operations, segments, changed bundles), and their time.
    pub(crate) reorders: u32,
    pub(crate) reorder_time: Duration,
}

/// Counters of the layers scripts created, destroyed or reordered after load.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayerStats {
    /// Objects added to the scene model (pool growth), and the time it took (in the scripts'
    /// `createLayer` / `registerAsset` calls).
    pub objects_added: u32,
    pub add_time: Duration,
    /// `createLayer` calls served by a parked object (no allocation of scene or GPU
    /// resources).
    pub reused: u32,
    /// Objects given GPU resources, and the time it took.
    pub gpu_created: u32,
    pub gpu_time: Duration,
    /// Draw-order rebuilds and their time (frame operations, segments, changed bundles).
    pub reorders: u32,
    pub reorder_time: Duration,
}

impl RuntimeLayers {
    /// `marks` = `(object, index of its first operation in ops)` in render order.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        vfs: Arc<Vfs>,
        shaders: Box<dyn ShaderProvider>,
        textures: TextureCache,
        opts: SceneOptions,
        root: ScopeId,
        full: RtId,
        ops: &[Op],
        marks: &[(ObjectIndex, usize)],
    ) -> Self {
        let mut object_ops = HashMap::new();
        for (k, &(idx, start)) in marks.iter().enumerate() {
            let end = marks.get(k + 1).map_or(ops.len(), |m| m.1);
            if end > start {
                object_ops.insert(idx, ops[start..end].to_vec());
            }
        }
        let first = marks.first().map_or(ops.len(), |m| m.1);
        RuntimeLayers {
            vfs,
            shaders: Box::new(crate::shaders::Memoized::new(shaders)),
            textures,
            opts,
            root,
            full,
            prefix: ops[..first].to_vec(),
            tail: marks.iter().map(|m| m.0).collect(),
            object_ops,
            parked: HashMap::new(),
            text_ids: HashMap::new(),
            created: 0,
            create_time: Duration::ZERO,
            reorders: 0,
            reorder_time: Duration::ZERO,
        }
    }

    /// Keeps in `tail` only the drawn objects that scripts do not order.
    pub(crate) fn set_tail(&mut self, scripts: &SceneScripts) {
        let ops = &self.object_ops;
        self.tail
            .retain(|idx| ops.contains_key(idx) && !scripts.lists(*idx));
    }

    fn ops(&self, scripts: &SceneScripts) -> Vec<Op> {
        let mut ops = self.prefix.clone();
        for idx in scripts.render_objects().chain(self.tail.iter().copied()) {
            if let Some(o) = self.object_ops.get(&idx) {
                ops.extend_from_slice(o);
            }
        }
        ops
    }
}

/// What one created image needs once its passes exist.
struct ImageSetup {
    img: PendingImage,
    handle: ImageHandle,
    live: Vec<LiveUniform>,
    animated: Vec<AnimatedPass>,
    time: bool,
    frame_inputs: bool,
}

/// Passes of a prepared image and their uniforms: the image branch of `build_scene`
/// phase 2 (keep both in step), without puppets.
fn add_image(
    b: &mut SceneBuilder<'_>,
    project: &Project,
    img: PendingImage,
) -> Result<ImageSetup, String> {
    let passes: Vec<ImagePass> = img.passes.iter().map(|p| p.pass.clone()).collect();
    let handle = add_image_with_puppet(b, &img.spec, &passes, None).map_err(|e| e.to_string())?;
    let s = &project.settings;
    let general = project.scene.as_ref().ok_or("not a scene project")?.general;
    let ambient = s.value(general.ambient_color).vec3();
    let skylight = s.value(general.skylight_color).vec3();
    let mut live = Vec::new();
    let (mut time, mut frame_inputs) = (false, false);
    for (k, &pass) in handle.passes.iter().enumerate() {
        if let Some(iface) = b.shader_interface(img.passes[k].pass.shader) {
            time |= iface.uses_frame_globals || iface.field("g_Time").is_some();
            frame_inputs |= iface.uses_frame_globals
                || iface.uniforms.iter().any(|u| {
                    lumenwall_render::dynbuf::frame_globals::FIELDS
                        .iter()
                        .any(|g| g.0 == u.name)
                });
        }
        let values = img.values;
        let alpha = s.value(values.alpha).float();
        let color = s.value(values.color).vec4();
        let bright = s.value(values.brightness).float();
        let declared = img.passes[k]
            .constants
            .iter()
            .any(|c| c.0.uniform == "g_CompositeColor");
        let mut put = |b: &mut SceneBuilder<'_>,
                       name: &str,
                       v: &[f32],
                       setting: Option<(lumenwall_scene::SettingId, u8)>| {
            if let Some(slot) = b.uniform(pass, name) {
                b.shadow_mut().write_floats(&slot, v);
                if let Some((setting, components)) = setting {
                    live.push(LiveUniform {
                        setting,
                        slot,
                        components,
                    });
                }
            }
        };
        put(b, "g_Alpha", &[alpha], Some((values.alpha, 1)));
        put(b, "g_UserAlpha", &[alpha], Some((values.alpha, 1)));
        let rgb = color.truncate().to_array();
        put(b, "g_Color", &rgb, Some((values.color, 3)));
        put(b, "g_Color4", &color.to_array(), Some((values.color, 4)));
        put(b, "g_Brightness", &[bright], Some((values.brightness, 1)));
        if !declared {
            put(b, "g_CompositeColor", &rgb, Some((values.color, 3)));
        }
        put(b, "g_LightAmbientColor", &ambient.to_array(), None);
        put(b, "g_LightSkylightColor", &skylight.to_array(), None);
        for (param, value, setting) in &img.passes[k].constants {
            if let Some(slot) = b.uniform(pass, &param.uniform) {
                if param.components == 0 {
                    b.shadow_mut().write_ints(&slot, &[value.int()]);
                } else {
                    b.shadow_mut()
                        .write_floats(&slot, &value_floats(value, param.components));
                }
                if let Some(setting) = setting {
                    live.push(LiveUniform {
                        setting: *setting,
                        slot,
                        components: param.components,
                    });
                }
            }
        }
    }
    let mut animated = Vec::new();
    if let Some(main) = &img.main
        && main.is_animated()
    {
        time = true;
        for (k, &pass) in handle.passes.iter().enumerate() {
            if img.passes[k].animated_slot0 {
                animated.push(AnimatedPass {
                    pass,
                    texture: main.clone(),
                    rotation: b.uniform(pass, "g_Texture0Rotation"),
                    translation: b.uniform(pass, "g_Texture0Translation"),
                });
            }
        }
    }
    Ok(ImageSetup {
        img,
        handle,
        live,
        animated,
        time,
        frame_inputs,
    })
}

/// `scene.json` form of a created particle object with its current transform (the
/// simulation reads it when it starts).
fn particle_object_json(project: &Project, idx: ObjectIndex, config: Option<&Json>) -> Json {
    let mut map = match config {
        Some(Json::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    if let Some(scene) = &project.scene {
        let obj = scene.object(idx);
        let s = &project.settings;
        let v3 = |id| {
            let v = s.value(id).vec3();
            Json::String(format!("{} {} {}", v.x, v.y, v.z))
        };
        map.insert("origin".into(), v3(obj.origin));
        if let ObjectKind::Particle(p) = &obj.kind {
            map.insert("scale".into(), v3(p.scale));
            map.insert("angles".into(), v3(p.angles));
        }
    }
    Json::Object(map)
}

impl SceneRuntime {
    /// Counters of the layers scripts created after load; `None` when the scene's scripts
    /// do not create, destroy or sort layers.
    pub fn layer_stats(&self) -> Option<LayerStats> {
        let l = self.layers.as_ref()?;
        let d = self.scripts.as_ref().and_then(|s| s.dynamic());
        Some(LayerStats {
            objects_added: d.map_or(0, |d| d.added),
            add_time: d.map_or(Duration::ZERO, |d| d.add_time),
            reused: d.map_or(0, |d| d.reused),
            gpu_created: l.created,
            gpu_time: l.create_time,
            reorders: l.reorders,
            reorder_time: l.reorder_time,
        })
    }

    /// Applies what scripts changed in the layer list during their tick (or at load): GPU
    /// resources of new objects, parking and reuse, anchors, then the draw order. Does
    /// nothing (one check) when nothing changed.
    pub(super) fn apply_layer_changes(&mut self, ctx: &GpuContext, project: &mut Project) {
        let Some(scripts) = &mut self.scripts else {
            return;
        };
        let aligned = scripts.take_aligned();
        let structure = scripts.take_structure();
        for idx in aligned {
            let Some(ObjectKind::Image(i)) = project.scene.as_ref().map(|s| &s.object(idx).kind)
            else {
                continue;
            };
            let a = align(i.alignment);
            if let Some(img) = self.images.iter_mut().find(|img| img.object == idx) {
                img.handle.set_alignment(a);
            } else if let Some(img) = self
                .layers
                .as_mut()
                .and_then(|l| l.parked.get_mut(&idx))
                .and_then(|p| p.image.as_mut())
            {
                img.handle.set_alignment(a);
            }
        }
        let Some(st) = structure else {
            return;
        };
        let Some(mut layers) = self.layers.take() else {
            return;
        };
        let t0 = Instant::now();
        for &idx in &st.created {
            let t = Instant::now();
            self.instantiate(ctx, project, &mut layers, idx);
            layers.created += 1;
            layers.create_time += t.elapsed();
        }
        let mut touched: Vec<ObjectIndex> = Vec::new();
        for &idx in st.created.iter().chain(&st.reused).chain(&st.removed) {
            if !touched.contains(&idx) {
                touched.push(idx);
            }
        }
        let props = (!st.reused.is_empty()).then(|| properties_json(project));
        for idx in touched {
            let live = self.scripts.as_ref().is_some_and(|s| s.is_live(idx));
            let reused = st.reused.contains(&idx);
            if !live || reused {
                self.park(&mut layers, project, idx);
            }
            if live {
                self.unpark(ctx, &mut layers, project, idx, reused, props.as_deref());
            }
        }
        if st.reordered || !st.created.is_empty() || !st.removed.is_empty() {
            let t = Instant::now();
            if let Some(s) = &self.scripts {
                self.scene.set_ops(layers.ops(s));
                // Bundles of the changed runs, now rather than in the next encode (same frame).
                self.scene.refresh(ctx);
            }
            layers.reorders += 1;
            layers.reorder_time += t.elapsed();
        }
        tracing::debug!(
            "layers: {} created, {} reused, {} removed, order {}: {:.0} µs",
            st.created.len(),
            st.reused.len(),
            st.removed.len(),
            st.reordered,
            t0.elapsed().as_secs_f64() * 1e6
        );
        self.layers = Some(layers);
    }

    /// GPU resources of an object added after load; the result is parked.
    fn instantiate(
        &mut self,
        ctx: &GpuContext,
        project: &mut Project,
        l: &mut RuntimeLayers,
        idx: ObjectIndex,
    ) {
        let Some(scene) = project.scene.as_ref() else {
            return;
        };
        let n = scene.objects.len();
        let i = idx.index();
        if self.build.created.len() < n {
            self.build.created.resize(n, false);
            self.build.transforms.resize(n, Default::default());
            self.build.image_passes.resize(n, None);
        }
        self.build.created[i] = true;
        self.build.transforms[i] = world_transform(project, &self.build, idx);
        self.build.image_passes[i] = project.image_passes(idx);
        let label = project.strings.resolve(scene.object(idx).name).to_owned();
        let config = self
            .scripts
            .as_ref()
            .and_then(|s| s.dynamic())
            .and_then(|d| d.config_of(idx))
            .cloned();
        enum Kind {
            Image,
            Text,
            Particle(Result<Json, String>),
            Other,
        }
        let kind = match &scene.object(idx).kind {
            ObjectKind::Image(_) => Kind::Image,
            ObjectKind::Text(_) => Kind::Text,
            ObjectKind::Particle(p) => Kind::Particle(match &p.source {
                ParticleSource::File(sym) => {
                    let path = project.strings.resolve(*sym);
                    l.vfs
                        .read(path)
                        .map_err(|e| e.to_string())
                        .and_then(|b| {
                            serde_json::from_slice::<Json>(particles::strip_bom(&b))
                                .map_err(|e| e.to_string())
                        })
                        .map_err(|e| format!("particle file '{path}': {e}"))
                }
                ParticleSource::Inline(raw) => {
                    serde_json::from_str::<Json>(raw.get()).map_err(|e| e.to_string())
                }
            }),
            _ => Kind::Other,
        };
        let parked = match kind {
            Kind::Image => self.instantiate_image(ctx, project, l, idx, None),
            Kind::Text => self.instantiate_image(ctx, project, l, idx, config.as_ref()),
            // Particles off: an inert layer.
            Kind::Particle(_) if !l.opts.particles => Ok(Parked::default()),
            Kind::Particle(definition) => definition
                .and_then(|d| self.instantiate_particle(ctx, project, l, idx, &d, config.as_ref())),
            // Sounds and groups draw nothing.
            Kind::Other => Ok(Parked::default()),
        };
        match parked {
            Ok(p) => {
                l.parked.insert(idx, p);
            }
            Err(e) => tracing::warn!("created layer '{label}' not rendered: {e}"),
        }
    }

    fn instantiate_image(
        &mut self,
        ctx: &GpuContext,
        project: &mut Project,
        l: &mut RuntimeLayers,
        idx: ObjectIndex,
        text_config: Option<&Json>,
    ) -> Result<Parked, String> {
        let (size, root, full) = (self.size, l.root, l.full);
        let build = &self.build;
        let fonts = &mut self.fonts;
        let RuntimeLayers {
            vfs,
            shaders,
            textures,
            opts,
            ..
        } = &mut *l;
        let is_text = matches!(
            project.scene.as_ref().map(|s| &s.object(idx).kind),
            Some(ObjectKind::Text(_))
        );
        let (r, ext) = self.scene.extend(ctx, |b| -> Result<_, String> {
            let text = if is_text {
                let fonts = fonts.get_or_insert_with(Box::default);
                match text::prepare(b, project, vfs, fonts, textures, idx, text_config)? {
                    Some(t) => Some(t),
                    None => return Ok(None),
                }
            } else {
                None
            };
            let Some(img) = prepare_image(
                b,
                project,
                vfs,
                textures,
                shaders.as_mut(),
                build,
                idx,
                root,
                full,
                size,
                opts,
                None,
            )?
            else {
                return Ok(None);
            };
            let mut setup = add_image(b, project, img)?;
            let text = match (text, setup.handle.passes.first()) {
                (Some(mut t), Some(&glyph)) => {
                    t.runtime.attach(b, glyph, &project.settings);
                    setup
                        .live
                        .retain(|u| Some(&u.slot) != t.runtime.color_slot());
                    Some((t.runtime, t.offset))
                }
                _ => None,
            };
            Ok(Some((setup, text)))
        });
        let ext = ext.map_err(|e| e.to_string())?;
        let Some((setup, text)) = r? else {
            return Ok(Parked::default());
        };
        l.object_ops.insert(idx, ext.ops);
        self.live.extend(setup.live);
        self.frame_inputs |= setup.frame_inputs;
        if setup.time || !setup.animated.is_empty() {
            self.animated = true;
            if let Some(s) = &mut self.scripts {
                s.base_animated = true;
            }
        }
        let size_px = setup.img.spec.size;
        if let Some(s) = &mut self.scripts {
            s.set_layer_size(idx, size_px);
        }
        let mut anchor = glam::Vec2::ZERO;
        let text = text.map(|(t, offset)| {
            anchor = glam::Vec2::from(offset);
            l.text_ids.insert(idx, t.object_id);
            t
        });
        let image = ImageRuntime {
            object: idx,
            handle: setup.handle,
            transform: node(world_transform(project, &self.build, idx)),
            chain: None,
            fullscreen: setup.img.fullscreen,
            parallax_depth: setup.img.parallax_depth,
            visible: visibility_chain(project, &self.build, idx),
            shown: true,
            size: size_px,
            anchor,
        };
        Ok(Parked {
            image: Some(image),
            animated: setup.animated,
            particle: None,
            text,
        })
    }

    fn instantiate_particle(
        &mut self,
        ctx: &GpuContext,
        project: &Project,
        l: &mut RuntimeLayers,
        idx: ObjectIndex,
        definition: &Json,
        config: Option<&Json>,
    ) -> Result<Parked, String> {
        let object_json = particle_object_json(project, idx, config);
        let props = properties_json(project);
        let object_id = project.scene.as_ref().map_or(0, |s| s.object(idx).id);
        let setup = particles::ParticleSetup {
            object_id,
            object_json: &object_json,
            definition,
            scene_size: self.size,
            target: l.full,
            visible: visibility_chain(project, &self.build, idx),
            properties: &props,
        };
        let RuntimeLayers {
            vfs,
            shaders,
            textures,
            ..
        } = &mut *l;
        let (r, ext) = self.scene.extend(ctx, |b| {
            particles::prepare(b, vfs, textures, shaders.as_mut(), &setup)
                .and_then(|p| p.finish_respawnable(b))
        });
        let ext = ext.map_err(|e| e.to_string())?;
        let mut rt = r?;
        rt.object = Some(idx);
        l.object_ops.insert(idx, ext.ops);
        self.frame_inputs = true;
        self.animated = true;
        if let Some(s) = &mut self.scripts {
            s.base_animated = true;
        }
        Ok(Parked {
            particle: Some(rt),
            ..Parked::default()
        })
    }

    /// Moves the runtime state of `idx` out of the frame loop.
    fn park(&mut self, l: &mut RuntimeLayers, project: &Project, idx: ObjectIndex) {
        let mut p = l.parked.remove(&idx).unwrap_or_default();
        if let Some(k) = self.images.iter().position(|img| img.object == idx) {
            let img = self.images.swap_remove(k);
            let passes = &img.handle.passes;
            let mut k = 0;
            while k < self.animated_passes.len() {
                if passes.contains(&self.animated_passes[k].pass) {
                    p.animated.push(self.animated_passes.swap_remove(k));
                } else {
                    k += 1;
                }
            }
            p.image = Some(img);
        }
        if let Some(k) = self.particles.iter().position(|r| r.object == Some(idx)) {
            p.particle = Some(self.particles.swap_remove(k));
        }
        let text_id = l
            .text_ids
            .get(&idx)
            .copied()
            .or_else(|| project.scene.as_ref().map(|s| s.object(idx).id));
        if let Some(k) = self.texts.iter().position(|t| Some(t.object_id) == text_id) {
            p.text = Some(self.texts.swap_remove(k));
        }
        l.parked.insert(idx, p);
    }

    /// Puts the parked state of `idx` back in the frame loop; a reused object starts again
    /// (transform, particle emission, text id).
    fn unpark(
        &mut self,
        ctx: &GpuContext,
        l: &mut RuntimeLayers,
        project: &Project,
        idx: ObjectIndex,
        reused: bool,
        props: Option<&[(String, Json)]>,
    ) {
        let Some(p) = l.parked.remove(&idx) else {
            return;
        };
        if let Some(mut img) = p.image {
            img.transform = node(world_transform(project, &self.build, idx));
            self.images.push(img);
        }
        self.animated_passes.extend(p.animated);
        if let Some(mut rt) = p.particle {
            if reused {
                let config = self
                    .scripts
                    .as_ref()
                    .and_then(|s| s.dynamic())
                    .and_then(|d| d.config_of(idx));
                let json = particle_object_json(project, idx, config);
                rt.respawn(&mut self.scene, ctx, &json, props.unwrap_or(&[]));
            }
            self.particles.push(rt);
        }
        if let Some(mut t) = p.text {
            if let Some(id) = project.scene.as_ref().map(|s| s.object(idx).id) {
                t.object_id = id;
                l.text_ids.insert(idx, id);
            }
            self.texts.push(t);
        }
        // Visibility, transforms and uniforms of the restored values are written by the frame.
        self.scene.mark_dirty();
    }
}

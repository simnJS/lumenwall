//! `lumen-scene` model → `lumen-render` graph (spec 04 §4–§17), and the per-frame update of
//! the resulting scene.
//!
//! Construction runs in two phases so that render-target names resolve in **creation** order
//! (spec 04 §4.5) while draws are emitted in **render** order (§4.6):
//!
//! 1. creation order: textures, composites A/B, effect `fbos`, slot candidates, targets,
//!    shaders and constants of every image; particle systems and their vertex streams;
//! 2. render order: `add_image` / particle passes / bloom, which append the draws.

use std::collections::HashMap;
use std::rc::Rc;

use glam::{Mat4, Vec2, Vec3};
use lumen_assets::Vfs;
use lumen_formats::mdl::PuppetModel;
use lumen_particles::ParallaxInput;
use lumen_render::bloom::{BloomParams, BloomShaders, add_bloom};
use lumen_render::dynbuf::{FrameInputs, UniformSlot, UniformType, VertexRange};
use lumen_render::image::composite_sampler;
use lumen_render::math::{
    Alignment as RAlign, HAlign, NodeTransform, OrthoCamera, Parallax, ParallaxSmoothing, Pointer,
    VAlign, parallax_translation, scene_logical_size, trunc_u32,
};
use lumen_render::targets::{ScopeId, scaled_size};
use lumen_render::texture::{SamplerKey, TexFlags};
use lumen_render::{
    BlendMode, ClampMode, CullMode, GpuContext, ImageFrame, ImageHandle, ImagePass, ImageSpec, Op,
    PassId, PuppetGeometry, RenderScene, RtId, SceneBuilder, SkinVertex, SlotSource, TextureRef,
    Vertex, add_image_with_puppet,
};
use lumen_scene::{
    BuildOptions, ConstantSource, ImagePasses, MainTextureFormat, ObjectIndex, ObjectKind,
    ParticleSource, PassKind as ScenePassKind, Project, SceneBuild, SettingId, Value, ValueTag,
};
use serde_json::Value as Json;

use crate::assets::SceneSource;
use crate::particles::{self, ParticleFrame, ParticleRuntime, PendingParticle};
use crate::plan::DebugPlan;
use crate::puppet::{Layer, MAX_GPU_BONES, MAX_STEP, Puppet, Rig, decompose_attachment};
use crate::shaders::{ConstantParam, PassKind, ShaderProvider, ShaderRequest};
use crate::texture::{LoadedTexture, TextureCache};

/// Scene construction settings.
#[derive(Clone, Debug)]
pub struct SceneOptions {
    /// Output size, used when the scene size is automatic (spec 04 §4.1).
    pub output_size: (u32, u32),
    /// Wrap mode of `_rt_FullFrameBuffer` (spec 04 §21.3).
    pub clamp: ClampMode,
    pub particles: bool,
    pub parallax: bool,
    pub debug: DebugPlan,
}

impl Default for SceneOptions {
    fn default() -> Self {
        Self {
            output_size: (1920, 1080),
            clamp: ClampMode::Clamp,
            particles: true,
            parallax: true,
            debug: DebugPlan::default(),
        }
    }
}

/// Load statistics.
#[derive(Clone, Copy, Debug, Default)]
pub struct SceneStats {
    pub images: usize,
    pub passes: usize,
    pub particle_systems: usize,
    pub textures: usize,
    pub texture_bytes: u64,
    pub stand_in_shaders: usize,
    pub skipped_objects: usize,
    /// Images with a puppet skeleton (spec 08), and how many are skinned on the GPU.
    pub puppets: usize,
    pub gpu_skinned_puppets: usize,
    /// Objects hanging from a puppet attachment.
    pub attached_objects: usize,
}

/// A uniform fed by a setting, rewritten when the setting changes (properties, scripts).
struct LiveUniform {
    setting: SettingId,
    slot: UniformSlot,
    components: u8,
}

/// Image-level settings written into every pass of an image (spec 04 §13.2).
#[derive(Clone, Copy)]
struct ImageValues {
    alpha: SettingId,
    color: SettingId,
    brightness: SettingId,
}

struct ImageRuntime {
    object: ObjectIndex,
    handle: ImageHandle,
    transform: NodeTransform,
    /// Transform chain from the root when an ancestor (or the image) hangs from a puppet
    /// attachment: recomposed every frame (spec 08 §9.10). `None` = `transform` is used.
    chain: Option<Box<[ChainStep]>>,
    fullscreen: bool,
    parallax_depth: SettingId,
    /// Own `visible` then each created ancestor's (see [`visibility_chain`]).
    visible: Box<[SettingId]>,
    shown: bool,
}

/// One link of a transform chain (spec 04 §8.1 with the attachment nodes of spec 08 §9.10).
#[derive(Clone, Copy, Debug)]
enum ChainStep {
    /// Local transform of an object.
    Object(ObjectIndex),
    /// Attachment frame of a puppet (index into `SceneRuntime::puppets`).
    Attachment { puppet: usize, index: usize },
}

/// How a puppet pose reaches the mesh (spec 08 §9.8).
enum PuppetOutput {
    /// GPU skinning: the `g_Bones` palette of the passes drawing the mesh.
    Palette {
        slots: Vec<UniformSlot>,
        half: Vec2,
        palette: Vec<f32>,
    },
    /// CPU skinning: the mesh vertices are rewritten (shader without a skinning path).
    Vertices {
        range: VertexRange,
        half: Vec2,
        rest: Vec<Vertex>,
        skin: Vec<SkinVertex>,
        out: Vec<Vertex>,
    },
}

/// Animated skeleton of an image object (spec 08 §9).
struct PuppetRuntime {
    puppet: Puppet,
    /// `visible` of the image and its created ancestors: a hidden image does not advance its
    /// layers (spec 08 §9.11).
    visible: Box<[SettingId]>,
    /// `(rate, blend, visible)` settings of each layer, read every frame.
    layer_settings: Vec<(SettingId, SettingId, SettingId)>,
    /// `None` when the image draws no mesh (e.g. not set up): attachments only.
    output: Option<PuppetOutput>,
    /// The pose changed since `output` was last written.
    pending: bool,
}

impl PuppetRuntime {
    fn write_output(&mut self, scene: &mut RenderScene) {
        match &mut self.output {
            Some(PuppetOutput::Palette {
                slots,
                half,
                palette,
            }) => {
                self.puppet.write_palette(*half, palette);
                let sh = scene.shadow_mut();
                for slot in slots.iter() {
                    sh.write_floats(slot, palette);
                }
            }
            Some(PuppetOutput::Vertices {
                range,
                half,
                rest,
                skin,
                out,
            }) => {
                self.puppet.skin_vertices(*half, rest, skin, out);
                scene.set_vertices(*range, out);
            }
            None => {}
        }
        self.pending = false;
    }
}

/// A pass whose slot 0 is an animated texture (spec 04 §16.3).
struct AnimatedPass {
    pass: PassId,
    texture: Rc<LoadedTexture>,
    rotation: Option<UniformSlot>,
    translation: Option<UniformSlot>,
}

/// One pass of an image, prepared in creation order.
struct PreparedPass {
    pass: ImagePass,
    /// `(uniform, values, setting)` written once the pass exists.
    constants: Vec<(ConstantParam, Value, Option<SettingId>)>,
}

struct PendingImage {
    object: ObjectIndex,
    spec: ImageSpec,
    passes: Vec<PreparedPass>,
    values: ImageValues,
    fullscreen: bool,
    parallax_depth: SettingId,
    main: Option<Rc<LoadedTexture>>,
    puppet: Option<PuppetGeometry>,
    /// Pass 0 was compiled with the `SKINNING` combo (spec 08 §9.8).
    skinning: bool,
}

enum Pending {
    Image(Box<PendingImage>),
    Particle(Box<PendingParticle>),
}

/// A scene ready to render, with everything its per-frame update needs.
pub struct SceneRuntime {
    pub scene: RenderScene,
    /// `_rt_FullFrameBuffer`.
    pub full: RtId,
    /// Logical size `(tronc(W), tronc(H))`.
    pub logical: (u32, u32),
    /// Projection size `(W, H)`.
    pub size: Vec2,
    pub stats: SceneStats,
    build: SceneBuild,
    view_projection: Mat4,
    fov: f32,
    near: f32,
    far: f32,
    images: Vec<ImageRuntime>,
    particles: Vec<ParticleRuntime>,
    puppets: Vec<PuppetRuntime>,
    animated_passes: Vec<AnimatedPass>,
    live: Vec<LiveUniform>,
    parallax: Parallax,
    parallax_on: bool,
    parallax_amount: SettingId,
    parallax_delay: SettingId,
    parallax_influence: SettingId,
    /// Something changes over time without input (g_Time users, particles, animations).
    pub animated: bool,
    /// The pointer changes the output (g_PointerPosition users, parallax).
    pub uses_pointer: bool,
    pointer_last: [f32; 2],
}

/// Inputs of one frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameState<'a> {
    /// Seconds since start (kept in f64, spec 04 Q24).
    pub time: f64,
    /// Seconds since the previous rendered frame.
    pub dt: f32,
    pub daytime: f32,
    pub pointer: Pointer,
    pub audio16: &'a [f32; 16],
    pub audio32: &'a [f32; 32],
    pub audio64: &'a [f32; 64],
}

fn align(a: lumen_scene::Alignment) -> RAlign {
    RAlign {
        v: if a.top {
            VAlign::Top
        } else if a.bottom {
            VAlign::Bottom
        } else {
            VAlign::Center
        },
        h: if a.left {
            HAlign::Left
        } else if a.right {
            HAlign::Right
        } else {
            HAlign::Center
        },
    }
}

fn blend(b: lumen_scene::Blending) -> BlendMode {
    match b {
        lumen_scene::Blending::Normal => BlendMode::Normal,
        lumen_scene::Blending::Translucent => BlendMode::Translucent,
        lumen_scene::Blending::Additive => BlendMode::Additive,
    }
}

fn node(t: lumen_scene::Transform) -> NodeTransform {
    NodeTransform {
        origin: t.origin,
        scale: t.scale,
        angle: t.angle,
    }
}

/// Float view of a value with `n` components (0 = integer, written as float bits by caller).
fn value_floats(v: &Value, n: u8) -> [f32; 4] {
    match n {
        0 | 1 => [v.float(), 0.0, 0.0, 0.0],
        2 => {
            let x = v.vec2();
            [x.x, x.y, 0.0, 0.0]
        }
        3 => {
            let x = v.vec3();
            [x.x, x.y, x.z, 0.0]
        }
        _ => v.vec4().to_array(),
    }
}

/// JSON form of a property value, as `lumen-particles` parses bound values.
pub fn property_json(v: &Value) -> Json {
    match v.tag() {
        ValueTag::Bool => Json::Bool(v.boolean()),
        ValueTag::Int => Json::from(v.int()),
        ValueTag::Float => Json::from(f64::from(v.float())),
        ValueTag::Vec2 => {
            let x = v.vec2();
            Json::String(format!("{} {}", x.x, x.y))
        }
        ValueTag::Vec3 => {
            let x = v.vec3();
            Json::String(format!("{} {} {}", x.x, x.y, x.z))
        }
        ValueTag::Vec4 => {
            let x = v.vec4();
            Json::String(format!("{} {} {} {}", x.x, x.y, x.z, x.w))
        }
        _ => Json::String(v.text().to_owned()),
    }
}

/// User properties of a project as `(name, json)`.
pub fn properties_json(project: &Project) -> Vec<(String, Json)> {
    project
        .properties
        .iter()
        .map(|(_, p)| (p.name().to_owned(), property_json(p.value())))
        .collect()
}

/// Passes kept by the debug filters (`base-only`, `skip-effect`).
fn filter_passes(passes: &ImagePasses, debug: &DebugPlan, effect_ids: &[i32]) -> ImagePasses {
    let mut out = passes.clone();
    if !debug.base_only && debug.skip_effects.is_empty() {
        return out;
    }
    out.passes.retain(|p| {
        if debug.base_only {
            return p.effect.is_none() && p.over == lumen_scene::OverrideRef::None;
        }
        match p.effect {
            Some((e, _)) => effect_ids
                .get(e as usize)
                .is_none_or(|id| !debug.skip_effects.contains(&(*id as u32))),
            None => true,
        }
    });
    out
}

/// Raw `scene.json` objects by id (particle systems read their object as written).
fn raw_objects(vfs: &Vfs, file: &str) -> HashMap<i64, Json> {
    let Ok(bytes) = vfs.read(file) else {
        return HashMap::new();
    };
    let Ok(json) = serde_json::from_slice::<Json>(particles::strip_bom(&bytes)) else {
        return HashMap::new();
    };
    json.get("objects")
        .and_then(Json::as_array)
        .map(|objs| {
            objs.iter()
                .filter_map(|o| Some((o.get("id")?.as_i64()?, o.clone())))
                .collect()
        })
        .unwrap_or_default()
}

/// Builds the render graph of a scene project.
pub fn build_scene(
    ctx: &GpuContext,
    project: &mut Project,
    vfs: &Vfs,
    shaders: &mut dyn ShaderProvider,
    opts: &SceneOptions,
) -> Result<SceneRuntime, String> {
    let src = SceneSource(vfs);
    let mut bopts = BuildOptions::new(opts.output_size.0, opts.output_size.1);
    bopts.particles_enabled = opts.particles;
    let build = project.build(&src, &bopts).map_err(|e| e.to_string())?;
    for w in project.warnings() {
        tracing::debug!("scene: {w}");
    }
    let scene = project.scene.as_ref().ok_or("not a scene project")?;
    let size = build.size;
    let logical = scene_logical_size(size.x, size.y);
    let settings = &project.settings;
    let camera = OrthoCamera {
        width: size.x,
        height: size.y,
        eye: scene.camera.eye,
        center: scene.camera.center,
        up: scene.camera.up,
        near: settings.value(scene.camera.nearz).float(),
        far: settings.value(scene.camera.farz).float(),
    };
    let view_projection = camera.view_projection();
    let general = scene.general;
    let ambient = settings.value(general.ambient_color).vec3();
    let skylight = settings.value(general.skylight_color).vec3();

    let mut b = SceneBuilder::new(ctx, (size.x, size.y));
    let full_sampler = SamplerKey {
        address: opts.clamp.address(),
        nearest: false,
        mipmaps: false,
    };
    let st = b.targets.create_scene_targets(ctx, logical, full_sampler);
    let root = b.targets.scene_scope();
    let mut textures = TextureCache::default();
    let mut stats = SceneStats::default();
    let raw = raw_objects(vfs, &project.file);
    let props = properties_json(project);
    // Puppet skeletons by object index (spec 08 §9), drawn or not: attachments need them.
    let mut puppets: Vec<PuppetRuntime> = Vec::new();
    let mut puppet_of: HashMap<usize, usize> = HashMap::new();

    // Phase 1: creation order.
    let mut pending: HashMap<usize, Pending> = HashMap::new();
    let creation = build.creation_order.clone();
    for idx in creation {
        if build.bloom.is_some_and(|bl| bl.object == idx) {
            continue;
        }
        let scene = project.scene.as_ref().ok_or("not a scene project")?;
        let obj = scene.object(idx);
        let id = obj.id;
        if opts
            .debug
            .only_object
            .is_some_and(|o| o as i64 != i64::from(id))
            || opts
                .debug
                .skip_objects
                .iter()
                .any(|&s| i64::from(s) == i64::from(id))
        {
            stats.skipped_objects += 1;
            continue;
        }
        match &obj.kind {
            ObjectKind::Image(_) => {
                let model = load_puppet(project, vfs, idx);
                if let Some(rt) = model
                    .as_ref()
                    .and_then(|m| puppet_runtime(project, &build, idx, m, raw.get(&i64::from(id))))
                {
                    puppet_of.insert(idx.index(), puppets.len());
                    puppets.push(rt);
                }
                match prepare_image(
                    &mut b,
                    project,
                    vfs,
                    &mut textures,
                    shaders,
                    &build,
                    idx,
                    root,
                    st.full,
                    size,
                    opts,
                    model.as_ref(),
                ) {
                    Ok(Some(img)) => {
                        pending.insert(idx.index(), Pending::Image(Box::new(img)));
                    }
                    Ok(None) => stats.skipped_objects += 1,
                    Err(e) => {
                        tracing::warn!("image object {id} not rendered: {e}");
                        stats.skipped_objects += 1;
                    }
                }
            }
            ObjectKind::Particle(p) => {
                let definition = match &p.source {
                    ParticleSource::File(sym) => {
                        let path = project.strings.resolve(*sym);
                        vfs.read(path)
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
                };
                let object_json = raw.get(&i64::from(id)).cloned().unwrap_or(Json::Null);
                let r = definition.and_then(|definition| {
                    particles::prepare(
                        &mut b,
                        vfs,
                        &mut textures,
                        shaders,
                        &particles::ParticleSetup {
                            object_id: id,
                            object_json: &object_json,
                            definition: &definition,
                            scene_size: size,
                            target: st.full,
                            visible: visibility_chain(project, &build, idx),
                            properties: &props,
                        },
                    )
                });
                match r {
                    Ok(p) => {
                        pending.insert(idx.index(), Pending::Particle(Box::new(p)));
                    }
                    Err(e) => {
                        tracing::warn!("particle object {id} not rendered: {e}");
                        stats.skipped_objects += 1;
                    }
                }
            }
            ObjectKind::Text(_) => {
                tracing::warn!(
                    "text object {id} not rendered (text objects are not integrated yet)"
                );
                stats.skipped_objects += 1;
            }
            _ => {}
        }
    }

    // Late scene targets (spec 04 §4.3, after the objects).
    let bloom = build.bloom.map(|bl| {
        let targets = b.targets.create_bloom_targets(ctx, logical);
        let composites = b.targets.create_bloom_composites(ctx, logical);
        (bl, targets, composites)
    });

    // Phase 2: render order.
    let clear = build.clear_color;
    b.push(Op::Clear {
        target: st.full,
        color: [
            f64::from(clear.x),
            f64::from(clear.y),
            f64::from(clear.z),
            1.0,
        ],
    });
    let mut images = Vec::new();
    let mut particle_rts = Vec::new();
    let mut animated_passes = Vec::new();
    let mut live = Vec::new();
    let mut uses_time = false;
    let mut uses_pointer = false;
    let render_order = build.render_order.clone();
    for idx in render_order {
        if let Some((bl, targets, composites)) = &bloom
            && bl.object == idx
        {
            let shaders = BloomShaders::internal(&mut b).map_err(|e| e.to_string())?;
            add_bloom(
                &mut b,
                &shaders,
                &st,
                targets,
                *composites,
                BloomParams {
                    strength: bl.strength,
                    threshold: bl.threshold,
                },
            )
            .map_err(|e| e.to_string())?;
            stats.passes += 5;
            continue;
        }
        match pending.remove(&idx.index()) {
            Some(Pending::Image(img)) => {
                let img = *img;
                let label = img.spec.label.clone();
                let passes: Vec<ImagePass> = img.passes.iter().map(|p| p.pass.clone()).collect();
                let handle =
                    match add_image_with_puppet(&mut b, &img.spec, &passes, img.puppet.as_ref()) {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("{label} not rendered: {e}");
                            continue;
                        }
                    };
                stats.images += 1;
                stats.passes += handle.passes.len();
                for (k, &pass) in handle.passes.iter().enumerate() {
                    if opts.debug.pass_log {
                        tracing::info!("{label} pass {k}: {:?}", handle.plan[k]);
                    }
                    if let Some(iface) = b.shader_interface(img.passes[k].pass.shader) {
                        // The frame block carries g_Time: a shader binding it is time-driven.
                        uses_time |= iface.uses_frame_globals || iface.field("g_Time").is_some();
                        uses_pointer |=
                            iface.uses_frame_globals || iface.field("g_PointerPosition").is_some();
                    }
                    // Image values (§13.2): live (Q7).
                    let s = &project.settings;
                    let values = img.values;
                    let alpha = s.value(values.alpha).float();
                    let color = s.value(values.color).vec4();
                    let bright = s.value(values.brightness).float();
                    let declared: Vec<&str> = img.passes[k]
                        .constants
                        .iter()
                        .map(|c| c.0.uniform.as_str())
                        .collect();
                    let mut put =
                        |b: &mut SceneBuilder<'_>,
                         name: &str,
                         v: &[f32],
                         setting: Option<(SettingId, u8)>| {
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
                    put(&mut b, "g_Alpha", &[alpha], Some((values.alpha, 1)));
                    put(&mut b, "g_UserAlpha", &[alpha], Some((values.alpha, 1)));
                    put(
                        &mut b,
                        "g_Color",
                        &color.truncate().to_array(),
                        Some((values.color, 3)),
                    );
                    put(
                        &mut b,
                        "g_Color4",
                        &color.to_array(),
                        Some((values.color, 4)),
                    );
                    put(
                        &mut b,
                        "g_Brightness",
                        &[bright],
                        Some((values.brightness, 1)),
                    );
                    if !declared.contains(&"g_CompositeColor") {
                        put(
                            &mut b,
                            "g_CompositeColor",
                            &color.truncate().to_array(),
                            Some((values.color, 3)),
                        );
                    }
                    put(&mut b, "g_LightAmbientColor", &ambient.to_array(), None);
                    put(&mut b, "g_LightSkylightColor", &skylight.to_array(), None);
                    // Material constants (§13.1).
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
                // Animated main texture: frame selection on the passes that bind it.
                if let Some(main) = &img.main
                    && main.is_animated()
                {
                    uses_time = true;
                    for (k, &pass) in handle.passes.iter().enumerate() {
                        if img.passes[k].pass.slot0_variants.is_empty() {
                            continue;
                        }
                        animated_passes.push(AnimatedPass {
                            pass,
                            texture: main.clone(),
                            rotation: b.uniform(pass, "g_Texture0Rotation"),
                            translation: b.uniform(pass, "g_Texture0Translation"),
                        });
                    }
                }
                if let Some(&pi) = puppet_of.get(&img.object.index()) {
                    bind_puppet(&mut b, &mut puppets[pi], &img, &handle);
                }
                let transform = node(project.world_transform(img.object, &build));
                images.push(ImageRuntime {
                    object: img.object,
                    handle,
                    transform,
                    chain: None,
                    fullscreen: img.fullscreen,
                    parallax_depth: img.parallax_depth,
                    visible: visibility_chain(project, &build, img.object),
                    shown: true,
                });
            }
            Some(Pending::Particle(p)) => match p.finish(&mut b) {
                Ok(rt) => {
                    stats.particle_systems += 1;
                    stats.passes += 1;
                    particle_rts.push(rt);
                }
                Err(e) => tracing::warn!("particle system not rendered: {e}"),
            },
            None => {}
        }
    }

    // Children hanging from puppet attachments (spec 08 §9.10).
    let mut attached = attachments(project, &build, &raw, &puppet_of, &puppets);
    // A skeleton that neither draws a mesh nor carries a child does no work per frame.
    let carriers: std::collections::HashSet<usize> = attached
        .values()
        .filter_map(|s| match s {
            ChainStep::Attachment { puppet, .. } => Some(*puppet),
            ChainStep::Object(_) => None,
        })
        .collect();
    let mut remap = vec![usize::MAX; puppets.len()];
    let mut kept = Vec::with_capacity(puppets.len());
    for (i, p) in puppets.into_iter().enumerate() {
        if p.output.is_some() || carriers.contains(&i) {
            remap[i] = kept.len();
            kept.push(p);
        }
    }
    let puppets = kept;
    for step in attached.values_mut() {
        if let ChainStep::Attachment { puppet, .. } = step {
            *puppet = remap[*puppet];
        }
    }
    stats.puppets = puppets.len();
    stats.gpu_skinned_puppets = puppets
        .iter()
        .filter(|p| matches!(p.output, Some(PuppetOutput::Palette { .. })))
        .count();
    stats.attached_objects = attached.len();
    if !attached.is_empty() {
        for img in &mut images {
            img.chain = attachment_chain(project, &build, &attached, img.object);
        }
    }

    stats.textures = textures.len();
    stats.texture_bytes = textures.uploaded_bytes;
    stats.stand_in_shaders = shaders.replaced();
    let mut scene_rt = b.build().map_err(|e| e.to_string())?;
    // Images invisible at start: their passes are disabled (Q8, live visibility).
    for img in &mut images {
        let shown = chain_visible(&project.settings, &img.visible);
        if !shown {
            scene_rt.set_enabled(ctx, &img.handle.passes, false);
            img.shown = false;
        }
    }
    let general = project.scene.as_ref().ok_or("not a scene project")?.general;
    let parallax_on = opts.parallax && project.settings.value(general.camera_parallax).boolean();
    let camera = project.scene.as_ref().ok_or("not a scene project")?.camera;
    let fov = project.settings.value(camera.fov).float();
    Ok(SceneRuntime {
        animated: uses_time
            || !particle_rts.is_empty()
            || !animated_passes.is_empty()
            || puppets.iter().any(|p| p.puppet.is_animated())
            || scene_rt.accumulates(),
        uses_pointer: uses_pointer || parallax_on || !particle_rts.is_empty(),
        scene: scene_rt,
        full: st.full,
        logical,
        size,
        stats,
        build,
        view_projection,
        fov,
        near: settings_float(project, camera.nearz),
        far: settings_float(project, camera.farz),
        images,
        particles: particle_rts,
        puppets,
        animated_passes,
        live,
        parallax: Parallax::default(),
        parallax_on,
        parallax_amount: general.camera_parallax_amount,
        parallax_delay: general.camera_parallax_delay,
        parallax_influence: general.camera_parallax_mouse_influence,
        pointer_last: [0.0; 2],
    })
}

/// `(upper-case name, value)` combos.
type ComboList = Vec<(String, i32)>;

/// Material combos (with the `TEX0FORMAT` forcing of spec 04 §13.2) and override combos of a
/// pass, upper-case names (spec 02 §12.4).
fn split_combos(
    project: &Project,
    idx: ObjectIndex,
    passes: &ImagePasses,
    k: usize,
    main_format: MainTextureFormat,
) -> (ComboList, ComboList) {
    let mut material = ComboList::new();
    let mut overrides = ComboList::new();
    let Some(scene) = &project.scene else {
        return (material, overrides);
    };
    let Some(desc) = passes.passes.get(k) else {
        return (material, overrides);
    };
    let s = &project.strings;
    let add = |list: &mut Vec<(String, i32)>, name: &str, v: i32| {
        let name = name.to_ascii_uppercase();
        if !list.iter().any(|(n, _)| *n == name) {
            list.push((name, v));
        }
    };
    if let ScenePassKind::Material { material: m, pass } = desc.kind
        && let Some(mp) = scene.material(m).passes.get(pass as usize)
    {
        match main_format {
            MainTextureFormat::Rg88 => add(&mut material, "TEX0FORMAT", 8),
            MainTextureFormat::R8 => add(&mut material, "TEX0FORMAT", 9),
            MainTextureFormat::Other => {}
        }
        for c in &mp.combos {
            add(&mut material, s.resolve(c.key), c.value);
        }
    }
    match desc.over {
        lumen_scene::OverrideRef::Effect { effect, index } => {
            if let Some(ov) = scene
                .object(idx)
                .as_image()
                .and_then(|i| i.effects.get(effect as usize))
                .and_then(|e| e.overrides.get(index as usize))
            {
                for c in &ov.combos {
                    add(&mut overrides, s.resolve(c.key), c.value);
                }
            }
        }
        lumen_scene::OverrideRef::Synthetic(i) => {
            if let Some(ov) = passes.synthetic.get(i as usize) {
                for (key, v) in &ov.combos {
                    add(&mut overrides, s.resolve(*key), *v);
                }
            }
        }
        lumen_scene::OverrideRef::None => {}
    }
    (material, overrides)
}

/// `visible` settings of an object and of each created ancestor, nearest first.
///
/// Lumen deviation (parity policy rule 5): Wallpaper Engine hides the children of a hidden
/// layer; the reference ignores the parent's visibility (spec 02 Q16). Without it, alternative
/// layouts switched by a property (e.g. a "small characters" variant) show both at once.
pub(crate) fn visibility_chain(
    project: &Project,
    build: &SceneBuild,
    idx: ObjectIndex,
) -> Box<[SettingId]> {
    let Some(scene) = &project.scene else {
        return Box::new([]);
    };
    let mut chain = vec![scene.object(idx).visible_setting()];
    let mut cur = idx;
    while chain.len() <= lumen_scene::MAX_PARENT_DEPTH {
        let Some(parent) = scene
            .object(cur)
            .parent
            .and_then(|p| build.created_object(p))
        else {
            break;
        };
        chain.push(scene.object(parent).visible_setting());
        cur = parent;
    }
    chain.into_boxed_slice()
}

/// Effective visibility of a chain from [`visibility_chain`].
pub(crate) fn chain_visible(settings: &lumen_scene::Settings, chain: &[SettingId]) -> bool {
    chain.iter().all(|&s| settings.value(s).boolean())
}

fn settings_float(project: &Project, id: SettingId) -> f32 {
    project.settings.value(id).float()
}

/// Prepares one image in creation order. `Ok(None)`: nothing to draw.
#[allow(clippy::too_many_arguments)]
fn prepare_image(
    b: &mut SceneBuilder<'_>,
    project: &mut Project,
    vfs: &Vfs,
    textures: &mut TextureCache,
    shaders: &mut dyn ShaderProvider,
    build: &SceneBuild,
    idx: ObjectIndex,
    root: ScopeId,
    scene_target: RtId,
    scene_size: Vec2,
    opts: &SceneOptions,
    puppet: Option<&PuppetModel>,
) -> Result<Option<PendingImage>, String> {
    let ctx = b.context();
    let Some(passes) = build.image_passes[idx.index()].clone() else {
        return Ok(None);
    };
    if !passes.draws || passes.passes.is_empty() {
        return Ok(None);
    }
    let scene = project.scene.as_ref().ok_or("not a scene project")?;
    let obj = scene.object(idx);
    let id = obj.id;
    let img = obj.as_image().ok_or("not an image")?;
    let model = scene.model(img.model.ok_or("image without model")?);
    let (fullscreen, passthrough, solid) = (model.fullscreen, model.passthrough, model.solid_layer);
    // GPU skinning of the mesh pass through the shader's own path (spec 08 §9.8).
    let bones = puppet.map_or(0, PuppetModel::bone_count);
    let mut skinning = (1..=MAX_GPU_BONES).contains(&bones);
    let effect_ids: Vec<i32> = img.effects.iter().map(|e| e.id).collect();
    let effects: Vec<lumen_scene::EffectId> = img.effects.iter().map(|e| e.effect).collect();
    let values = ImageValues {
        alpha: img.alpha,
        color: img.color,
        brightness: img.brightness,
    };
    let (parallax_depth, alignment, mut s) = (img.parallax_depth, img.alignment, img.size);
    let passes = filter_passes(&passes, &opts.debug, &effect_ids);
    if passes.passes.is_empty() {
        return Ok(None);
    }

    // Main texture (spec 04 §8.2).
    let main_ref = project.main_texture(idx);
    let mut main_tex: Option<Rc<LoadedTexture>> = None;
    let mut main_name: Option<String> = None;
    let (main, main_sizes, main_flags, main_format) = match main_ref {
        Some(lumen_scene::TextureRef::File(sym)) => {
            let name = project.strings.resolve(sym).to_owned();
            let t = textures
                .get(b, vfs, &name)
                .ok_or_else(|| format!("main texture '{name}' cannot be loaded"))?;
            main_name = Some(name);
            let r = (
                TextureRef::Texture(t.id()),
                (t.storage, t.real),
                t.flags,
                t.main_format,
            );
            main_tex = Some(t);
            r
        }
        Some(lumen_scene::TextureRef::RenderTarget(sym)) => {
            let name = project.strings.resolve(sym);
            let rt = b
                .targets
                .resolve(root, name)
                .ok_or_else(|| format!("main render target '{name}' not found"))?;
            let sz = b.targets.get(rt).size();
            (
                TextureRef::Target(rt),
                (sz, sz),
                TexFlags(0),
                MainTextureFormat::Other,
            )
        }
        _ => {
            if solid && s == Vec2::ZERO {
                s = scene_size;
            }
            if s.x <= 0.0 || s.y <= 0.0 {
                return Err("image without texture and without size".into());
            }
            let sz = (trunc_u32(s.x).max(1), trunc_u32(s.y).max(1));
            let rt = b.targets.create_anonymous(
                ctx,
                "lumen.blank",
                sz,
                SamplerKey::for_render_target(TexFlags(0)),
            );
            (
                TextureRef::Target(rt),
                (sz, sz),
                TexFlags(0),
                MainTextureFormat::Other,
            )
        }
    };
    if s.x == 0.0 || s.y == 0.0 {
        s = Vec2::new(main_sizes.1.0 as f32, main_sizes.1.1 as f32);
    }
    if fullscreen {
        s = scene_size;
    }
    if s.x <= 0.0 || s.y <= 0.0 {
        return Err("zero-sized image".into());
    }
    let rt_sampler = composite_sampler(main_flags);
    let composites = b.targets.create_composites(
        ctx,
        i64::from(id),
        (trunc_u32(s.x).max(1), trunc_u32(s.y).max(1)),
        rt_sampler,
    );
    let image_scope = b.targets.new_scope(root);
    let mut effect_scopes: HashMap<u32, ScopeId> = HashMap::new();

    let mut prepared = Vec::with_capacity(passes.passes.len());
    for k in 0..passes.passes.len() {
        let desc = passes.passes[k];
        // Effect scope and its fbos (spec 04 §9.5), created on first use.
        let scope = match desc.effect {
            Some((ei, _)) => {
                if let Some(s) = effect_scopes.get(&ei) {
                    *s
                } else {
                    let sc = b.targets.new_scope(image_scope);
                    let scene = project.scene.as_ref().ok_or("not a scene project")?;
                    if let Some(eid) = effects.get(ei as usize) {
                        for fbo in &scene.effect(*eid).fbos {
                            let name = project.strings.resolve(fbo.name);
                            let scale = if fbo.scale > 0.0 { fbo.scale } else { 1.0 };
                            b.targets.create(
                                ctx,
                                sc,
                                name,
                                scaled_size((s.x, s.y), scale),
                                rt_sampler,
                            );
                        }
                    }
                    effect_scopes.insert(ei, sc);
                    sc
                }
            }
            None => image_scope,
        };
        let scene = project.scene.as_ref().ok_or("not a scene project")?;
        // The material's own blending: lumen-render applies the transfer of §9.8 itself
        // (`ImagePasses::blending` already carries it and must not be applied twice).
        let (shader_name, cull, kind, own_blend) = match desc.kind {
            ScenePassKind::Material { material, pass } => {
                let mp = scene
                    .material(material)
                    .passes
                    .get(pass as usize)
                    .ok_or("material pass out of range")?;
                (
                    project.strings.resolve(mp.shader).to_owned(),
                    match mp.cull_mode {
                        lumen_scene::CullMode::Normal => CullMode::Back,
                        lumen_scene::CullMode::NoCull => CullMode::None,
                    },
                    if k == 0 {
                        PassKind::ImageBase
                    } else {
                        PassKind::ImageEffect
                    },
                    mp.blending,
                )
            }
            ScenePassKind::Copy { .. } => (
                "commands/copy".to_owned(),
                CullMode::None,
                PassKind::Copy,
                lumen_scene::Blending::Normal,
            ),
        };
        let (material_combos, override_combos) =
            split_combos(project, idx, &passes, k, main_format);
        let used_slots: Vec<u32> = project
            .pass_compile_textures(idx, &passes, k)
            .entries()
            .iter()
            .map(|e| e.0)
            .collect();
        let mut request = ShaderRequest {
            kind,
            name: &shader_name,
            override_combos: &override_combos,
            material_combos: &material_combos,
            used_slots: &used_slots,
            slot0_format: main_tex.as_ref().map(|t| t.format).filter(|_| k == 0),
        };
        let shader = if k == 0 && skinning {
            // Engine combos, declared by no annotation: SKINNING and BONECOUNT = max(1, n).
            let mut combos = override_combos.clone();
            combos.push(("SKINNING".into(), 1));
            combos.push(("BONECOUNT".into(), bones as i32));
            request.override_combos = &combos;
            match shaders.pass_shader(b, vfs, &request)? {
                s if !s.stand_in => s,
                _ => {
                    // No usable skinning path: same shader without it, CPU skinning.
                    skinning = false;
                    request.override_combos = &override_combos;
                    shaders.pass_shader(b, vfs, &request)?
                }
            }
        } else {
            shaders.pass_shader(b, vfs, &request)?
        };
        // Texture slots (§12.3), highest-priority resolvable candidate.
        let defaults: Vec<(u32, &str)> = shader
            .texture_defaults
            .iter()
            .map(|(s, n)| (*s, n.as_str()))
            .collect();
        let slots_ref = project.pass_textures(idx, &passes, k, &defaults, &mut |name| {
            textures.exists(vfs, name)
        });
        let mut pass = ImagePass::new(format!("object {id} pass {k} ({shader_name})"), shader.id);
        pass.blend = blend(own_blend);
        pass.cull = cull;
        for (slot, r) in slots_ref {
            let slot = slot as usize;
            if slot >= pass.slots.len() {
                continue;
            }
            pass.slots[slot] = match r {
                lumen_scene::TextureRef::File(sym) => {
                    let name = project.strings.resolve(sym);
                    if k == 0 && slot == 0 && main_name.as_deref() == Some(name) {
                        None
                    } else {
                        textures
                            .get(b, vfs, name)
                            .map(|t| SlotSource::Texture(TextureRef::Texture(t.id())))
                    }
                }
                lumen_scene::TextureRef::RenderTarget(sym) => {
                    let name = project.strings.resolve(sym);
                    match b.targets.resolve(scope, name) {
                        Some(rt) => Some(SlotSource::Texture(TextureRef::Target(rt))),
                        None => {
                            tracing::warn!(
                                "object {id} pass {k}: render target '{name}' not found"
                            );
                            None
                        }
                    }
                }
                lumen_scene::TextureRef::Previous => Some(SlotSource::Previous),
                lumen_scene::TextureRef::OwnComposite => {
                    Some(SlotSource::Texture(TextureRef::Target(composites.a)))
                }
            };
        }
        if k == 0
            && let Some(t) = &main_tex
            && t.is_animated()
            && pass.slots[0].is_none()
        {
            pass.slot0_variants = t.ids.iter().map(|&i| TextureRef::Texture(i)).collect();
        }
        if let Some(target) = project.pass_target(idx, &passes, k) {
            let name = project.strings.resolve(target);
            pass.target = b.targets.resolve(scope, name);
            if pass.target.is_none() {
                tracing::warn!(
                    "object {id} pass {k}: target '{name}' not found, drawn as a chain pass"
                );
            }
        }
        // Material constants (§13.1).
        let aliases: Vec<&str> = shader.constants.iter().map(|c| c.key.as_str()).collect();
        let sources = project.pass_constants(idx, &passes, k, &aliases);
        let mut constants = Vec::with_capacity(sources.len());
        for (param, source) in shader.constants.iter().zip(sources) {
            let (value, setting) = match source {
                ConstantSource::Default => (Value::from_vec4(param.default.into()), None),
                ConstantSource::Setting(s) => (project.settings.value(s).clone(), Some(s)),
                ConstantSource::Fixed(v) => (v, None),
            };
            constants.push((param.clone(), value, setting));
        }
        prepared.push(PreparedPass { pass, constants });
    }

    // Puppet mesh (spec 04 §20) with its bone weights when it has a skeleton (spec 08).
    let puppet = puppet.map(|m| PuppetGeometry {
        vertices: m
            .mesh
            .placed_positions([s.x, s.y])
            .zip(&m.mesh.uvs)
            .map(|(position, uv)| Vertex { position, uv: *uv })
            .collect(),
        indices: m.mesh.indices.clone(),
        skin: if m.skeleton.is_some() {
            m.skin
                .iter()
                .map(|v| SkinVertex::new(v.joints, v.weights, v.normal, v.tangent))
                .collect()
        } else {
            Vec::new()
        },
    });
    let label = format!(
        "object {id} ({})",
        project
            .strings
            .resolve(project.scene.as_ref().ok_or("no scene")?.object(idx).name)
    );
    let final_to_scene = !(opts.debug.no_solid_final && solid);
    let spec = ImageSpec {
        label,
        size: s,
        alignment: align(alignment),
        main,
        main_sizes,
        animated: main_tex.as_ref().is_some_and(|t| t.is_animated()),
        passthrough,
        fullscreen,
        composites,
        scene_target,
        final_to_scene,
    };
    Ok(Some(PendingImage {
        object: idx,
        spec,
        passes: prepared,
        values,
        fullscreen,
        parallax_depth,
        main: main_tex,
        puppet,
        skinning,
    }))
}

/// Reads and parses the puppet file of an image's model (spec 08). Problems are logged with
/// the path; a mesh failure keeps the standard quad (spec 01 §3.1).
fn load_puppet(project: &Project, vfs: &Vfs, idx: ObjectIndex) -> Option<PuppetModel> {
    let scene = project.scene.as_ref()?;
    let obj = scene.object(idx);
    let path = project
        .strings
        .resolve(scene.model(obj.as_image()?.model?).puppet?);
    let id = obj.id;
    let bytes = vfs
        .read(path)
        .inspect_err(|e| tracing::warn!("object {id}: puppet '{path}': {e}"))
        .ok()?;
    match lumen_formats::mdl::parse_model(&bytes) {
        Ok(m) => {
            for w in &m.warnings {
                tracing::warn!("object {id}: puppet '{path}': {w}");
            }
            tracing::debug!(
                "object {id}: puppet '{path}': {} vertices, {} bones, {} attachment(s), {} animation(s)",
                m.mesh.positions.len(),
                m.bone_count(),
                m.attachments.len(),
                m.animations.len()
            );
            Some(m)
        }
        Err(e) => {
            tracing::warn!("object {id}: puppet '{path}': {e}");
            None
        }
    }
}

/// `key` of a raw `scene.json` object, also accepting the `{"value": …}` form.
fn raw_value<'a>(obj: Option<&'a Json>, key: &str) -> Option<&'a Json> {
    let v = obj?.get(key)?;
    Some(v.get("value").unwrap_or(v))
}

/// Skeleton and animation layers of a puppet image (spec 08 §9.1, §9.2). `None` without a
/// valid skeleton.
fn puppet_runtime(
    project: &Project,
    build: &SceneBuild,
    idx: ObjectIndex,
    model: &PuppetModel,
    raw: Option<&Json>,
) -> Option<PuppetRuntime> {
    let rig = Rig::new(model)?;
    let scene = project.scene.as_ref()?;
    let obj = scene.object(idx);
    let image = obj.as_image()?;
    let raw_layers = raw_value(raw, "animationlayers").and_then(Json::as_array);
    let settings = &project.settings;
    let mut layers = Vec::with_capacity(image.animation_layers.len());
    let mut layer_settings = Vec::with_capacity(image.animation_layers.len());
    for (i, l) in image.animation_layers.iter().enumerate() {
        let wanted = settings.value(l.animation).int();
        let animation = u32::try_from(wanted)
            .ok()
            .and_then(|a| rig.animation_index(a));
        if animation.is_none() {
            tracing::warn!(
                "object {}: animation layer {}: animation {wanted} not in the puppet, layer inactive",
                obj.id,
                l.id
            );
        }
        // Keys lumen-scene does not model (spec 08 §9.2), read from the raw object.
        let extra = raw_layers.and_then(|a| a.get(i));
        let flag = |key: &str| raw_value(extra, key).and_then(Json::as_bool) == Some(true);
        let mut layer = Layer::new(animation);
        layer.additive = flag("additive");
        layer.blend_in = flag("blendin");
        layer.blend_out = flag("blendout");
        layer.blend_time = raw_value(extra, "blendtime")
            .and_then(Json::as_f64)
            .map_or(0.5, |v| v as f32);
        layer.rate = settings.value(l.rate).float();
        layer.blend = settings.value(l.blend).float();
        layer.visible = settings.value(l.visible).boolean();
        if let Some(a) = animation.and_then(|a| rig.animations().get(a))
            && a.mode == lumen_formats::mdl::AnimationMode::Unknown
        {
            tracing::warn!(
                "object {}: animation {} has mode '{}', played as 'loop'",
                obj.id,
                a.id,
                a.mode_name
            );
        }
        layers.push(layer);
        layer_settings.push((l.rate, l.blend, l.visible));
    }
    let mut puppet = Puppet::new(rig, layers);
    puppet.evaluate();
    Some(PuppetRuntime {
        puppet,
        visible: visibility_chain(project, build, idx),
        layer_settings,
        output: None,
        pending: true,
    })
}

/// Chooses how the pose of a drawn puppet reaches its mesh: the `g_Bones` palette when the
/// mesh passes were compiled with `SKINNING` and expose it, CPU skinning otherwise.
fn bind_puppet(
    b: &mut SceneBuilder<'_>,
    rt: &mut PuppetRuntime,
    img: &PendingImage,
    handle: &ImageHandle,
) {
    let (Some(range), Some(geometry)) = (handle.puppet_vertices(), img.puppet.as_ref()) else {
        return;
    };
    if geometry.skin.len() != geometry.vertices.len() {
        return;
    }
    let n = rt.puppet.rig().bone_count();
    let half = img.spec.size * 0.5;
    let palette_slot = |b: &SceneBuilder<'_>, pass: PassId| {
        b.uniform(pass, "g_Bones").filter(|slot| {
            matches!(&slot.ty, UniformType::Array { element, len, .. }
                if **element == UniformType::Matrix { columns: 4, rows: 3 } && *len as usize >= n)
        })
    };
    let slots: Vec<UniformSlot> = handle
        .puppet_passes()
        .iter()
        .filter_map(|&p| palette_slot(b, p))
        .collect();
    let reads_skin = b
        .shader_interface(img.passes[0].pass.shader)
        .is_some_and(|i| {
            ["a_BlendIndices", "a_BlendWeights"]
                .iter()
                .all(|name| i.vertex_input_names.iter().any(|(_, n)| n == name))
        });
    let gpu = img.skinning && reads_skin && slots.len() == handle.puppet_passes().len();
    let mut palette = vec![0.0; 12 * n];
    if gpu {
        rt.puppet.write_palette(half, &mut palette);
    } else {
        // Skinned on the CPU: a palette the shader may still read must be the identity.
        for m in palette.as_chunks_mut::<12>().0 {
            m.copy_from_slice(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
        }
    }
    for slot in &slots {
        b.shadow_mut().write_floats(slot, &palette);
    }
    rt.output = Some(if gpu {
        PuppetOutput::Palette {
            slots,
            half,
            palette,
        }
    } else {
        tracing::debug!("{}: puppet skinned on the CPU", img.spec.label);
        PuppetOutput::Vertices {
            range,
            half,
            rest: geometry.vertices.clone(),
            skin: geometry.skin.clone(),
            out: geometry.vertices.clone(),
        }
    });
    rt.pending = true;
}

/// Objects hanging from an attachment of their parent's puppet: object index →
/// `(puppet, attachment)` (spec 08 §9.9). Invalid keys are logged once and ignored.
fn attachments(
    project: &Project,
    build: &SceneBuild,
    raw: &HashMap<i64, Json>,
    puppet_of: &HashMap<usize, usize>,
    puppets: &[PuppetRuntime],
) -> HashMap<usize, ChainStep> {
    let mut out = HashMap::new();
    let Some(scene) = &project.scene else {
        return out;
    };
    for (i, obj) in scene.objects.iter().enumerate() {
        let Some(name) =
            raw_value(raw.get(&i64::from(obj.id)), "attachment").and_then(Json::as_str)
        else {
            continue;
        };
        let Some(parent) = obj.parent.and_then(|p| build.created_object(p)) else {
            continue;
        };
        let found = puppet_of.get(&parent.index()).and_then(|&pi| {
            puppets[pi]
                .puppet
                .rig()
                .attachment(name)
                .map(|index| ChainStep::Attachment { puppet: pi, index })
        });
        match found {
            Some(step) => {
                out.insert(i, step);
            }
            None => tracing::warn!(
                "object {}: attachment '{name}' not found in the puppet of its parent, ignored",
                obj.id
            ),
        }
    }
    out
}

/// Root-to-object transform chain with the attachment nodes inserted, or `None` when no
/// attachment is involved.
fn attachment_chain(
    project: &Project,
    build: &SceneBuild,
    attached: &HashMap<usize, ChainStep>,
    idx: ObjectIndex,
) -> Option<Box<[ChainStep]>> {
    let scene = project.scene.as_ref()?;
    let mut steps = Vec::new();
    let mut any = false;
    let mut cur = idx;
    loop {
        steps.push(ChainStep::Object(cur));
        if let Some(step) = attached.get(&cur.index()) {
            steps.push(*step);
            any = true;
        }
        if steps.len() > 2 * (lumen_scene::MAX_PARENT_DEPTH + 1) {
            break;
        }
        match scene
            .object(cur)
            .parent
            .and_then(|p| build.created_object(p))
        {
            Some(parent) => cur = parent,
            None => break,
        }
    }
    steps.reverse();
    any.then(|| steps.into_boxed_slice())
}

/// Resolved transform of a chain (spec 04 §8.1, spec 08 §9.10). Allocation-free.
fn compose_chain(
    project: &Project,
    puppets: &[PuppetRuntime],
    chain: &[ChainStep],
) -> NodeTransform {
    let mut acc = lumen_scene::Transform::default();
    for step in chain {
        let local = match *step {
            ChainStep::Object(i) => project.local_transform(i),
            ChainStep::Attachment { puppet, index } => {
                let n = decompose_attachment(&puppets[puppet].puppet.attachment_frame(index));
                lumen_scene::Transform {
                    origin: n.origin,
                    scale: n.scale,
                    angle: n.angle,
                }
            }
        };
        acc = acc.then(&local);
    }
    node(acc)
}

impl SceneRuntime {
    /// Per-frame update: clocks, pointer, parallax, images, animated textures, particles.
    /// Allocation-free; only marks the scene dirty when something changed.
    pub fn update(&mut self, ctx: &GpuContext, project: &Project, f: &FrameState<'_>) {
        let time = f.time as f32;
        self.scene.update_frame(&FrameInputs {
            time,
            daytime: f.daytime,
            pointer: f.pointer.position.to_array(),
            pointer_last: self.pointer_last,
            audio16: f.audio16,
            audio32: f.audio32,
            audio64: f.audio64,
        });
        self.pointer_last = f.pointer.position.to_array();
        let settings = &project.settings;
        let amount = settings.value(self.parallax_amount).float();
        if self.parallax_on {
            self.parallax.update(
                f.pointer.position,
                amount,
                settings.value(self.parallax_influence).float(),
                settings.value(self.parallax_delay).float(),
                f.dt,
                ParallaxSmoothing::WallpaperEngine,
            );
        }
        // Puppets (spec 08 §9.11): clocks, poses, then their meshes; before the transforms.
        let dt = f64::from(f.dt).clamp(0.0, MAX_STEP);
        for p in &mut self.puppets {
            for (l, &(rate, blend, visible)) in p.puppet.layers.iter_mut().zip(&p.layer_settings) {
                l.rate = settings.value(rate).float();
                l.blend = settings.value(blend).float();
                l.visible = settings.value(visible).boolean();
            }
            if chain_visible(settings, &p.visible) {
                p.puppet.advance(dt);
            }
            p.pending |= p.puppet.evaluate();
            if p.pending {
                p.write_output(&mut self.scene);
            }
        }
        let center = Vec3::new(self.size.x * 0.5, self.size.y * 0.5, 0.0);
        for img in &mut self.images {
            let shown = chain_visible(settings, &img.visible);
            if shown != img.shown {
                self.scene.set_enabled(ctx, &img.handle.passes, shown);
                img.shown = shown;
            }
            if !shown {
                continue;
            }
            let mut transform = match &img.chain {
                Some(chain) => compose_chain(project, &self.puppets, chain),
                None => img.transform,
            };
            if img.fullscreen {
                transform.origin = center;
            }
            let parallax = if self.parallax_on {
                parallax_translation(
                    settings.value(img.parallax_depth).vec2(),
                    amount,
                    self.parallax.displacement,
                    self.size.x,
                )
            } else {
                Vec3::ZERO
            };
            img.handle.update(
                &mut self.scene,
                &ImageFrame {
                    view_projection: self.view_projection,
                    transform,
                    parallax,
                    scene_size: self.size,
                },
            );
        }
        for a in &self.animated_passes {
            let Some(anim) = &a.texture.animation else {
                continue;
            };
            let n = a.texture.ids.len();
            let state = anim.animation.state_at(anim.total, f.time, n, |i| {
                anim.sizes.get(i).copied().unwrap_or([1, 1])
            });
            self.scene.set_variant(a.pass, state.image as usize);
            let sh = self.scene.shadow_mut();
            if let Some(s) = &a.rotation {
                sh.write_floats(s, &state.rotation);
            }
            if let Some(s) = &a.translation {
                sh.write_floats(s, &state.translation);
            }
        }
        if !self.particles.is_empty() {
            let pf = ParticleFrame {
                time,
                size: self.size,
                mouse: f.pointer.position,
                view_projection: self.view_projection,
                fov: self.fov,
                near: self.near,
                far: self.far,
                parallax: self.parallax_on.then_some(ParallaxInput {
                    displacement: self.parallax.displacement,
                    amount,
                }),
            };
            for p in &mut self.particles {
                p.update(ctx, &mut self.scene, settings, &pf);
            }
        }
    }

    /// A property changed: refresh transforms, live uniforms and particle bindings.
    pub fn property_changed(&mut self, project: &Project, name: &str) {
        for img in &mut self.images {
            img.transform = node(project.world_transform(img.object, &self.build));
        }
        let sh = self.scene.shadow_mut();
        for l in &self.live {
            let v = project.settings.value(l.setting);
            if l.components == 0 {
                sh.write_ints(&l.slot, &[v.int()]);
            } else {
                sh.write_floats(&l.slot, &value_floats(v, l.components));
            }
        }
        if let Some(id) = project.property(name) {
            let json = property_json(project.properties.get(id).value());
            for p in &mut self.particles {
                p.set_property(name, &json);
            }
        }
        self.scene.mark_dirty();
    }

    /// Live particles over every system (statistics).
    pub fn live_particles(&self) -> usize {
        self.particles.iter().map(ParticleRuntime::live).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_values_as_particle_json() {
        assert_eq!(property_json(&Value::from_bool(true)), Json::Bool(true));
        assert_eq!(property_json(&Value::from_int(3)), Json::from(3));
        assert_eq!(
            property_json(&Value::from_vec3(Vec3::new(1.0, 0.5, 0.0))),
            Json::String("1 0.5 0".into())
        );
        assert_eq!(
            property_json(&Value::from_text("abc")),
            Json::String("abc".into())
        );
    }

    #[test]
    fn uniform_views() {
        let v = Value::from_vec4(glam::Vec4::new(1.0, 2.0, 3.0, 4.0));
        assert_eq!(value_floats(&v, 1), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(value_floats(&v, 2), [1.0, 2.0, 0.0, 0.0]);
        assert_eq!(value_floats(&v, 3), [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(value_floats(&v, 4), [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn alignment_flags() {
        let a = align(lumen_scene::Alignment::parse("topleft"));
        assert_eq!((a.v, a.h), (VAlign::Top, HAlign::Left));
        let a = align(lumen_scene::Alignment::parse("center"));
        assert_eq!((a.v, a.h), (VAlign::Center, HAlign::Center));
        let a = align(lumen_scene::Alignment::parse("bottomright"));
        assert_eq!((a.v, a.h), (VAlign::Bottom, HAlign::Right));
    }
}

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
use lumen_particles::ParallaxInput;
use lumen_render::dynbuf::{FrameInputs, UniformSlot, frame_globals};
use lumen_render::image::composite_sampler;
use lumen_render::math::{
    Alignment as RAlign, HAlign, NodeTransform, OrthoCamera, Parallax, ParallaxSmoothing, Pointer,
    VAlign, parallax_translation, scene_logical_size, trunc_u32,
};
use lumen_render::targets::{ScopeId, scaled_size};
use lumen_render::texture::{SamplerKey, TexFlags};
use lumen_render::{
    BlendMode, ClampMode, CullMode, GpuContext, ImageFrame, ImageHandle, ImagePass, ImageSpec, Op,
    PassId, PuppetGeometry, RenderScene, RtId, SceneBuilder, SlotSource, TextureRef, Vertex,
    add_image_with_puppet,
};
use lumen_scene::{
    BuildOptions, ConstantSource, ImagePasses, MAX_PARENT_DEPTH, MainTextureFormat, ObjectIndex,
    ObjectKind, ParticleSource, PassKind as ScenePassKind, Project, SceneBuild, SettingId,
    Transform, Value, ValueTag,
};
use serde_json::Value as Json;

use crate::assets::SceneSource;
use crate::particles::{self, ParticleFrame, ParticleRuntime, PendingParticle};
use crate::plan::DebugPlan;
use crate::script::{SceneScripts, TextSink};
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
    fullscreen: bool,
    parallax_depth: SettingId,
    /// Own `visible` then each created ancestor's (see [`visibility_chain`]).
    visible: Box<[SettingId]>,
    shown: bool,
    /// Drawn size in pixels (script hit tests and `ILayer.size`).
    size: Vec2,
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
    /// Some pass reads the frame globals (`g_Time`, pointer, spectrum…). When none does, they
    /// are not rewritten, so a frame driven only by scripts that change nothing stays clean.
    frame_inputs: bool,
    /// SceneScript runtime; `None` (and no cost) for a scene without scripts.
    scripts: Option<Box<SceneScripts>>,
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
    /// Output-side inputs, read by scripts only.
    pub screen: ScreenState,
}

/// The output a frame is rendered for, as scripts see it (`engine.screenResolution`,
/// `input.cursorScreenPosition`, `input.cursorLeftDown`, `resizeScreen`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScreenState {
    /// Output size in pixels; `(0, 0)` = unknown (the scene size is used).
    pub size: (u32, u32),
    /// Cursor in output pixels, origin at the bottom-left corner.
    pub cursor: Vec2,
    /// Left mouse button held.
    pub left_down: bool,
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
pub(crate) fn value_floats(v: &Value, n: u8) -> [f32; 4] {
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
    let mut frame_inputs = false;
    let render_order = build.render_order.clone();
    for idx in render_order {
        if let Some((bl, targets, composites)) = &bloom
            && bl.object == idx
        {
            // The WE bloom materials, through lumen-render's five-pass chain (spec 04 §17).
            crate::bloom::add_scene_bloom(
                &mut b,
                project,
                &build,
                vfs,
                shaders,
                &st,
                targets,
                *composites,
                *bl,
            )?;
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
                        frame_inputs |= iface.uses_frame_globals
                            || iface
                                .uniforms
                                .iter()
                                .any(|u| frame_globals::FIELDS.iter().any(|g| g.0 == u.name));
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
                let transform = node(project.world_transform(img.object, &build));
                images.push(ImageRuntime {
                    object: img.object,
                    handle,
                    transform,
                    fullscreen: img.fullscreen,
                    parallax_depth: img.parallax_depth,
                    visible: visibility_chain(project, &build, img.object),
                    shown: true,
                    size: img.spec.size,
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
    let animated = uses_time
        || !particle_rts.is_empty()
        || !animated_passes.is_empty()
        || scene_rt.accumulates();
    // Spec 06 §8.1: scripts load once every object exists; what they write at load reaches
    // the renderer with the first update.
    let sizes: Vec<(ObjectIndex, Vec2)> = images.iter().map(|i| (i.object, i.size)).collect();
    let mut scripts = SceneScripts::load(project, &build, &sizes, size);
    if let Some(s) = &mut scripts {
        s.base_animated = animated;
    }
    // Particle and bloom shaders are not inspected: counted as readers.
    let frame_inputs = frame_inputs || !particle_rts.is_empty() || bloom.is_some();
    Ok(SceneRuntime {
        animated: animated || scripts.as_ref().is_some_and(|s| s.needs_frames()),
        uses_pointer: uses_pointer || parallax_on || !particle_rts.is_empty() || scripts.is_some(),
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
        animated_passes,
        live,
        parallax: Parallax::default(),
        parallax_on,
        parallax_amount: general.camera_parallax_amount,
        parallax_delay: general.camera_parallax_delay,
        parallax_influence: general.camera_parallax_mouse_influence,
        pointer_last: [0.0; 2],
        frame_inputs,
        scripts,
    })
}

/// `(upper-case name, value)` combos.
type ComboList = Vec<(String, i32)>;

/// Material combos (with the `TEX0FORMAT` forcing of spec 04 §13.2) and override combos of a
/// pass, upper-case names (spec 02 §12.4).
pub(crate) fn split_combos(
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

/// [`Project::world_transform`] without allocation (per-frame use by scripts): the same parent
/// walk, cut after [`MAX_PARENT_DEPTH`] parents, composed from the root down.
pub(crate) fn world_transform(
    project: &Project,
    build: &SceneBuild,
    idx: ObjectIndex,
) -> Transform {
    let Some(scene) = &project.scene else {
        return Transform::default();
    };
    let mut chain = [idx; MAX_PARENT_DEPTH + 1];
    let mut n = 1;
    let mut cur = idx;
    while let Some(p) = scene.objects.get(cur.index()).and_then(|o| o.parent) {
        let Some(j) = build.created_object(p) else {
            break;
        };
        if n > MAX_PARENT_DEPTH {
            break;
        }
        chain[n] = j;
        n += 1;
        cur = j;
    }
    let mut acc = project.local_transform(chain[n - 1]);
    for &i in chain[..n - 1].iter().rev() {
        acc = acc.then(&project.local_transform(i));
    }
    acc
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
    let puppet_path = model.puppet.map(|p| project.strings.resolve(p).to_owned());
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
        let (shader_name, cull, kind) = match desc.kind {
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
                )
            }
            ScenePassKind::Copy { .. } => {
                ("commands/copy".to_owned(), CullMode::None, PassKind::Copy)
            }
        };
        let (material_combos, override_combos) =
            split_combos(project, idx, &passes, k, main_format);
        let used_slots: Vec<u32> = project
            .pass_compile_textures(idx, &passes, k)
            .entries()
            .iter()
            .map(|e| e.0)
            .collect();
        let shader = shaders.pass_shader(
            b,
            vfs,
            &ShaderRequest {
                kind,
                name: &shader_name,
                override_combos: &override_combos,
                material_combos: &material_combos,
                used_slots: &used_slots,
                slot0_format: main_tex.as_ref().map(|t| t.format).filter(|_| k == 0),
            },
        )?;
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
        // Blending contract: lumen-scene gives the material's own mode, lumen-render applies
        // the first/last transfer of spec 04 §9.8 to the final chain (tests/blend_contract.rs).
        pass.blend = blend(desc.blending);
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

    // Puppet mesh in its rest pose (spec 04 §20; skeleton and animations are not applied).
    let puppet = puppet_path.and_then(|path| {
        let bytes = vfs
            .read(&path)
            .inspect_err(|e| tracing::warn!("object {id}: puppet '{path}': {e}"))
            .ok()?;
        match lumen_formats::mdl::parse_puppet(&bytes) {
            Ok(mesh) => Some(PuppetGeometry {
                vertices: mesh
                    .placed_positions([s.x, s.y])
                    .zip(&mesh.uvs)
                    .map(|(position, uv)| Vertex { position, uv: *uv })
                    .collect(),
                indices: mesh.indices,
            }),
            Err(e) => {
                tracing::warn!("object {id}: puppet '{path}': {e}");
                None
            }
        }
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
    }))
}

impl SceneRuntime {
    /// Per-frame update: scripts, clocks, pointer, parallax, images, animated textures,
    /// particles. Allocation-free (scripts too, for numeric and vector values); only marks the
    /// scene dirty when something changed.
    pub fn update(&mut self, ctx: &GpuContext, project: &mut Project, f: &FrameState<'_>) {
        if let Some(s) = &mut self.scripts {
            let changes = s.tick(project, &self.build, f, self.size);
            if changes.transforms {
                for img in &mut self.images {
                    img.transform = node(world_transform(project, &self.build, img.object));
                }
            }
            if changes.uniforms {
                write_live(&mut self.scene, &self.live, project);
            }
            self.animated = s.base_animated || s.needs_frames();
        }
        let project = &*project;
        let time = f.time as f32;
        if self.frame_inputs {
            self.scene.update_frame(&FrameInputs {
                time,
                daytime: f.daytime,
                pointer: f.pointer.position.to_array(),
                pointer_last: self.pointer_last,
                audio16: f.audio16,
                audio32: f.audio32,
                audio64: f.audio64,
            });
        }
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
            let mut transform = img.transform;
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

    /// A property changed: refresh transforms, live uniforms and particle bindings; scripts
    /// receive `applyUserProperties` at the next update.
    pub fn property_changed(&mut self, project: &Project, name: &str) {
        for img in &mut self.images {
            img.transform = node(project.world_transform(img.object, &self.build));
        }
        write_live(&mut self.scene, &self.live, project);
        if let Some(id) = project.property(name) {
            let json = property_json(project.properties.get(id).value());
            for p in &mut self.particles {
                p.set_property(name, &json);
            }
        }
        if let Some(s) = &mut self.scripts {
            s.property_changed(project, name);
            self.animated = true;
        }
        self.scene.mark_dirty();
    }

    /// Live particles over every system (statistics).
    pub fn live_particles(&self) -> usize {
        self.particles.iter().map(ParticleRuntime::live).sum()
    }

    /// Registers the receiver of the text computed by scripts for text layers (see
    /// [`TextSink`]). Does nothing for a scene without scripts.
    pub fn set_text_sink(&mut self, sink: Box<dyn TextSink>) {
        if let Some(s) = &mut self.scripts {
            s.set_text_sink(sink);
        }
    }

    /// Current text of a text object whose `text` is driven by a script (its initial text
    /// until the script changes it); `None` for other objects.
    pub fn script_text(&self, object: i32) -> Option<impl std::ops::Deref<Target = str> + '_> {
        self.scripts.as_ref()?.text(object)
    }

    /// Script counters, and the mean time of the per-frame script work; `None` without
    /// scripts.
    pub fn script_stats(&self) -> Option<(lumen_script::ScriptStats, std::time::Duration)> {
        self.scripts.as_ref().map(|s| (s.stats(), s.mean_tick()))
    }

    /// Media metadata or artwork changed (spec 06 §8.4): `mediaPropertiesChanged`,
    /// `mediaPlaybackChanged`, `mediaTimelineChanged` and `mediaThumbnailChanged` run at the
    /// next update. Entry point for a media source (none is wired yet).
    pub fn media_changed(&mut self, media: lumen_script::MediaSnapshot) {
        if let Some(s) = &mut self.scripts {
            s.media_changed(media);
            self.animated = true;
        }
    }

    /// A media source appeared or vanished (`mediaStatusChanged`, next update).
    pub fn media_status(&mut self, enabled: bool) {
        if let Some(s) = &mut self.scripts {
            s.media_status(enabled);
            self.animated = true;
        }
    }

    /// Runs `destroy()` of every script and writes `localStorage`. Call before dropping the
    /// scene; the scripts are inert afterwards.
    pub fn shutdown_scripts(&mut self, project: &mut Project) {
        if let Some(s) = &mut self.scripts {
            s.shutdown(project);
        }
        self.scripts = None;
    }
}

/// Rewrites every uniform fed by a setting. Unchanged values do not mark the scene dirty.
fn write_live(scene: &mut RenderScene, live: &[LiveUniform], project: &Project) {
    let sh = scene.shadow_mut();
    for l in live {
        let v = project.settings.value(l.setting);
        if l.components == 0 {
            sh.write_ints(&l.slot, &[v.int()]);
        } else {
            sh.write_floats(&l.slot, &value_floats(v, l.components));
        }
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

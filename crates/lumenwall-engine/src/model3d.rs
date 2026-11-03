//! 3D content of a scene (spec 09): model and sprite objects, the perspective camera and its
//! paths, lights, the shared depth buffer and the planar reflection `_rt_Reflection`.
//!
//! Built next to the 2D graph of `scene.rs`: [`Builder3d::prepare`] runs before the creation
//! loop (meshes, materials, shaders, textures, render targets), [`Builder3d::emit_reflection`]
//! and [`Builder3d::emit`] append the draws in render order, [`Scene3d::update`] writes the
//! camera, light and object uniforms of a frame (compare-on-write: an unchanged frame stays
//! clean). Nothing here runs for a scene without 3D content.

use std::collections::HashMap;

use glam::{Mat3, Mat4, Vec3, Vec4};
use lumenwall_assets::Vfs;
use lumenwall_formats::mdl::{StaticMesh, StaticModel, VertexLayout, parse_static_model};
use lumenwall_render::dynbuf::{UniformSlot, UniformType};
use lumenwall_render::math::{look_at, perspective_upstream, perspective_we};
use lumenwall_render::targets::ScopeId;
use lumenwall_render::texture::SamplerKey;
use lumenwall_render::{
    BlendMode, CullMode, DepthState, Geometry, GpuContext, MeshAttribute, MeshLayout, Op, PassDesc,
    PassId, RenderScene, RtId, SceneBuilder, ShaderId, SlotBinding, Vertex,
};
use lumenwall_scene::{
    DepthMode, LightKind, MaterialId, ObjectIndex, ObjectKind, Project, SceneBuild, SettingId,
    Value,
};
use serde_json::Value as Json;

use crate::assets::SceneSource;
use crate::scene::{LiveUniform, chain_visible, value_floats, visibility_chain};
use crate::shaders::{ConstantParam, PassKind, ShaderProvider, ShaderRequest};
use crate::texture::TextureCache;

/// Most lights a scene uses (spec 09 §2.2).
pub(crate) const MAX_LIGHTS: usize = 4;

/// Render target of the planar reflection (spec 09 §3.7).
const REFLECTION: &str = "_rt_Reflection";

/// One key frame of a camera path (spec 09 §1.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CameraKey {
    pub time: f32,
    pub pose: Pose,
}

/// Position, target and up vector of the camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pose {
    pub eye: Vec3,
    pub center: Vec3,
    pub up: Vec3,
}

/// A camera path: duration and key frames, instants already rescaled (§3.2.4 step 3).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CameraPath {
    pub duration: f32,
    pub keys: Vec<CameraKey>,
}

fn vec3_of(j: Option<&Json>) -> Option<Vec3> {
    let s = j?.as_str()?;
    let mut it = s.split_whitespace().map(str::parse::<f32>);
    let v = Vec3::new(it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    Some(v)
}

/// Paths of a `camera_*.json` file (spec 09 §1.3). Invalid entries are skipped.
pub(crate) fn parse_paths(json: &Json) -> Vec<CameraPath> {
    let mut out = Vec::new();
    for p in json
        .get("paths")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        let duration = p.get("duration").and_then(Json::as_f64).unwrap_or(0.0) as f32;
        let mut keys: Vec<CameraKey> = p
            .get("transforms")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| {
                Some(CameraKey {
                    time: t.get("timestamp").and_then(Json::as_f64).unwrap_or(0.0) as f32,
                    pose: Pose {
                        eye: vec3_of(t.get("eye"))?,
                        center: vec3_of(t.get("center"))?,
                        up: vec3_of(t.get("up")).unwrap_or(Vec3::Y),
                    },
                })
            })
            .collect();
        if keys.is_empty() || duration <= 0.0 {
            continue;
        }
        keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        // §3.2.4 step 3 (QO-3D-5): instants are stretched over the duration when the last one
        // differs from it (`demon_core`).
        let last = keys.last().map_or(0.0, |k| k.time);
        if last > 0.0 && last != duration {
            let k = duration / last;
            for key in &mut keys {
                key.time *= k;
            }
        }
        out.push(CameraPath { duration, keys });
    }
    out
}

/// Camera pose at scene time `time` (spec 09 §3.2.4): paths in order, each once, then the
/// first again; linear interpolation between the surrounding key frames, `up` renormalised.
pub(crate) fn path_pose(paths: &[CameraPath], time: f64) -> Option<Pose> {
    let total: f64 = paths.iter().map(|p| f64::from(p.duration)).sum();
    if paths.is_empty() || total <= 0.0 {
        return None;
    }
    let mut t = time.rem_euclid(total);
    let mut path = &paths[0];
    for p in paths {
        if t < f64::from(p.duration) {
            path = p;
            break;
        }
        t -= f64::from(p.duration);
    }
    let tau = t as f32;
    let keys = &path.keys;
    let pose = match keys.iter().position(|k| k.time > tau) {
        None => keys[keys.len() - 1].pose,
        Some(0) => keys[0].pose,
        Some(i) => {
            let (a, b) = (keys[i - 1], keys[i]);
            let span = b.time - a.time;
            let f = if span > 0.0 {
                (tau - a.time) / span
            } else {
                0.0
            };
            Pose {
                eye: a.pose.eye.lerp(b.pose.eye, f),
                center: a.pose.center.lerp(b.pose.center, f),
                up: a.pose.up.lerp(b.pose.up, f),
            }
        }
    };
    Some(Pose {
        up: pose.up.normalize_or(Vec3::Y),
        ..pose
    })
}

/// Local matrix of a 3D object (spec 09 §3.3): `T(origin)·Rz(a.z)·Ry(a.y)·Rx(a.x)·S(scale)`,
/// angles in radians (QO-3D-27).
pub(crate) fn local_matrix(origin: Vec3, angles: Vec3, scale: Vec3) -> Mat4 {
    Mat4::from_translation(origin)
        * Mat4::from_rotation_z(angles.z)
        * Mat4::from_rotation_y(angles.y)
        * Mat4::from_rotation_x(angles.x)
        * Mat4::from_scale(scale)
}

/// `(M₃ₓ₃)⁻ᵀ` (spec 09 §2.9), identity when `M` is singular.
fn normal_matrix(m: &Mat4) -> Mat3 {
    let m3 = Mat3::from_mat4(*m);
    if m3.determinant().abs() < 1e-12 {
        Mat3::IDENTITY
    } else {
        m3.inverse().transpose()
    }
}

/// Created objects from the root down to `idx` (parents through `build.created_object`).
fn chain_of(project: &Project, build: &SceneBuild, idx: ObjectIndex) -> Box<[ObjectIndex]> {
    let Some(scene) = &project.scene else {
        return Box::new([idx]);
    };
    let mut chain = vec![idx];
    let mut cur = idx;
    while chain.len() <= lumenwall_scene::MAX_PARENT_DEPTH {
        let Some(p) = scene
            .object(cur)
            .parent
            .and_then(|p| build.created_object(p))
        else {
            break;
        };
        chain.push(p);
        cur = p;
    }
    chain.reverse();
    chain.into_boxed_slice()
}

/// World matrix of a chain (spec 09 §3.3: `M = M_parent · L`). Allocation-free.
fn world_matrix(project: &Project, chain: &[ObjectIndex]) -> Mat4 {
    let Some(scene) = &project.scene else {
        return Mat4::IDENTITY;
    };
    let s = &project.settings;
    chain.iter().fold(Mat4::IDENTITY, |acc, &i| {
        let o = scene.object(i);
        let (scale, angles) = o.transform_settings();
        acc * local_matrix(
            s.value(o.origin).vec3(),
            angles.map_or(Vec3::ZERO, |a| s.value(a).vec3()),
            s.value(scale).vec3(),
        )
    })
}

/// Vertex attributes of a `.mdl` mesh, by shader input name (spec 09 §1.14).
fn mesh_layout(l: &VertexLayout) -> MeshLayout {
    use wgpu::VertexFormat as F;
    let mut attributes = vec![MeshAttribute {
        name: "a_Position".into(),
        format: F::Float32x3,
        offset: 0,
    }];
    let mut add = |name: &str, format: F, offset: Option<u32>| {
        if let Some(offset) = offset {
            attributes.push(MeshAttribute {
                name: name.into(),
                format,
                offset,
            });
        }
    };
    add("a_Normal", F::Float32x3, l.normal);
    add("a_Tangent4", F::Float32x4, l.tangent);
    add("a_BlendIndices", F::Uint32x4, l.joints);
    add("a_BlendWeights", F::Float32x4, l.weights);
    add("a_TexCoord", F::Float32x2, l.uv);
    // With one UV set the vertex fetch completes (u, v) into (u, v, 0, 1).
    add(
        "a_TexCoordVec4",
        if l.two_uv_sets {
            F::Float32x4
        } else {
            F::Float32x2
        },
        l.uv,
    );
    MeshLayout {
        stride: l.stride,
        attributes,
    }
}

/// Uniform slots of one 3D pass (spec 09 §2.9), resolved once.
#[derive(Default)]
struct Slots {
    model: Option<UniformSlot>,
    model_inverse: Option<UniformSlot>,
    normal_model: Option<UniformSlot>,
    view_projection: Option<UniformSlot>,
    mvp: Option<UniformSlot>,
    mvp_inverse: Option<UniformSlot>,
    effect_mvp: Option<UniformSlot>,
    effect_model: Option<UniformSlot>,
    eye: Option<UniformSlot>,
    view_right: Option<UniformSlot>,
    view_up: Option<UniformSlot>,
    lights_position: Option<UniformSlot>,
    lights_color: Option<UniformSlot>,
    lumen_position: Option<UniformSlot>,
    lumen_color: Option<UniformSlot>,
    ambient: Option<UniformSlot>,
    skylight: Option<UniformSlot>,
}

fn is_mat(slot: &UniformSlot, n: u8) -> bool {
    matches!(slot.ty, UniformType::Matrix { columns, rows } if columns == n && rows == n)
}

impl Slots {
    fn resolve(b: &SceneBuilder<'_>, pass: PassId) -> Self {
        let u = |name: &str| b.uniform(pass, name);
        // A declared type other than the engine's is not written (spec 03 §12.4).
        let m4 = |name: &str| u(name).filter(|s| is_mat(s, 4));
        Self {
            model: m4("g_ModelMatrix"),
            model_inverse: m4("g_ModelMatrixInverse"),
            normal_model: u("g_NormalModelMatrix").filter(|s| is_mat(s, 3)),
            view_projection: m4("g_ViewProjectionMatrix"),
            mvp: m4("g_ModelViewProjectionMatrix"),
            mvp_inverse: m4("g_ModelViewProjectionMatrixInverse"),
            effect_mvp: m4("g_EffectModelViewProjectionMatrix"),
            effect_model: m4("g_EffectModelMatrix"),
            eye: u("g_EyePosition"),
            view_right: u("g_ViewRight"),
            view_up: u("g_ViewUp"),
            lights_position: u("g_LightsPosition"),
            lights_color: u("g_LightsColorRadius"),
            // `LightingV1` module of lumenwall-shader (spec 09 §2.4).
            lumen_position: u("g_LumenLightPosition"),
            lumen_color: u("g_LumenLightColor"),
            ambient: u("g_LightAmbientColor"),
            skylight: u("g_LightSkylightColor"),
        }
    }
}

/// Camera values written into the passes of a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CameraFrame {
    /// `P · V` in the Wallpaper Engine convention (3D passes).
    pub view_projection: Mat4,
    /// The same camera for passes that keep the upstream clip fix (particles).
    pub view_projection_upstream: Mat4,
    pub eye: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
}

/// Light values of a frame (spec 09 §2.3).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LightFrame {
    positions: [f32; 3 * MAX_LIGHTS],
    color_radius: [f32; 4 * MAX_LIGHTS],
    /// `LightingV1`: (position, exponent).
    position_exponent: [f32; 4 * MAX_LIGHTS],
}

struct DrawPass {
    pass: PassId,
    /// Drawn into `_rt_Reflection` with the mirrored view.
    reflected: bool,
    slots: Slots,
}

/// A drawn 3D object (model or sprite).
struct ObjectRuntime {
    object: ObjectIndex,
    chain: Box<[ObjectIndex]>,
    visible: Box<[SettingId]>,
    shown: bool,
    passes: Vec<DrawPass>,
    pass_ids: Vec<PassId>,
}

struct LightRuntime {
    chain: Box<[ObjectIndex]>,
    visible: Box<[SettingId]>,
    color: SettingId,
    intensity: SettingId,
    radius: SettingId,
    exponent: SettingId,
}

/// One pass of a mesh, prepared before the draws exist.
struct PreparedPass {
    label: String,
    shader: ShaderId,
    blend: BlendMode,
    cull: CullMode,
    depth: DepthState,
    slots: [Option<SlotBinding>; 8],
    constants: Vec<(ConstantParam, Value, Option<SettingId>)>,
}

struct PreparedObject {
    /// Static mesh, or the sprite quad.
    geometry: Vec<(Geometry, Vec<PreparedPass>)>,
    reflected: bool,
    /// Centre of the model-space bounding box (transparent sorting, §3.6).
    center: Vec3,
}

/// Centre of the bounding box of the meshes of a model.
fn model_center(model: &StaticModel) -> Vec3 {
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for mesh in &model.meshes {
        match mesh.bounds {
            Some((a, b)) => {
                lo = lo.min(Vec3::from(a));
                hi = hi.max(Vec3::from(b));
            }
            None => {
                for i in 0..mesh.vertex_count() {
                    let p = Vec3::from(mesh.position(i));
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
            }
        }
    }
    if lo.x <= hi.x {
        (lo + hi) * 0.5
    } else {
        Vec3::ZERO
    }
}

/// Values shared by every 3D draw, read once.
#[derive(Clone, Copy)]
struct Common {
    target: RtId,
    reflection: Option<RtId>,
    clear: [f64; 4],
}

/// Load-time state of the 3D content of a scene.
pub(crate) struct Builder3d {
    perspective: bool,
    pose: Pose,
    fov: SettingId,
    near: f32,
    far: f32,
    ambient: SettingId,
    skylight: SettingId,
    paths: Vec<CameraPath>,
    /// §3.6: blended objects sorted back to front.
    sort_transparent: bool,
    shake: Option<Shake>,
    camera_fade: bool,
    fade: Option<FadePass>,
    common: Common,
    objects: HashMap<usize, PreparedObject>,
    lights: Vec<LightRuntime>,
    drawn: Vec<ObjectRuntime>,
    live: Vec<LiveUniform>,
    pub uses_time: bool,
    pub frame_inputs: bool,
    pub meshes: usize,
}

/// Material slots of a pass: material textures, then the shader's sampler defaults.
#[allow(clippy::too_many_arguments)]
fn pass_slots(
    b: &mut SceneBuilder<'_>,
    project: &Project,
    vfs: &Vfs,
    textures: &mut TextureCache,
    root: ScopeId,
    material: &lumenwall_scene::MaterialPass,
    defaults: &[(u32, String)],
    reflection: &mut Option<RtId>,
    size: (u32, u32),
    label: &str,
) -> [Option<SlotBinding>; 8] {
    let mut names: [Option<String>; 8] = Default::default();
    for &(slot, name) in material.textures.entries() {
        if let Some(n) = names.get_mut(slot as usize) {
            *n = Some(project.strings.resolve(name).to_owned());
        }
    }
    for (slot, name) in defaults {
        if let Some(n) = names.get_mut(*slot as usize)
            && n.is_none()
        {
            *n = Some(name.clone());
        }
    }
    let mut out = [None; 8];
    for (slot, name) in names.iter().enumerate() {
        let Some(name) = name else { continue };
        if name.starts_with("_alias_") {
            // Engine aliases (`_alias_lightCookie`, spec 09 §1.13): cookies are not rendered.
            tracing::debug!("{label}: slot {slot} '{name}' not provided");
            continue;
        }
        out[slot] = if name.starts_with("_rt_") {
            if name == REFLECTION && reflection.is_none() {
                // §3.7: created only when a material references it, in the scene scope.
                let ctx = b.context();
                let rt = b
                    .targets
                    .create(ctx, root, REFLECTION, size, SamplerKey::CLAMP_LINEAR);
                b.add_depth(rt);
                *reflection = Some(rt);
            }
            match b.targets.resolve(root, name) {
                Some(rt) => Some(SlotBinding::target(rt)),
                None => {
                    tracing::warn!("{label}: render target '{name}' not available");
                    None
                }
            }
        } else {
            textures
                .get(b, vfs, name)
                .map(|t| SlotBinding::texture(t.id()))
        };
    }
    out
}

/// Shader, states, textures and constants of every pass of a material (spec 09 §3.5.1).
#[allow(clippy::too_many_arguments)]
fn prepare_material(
    b: &mut SceneBuilder<'_>,
    project: &Project,
    vfs: &Vfs,
    textures: &mut TextureCache,
    shaders: &mut dyn ShaderProvider,
    root: ScopeId,
    material: MaterialId,
    reflection: &mut Option<RtId>,
    size: (u32, u32),
    label: &str,
    perspective: bool,
) -> Result<Vec<PreparedPass>, String> {
    let scene = project.scene.as_ref().ok_or("not a scene")?;
    let mut out = Vec::new();
    for (k, mp) in scene.material(material).passes.iter().enumerate() {
        let name = project.strings.resolve(mp.shader).to_owned();
        let material_combos: Vec<(String, i32)> = mp
            .combos
            .iter()
            .map(|c| (project.strings.resolve(c.key).to_owned(), c.value))
            .collect();
        // Engine combos of 3D passes (spec 09 §2.10): HLSL = 1 with GLSL = 1; models of a 2D
        // scene also get SCENE_ORTHO = 1 (§3.12, §2.4).
        let mut override_combos = vec![("HLSL".to_owned(), 1)];
        if !perspective {
            override_combos.push(("SCENE_ORTHO".to_owned(), 1));
        }
        let used_slots: Vec<u32> = mp.textures.entries().iter().map(|e| e.0).collect();
        let shader = shaders.pass_shader(
            b,
            vfs,
            &ShaderRequest {
                kind: PassKind::Model,
                name: &name,
                override_combos: &override_combos,
                material_combos: &material_combos,
                used_slots: &used_slots,
                slot0_format: None,
            },
        )?;
        let pass_label = format!("{label} pass {k} ({name})");
        let slots = pass_slots(
            b,
            project,
            vfs,
            textures,
            root,
            mp,
            &shader.texture_defaults,
            reflection,
            size,
            &pass_label,
        );
        let mut constants = Vec::with_capacity(shader.constants.len());
        for param in &shader.constants {
            let find = |list: &[lumenwall_scene::Constant]| {
                list.iter()
                    .find(|c| project.strings.resolve(c.name) == param.key)
                    .map(|c| c.setting)
            };
            // §1.11.3: `usershadervalues` win over `constantshadervalues`.
            let setting = find(&mp.user_constants).or_else(|| find(&mp.constants));
            let value = match setting {
                Some(s) => project.settings.value(s).clone(),
                None => Value::from_vec4(param.default.into()),
            };
            constants.push((param.clone(), value, setting));
        }
        out.push(PreparedPass {
            label: pass_label,
            shader: shader.id,
            blend: match mp.blending {
                lumenwall_scene::Blending::Normal => BlendMode::Normal,
                lumenwall_scene::Blending::Translucent => BlendMode::Translucent,
                lumenwall_scene::Blending::Additive => BlendMode::Additive,
            },
            cull: match mp.cull_mode {
                lumenwall_scene::CullMode::Normal => CullMode::Back,
                lumenwall_scene::CullMode::NoCull => CullMode::None,
            },
            depth: DepthState {
                test: mp.depth_test == DepthMode::Enabled,
                write: mp.depth_write == DepthMode::Enabled,
            },
            slots,
            constants,
        });
    }
    Ok(out)
}

impl Builder3d {
    /// Prepares the 3D content of a scene, before the creation loop. `None` when the scene is
    /// orthographic and has no model or sprite (2D scenes pay nothing).
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        b: &mut SceneBuilder<'_>,
        project: &mut Project,
        vfs: &Vfs,
        textures: &mut TextureCache,
        shaders: &mut dyn ShaderProvider,
        build: &SceneBuild,
        root: ScopeId,
        target: RtId,
        logical: (u32, u32),
    ) -> Option<Self> {
        let scene = project.scene.as_ref()?;
        let perspective = scene.general.orthogonal_projection.is_none();
        let candidates: Vec<ObjectIndex> = build
            .creation_order
            .iter()
            .copied()
            .filter(|i| {
                matches!(
                    scene.object(*i).kind,
                    ObjectKind::Model3d(_) | ObjectKind::Sprite(_)
                )
            })
            .collect();
        if !perspective && candidates.is_empty() {
            return None;
        }
        let v = &project.settings;
        let (mut near, mut far) = (
            v.value(scene.camera.nearz).float(),
            v.value(scene.camera.farz).float(),
        );
        if perspective && (near <= 0.0 || far <= near) {
            tracing::warn!(
                "camera planes {near}/{far} invalid, defaults 0.01/10000 used (spec 09 §3.2.2)"
            );
            (near, far) = (0.01, 10000.0);
        }
        let clear = build.clear_color;
        let mut this = Self {
            perspective,
            pose: Pose {
                eye: scene.camera.eye,
                center: scene.camera.center,
                up: scene.camera.up,
            },
            fov: scene.camera.fov,
            near,
            far,
            ambient: scene.general.ambient_color,
            skylight: scene.general.skylight_color,
            paths: read_paths(project, vfs),
            // QO-3D-30 settled on `neon_sunset`: its preview shows the terrain drawn before the
            // sun quad that comes first in the file (otherwise the quad's depth cuts a hole in
            // the terrain). Absent counts as true, like the key of WE's 3D templates.
            sort_transparent: scene.general.transparent_sorting != Some(false),
            shake: v
                .value(scene.general.camera_shake)
                .boolean()
                .then(|| Shake {
                    amplitude: v.value(scene.general.camera_shake_amplitude).float(),
                    roughness: v.value(scene.general.camera_shake_roughness).float(),
                    speed: v.value(scene.general.camera_shake_speed).float(),
                }),
            camera_fade: v.value(scene.general.camera_fade).boolean(),
            fade: None,
            common: Common {
                target,
                reflection: None,
                clear: [
                    f64::from(clear.x),
                    f64::from(clear.y),
                    f64::from(clear.z),
                    1.0,
                ],
            },
            objects: HashMap::new(),
            lights: Vec::new(),
            drawn: Vec::new(),
            live: Vec::new(),
            uses_time: false,
            frame_inputs: false,
            meshes: 0,
        };
        let mut reflection = None;
        for idx in candidates {
            let r = this.prepare_object(
                b,
                project,
                vfs,
                textures,
                shaders,
                root,
                idx,
                &mut reflection,
                logical,
            );
            let id = project.scene.as_ref().map_or(0, |s| s.object(idx).id);
            match r {
                Ok(Some(p)) => {
                    this.objects.insert(idx.index(), p);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("3D object {id} not drawn: {e}"),
            }
        }
        this.common.reflection = reflection;
        // §3.1: one depth buffer for the scene target, only when something 3D is drawn.
        if !this.objects.is_empty() {
            b.add_depth(target);
        }
        // §2.2: lights in file order.
        let scene = project.scene.as_ref()?;
        for &idx in &build.creation_order {
            if let ObjectKind::Light(l) = &scene.object(idx).kind {
                if l.kind != LightKind::Point {
                    tracing::warn!(
                        "light {}: {:?} lights are not rendered yet",
                        scene.object(idx).id,
                        l.kind
                    );
                    continue;
                }
                this.lights.push(LightRuntime {
                    chain: chain_of(project, build, idx),
                    visible: visibility_chain(project, build, idx),
                    color: l.color,
                    intensity: l.intensity,
                    radius: l.radius,
                    exponent: l.exponent,
                });
            }
        }
        Some(this)
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_object(
        &mut self,
        b: &mut SceneBuilder<'_>,
        project: &mut Project,
        vfs: &Vfs,
        textures: &mut TextureCache,
        shaders: &mut dyn ShaderProvider,
        root: ScopeId,
        idx: ObjectIndex,
        reflection: &mut Option<RtId>,
        size: (u32, u32),
    ) -> Result<Option<PreparedObject>, String> {
        let scene = project.scene.as_ref().ok_or("not a scene")?;
        let obj = scene.object(idx);
        let id = obj.id;
        let label = format!("object {id} ({})", project.strings.resolve(obj.name));
        match &obj.kind {
            ObjectKind::Model3d(m) => {
                let path = project.strings.resolve(m.file).to_owned();
                let (skin, reflected) = (m.skin, m.reflected);
                let bytes = vfs.read(&path).map_err(|e| format!("'{path}': {e}"))?;
                let model: StaticModel = parse_static_model(&bytes)
                    .map_err(|e| format!("'{path}' (MDLV{:04}): {e}", version_of(&bytes)))?;
                for w in &model.warnings {
                    tracing::warn!("{label}: '{path}': {w}");
                }
                let src = SceneSource(vfs);
                let mut geometry = Vec::with_capacity(model.meshes.len());
                for (k, mesh) in model.meshes.iter().enumerate() {
                    let Some((material_path, fallback)) =
                        model.material(k, u32::try_from(skin).unwrap_or(u32::MAX))
                    else {
                        continue;
                    };
                    if fallback {
                        tracing::warn!(
                            "{label}: skin {skin} missing in '{path}', skin 0 used (spec 09 §1.11.1)"
                        );
                    }
                    let material = match project.load_model_material(&src, material_path) {
                        Ok(Some(m)) => m,
                        Ok(None) => {
                            tracing::warn!("{label}: mesh {k}: material '{material_path}' missing");
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!("{label}: mesh {k}: {e}");
                            continue;
                        }
                    };
                    let mesh_label = format!("{label} mesh {k}");
                    let passes = match prepare_material(
                        b,
                        project,
                        vfs,
                        textures,
                        shaders,
                        root,
                        material,
                        reflection,
                        size,
                        &mesh_label,
                        self.perspective,
                    ) {
                        Ok(p) => p,
                        Err(e) => {
                            // §3.5.1: the other meshes stay drawn.
                            tracing::warn!("{mesh_label} not drawn: {e}");
                            continue;
                        }
                    };
                    let handle = upload_mesh(b, &mesh_label, mesh);
                    self.meshes += 1;
                    geometry.push((Geometry::Mesh(handle), passes));
                }
                // Meshes whose passes all blend (glass) are drawn after the opaque ones of the
                // model, in file order otherwise. Spec 09 §3.5.1 keeps the file order; the
                // `fantasticcar` preview shows the interior through the windshield, which comes
                // before it in `body.mdl` and writes depth (parity rule 5).
                geometry.sort_by_key(|(_, passes)| {
                    !passes.is_empty() && passes.iter().all(|p| p.blend != BlendMode::Normal)
                });
                Ok((!geometry.is_empty()).then_some(PreparedObject {
                    geometry,
                    reflected,
                    center: model_center(&model),
                }))
            }
            ObjectKind::Sprite(s) => {
                let Some(material) = s.material else {
                    return Err(format!(
                        "material '{}' missing",
                        project.strings.resolve(s.file)
                    ));
                };
                let passes = prepare_material(
                    b,
                    project,
                    vfs,
                    textures,
                    shaders,
                    root,
                    material,
                    reflection,
                    size,
                    &label,
                    self.perspective,
                )?;
                // §1.7: four vertices at the origin, UVs at the corners, two triangles.
                let corners = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
                let vertices: Vec<Vertex> = corners
                    .iter()
                    .map(|&uv| Vertex {
                        position: [0.0; 3],
                        uv,
                    })
                    .collect();
                let v = b.add_vertices(&vertices);
                let i = b.add_indices(&[0, 1, 2, 0, 2, 3]);
                Ok(Some(PreparedObject {
                    geometry: vec![(
                        Geometry::Indexed {
                            vertices: v,
                            indices: i,
                        },
                        passes,
                    )],
                    reflected: false,
                    center: Vec3::ZERO,
                }))
            }
            _ => Ok(None),
        }
    }

    /// Transparent sorting (spec 09 §3.6): the models whose passes all blend are drawn back to
    /// front (distance from the eye to their transformed bounding-box centre, file order on
    /// ties) from the rank of the first of them; the other objects that followed it come next,
    /// in their order (QO-3D-30: images such as `neon_sunset`'s film grain layer stay after
    /// the 3D content). Lumenwall sorts once, with the camera of the first frame.
    pub fn sort_render_order(
        &self,
        project: &Project,
        build: &SceneBuild,
        order: &mut [ObjectIndex],
    ) {
        if !self.sort_transparent {
            return;
        }
        let blended = |idx: &ObjectIndex| {
            self.objects.get(&idx.index()).is_some_and(|o| {
                !o.geometry.is_empty()
                    && o.geometry
                        .iter()
                        .flat_map(|(_, p)| p)
                        .all(|p| p.blend != BlendMode::Normal)
            })
        };
        let slots: Vec<usize> = (0..order.len()).filter(|&i| blended(&order[i])).collect();
        if slots.len() < 2 {
            return;
        }
        let eye = path_pose(&self.paths, 0.0).unwrap_or(self.pose).eye;
        let mut items: Vec<(f32, usize, ObjectIndex)> = slots
            .iter()
            .map(|&i| {
                let idx = order[i];
                let center = self.objects[&idx.index()].center;
                let world = world_matrix(project, &chain_of(project, build, idx));
                (world.transform_point3(center).distance(eye), i, idx)
            })
            .collect();
        items.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let first = slots[0];
        let rest: Vec<ObjectIndex> = order[first..]
            .iter()
            .copied()
            .filter(|i| !blended(i))
            .collect();
        let mut k = first;
        for (_, _, idx) in &items {
            order[k] = *idx;
            k += 1;
        }
        for idx in rest {
            order[k] = idx;
            k += 1;
        }
    }

    /// Whether `idx` is a prepared 3D object (drawn by [`Self::emit`]).
    pub fn owns(&self, idx: ObjectIndex) -> bool {
        self.objects.contains_key(&idx.index())
    }

    /// The reflection pass (spec 09 §3.7): cleared, then the visible `reflected` objects in
    /// render order with the mirrored view. Called before the scene target's clear.
    pub fn emit_reflection(
        &mut self,
        b: &mut SceneBuilder<'_>,
        project: &Project,
        build: &SceneBuild,
    ) {
        let Some(refl) = self.common.reflection else {
            return;
        };
        b.push(Op::Clear {
            target: refl,
            color: self.common.clear,
        });
        for &idx in &build.render_order {
            if !self.objects.get(&idx.index()).is_some_and(|o| o.reflected) {
                continue;
            }
            let passes = self.add_passes(b, project, idx, refl, true);
            self.attach(project, build, idx, passes);
        }
    }

    /// Main passes of a 3D object, at its rank in the render order. `false` when `idx` is not
    /// a prepared 3D object.
    pub fn emit(
        &mut self,
        b: &mut SceneBuilder<'_>,
        project: &Project,
        build: &SceneBuild,
        idx: ObjectIndex,
    ) -> bool {
        if !self.owns(idx) {
            return false;
        }
        let target = self.common.target;
        let passes = self.add_passes(b, project, idx, target, false);
        self.attach(project, build, idx, passes);
        true
    }

    fn attach(
        &mut self,
        project: &Project,
        build: &SceneBuild,
        idx: ObjectIndex,
        passes: Vec<DrawPass>,
    ) {
        let ids: Vec<PassId> = passes.iter().map(|p| p.pass).collect();
        match self.drawn.iter_mut().find(|o| o.object == idx) {
            Some(o) => {
                o.pass_ids.extend(&ids);
                o.passes.extend(passes);
            }
            None => self.drawn.push(ObjectRuntime {
                object: idx,
                chain: chain_of(project, build, idx),
                visible: visibility_chain(project, build, idx),
                shown: true,
                passes,
                pass_ids: ids,
            }),
        }
    }

    fn add_passes(
        &mut self,
        b: &mut SceneBuilder<'_>,
        project: &Project,
        idx: ObjectIndex,
        target: RtId,
        reflected: bool,
    ) -> Vec<DrawPass> {
        let mut out = Vec::new();
        let Some(obj) = self.objects.get(&idx.index()) else {
            return out;
        };
        let values = object_values(project, idx);
        for (geometry, passes) in &obj.geometry {
            for p in passes {
                let mut desc = PassDesc::new(p.label.clone(), p.shader, target, *geometry);
                desc.blend = p.blend;
                desc.cull = p.cull;
                desc.depth = p.depth;
                desc.slots = p.slots;
                // §3.5.1: counter-clockwise front faces in the WE convention. The reflection is
                // mirrored twice (world and clip y, see `Scene3d::update`): same winding.
                desc.front_face = wgpu::FrontFace::Ccw;
                let pass = match b.add_pass(desc) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("{}: {e}", p.label);
                        continue;
                    }
                };
                b.push(Op::Draw(pass));
                if let Some(iface) = b.shader_interface(p.shader) {
                    self.uses_time |= iface.uses_frame_globals || iface.field("g_Time").is_some();
                    self.frame_inputs |= iface.uses_frame_globals
                        || iface.uniforms.iter().any(|u| {
                            lumenwall_render::dynbuf::frame_globals::FIELDS
                                .iter()
                                .any(|g| g.0 == u.name)
                        });
                }
                for (param, value, setting) in &p.constants {
                    if let Some(slot) = b.uniform(pass, &param.uniform) {
                        if param.components == 0 {
                            b.shadow_mut().write_ints(&slot, &[value.int()]);
                        } else {
                            b.shadow_mut()
                                .write_floats(&slot, &value_floats(value, param.components));
                        }
                        if let Some(setting) = setting {
                            self.live.push(LiveUniform {
                                setting: *setting,
                                slot,
                                components: param.components,
                            });
                        }
                    }
                }
                // Object values (§2.9), live.
                if let Some((alpha, color, brightness)) = values {
                    let s = &project.settings;
                    let c = s.value(color).vec4();
                    for (name, v, setting, n) in [
                        ("g_Alpha", vec![s.value(alpha).float()], alpha, 1u8),
                        ("g_UserAlpha", vec![s.value(alpha).float()], alpha, 1),
                        ("g_Color", c.truncate().to_array().to_vec(), color, 3),
                        ("g_Color4", c.to_array().to_vec(), color, 4),
                        (
                            "g_Brightness",
                            vec![s.value(brightness).float()],
                            brightness,
                            1,
                        ),
                    ] {
                        if let Some(slot) = b.uniform(pass, name) {
                            b.shadow_mut().write_floats(&slot, &v);
                            self.live.push(LiveUniform {
                                setting,
                                slot,
                                components: n,
                            });
                        }
                    }
                }
                let size = b.scene_size();
                if let Some(slot) = b.uniform(pass, "g_Screen") {
                    b.shadow_mut()
                        .write_floats(&slot, &[size.0, size.1, size.0 / size.1.max(1.0)]);
                }
                out.push(DrawPass {
                    pass,
                    reflected,
                    slots: Slots::resolve(b, pass),
                });
            }
        }
        out
    }

    /// Runtime part, once every draw exists. Live uniforms join the scene's list.
    /// Camera fade (spec 09 §3.2.5): a pass drawn after everything else (bloom included) that
    /// darkens the scene target around path changes. Only with `camerafade` and two paths or
    /// more; disabled while the fade factor is 1.
    pub fn emit_fade(&mut self, b: &mut SceneBuilder<'_>) {
        if !self.camera_fade || self.paths.len() < 2 || !self.perspective {
            return;
        }
        const FADE_WGSL: &str = "struct Pass { g_LumenFade: vec4<f32> }\n@group(0) @binding(0) var<uniform> u: Pass;\n@vertex fn vs(@location(0) a_Position: vec3<f32>) -> @builtin(position) vec4<f32> { return vec4<f32>(a_Position.xy, 0.0, 1.0); }\n@fragment fn fs() -> @location(0) vec4<f32> { return u.g_LumenFade; }\n";
        let shader =
            lumenwall_render::ShaderDesc::from_wgsl("lumenwall.camerafade", FADE_WGSL, "vs", "fs")
                .map_err(|e| e.to_string())
                .and_then(|d| b.add_shader(d).map_err(|e| e.to_string()));
        let shader = match shader {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("camera fade not drawn: {e}");
                return;
            }
        };
        let quad = b.add_vertices(&lumenwall_render::math::unit_quad());
        let mut desc = PassDesc::new(
            "camera fade",
            shader,
            self.common.target,
            Geometry::Vertices(quad),
        );
        desc.blend = BlendMode::Translucent;
        let Ok(pass) = b.add_pass(desc) else { return };
        b.push(Op::Draw(pass));
        if let Some(slot) = b.uniform(pass, "g_LumenFade") {
            self.fade = Some(FadePass {
                pass,
                slot,
                shown: true,
            });
        }
    }

    pub fn finish(self, live: &mut Vec<LiveUniform>) -> Scene3d {
        live.extend(self.live);
        Scene3d {
            shake: self.shake,
            fade: self.fade,
            perspective: self.perspective,
            pose: self.pose,
            fov: self.fov,
            near: self.near,
            far: self.far,
            ambient: self.ambient,
            skylight: self.skylight,
            paths: self.paths,
            lights: self.lights,
            objects: self.drawn,
        }
    }
}

/// Camera paths of `scene.json` (spec 09 §1.3).
///
/// `LUMENWALL_STATIC_CAMERA=1` keeps the pose of `scene.json` (debugging aid: the editor
/// thumbnails `preview.jpg` are often taken from it).
fn read_paths(project: &Project, vfs: &Vfs) -> Vec<CameraPath> {
    let mut out = Vec::new();
    let Some(scene) = &project.scene else {
        return out;
    };
    if std::env::var_os("LUMENWALL_STATIC_CAMERA").is_some_and(|v| v == "1") {
        tracing::info!("LUMENWALL_STATIC_CAMERA: camera paths ignored");
        return out;
    }
    for &p in &scene.camera_paths {
        let path = project.strings.resolve(p);
        let json = vfs.read(path).map_err(|e| e.to_string()).and_then(|b| {
            serde_json::from_slice::<Json>(crate::particles::strip_bom(&b))
                .map_err(|e| e.to_string())
        });
        match json {
            Ok(j) => out.extend(parse_paths(&j)),
            Err(e) => tracing::warn!("camera path '{path}': {e}"),
        }
    }
    out
}

/// `.mdl` version digits for messages.
fn version_of(bytes: &[u8]) -> u32 {
    bytes
        .get(4..8)
        .and_then(|d| std::str::from_utf8(d).ok())
        .and_then(|d| d.parse().ok())
        .unwrap_or(0)
}

fn upload_mesh(
    b: &mut SceneBuilder<'_>,
    label: &str,
    mesh: &StaticMesh,
) -> lumenwall_render::MeshId {
    b.add_mesh(
        label,
        mesh_layout(&mesh.layout),
        &mesh.vertices,
        &mesh.indices,
    )
}

/// `alpha`, `color`, `brightness` settings of a model object.
fn object_values(project: &Project, idx: ObjectIndex) -> Option<(SettingId, SettingId, SettingId)> {
    match &project.scene.as_ref()?.object(idx).kind {
        ObjectKind::Model3d(m) => Some((m.alpha, m.color, m.brightness)),
        _ => None,
    }
}

/// 3D state of a running scene.
/// Camera shake of `general` (spec 09 §3.2.6).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shake {
    amplitude: f32,
    roughness: f32,
    speed: f32,
}

impl Shake {
    /// Offset added to the eye and the target (QO-3D-25, provisional): Perlin noise slices of
    /// distinct seeds per axis at `t·speed`, the vertical one with a double-frequency octave
    /// weighted by `roughness`. Deterministic and frame-rate independent.
    fn offset(&self, time: f64) -> Vec3 {
        // parity: WE's shake formula is unknown; this follows the proposal of spec 09 §3.2.6.
        let x = time * f64::from(self.speed);
        let n = |seed: f64, x: f64| lumenwall_particles::noise(x, seed, 0.5) as f32;
        let y = n(31.7, x) + self.roughness * n(53.3, 2.0 * x);
        Vec3::new(n(11.3, x), y, n(71.9, x)) * self.amplitude
    }
}

/// The camera fade pass and its colour uniform (black, alpha `1 − φ`).
struct FadePass {
    pass: PassId,
    slot: UniformSlot,
    shown: bool,
}

/// Fade factor `φ` of spec 09 §3.2.5 (QO-3D-24, provisional): 1 → 0 over the last 0.5 s of a
/// path, 0 → 1 over the first 0.5 s of the next (not at the very start of the wallpaper).
fn fade_factor(paths: &[CameraPath], time: f64) -> f32 {
    const RAMP: f64 = 0.5;
    let total: f64 = paths.iter().map(|p| f64::from(p.duration)).sum();
    if paths.len() < 2 || total <= 0.0 {
        return 1.0;
    }
    let mut t = time.rem_euclid(total);
    let first_cycle = time < total;
    for (k, p) in paths.iter().enumerate() {
        let d = f64::from(p.duration);
        if t < d {
            let fade_in = if k == 0 && first_cycle { 1.0 } else { t / RAMP };
            let fade_out = (d - t) / RAMP;
            return fade_in.min(fade_out).clamp(0.0, 1.0) as f32;
        }
        t -= d;
    }
    1.0
}

pub(crate) struct Scene3d {
    shake: Option<Shake>,
    fade: Option<FadePass>,
    perspective: bool,
    paths: Vec<CameraPath>,
    /// Static pose of `scene.json`.
    pose: Pose,
    fov: SettingId,
    near: f32,
    far: f32,
    ambient: SettingId,
    skylight: SettingId,
    lights: Vec<LightRuntime>,
    objects: Vec<ObjectRuntime>,
}

impl Scene3d {
    /// Whether the camera moves by itself (a path with distinct key frames, or shake).
    pub fn camera_moves(&self) -> bool {
        self.shake.is_some()
            || self.paths.len() > 1
            || self
                .paths
                .iter()
                .any(|p| p.keys.windows(2).any(|w| w[0].pose != w[1].pose))
    }

    /// Camera of the frame (spec 09 §3.2): pose from the paths or `scene.json`, `V`, `P`.
    pub fn camera(&self, project: &Project, time: f64, size: (f32, f32)) -> CameraFrame {
        let mut pose = path_pose(&self.paths, time).unwrap_or(self.pose);
        if let Some(shake) = &self.shake {
            let d = shake.offset(time);
            pose.eye += d;
            pose.center += d;
        }
        let aspect = size.0 / size.1.max(1.0);
        let (view, proj, proj_up) = if self.perspective {
            let fov = project.settings.value(self.fov).float();
            let fov = if fov > 0.0 && fov < 180.0 { fov } else { 50.0 };
            (
                look_at(pose.eye, pose.center, pose.up),
                perspective_we(fov, aspect, self.near, self.far),
                perspective_upstream(fov, aspect, self.near, self.far),
            )
        } else {
            // §3.12: models of a 2D scene, pixel units, y up, depth spread over ±farz.
            let (w, h) = size;
            let f = self.far.max(1.0);
            let ortho = Mat4::from_cols(
                Vec4::new(2.0 / w, 0.0, 0.0, 0.0),
                Vec4::new(0.0, 2.0 / h, 0.0, 0.0),
                Vec4::new(0.0, 0.0, -0.5 / f, 0.0),
                Vec4::new(0.0, 0.0, 0.5, 1.0),
            );
            let t = Mat4::from_translation(Vec3::new(-w / 2.0, -h / 2.0, 0.0));
            let flip = Mat4::from_scale(Vec3::new(1.0, -1.0, 2.0))
                * Mat4::from_translation(Vec3::new(0.0, 0.0, -0.5));
            (Mat4::IDENTITY, ortho * t, flip * ortho * t)
        };
        let rows = view.transpose();
        let right = rows.x_axis.truncate();
        let up = rows.y_axis.truncate();
        let forward = -rows.z_axis.truncate();
        let eye = if self.perspective {
            pose.eye
        } else {
            Vec3::new(size.0 / 2.0, size.1 / 2.0, self.far)
        };
        CameraFrame {
            view_projection: proj * view,
            view_projection_upstream: proj_up * view,
            eye,
            right,
            up,
            forward,
        }
    }

    /// Per-frame update: camera, lights, visibility and the uniforms of every 3D pass.
    /// Allocation-free; unchanged values do not dirty the scene.
    pub fn update(
        &mut self,
        ctx: &GpuContext,
        scene: &mut RenderScene,
        project: &Project,
        camera: &CameraFrame,
        time: f64,
    ) {
        if let Some(f) = &mut self.fade {
            let phi = fade_factor(&self.paths, time);
            let shown = phi < 1.0;
            if shown != f.shown {
                scene.set_enabled(ctx, &[f.pass], shown);
                f.shown = shown;
            }
            if shown {
                scene
                    .shadow_mut()
                    .write_floats(&f.slot, &[0.0, 0.0, 0.0, 1.0 - phi]);
            }
        }
        let settings = &project.settings;
        // §2.2, §2.3: the first four visible lights.
        let mut lights = LightFrame::default();
        let mut n = 0;
        for l in &self.lights {
            if n == MAX_LIGHTS || !chain_visible(settings, &l.visible) {
                continue;
            }
            let p = world_matrix(project, &l.chain).w_axis.truncate();
            // QO-3D-16: colour × intensity.
            let c = settings.value(l.color).vec3() * settings.value(l.intensity).float();
            lights.positions[3 * n..3 * n + 3].copy_from_slice(&p.to_array());
            lights.color_radius[4 * n..4 * n + 4].copy_from_slice(&[
                c.x,
                c.y,
                c.z,
                settings.value(l.radius).float(),
            ]);
            lights.position_exponent[4 * n..4 * n + 4].copy_from_slice(&[
                p.x,
                p.y,
                p.z,
                settings.value(l.exponent).float(),
            ]);
            n += 1;
        }
        let ambient = settings.value(self.ambient).vec3().to_array();
        let skylight = settings.value(self.skylight).vec3().to_array();
        // §3.7: mirrored view `V·Sc(1, −1, 1)` and eye. QO-3D-31: the shaders read the RT at
        // `v = y_ndc·0.5 + 0.5` (no HLSL branch), so the picture is stored upside down (clip y
        // flipped) for the reflection to land under the object at the same screen position, as
        // on the `fantasticcar` preview.
        let mirror = Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0));
        let reflected_vp = mirror * camera.view_projection * mirror;
        let reflected_eye = camera.eye * Vec3::new(1.0, -1.0, 1.0);
        for o in &mut self.objects {
            let shown = chain_visible(settings, &o.visible);
            if shown != o.shown {
                scene.set_enabled(ctx, &o.pass_ids, shown);
                o.shown = shown;
            }
            if !shown {
                continue;
            }
            let model = world_matrix(project, &o.chain);
            let model_inverse = model.inverse();
            let normal = normal_matrix(&model).to_cols_array();
            for p in &o.passes {
                let (vp, eye) = if p.reflected {
                    (reflected_vp, reflected_eye)
                } else {
                    (camera.view_projection, camera.eye)
                };
                let mvp = vp * model;
                let sh = scene.shadow_mut();
                let s = &p.slots;
                let m4 = |sh: &mut lumenwall_render::dynbuf::Shadow,
                          slot: &Option<UniformSlot>,
                          m: &Mat4| {
                    if let Some(slot) = slot {
                        sh.write_mat4(slot, m);
                    }
                };
                m4(sh, &s.model, &model);
                m4(sh, &s.effect_model, &model);
                m4(sh, &s.model_inverse, &model_inverse);
                m4(sh, &s.view_projection, &vp);
                m4(sh, &s.mvp, &mvp);
                m4(sh, &s.effect_mvp, &mvp);
                if s.mvp_inverse.is_some() {
                    m4(sh, &s.mvp_inverse, &mvp.inverse());
                }
                let fl = |sh: &mut lumenwall_render::dynbuf::Shadow,
                          slot: &Option<UniformSlot>,
                          v: &[f32]| {
                    if let Some(slot) = slot {
                        sh.write_floats(slot, v);
                    }
                };
                fl(sh, &s.normal_model, &normal);
                fl(sh, &s.eye, &eye.to_array());
                fl(sh, &s.view_right, &camera.right.to_array());
                fl(sh, &s.view_up, &camera.up.to_array());
                fl(sh, &s.lights_position, &lights.positions);
                fl(sh, &s.lights_color, &lights.color_radius);
                fl(sh, &s.lumen_position, &lights.position_exponent);
                fl(sh, &s.lumen_color, &lights.color_radius);
                fl(sh, &s.ambient, &ambient);
                fl(sh, &s.skylight, &skylight);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() < 1e-3
    }

    #[test]
    fn model_matrix_vectors_m_1_m_2_n_1() {
        // M-1 (spec 09 §4.5).
        let m = local_matrix(
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::new(0.0, std::f32::consts::FRAC_PI_2, 0.0),
            Vec3::splat(2.0),
        );
        assert!(close(m.transform_point3(Vec3::X), Vec3::new(1.0, 2.0, 1.0)));
        // M-2 (provisional, QO-3D-27): Rz·Ry·Rx.
        let m = local_matrix(
            Vec3::ZERO,
            Vec3::new(
                std::f32::consts::FRAC_PI_2,
                0.0,
                std::f32::consts::FRAC_PI_2,
            ),
            Vec3::ONE,
        );
        assert!(close(m.transform_point3(Vec3::Y), Vec3::Z));
        // N-1.
        let m = local_matrix(Vec3::ZERO, Vec3::ZERO, Vec3::new(1.0, 2.0, 1.0));
        let n = normal_matrix(&m) * Vec3::new(1.0, 1.0, 0.0);
        assert!(close(n, Vec3::new(1.0, 0.5, 0.0)));
        assert!(close(n.normalize(), Vec3::new(0.894427, 0.447214, 0.0)));
    }

    fn path(duration: f32, keys: &[(f32, [f32; 3])]) -> CameraPath {
        let json = serde_json::json!({ "paths": [{
            "duration": duration, "name": "",
            "transforms": keys.iter().map(|(t, e)| serde_json::json!({
                "timestamp": t, "eye": format!("{} {} {}", e[0], e[1], e[2]),
                "center": "0 0 0", "up": "0 2 0", "zoom": 1
            })).collect::<Vec<_>>()
        }]});
        parse_paths(&json).remove(0)
    }

    #[test]
    fn camera_paths_c_1_c_2_c_3() {
        // C-1: arsenal path 0 at τ = 15.
        let p = path(
            30.0,
            &[
                (0.0, [-3.544, 2.168, 2.274]),
                (30.0, [-2.618, 1.308, 2.399]),
            ],
        );
        let pose = path_pose(&[p], 15.0).unwrap();
        assert!(close(pose.eye, Vec3::new(-3.081, 1.738, 2.3365)));
        assert!(close(pose.up, Vec3::Y), "up renormalised");
        // C-2: demon_core, instants 0 and 40 over 300 s: half-way at τ = 150.
        let p = path(300.0, &[(0.0, [0.0, 0.0, 0.0]), (40.0, [10.0, 0.0, 0.0])]);
        assert_eq!(p.keys[1].time, 300.0);
        let pose = path_pose(&[p], 150.0).unwrap();
        assert!(close(pose.eye, Vec3::new(5.0, 0.0, 0.0)));
        // C-3: 30 s and 40 s paths, g_Time = 75 → path 0 at τ = 5.
        let a = path(30.0, &[(0.0, [0.0, 0.0, 0.0]), (30.0, [30.0, 0.0, 0.0])]);
        let b = path(40.0, &[(0.0, [100.0, 0.0, 0.0]), (40.0, [140.0, 0.0, 0.0])]);
        let pose = path_pose(&[a.clone(), b.clone()], 75.0).unwrap();
        assert!(close(pose.eye, Vec3::new(5.0, 0.0, 0.0)));
        let pose = path_pose(&[a, b], 40.0).unwrap();
        assert!(close(pose.eye, Vec3::new(110.0, 0.0, 0.0)));
        // One key frame: constant pose.
        let p = path(10.0, &[(0.0, [1.0, 2.0, 3.0])]);
        assert!(close(
            path_pose(&[p], 7.0).unwrap().eye,
            Vec3::new(1.0, 2.0, 3.0)
        ));
    }

    #[test]
    fn camera_fade_and_shake() {
        let a = path(30.0, &[(0.0, [0.0, 0.0, 0.0])]);
        let b = path(40.0, &[(0.0, [1.0, 0.0, 0.0])]);
        let paths = [a.clone(), b];
        assert_eq!(fade_factor(&paths, 0.0), 1.0, "no fade-in at start");
        assert_eq!(fade_factor(&paths, 10.0), 1.0);
        assert!((fade_factor(&paths, 29.75) - 0.5).abs() < 1e-4);
        assert!((fade_factor(&paths, 30.25) - 0.5).abs() < 1e-4);
        assert!(
            (fade_factor(&paths, 70.1) - 0.2).abs() < 1e-4,
            "second cycle fades in"
        );
        assert_eq!(fade_factor(&[a], 29.9), 1.0, "one path: no fade");
        let s = Shake {
            amplitude: 0.01,
            roughness: 0.1,
            speed: 5.0,
        };
        let d = s.offset(1.234);
        assert!(d.abs().max_element() <= 0.011 * 1.2 && d != Vec3::ZERO);
        assert_eq!(s.offset(1.234), d, "deterministic");
    }

    #[test]
    fn reflection_vector_r_1() {
        // R-1: the mirrored view sees (1, 2, 3) where V sees (1, −2, 3).
        let v = look_at(Vec3::new(0.0, 0.1, 3.3), Vec3::ZERO, Vec3::Y);
        let mirrored = v * Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0));
        let p = Vec3::new(1.0, 2.0, 3.0);
        assert!(close(
            mirrored.transform_point3(p),
            v.transform_point3(Vec3::new(1.0, -2.0, 3.0))
        ));
    }
}

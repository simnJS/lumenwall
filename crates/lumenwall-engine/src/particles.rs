//! Particle systems: `lumenwall-particles` simulation → a vertex stream of the render graph
//! (spec 05 §12). The CPU writes the vertices into one reused buffer, uploaded with one
//! `write_buffer` per system and frame; the index pattern is constant and uploaded once.

use glam::{Mat4, Vec2, Vec3};
use lumenwall_assets::Vfs;
use lumenwall_particles::{
    BlendMode as PBlend, DepthRange, FrameInput, GeometryKind, ParallaxInput, ParticleDefinition,
    ParticleObject, ParticleSystem, RenderInfo, SpriteSheet, SystemOptions, TextureInfo,
    UniformInput, UserValue, write_quad_indices,
};
use lumenwall_render::dynbuf::UniformSlot;
use lumenwall_render::{
    BlendMode, DepthState, Geometry, PassDesc, RenderScene, RtId, SceneBuilder, SlotBinding,
    StreamAttribute, StreamId, StreamLayout,
};
use lumenwall_scene::SettingId;
use serde_json::Value as Json;

use crate::model3d::{CameraFrame, local_matrix};
use crate::shaders::{PassKind, ShaderProvider, ShaderRequest};
use crate::texture::TextureCache;

/// First pass of a particle material (spec 05 §12.1).
struct ParticleMaterial {
    shader: String,
    blending: Option<String>,
    texture: Option<String>,
    combos: Vec<(String, i32)>,
    overbright: f32,
    constants: serde_json::Map<String, Json>,
    /// `usershadervalues`: `(user property, material constant)`; the constant follows the
    /// property (same key as for image materials, see `lumenwall-scene`).
    user_constants: Vec<(String, String)>,
    /// `depthtest` / `depthwrite` (also their misspelt forms), used in 3D scenes only.
    depth: DepthState,
}

fn read_material(vfs: &Vfs, path: &str) -> Result<ParticleMaterial, String> {
    let bytes = vfs
        .read(path)
        .map_err(|e| format!("material '{path}': {e}"))?;
    let json: Json =
        serde_json::from_slice(strip_bom(&bytes)).map_err(|e| format!("material '{path}': {e}"))?;
    let pass = json
        .get("passes")
        .and_then(Json::as_array)
        .and_then(|p| p.first())
        .ok_or_else(|| format!("material '{path}' has no pass"))?;
    let shader = pass
        .get("shader")
        .and_then(Json::as_str)
        .unwrap_or("genericparticle")
        .to_owned();
    let texture = pass
        .get("textures")
        .and_then(Json::as_array)
        .and_then(|t| t.iter().find_map(Json::as_str))
        .map(str::to_owned);
    let combos = pass
        .get("combos")
        .and_then(Json::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| {
                    let v = v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))?;
                    Some((k.to_ascii_uppercase(), v as i32))
                })
                .collect()
        })
        .unwrap_or_default();
    let overbright = pass
        .get("constantshadervalues")
        .and_then(|c| c.get("ui_editor_properties_overbright"))
        .and_then(|v| v.as_f64().or_else(|| v.get("value").and_then(Json::as_f64)))
        .unwrap_or(1.0) as f32;
    let enabled = |keys: [&str; 2]| {
        keys.iter()
            .find_map(|k| pass.get(*k).and_then(Json::as_str))
            .is_some_and(|v| v == "enabled")
    };
    let depth = DepthState {
        test: enabled(["depthtest", "depthtesting"]),
        write: enabled(["depthwrite", "depthwriting"]),
    };
    Ok(ParticleMaterial {
        depth,
        shader,
        blending: pass
            .get("blending")
            .and_then(Json::as_str)
            .map(str::to_owned),
        texture,
        combos,
        overbright,
        constants: pass
            .get("constantshadervalues")
            .and_then(Json::as_object)
            .cloned()
            .unwrap_or_default(),
        user_constants: pass
            .get("usershadervalues")
            .and_then(Json::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(p, k)| Some((p.clone(), k.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Float components of a `constantshadervalues` entry (number, `"x y z"`, or `{"value": …}`).
fn constant_floats(v: &Json) -> Option<[f32; 4]> {
    match v {
        Json::Number(n) => n.as_f64().map(|f| [f as f32, 0.0, 0.0, 0.0]),
        Json::Bool(b) => Some([f32::from(u8::from(*b)), 0.0, 0.0, 0.0]),
        Json::String(s) => {
            let mut out = [0.0; 4];
            for (i, t) in s.split_whitespace().take(4).enumerate() {
                out[i] = t.parse().ok()?;
            }
            Some(out)
        }
        Json::Object(o) => o.get("value").and_then(constant_floats),
        _ => None,
    }
}

pub(crate) fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)
}

/// Uniform slots written every frame.
#[derive(Default)]
struct Slots {
    model: Option<UniformSlot>,
    model_inverse: Option<UniformSlot>,
    view_projection: Option<UniformSlot>,
    mvp: Option<UniformSlot>,
    mvp_inverse: Option<UniformSlot>,
    orientation_up: Option<UniformSlot>,
    orientation_right: Option<UniformSlot>,
    orientation_forward: Option<UniformSlot>,
    view_up: Option<UniformSlot>,
    view_right: Option<UniformSlot>,
    eye: Option<UniformSlot>,
    render_var0: Option<UniformSlot>,
    render_var1: Option<UniformSlot>,
    brightness: Option<UniformSlot>,
    alpha: Option<UniformSlot>,
    user_alpha: Option<UniformSlot>,
    color: Option<UniformSlot>,
    color4: Option<UniformSlot>,
    refract: Option<UniformSlot>,
}

/// A particle system prepared at load (before its pass exists).
pub struct PendingParticle {
    pub object_id: i32,
    system: ParticleSystem,
    /// What [`ParticleRuntime::respawn`] needs; dropped by [`Self::finish`].
    def: ParticleDefinition,
    opts: SystemOptions,
    stream: StreamId,
    desc: PassDesc,
    overbright: f32,
    visible: Box<[SettingId]>,
    /// `(uniform, values, integer)` from the material constants.
    constants: Vec<(String, [f32; 4], bool)>,
    /// `(user property, uniform, integer)` of the `usershadervalues` constants.
    user_constants: Vec<(String, String, bool)>,
    world: Option<Mat4>,
}

/// A particle system of a running scene.
pub struct ParticleRuntime {
    system: ParticleSystem,
    stream: StreamId,
    slots: Slots,
    vertices: Vec<f32>,
    quad_floats: usize,
    overbright: f32,
    visible: Box<[SettingId]>,
    drawn: bool,
    /// `(user property, slot, integer)`: constants rewritten when the property changes.
    user_constants: Vec<(String, UniformSlot, bool)>,
    /// World model matrix of a system of a perspective scene (see [`world_model`]).
    world: Option<Mat4>,
    /// Definition and options of a system that can be started again (layers created by
    /// scripts, reused after `destroyLayer`); `None` for the scene's own systems.
    respawn: Option<Box<(ParticleDefinition, SystemOptions)>>,
    /// Scene object drawn (layers destroyed or created by scripts find their system by it).
    pub(crate) object: Option<lumenwall_scene::ObjectIndex>,
}

fn blend(b: PBlend) -> BlendMode {
    match b {
        PBlend::Normal => BlendMode::Normal,
        PBlend::Translucent => BlendMode::Translucent,
        PBlend::Additive => BlendMode::Additive,
    }
}

/// Applies the user properties to every value bound to them in a scene object.
fn bind_object_values(obj: &mut ParticleObject, props: &[(String, Json)]) {
    let set = |v: &mut UserValue| {
        if let Some(name) = v.property()
            && let Some((_, value)) = props.iter().find(|(n, _)| n == name)
        {
            v.set_from_json(value);
        }
    };
    for v in [
        &mut obj.origin,
        &mut obj.scale,
        &mut obj.angles,
        &mut obj.visible,
        &mut obj.parallax_depth,
    ] {
        set(v);
    }
    let o = &mut obj.instance_override;
    for v in [
        &mut o.enabled,
        &mut o.alpha,
        &mut o.size,
        &mut o.lifetime,
        &mut o.rate,
        &mut o.speed,
        &mut o.count,
        &mut o.color,
        &mut o.colorn,
    ] {
        set(v);
    }
}

/// Inputs of [`prepare`].
pub struct ParticleSetup<'a> {
    pub object_id: i32,
    /// The object as written in `scene.json`.
    pub object_json: &'a Json,
    /// Definition JSON (file or inline).
    pub definition: &'a Json,
    pub scene_size: Vec2,
    pub target: RtId,
    /// Own `visible` then the ancestors' (effective visibility).
    pub visible: Box<[SettingId]>,
    /// User properties as JSON values.
    pub properties: &'a [(String, Json)],
    /// Perspective scene (spec 09 §3.9): the system lives in world space.
    pub world: bool,
}

/// Builds the system, its stream and its pass description.
pub fn prepare(
    b: &mut SceneBuilder<'_>,
    vfs: &Vfs,
    textures: &mut TextureCache,
    shaders: &mut dyn ShaderProvider,
    s: &ParticleSetup<'_>,
) -> Result<PendingParticle, String> {
    let def = ParticleDefinition::from_json(s.definition);
    let material_path = def
        .material
        .clone()
        .ok_or("particle system without material")?;
    let material = read_material(vfs, &material_path)?;
    let texture = material
        .texture
        .as_deref()
        .and_then(|t| textures.get(b, vfs, t));
    let (sheet, info) = match &texture {
        Some(t) => {
            // spec 01 §2.9 / spec 05 §4.3: lumenwall-formats already resolved the sheet from TEXS
            // or the `.tex-json` companion; the particle crate's own resolution is the fallback.
            let sheet = if t.spritesheet.is_present() {
                SpriteSheet {
                    columns: t.spritesheet.columns,
                    rows: t.spritesheet.rows,
                    frames: t.spritesheet.frames,
                    duration: t.spritesheet.duration,
                }
            } else {
                let animated = t.animation.as_ref().map(|a| {
                    let first = a.animation.frames.first();
                    lumenwall_particles::AnimatedTexture {
                        frame0_width: first.map_or(0.0, |f| f.width_x.hypot(f.width_y)),
                        frame0_height: first.map_or(0.0, |f| f.height_x.hypot(f.height_y)),
                        frame_count: a.animation.frames.len() as u32,
                        total_duration: a.total as f32,
                    }
                });
                SpriteSheet::resolve(t.tex_json.as_ref(), animated.as_ref(), t.real.0, t.real.1)
            };
            (
                sheet,
                Some(TextureInfo {
                    gpu_width: t.storage.0,
                    gpu_height: t.storage.1,
                    image_width: t.real.0,
                    image_height: t.real.1,
                }),
            )
        }
        None => (SpriteSheet::default(), None),
    };
    let mut object = ParticleObject::from_json(s.object_json);
    bind_object_values(&mut object, s.properties);
    let world = s.world.then(|| world_model(&object));
    // In world space the system sees a zero-sized scene: no recentring of its origin.
    let (width, height) = if s.world {
        (0.0, 0.0)
    } else {
        (s.scene_size.x, s.scene_size.y)
    };
    let opts = SystemOptions {
        width,
        height,
        sprite_sheet: sheet,
        texture: info,
        seed: None,
    };
    let system = new_system(&def, &object, &opts, s.properties);
    let render: RenderInfo = system.render_info(material.blending.as_deref());
    let layout = StreamLayout {
        stride_floats: render.vertex_stride_floats as u32,
        attributes: render
            .attributes
            .iter()
            .map(|a| StreamAttribute {
                name: a.name.to_owned(),
                offset_floats: a.offset_floats,
                components: a.components,
            })
            .collect(),
    };
    let mut indices = Vec::new();
    write_quad_indices(&mut indices, render.max_quads);
    let label = format!("particles#{}", s.object_id);
    let stream = b.add_stream(&label, layout, render.max_vertices as u32, &indices);
    drop(indices);

    // spec 05 §12.1: combos imposed on top of the material's.
    let forced: Vec<(String, i32)> = render
        .combos
        .iter()
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
    let shader_name = render.shader_override.unwrap_or(&material.shader);
    let kind = match render.geometry {
        GeometryKind::Sprite => PassKind::Sprite {
            trail: forced.iter().any(|(k, v)| k == "TRAILRENDERER" && *v != 0),
        },
        GeometryKind::Rope => PassKind::Rope,
    };
    // TEX0FORMAT follows the format of the texture bound to slot 0 (spec 04 §13.2, spec 01 A.3).
    let mut material_combos = material.combos.clone();
    let tex0 = match texture.as_ref().map(|t| t.main_format) {
        Some(lumenwall_scene::MainTextureFormat::Rg88) => Some(8),
        Some(lumenwall_scene::MainTextureFormat::R8) => Some(9),
        _ => None,
    };
    if let Some(v) = tex0 {
        material_combos.retain(|(k, _)| k != "TEX0FORMAT");
        material_combos.insert(0, ("TEX0FORMAT".to_owned(), v));
    }
    let shader = shaders.pass_shader(
        b,
        vfs,
        &ShaderRequest {
            kind,
            name: shader_name,
            override_combos: &forced,
            material_combos: &material_combos,
            used_slots: &[0],
            slot0_format: texture.as_ref().map(|t| t.format),
        },
    )?;
    let mut desc = PassDesc::new(label, shader.id, s.target, Geometry::Stream(stream));
    desc.blend = blend(render.blending);
    if s.world {
        // spec 09 §3.9: depth test according to the material, against the models.
        desc.depth = material.depth;
    }
    if let Some(t) = &texture {
        // spec 05 §12.1: slot 0 is forced to the material's first texture.
        desc = desc.with_slot(0, SlotBinding::texture(t.id()));
    }
    let mut user_constants = Vec::new();
    let constants = shader
        .constants
        .iter()
        .map(|c| {
            let int = c.components == 0;
            let bound = material
                .user_constants
                .iter()
                .find(|(_, key)| *key == c.key)
                .and_then(|(prop, _)| s.properties.iter().find(|(n, _)| n == prop));
            if let Some((prop, value)) = bound {
                user_constants.push((prop.clone(), c.uniform.clone(), int));
                // Property values arrive as `property_json` writes them (numbers, "x y z w").
                if let Some(v) = constant_floats(value) {
                    return (c.uniform.clone(), v, int);
                }
            }
            let v = material
                .constants
                .get(&c.key)
                .and_then(constant_floats)
                .unwrap_or(c.default);
            (c.uniform.clone(), v, int)
        })
        .collect();
    Ok(PendingParticle {
        object_id: s.object_id,
        system,
        def,
        opts,
        stream,
        desc,
        overbright: material.overbright,
        visible: s.visible.clone(),
        constants,
        user_constants,
        world,
    })
}

/// World matrix of a particle system of a perspective scene (spec 09 §3.9): the model matrix of
/// spec 09 §3.3, times `Sc(1, −1, 1)`.
///
/// Lumenwall deviation (`lumenwall-particles` is unchanged): the simulation keeps the Y
/// inversions of spec 05 §2.2, which a world-space system must not have (QO-3D-33). The extra
/// mirror cancels them for every Y-inverted field (emitter origins, box directions, velocities,
/// gravity, turbulence frames); the fields spec 05 does not invert (sphere directions, control
/// point offsets, vortex axes) come out mirrored.
fn world_model(object: &ParticleObject) -> Mat4 {
    local_matrix(
        object.origin.vec3(),
        object.angles.vec3(),
        object.scale.vec3(),
    ) * Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0))
}

fn new_system(
    def: &ParticleDefinition,
    object: &ParticleObject,
    opts: &SystemOptions,
    properties: &[(String, Json)],
) -> ParticleSystem {
    let mut system = ParticleSystem::new(def, object, opts);
    for (name, value) in properties {
        system.set_user_property_json(name, value);
    }
    system
}

impl PendingParticle {
    /// [`Self::finish`] for a system that [`ParticleRuntime::respawn`] can start again.
    pub(crate) fn finish_respawnable(
        mut self,
        b: &mut SceneBuilder<'_>,
    ) -> Result<ParticleRuntime, String> {
        let respawn = Box::new((
            std::mem::take(&mut self.def),
            std::mem::take(&mut self.opts),
        ));
        let mut rt = self.finish(b)?;
        rt.respawn = Some(respawn);
        Ok(rt)
    }

    /// Adds the draw pass (render order) and resolves the uniform slots.
    pub fn finish(self, b: &mut SceneBuilder<'_>) -> Result<ParticleRuntime, String> {
        let pass = b
            .add_pass(self.desc)
            .map_err(|e| format!("particles#{}: {e}", self.object_id))?;
        b.push(lumenwall_render::Op::Draw(pass));
        for (name, v, int) in &self.constants {
            if let Some(slot) = b.uniform(pass, name) {
                if *int {
                    b.shadow_mut().write_ints(&slot, &[v[0] as i32]);
                } else {
                    b.shadow_mut().write_floats(&slot, v);
                }
            }
        }
        let user_constants = self
            .user_constants
            .into_iter()
            .filter_map(|(prop, name, int)| Some((prop, b.uniform(pass, &name)?, int)))
            .collect();
        let u = |n: &str| b.uniform(pass, n);
        let slots = Slots {
            model: u("g_ModelMatrix"),
            model_inverse: u("g_ModelMatrixInverse"),
            view_projection: u("g_ViewProjectionMatrix"),
            mvp: u("g_ModelViewProjectionMatrix"),
            mvp_inverse: u("g_ModelViewProjectionMatrixInverse"),
            orientation_up: u("g_OrientationUp"),
            orientation_right: u("g_OrientationRight"),
            orientation_forward: u("g_OrientationForward"),
            view_up: u("g_ViewUp"),
            view_right: u("g_ViewRight"),
            eye: u("g_EyePosition"),
            render_var0: u("g_RenderVar0"),
            render_var1: u("g_RenderVar1"),
            brightness: u("g_Brightness"),
            alpha: u("g_Alpha"),
            user_alpha: u("g_UserAlpha"),
            color: u("g_Color"),
            color4: u("g_Color4"),
            refract: u("g_RefractAmount"),
        };
        let quad_floats = self.system.quad_floats();
        let vertices = vec![0.0; self.system.max_quads() * quad_floats];
        Ok(ParticleRuntime {
            system: self.system,
            stream: self.stream,
            slots,
            vertices,
            quad_floats,
            overbright: self.overbright,
            visible: self.visible,
            drawn: false,
            user_constants,
            world: self.world,
            respawn: None,
            object: None,
        })
    }
}

/// Per-frame inputs shared by every system of a scene.
pub struct ParticleFrame {
    pub time: f32,
    pub size: Vec2,
    /// Pointer in scene texture coordinates (top-left origin).
    pub mouse: Vec2,
    pub view_projection: Mat4,
    pub fov: f32,
    pub near: f32,
    pub far: f32,
    pub parallax: Option<ParallaxInput>,
    /// Camera of a perspective scene (spec 09 §3.9); `None` in 2D.
    pub camera: Option<CameraFrame>,
}

impl ParticleRuntime {
    /// Simulates one frame and uploads the vertices. No allocation.
    pub fn update(
        &mut self,
        ctx: &lumenwall_render::GpuContext,
        scene: &mut RenderScene,
        settings: &lumenwall_scene::Settings,
        f: &ParticleFrame,
    ) {
        let visible = crate::scene::chain_visible(settings, &self.visible);
        if !visible {
            if self.drawn {
                scene.write_stream(ctx, self.stream, &[], 0);
                self.drawn = false;
            }
            return;
        }
        let (width, height) = if self.world.is_some() {
            (0.0, 0.0)
        } else {
            (f.size.x, f.size.y)
        };
        let input = FrameInput {
            time: f.time,
            width,
            height,
            mouse: f.mouse,
        };
        let quads = if self.system.update(&input) {
            self.system.write_vertices(&mut self.vertices)
        } else {
            0
        };
        let floats = quads * self.quad_floats;
        scene.write_stream(
            ctx,
            self.stream,
            &self.vertices[..floats],
            (quads * lumenwall_particles::QUAD_INDICES) as u32,
        );
        self.drawn = quads > 0;
        if quads == 0 {
            return;
        }
        let mut u = self.system.uniforms(&UniformInput {
            width: f.size.x,
            height: f.size.y,
            scene_view_projection: f.view_projection,
            fov_degrees: f.fov,
            near: f.near,
            far: f.far,
            // The pass shaders are in the upstream (GL) clip convention: lumenwall-render maps it.
            depth: DepthRange::NegOneToOne,
            parallax: f.parallax,
            brightness: self.overbright,
        });
        if let (Some(model), Some(cam)) = (self.world, f.camera) {
            // spec 09 §3.9: world space, the scene camera, no fixed eye at (0, 0, 1000).
            let inverse = model.inverse();
            let local = |v: Vec3| inverse.transform_vector3(v);
            u.model = model;
            u.model_inverse = inverse;
            u.view_projection = cam.view_projection_upstream;
            u.model_view_projection = cam.view_projection_upstream * model;
            u.model_view_projection_inverse = u.model_view_projection.inverse();
            u.eye_position = cam.eye;
            u.view_up = cam.up;
            u.view_right = cam.right;
            // Sprites face the camera: its axes in the system's local space.
            u.orientation_right = local(cam.right);
            u.orientation_up = local(cam.up);
            u.orientation_forward = local(-cam.forward);
        }
        let sh = scene.shadow_mut();
        let s = &self.slots;
        let m4 =
            |sh: &mut lumenwall_render::dynbuf::Shadow, slot: &Option<UniformSlot>, m: &Mat4| {
                if let Some(slot) = slot {
                    sh.write_mat4(slot, m);
                }
            };
        m4(sh, &s.model, &u.model);
        m4(sh, &s.model_inverse, &u.model_inverse);
        m4(sh, &s.view_projection, &u.view_projection);
        m4(sh, &s.mvp, &u.model_view_projection);
        m4(sh, &s.mvp_inverse, &u.model_view_projection_inverse);
        let fl =
            |sh: &mut lumenwall_render::dynbuf::Shadow, slot: &Option<UniformSlot>, v: &[f32]| {
                if let Some(slot) = slot {
                    sh.write_floats(slot, v);
                }
            };
        fl(sh, &s.orientation_up, &u.orientation_up.to_array());
        fl(sh, &s.orientation_right, &u.orientation_right.to_array());
        fl(
            sh,
            &s.orientation_forward,
            &u.orientation_forward.to_array(),
        );
        fl(sh, &s.view_up, &u.view_up.to_array());
        fl(sh, &s.view_right, &u.view_right.to_array());
        fl(sh, &s.eye, &u.eye_position.to_array());
        fl(sh, &s.render_var0, &u.render_var0.to_array());
        fl(sh, &s.render_var1, &u.render_var1.to_array());
        fl(sh, &s.brightness, &[u.brightness]);
        fl(sh, &s.alpha, &[u.alpha]);
        fl(sh, &s.user_alpha, &[u.alpha]);
        fl(sh, &s.color, &u.color.to_array());
        fl(sh, &s.color4, &u.color4.to_array());
        fl(sh, &s.refract, &[u.refract_amount]);
    }

    /// Re-applies a user property to the values and material constants bound to it.
    pub fn set_property(&mut self, scene: &mut RenderScene, name: &str, value: &Json) {
        self.system.set_user_property_json(name, value);
        for (prop, slot, int) in &self.user_constants {
            if prop != name {
                continue;
            }
            if let Some(v) = constant_floats(value) {
                if *int {
                    scene.shadow_mut().write_ints(slot, &[v[0] as i32]);
                } else {
                    scene.shadow_mut().write_floats(slot, &v);
                }
            }
        }
    }

    /// Live particles (statistics).
    pub fn live(&self) -> usize {
        self.system.len()
    }

    /// Starts the system again from its definition with the scene object `object_json` (a
    /// layer created by a script and reused: emitters fire anew at its current origin).
    /// Allocates the new simulation; the stream, pass and uniforms are kept. `false` when the
    /// system was not prepared with [`PendingParticle::finish_respawnable`].
    pub(crate) fn respawn(
        &mut self,
        scene: &mut RenderScene,
        ctx: &lumenwall_render::GpuContext,
        object_json: &Json,
        properties: &[(String, Json)],
    ) -> bool {
        let Some(r) = &self.respawn else {
            return false;
        };
        let mut object = ParticleObject::from_json(object_json);
        bind_object_values(&mut object, properties);
        self.system = new_system(&r.0, &object, &r.1, properties);
        if self.drawn {
            scene.write_stream(ctx, self.stream, &[], 0);
            self.drawn = false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenwall_assets::{Adapter, MemoryFiles};

    #[test]
    fn constants_from_json() {
        assert_eq!(
            constant_floats(&serde_json::json!(2.5)),
            Some([2.5, 0.0, 0.0, 0.0])
        );
        assert_eq!(
            constant_floats(&serde_json::json!("1 0.5 0.25")),
            Some([1.0, 0.5, 0.25, 0.0])
        );
        assert_eq!(
            constant_floats(&serde_json::json!({"user": "x", "value": true})),
            Some([1.0, 0.0, 0.0, 0.0])
        );
        assert_eq!(constant_floats(&serde_json::json!("a b")), None);
        assert_eq!(strip_bom(b"\xEF\xBB\xBF{}"), b"{}");
    }

    #[test]
    fn particle_material_first_pass() {
        let mut files = MemoryFiles::default();
        files.insert_static(
            "materials/p.json",
            br#"{"passes":[{"blending":"additive","shader":"genericparticle",
                "textures":[null,"particle/drop"],"combos":{"spritesheet":1},
                "constantshadervalues":{"ui_editor_properties_overbright":{"value":1.5}},
                "usershadervalues":{"schemecolor":"color1","bad":3}}]}"#,
        );
        let mut vfs = Vfs::new();
        vfs.mount_adapter("/", Adapter::Memory(files));
        let m = read_material(&vfs, "materials/p.json").unwrap();
        assert_eq!(m.shader, "genericparticle");
        assert_eq!(m.blending.as_deref(), Some("additive"));
        assert_eq!(m.texture.as_deref(), Some("particle/drop"));
        assert_eq!(m.combos, vec![("SPRITESHEET".to_owned(), 1)]);
        assert_eq!(m.overbright, 1.5);
        assert_eq!(
            m.user_constants,
            vec![("schemecolor".to_owned(), "color1".to_owned())]
        );
        assert!(read_material(&vfs, "materials/missing.json").is_err());
    }

    #[test]
    fn bound_object_values_follow_properties() {
        let json = serde_json::json!({
            "visible": {"user": "rain", "value": true},
            "instanceoverride": {"alpha": {"user": "a", "value": 0.5}}
        });
        let mut obj = ParticleObject::from_json(&json);
        bind_object_values(
            &mut obj,
            &[
                ("rain".to_owned(), Json::Bool(false)),
                ("a".to_owned(), serde_json::json!(0.25)),
            ],
        );
        assert!(!obj.visible.bool());
        assert_eq!(obj.instance_override.alpha.f32(), 0.25);
    }
}

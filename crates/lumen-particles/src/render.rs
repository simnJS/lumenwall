//! Geometry generation and render state (spec 05 §12).
//!
//! Vertices are written into a caller-provided buffer (a reused `Vec<f32>`, or a slice such as a
//! mapped staging buffer). The index pattern is constant (`0,1,2,2,3,0` + 4 per quad): generate it
//! once at capacity with [`write_quad_indices`] and only re-upload the vertices each frame.

use crate::model::{AnimationMode, RendererKind};
use crate::operator::life;
use crate::system::ParticleSystem;
use glam::{Mat4, Vec2, Vec3, Vec4};

/// Floats per sprite vertex (68-byte stride).
pub const SPRITE_VERTEX_FLOATS: usize = 17;
/// Floats per rope vertex (104-byte stride).
pub const ROPE_VERTEX_FLOATS: usize = 26;
/// Indices per quad.
pub const QUAD_INDICES: usize = 6;

/// One vertex attribute of the particle vertex formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexAttribute {
    /// Shader attribute name.
    pub name: &'static str,
    pub components: u32,
    /// Offset in floats from the start of the vertex.
    pub offset_floats: u32,
}

const fn attr(name: &'static str, components: u32, offset_floats: u32) -> VertexAttribute {
    VertexAttribute {
        name,
        components,
        offset_floats,
    }
}

/// Sprite vertex layout (§12.3).
pub const SPRITE_ATTRIBUTES: [VertexAttribute; 5] = [
    attr("a_Position", 3, 0),
    attr("a_TexCoordVec4", 4, 3),
    attr("a_Color", 4, 7),
    attr("a_TexCoordVec4C1", 4, 11),
    attr("a_TexCoordC2", 2, 15),
];

/// Rope vertex layout (§12.4).
pub const ROPE_ATTRIBUTES: [VertexAttribute; 7] = [
    attr("a_PositionVec4", 4, 0),
    attr("a_TexCoordVec4", 4, 4),
    attr("a_TexCoordVec4C1", 4, 8),
    attr("a_TexCoordVec4C2", 4, 12),
    attr("a_TexCoordVec4C3", 4, 16),
    attr("a_TexCoordC4", 2, 20),
    attr("a_Color", 4, 22),
];

/// Geometry mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryKind {
    /// One quad per particle (`sprite`, `spritetrail`).
    Sprite,
    /// One ribbon through all particles (`rope`, `ropetrail`).
    Rope,
}

/// Blend mode of the material's first pass (§12.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlendMode {
    /// `src × 1 + dst × 0` (colour and alpha).
    #[default]
    Normal,
    /// `src × αs + dst × (1 − αs)` (colour and alpha).
    Translucent,
    /// `src × αs + dst × 1` (colour and alpha).
    Additive,
}

impl BlendMode {
    /// Parses the pass `blending` key; absent or unknown → `Normal`.
    pub fn from_material(s: Option<&str>) -> Self {
        match s {
            Some("translucent") => Self::Translucent,
            Some("additive") => Self::Additive,
            _ => Self::Normal,
        }
    }
}

/// `g_RefractAmount` when the pass declares a non-zero `REFRACT` combo.
pub const REFRACT_AMOUNT: f32 = 0.05;

/// Static render description of a system (pipeline creation).
#[derive(Clone, Debug, PartialEq)]
pub struct RenderInfo {
    pub geometry: GeometryKind,
    /// Replaces the pass shader (`genericropeparticle` for ropes).
    pub shader_override: Option<&'static str>,
    /// Combos forced on top of the pass combos.
    pub combos: Vec<(&'static str, i32)>,
    pub blending: BlendMode,
    /// Depth clamp (or unclipped depth) while drawing the particles.
    pub depth_clamp: bool,
    pub vertex_stride_floats: usize,
    pub attributes: &'static [VertexAttribute],
    /// Vertex/index buffer capacities (§12.5).
    pub max_quads: usize,
    pub max_vertices: usize,
    pub max_indices: usize,
}

/// Depth range of the generated perspective projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DepthRange {
    /// `[0, 1]` (wgpu / D3D, the default for Lumen).
    #[default]
    ZeroToOne,
    /// `[-1, 1]` (OpenGL, as in the reference).
    NegOneToOne,
}

/// Parallax state of the scene (§2.4); `None` in [`UniformInput`] when parallax is disabled.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParallaxInput {
    /// Displacement `D`.
    pub displacement: Vec2,
    pub amount: f32,
}

/// Per-frame inputs of [`ParticleSystem::uniforms`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UniformInput {
    /// Scene projection size `W × H`.
    pub width: f32,
    pub height: f32,
    /// Scene camera `Projection · LookAt`, used by 2D systems.
    pub scene_view_projection: Mat4,
    /// Scene camera parameters, used by perspective systems.
    pub fov_degrees: f32,
    pub near: f32,
    pub far: f32,
    pub depth: DepthRange,
    pub parallax: Option<ParallaxInput>,
    /// Material constant `ui_editor_properties_overbright` (default 1).
    pub brightness: f32,
}

/// Particle-specific uniform values (§12.2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticleUniforms {
    pub model: Mat4,
    pub model_inverse: Mat4,
    pub view_projection: Mat4,
    pub model_view_projection: Mat4,
    pub model_view_projection_inverse: Mat4,
    pub orientation_up: Vec3,
    pub orientation_right: Vec3,
    pub orientation_forward: Vec3,
    pub view_up: Vec3,
    pub view_right: Vec3,
    pub eye_position: Vec3,
    pub render_var0: Vec4,
    pub render_var1: Vec4,
    pub brightness: f32,
    /// `g_Alpha` and `g_UserAlpha`.
    pub alpha: f32,
    pub color: Vec3,
    pub color4: Vec4,
    pub refract_amount: f32,
}

/// Eye of the particle camera (§12.2), also used in 2D.
pub const EYE: Vec3 = Vec3::new(0.0, 0.0, 1000.0);

/// Rope spline samples, allocated once at capacity.
#[derive(Debug, Default)]
pub(crate) struct RopeScratch {
    points: Vec<Vec3>,
    sizes: Vec<f32>,
    colors: Vec<Vec4>,
    arc: Vec<f32>,
}

impl RopeScratch {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            points: Vec::with_capacity(n),
            sizes: Vec::with_capacity(n),
            colors: Vec::with_capacity(n),
            arc: Vec::with_capacity(n),
        }
    }
}

const SPRITE_UVS: [(f32, f32); 4] = [(0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)];
const ROPE_UVS: [(f32, f32); 4] = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];

/// Uniform Catmull-Rom spline (§9.5).
#[inline]
fn catmull_rom(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: f32) -> Vec3 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * (2.0 * p1
        + (p2 - p0) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t3)
}

/// Writes the constant quad index pattern for `quads` quads (`0,1,2,2,3,0` + 4·q).
pub fn write_quad_indices(out: &mut Vec<u32>, quads: usize) {
    out.clear();
    out.reserve(quads * QUAD_INDICES);
    for q in 0..quads as u32 {
        let b = q * 4;
        out.extend_from_slice(&[b, b + 1, b + 2, b + 2, b + 3, b]);
    }
}

impl ParticleSystem {
    /// Geometry mode of this system.
    pub fn geometry(&self) -> GeometryKind {
        if self.renderer.kind.is_rope() {
            GeometryKind::Rope
        } else {
            GeometryKind::Sprite
        }
    }

    /// Floats per vertex.
    pub fn vertex_stride_floats(&self) -> usize {
        match self.geometry() {
            GeometryKind::Sprite => SPRITE_VERTEX_FLOATS,
            GeometryKind::Rope => ROPE_VERTEX_FLOATS,
        }
    }

    /// Floats per quad (4 vertices).
    pub fn quad_floats(&self) -> usize {
        4 * self.vertex_stride_floats()
    }

    /// Maximum number of quads (§12.5).
    pub fn max_quads(&self) -> usize {
        match self.geometry() {
            GeometryKind::Sprite => self.pool.cap,
            GeometryKind::Rope => {
                self.pool.cap.saturating_sub(1).max(1) * self.renderer.sigma() as usize
            }
        }
    }

    /// Static render description. `blending` is the `blending` key of the material's first pass.
    pub fn render_info(&self, blending: Option<&str>) -> RenderInfo {
        let geometry = self.geometry();
        let mut combos = vec![("THICKFORMAT", 1)];
        if self.sheet.active() {
            combos.push(("SPRITESHEET", 1));
        }
        if self.renderer.kind.is_trail() {
            combos.push(("TRAILRENDERER", 1));
        }
        let max_quads = self.max_quads();
        RenderInfo {
            geometry,
            shader_override: (geometry == GeometryKind::Rope).then_some("genericropeparticle"),
            combos,
            blending: BlendMode::from_material(blending),
            depth_clamp: true,
            vertex_stride_floats: self.vertex_stride_floats(),
            attributes: match geometry {
                GeometryKind::Sprite => &SPRITE_ATTRIBUTES,
                GeometryKind::Rope => &ROPE_ATTRIBUTES,
            },
            max_quads,
            max_vertices: max_quads * 4,
            max_indices: max_quads * QUAD_INDICES,
        }
    }

    /// Writes this frame's vertices into `out` and returns the number of quads written
    /// (draw `quads × 6` indices). Stops early if `out` is too small; call after
    /// [`ParticleSystem::update`] returned `true`.
    pub fn write_vertices(&mut self, out: &mut [f32]) -> usize {
        match self.geometry() {
            GeometryKind::Sprite => self.write_sprites(out),
            GeometryKind::Rope => self.write_rope(out),
        }
    }

    /// [`Self::write_vertices`] into a reused `Vec`: grows it only when needed (keep the same `Vec`
    /// across frames, or reserve `max_quads() × quad_floats()` once), then truncates it to the
    /// written length.
    pub fn write_vertices_vec(&mut self, out: &mut Vec<f32>) -> usize {
        let needed = self.frame_quads_upper_bound() * self.quad_floats();
        if out.len() < needed {
            if out.capacity() < needed {
                out.reserve_exact(self.max_quads() * self.quad_floats() - out.len());
            }
            out.resize(needed, 0.0);
        }
        let quads = self.write_vertices(out);
        out.truncate(quads * self.quad_floats());
        quads
    }

    /// Upper bound of the quads of the current frame.
    fn frame_quads_upper_bound(&self) -> usize {
        let n = self.pool.len;
        match self.geometry() {
            GeometryKind::Sprite => n,
            GeometryKind::Rope => n.saturating_sub(1) * self.renderer.sigma() as usize,
        }
    }

    fn write_sprites(&self, out: &mut [f32]) -> usize {
        const QUAD: usize = 4 * SPRITE_VERTEX_FLOATS;
        let p = self.pool.live_ref();
        let frames = self.sheet.frames as f32;
        let sheet = self.sheet.frames > 0;
        let random = self.animation_mode == AnimationMode::RandomFrame;
        let mut quads = 0;
        let drawn = SpriteIter {
            pos: p.pos,
            size: p.size,
            i: 0,
        };
        for (dst, i) in out.as_chunks_mut::<QUAD>().0.iter_mut().zip(drawn) {
            let pos = p.pos[i];
            let size = p.size[i];
            let frame = p.frame[i];
            let l = if sheet && frame >= 0.0 {
                if random {
                    (frame + 0.5) / frames
                } else {
                    frame / frames
                }
            } else {
                life(p.age[i], p.lifetime[i])
            };
            let (r, c, v) = (p.rot[i], p.color[i], p.vel[i]);
            let vert = [
                pos.x, pos.y, pos.z, 0.0, 0.0, r.z, size, c.x, c.y, c.z, p.alpha[i], v.x, v.y, v.z,
                l, r.x, r.y,
            ];
            for (dv, &(u, w)) in dst
                .as_chunks_mut::<SPRITE_VERTEX_FLOATS>()
                .0
                .iter_mut()
                .zip(&SPRITE_UVS)
            {
                *dv = vert;
                dv[3] = u;
                dv[4] = w;
            }
            quads += 1;
        }
        quads
    }

    fn write_rope(&mut self, out: &mut [f32]) -> usize {
        const QUAD: usize = 4 * ROPE_VERTEX_FLOATS;
        let p = self.pool.live_ref();
        let n = p.pos.len();
        if n < 2 {
            return 0;
        }
        let sigma = self.renderer.sigma() as usize;
        let s = &mut self.rope;
        s.points.clear();
        s.sizes.clear();
        s.colors.clear();
        s.arc.clear();
        // Step 1: spline sampling. No particle is filtered for ropes.
        let rgba = |i: usize| Vec3::from(p.color[i]).extend(p.alpha[i]);
        for i in 0..n - 1 {
            let p0 = Vec3::from(p.pos[i.saturating_sub(1)]);
            let p1 = Vec3::from(p.pos[i]);
            let p2 = Vec3::from(p.pos[i + 1]);
            let p3 = Vec3::from(p.pos[(i + 2).min(n - 1)]);
            let (s0, s1) = (p.size[i], p.size[i + 1]);
            let (c0, c1) = (rgba(i), rgba(i + 1));
            for k in 0..sigma {
                let t = k as f32 / sigma as f32;
                s.points.push(catmull_rom(p0, p1, p2, p3, t));
                s.sizes.push(s0 + t * (s1 - s0));
                s.colors.push(c0 + t * (c1 - c0));
            }
        }
        s.points.push(Vec3::from(p.pos[n - 1]));
        s.sizes.push(p.size[n - 1]);
        s.colors.push(rgba(n - 1));
        let m = s.points.len();

        // Step 2: UV parameters.
        let q = m - 1;
        let r = &self.renderer;
        let uvs = if r.uv_scale > 0.0 { r.uv_scale } else { 1.0 };
        let length = q as f32 / uvs + 1.0;
        let useful = length - 1.0;
        let smoothing = r.uv_smoothing && self.uniform_lifetimes && !r.uv_scrolling;
        let mut total = 0.0;
        if smoothing {
            s.arc.push(0.0);
            for j in 1..m {
                total += (s.points[j] - s.points[j - 1]).length();
                s.arc.push(total);
            }
        }
        let delta = if r.uv_scrolling && useful > 0.0 {
            (self.time % 10000.0) * useful
        } else {
            0.0
        };

        // Step 3: quads.
        let mut quads = 0;
        for (sq, dst) in out.as_chunks_mut::<QUAD>().0.iter_mut().take(q).enumerate() {
            let trail = if smoothing && total > 0.0 {
                s.arc[sq] / total * q as f32
            } else {
                sq as f32
            } + delta;
            let start = s.points[sq];
            let end = s.points[sq + 1];
            let prev = if sq == 0 { start } else { s.points[sq - 1] };
            let next = if sq + 2 < m { s.points[sq + 2] } else { end };
            let (cs, ce) = (s.colors[sq], s.colors[sq + 1]);
            let vert = [
                start.x,
                start.y,
                start.z,
                s.sizes[sq],
                end.x,
                end.y,
                end.z,
                length,
                prev.x,
                prev.y,
                prev.z,
                trail,
                next.x,
                next.y,
                next.z,
                s.sizes[sq + 1],
                ce.x,
                ce.y,
                ce.z,
                ce.w,
                0.0,
                0.0,
                cs.x,
                cs.y,
                cs.z,
                cs.w,
            ];
            for (dv, &(u, v)) in dst
                .as_chunks_mut::<ROPE_VERTEX_FLOATS>()
                .0
                .iter_mut()
                .zip(&ROPE_UVS)
            {
                *dv = vert;
                dv[20] = u;
                dv[21] = v;
            }
            quads += 1;
        }
        quads
    }

    /// Particle uniforms for this frame (§12.2).
    pub fn uniforms(&self, input: &UniformInput) -> ParticleUniforms {
        let obj = &self.object;
        let over = &obj.instance_override;
        let parallax = input.parallax.map_or(Vec3::ZERO, |px| {
            let d = obj.parallax_depth.vec2();
            // parity: depths below 0.65 in magnitude are pushed to ±0.65 (0 → +0.65).
            let min_depth = |v: f32| {
                if v.abs() < 0.65 {
                    if v == 0.0 { 0.65 } else { 0.65f32.copysign(v) }
                } else {
                    v
                }
            };
            let (dx, dy) = (min_depth(d.x), min_depth(d.y));
            // parity: both axes use the width W.
            Vec3::new(
                (dx + px.amount) * px.displacement.x * input.width,
                (dy + px.amount) * px.displacement.y * input.width,
                0.0,
            )
        });
        let angles = obj.angles.vec3();
        let model = Mat4::from_translation(self.origin + parallax)
            * Mat4::from_rotation_z(-angles.z)
            * Mat4::from_rotation_y(angles.y)
            * Mat4::from_rotation_x(-angles.x)
            * Mat4::from_scale(obj.scale.vec3());
        let view_projection = if self.perspective {
            let aspect = if input.height != 0.0 {
                input.width / input.height
            } else {
                1.0
            };
            let fov = input.fov_degrees.to_radians();
            let proj = match input.depth {
                DepthRange::ZeroToOne => Mat4::perspective_rh(fov, aspect, input.near, input.far),
                DepthRange::NegOneToOne => {
                    Mat4::perspective_rh_gl(fov, aspect, input.near, input.far)
                }
            };
            proj * Mat4::look_at_rh(EYE, Vec3::ZERO, Vec3::Y)
        } else {
            input.scene_view_projection
        };
        let mvp = view_projection * model;
        let r = &self.renderer;
        let render_var0 = match r.kind {
            RendererKind::SpriteTrail => Vec4::new(r.length, r.max_length, r.min_length, 0.0),
            RendererKind::RopeTrail => Vec4::new(r.length, 10.0, 0.0, 0.0),
            _ => Vec4::new(0.05, 10.0, 0.0, 0.0),
        };
        let sh = &self.sheet;
        let render_var1 = match self.texture {
            Some(t) if sh.frames > 0 && sh.columns > 0 && sh.rows > 0 => {
                let ratio = if t.gpu_width > 0 {
                    (t.gpu_height as f32 / sh.rows as f32)
                        / (t.gpu_width as f32 / sh.columns as f32)
                } else {
                    1.0
                };
                Vec4::new(
                    1.0 / sh.columns as f32,
                    1.0 / sh.rows as f32,
                    sh.frames as f32,
                    ratio,
                )
            }
            Some(t) if t.image_width > 0 => {
                Vec4::new(0.0, 0.0, 0.0, t.image_height as f32 / t.image_width as f32)
            }
            _ => Vec4::new(0.0, 0.0, 0.0, 1.0),
        };
        ParticleUniforms {
            model,
            model_inverse: model.inverse(),
            view_projection,
            model_view_projection: mvp,
            model_view_projection_inverse: mvp.inverse(),
            orientation_up: Vec3::Y,
            orientation_right: Vec3::X,
            orientation_forward: Vec3::Z,
            view_up: Vec3::Y,
            view_right: Vec3::X,
            // The eye is at Z = 1000 in 2D too: an eye on Z = 0 would make trails invisible.
            eye_position: EYE,
            render_var0,
            render_var1,
            brightness: input.brightness,
            alpha: over.alpha.f32(),
            color: over.color.vec3(),
            // parity: vec3 → vec4 conversion gives w = 0 when `instanceoverride` is present.
            color4: over.color.vec4(),
            refract_amount: REFRACT_AMOUNT,
        }
    }
}

/// Live indices of the particles drawn as sprites (finite position, size in `(0, 10000]`).
struct SpriteIter<'a> {
    pos: &'a [glam::Vec3A],
    size: &'a [f32],
    i: usize,
}

impl Iterator for SpriteIter<'_> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        while let (Some(p), Some(&size)) = (self.pos.get(self.i), self.size.get(self.i)) {
            let i = self.i;
            self.i += 1;
            // parity: non-finite positions and sizes outside (0, 10000] are skipped, yet still simulated.
            if p.is_finite() && size.is_finite() && size > 0.0 && size <= 10000.0 {
                return Some(i);
            }
        }
        None
    }
}

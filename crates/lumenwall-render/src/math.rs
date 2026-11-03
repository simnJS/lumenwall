//! Scene math: projection, camera, pointer, parallax, image geometry
//! (spec 04 §0.3, §4.1, §6, §7, §8, §10.2, §18.4, §24.2).
//!
//! Everything here is pure `f32` math with no GPU dependency, so it is unit
//! tested directly. Matrices follow the upstream clip convention (row 0 of a
//! render target at `y_ndc = -1`, depth in `[-1, 1]`): these exact values are
//! what WE shaders receive as uniforms. The wgpu convention is restored at the
//! very end of every vertex stage (see [`clip_fix`] and `shader::apply_clip_space_fix`).

use glam::{Mat4, Vec2, Vec3, Vec4};

/// `ortho(l, r, b, t, n, f)` of spec 04 §0.3 (right-handed, depth `[-1, 1]`).
#[must_use]
pub fn ortho(l: f32, r: f32, b: f32, t: f32, n: f32, f: f32) -> Mat4 {
    Mat4::from_cols(
        Vec4::new(2.0 / (r - l), 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / (t - b), 0.0, 0.0),
        Vec4::new(0.0, 0.0, -2.0 / (f - n), 0.0),
        Vec4::new(
            -(r + l) / (r - l),
            -(t + b) / (t - b),
            -(f + n) / (f - n),
            1.0,
        ),
    )
}

/// `ortho2(l, r, b, t) = ortho(l, r, b, t, -1, 1)`.
#[must_use]
pub fn ortho2(l: f32, r: f32, b: f32, t: f32) -> Mat4 {
    ortho(l, r, b, t, -1.0, 1.0)
}

/// Right-handed `lookAt` of spec 04 §0.3.
#[must_use]
pub fn look_at(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
    let f = (center - eye).normalize_or_zero();
    let s = f.cross(up).normalize_or_zero();
    let u = s.cross(f);
    Mat4::from_cols(
        Vec4::new(s.x, u.x, -f.x, 0.0),
        Vec4::new(s.y, u.y, -f.y, 0.0),
        Vec4::new(s.z, u.z, -f.z, 0.0),
        Vec4::new(-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0),
    )
}

/// The wgpu clip-space correction of spec 04 §24.2, applied by the GPU at the
/// end of every vertex stage: `(x, y, z, w) -> (x, -y, (z + w) / 2, w)`.
/// This CPU copy exists for tests and for code that projects points on the CPU.
#[must_use]
pub fn clip_fix(p: Vec4) -> Vec4 {
    Vec4::new(p.x, -p.y, (p.z + p.w) * 0.5, p.w)
}

/// Orthographic scene camera (spec 04 §6). Only orthographic projection
/// exists; `fov` is ignored upstream and therefore not represented.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrthoCamera {
    /// Projection width `W` in scene units.
    pub width: f32,
    /// Projection height `H` in scene units.
    pub height: f32,
    pub eye: Vec3,
    pub center: Vec3,
    pub up: Vec3,
    /// `nearz`, default 0.
    pub near: f32,
    /// `farz`, default 1000.
    pub far: f32,
}

impl OrthoCamera {
    /// Canonical camera: eye on +z looking at the origin, `up = +y`.
    #[must_use]
    pub fn canonical(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            eye: Vec3::new(0.0, 0.0, 1.0),
            center: Vec3::ZERO,
            up: Vec3::Y,
            near: 0.0,
            far: 1000.0,
        }
    }

    /// `P = ortho(-W/2, W/2, -H/2, H/2, near, far) · T(eye)`.
    #[must_use]
    pub fn projection(&self) -> Mat4 {
        // parity: the projection is post-multiplied by T(eye) (spec 04 §6).
        let (hw, hh) = (self.width * 0.5, self.height * 0.5);
        ortho(-hw, hw, -hh, hh, self.near, self.far) * Mat4::from_translation(self.eye)
    }

    /// `V = lookAt(eye, center, up)`.
    #[must_use]
    pub fn view(&self) -> Mat4 {
        look_at(self.eye, self.center, self.up)
    }

    /// `P · V`.
    #[must_use]
    pub fn view_projection(&self) -> Mat4 {
        self.projection() * self.view()
    }
}

/// Automatic projection size of spec 04 §4.1 (`general.orthogonalprojection.auto`).
///
/// `images` yields `(origin.xy, size)` of every image object of the file.
/// Falls back to `fallback` (bounding box of all outputs) when an extent is 0.
#[must_use]
pub fn auto_projection_size(
    images: impl IntoIterator<Item = (Vec2, Vec2)>,
    fallback: Vec2,
) -> Vec2 {
    // parity: the formula assumes origins centred on 0 (spec 04 Q3); kept as-is.
    let mut e = Vec2::ZERO;
    for (origin, size) in images {
        e.x = e.x.max(origin.x.abs() + size.x * 0.5);
        e.y = e.y.max(origin.y.abs() + size.y * 0.5);
    }
    if e.x > 0.0 && e.y > 0.0 {
        e * 2.0
    } else {
        fallback
    }
}

/// Integer logical size of a scene: `(tronc(W), tronc(H))` (spec 04 §3).
#[must_use]
pub fn scene_logical_size(width: f32, height: f32) -> (u32, u32) {
    (trunc_u32(width).max(1), trunc_u32(height).max(1))
}

/// Truncation toward zero of a non-negative float, saturating at 0.
#[must_use]
pub fn trunc_u32(v: f32) -> u32 {
    if v.is_nan() || v <= 0.0 { 0 } else { v as u32 }
}

/// A rectangle of an output, in pixels, origin bottom-left (spec 04 §7.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// UV window of the final composition (`u0, u1, v0, v1`, v measured from the top).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UvRange {
    pub u0: f32,
    pub u1: f32,
    pub v0: f32,
    pub v1: f32,
}

impl UvRange {
    /// The identity window `[0, 1] x [0, 1]`.
    pub const FULL: Self = Self {
        u0: 0.0,
        u1: 1.0,
        v0: 0.0,
        v1: 1.0,
    };
}

/// Pointer values of spec 04 §7.1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pointer {
    /// `g_PointerPosition`: (0, 0) = top-left of the scene, crop applied.
    pub position: Vec2,
    /// Pointer for scripts and particles (v measured from the bottom).
    pub normalized: Vec2,
}

/// `g_PointerPosition` from a mouse position `p` (pixels, origin bottom-left)
/// inside `viewport`, using the UV window of the previous composition.
#[must_use]
pub fn pointer_position(p: Vec2, viewport: Viewport, uv: UvRange) -> Pointer {
    let xn = ((p.x - viewport.x) / viewport.width).clamp(0.0, 1.0);
    let yn = ((p.y - viewport.y) / viewport.height).clamp(0.0, 1.0);
    let xn = if xn.is_nan() { 0.0 } else { xn };
    let yn = if yn.is_nan() { 0.0 } else { yn };
    let u = uv.u0 + xn * (uv.u1 - uv.u0);
    Pointer {
        position: Vec2::new(u, uv.v0 + (1.0 - yn) * (uv.v1 - uv.v0)),
        normalized: Vec2::new(u, uv.v0 + yn * (uv.v1 - uv.v0)),
    }
}

/// Pointer used when mouse input is disabled.
///
/// Deviation (spec 04 Q6): upstream feeds (0, 0), i.e. the bottom-left corner,
/// which gives a constant parallax offset. Wallpaper Engine keeps the camera
/// neutral, so Lumenwall uses the centre of the visible window.
#[must_use]
pub fn pointer_disabled(uv: UvRange) -> Pointer {
    let c = Vec2::new((uv.u0 + uv.u1) * 0.5, (uv.v0 + uv.v1) * 0.5);
    Pointer {
        position: c,
        normalized: c,
    }
}

/// Smoothing law of the parallax displacement (spec 04 §7.2, Q5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParallaxSmoothing {
    /// Wallpaper Engine semantics: `cameraparallaxdelay` is a time constant in
    /// seconds; 0 follows the pointer instantly. Frame-rate independent.
    #[default]
    WallpaperEngine,
    /// Upstream formula `k = clamp(delay · dt, 0, 1)` (delay 0 never moves).
    Reference,
}

/// Global parallax displacement `D` of a scene (spec 04 §7.2).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Parallax {
    pub displacement: Vec2,
}

impl Parallax {
    /// Advance `D` toward the pointer target.
    ///
    /// `amount` = `cameraparallaxamount`, `influence` = `cameraparallaxmouseinfluence`,
    /// `delay` = `cameraparallaxdelay`, `dt` in seconds.
    pub fn update(
        &mut self,
        pointer: Vec2,
        amount: f32,
        influence: f32,
        delay: f32,
        dt: f32,
        law: ParallaxSmoothing,
    ) {
        let target = (pointer - Vec2::splat(0.5)) * amount * influence;
        let k = match law {
            ParallaxSmoothing::Reference => (delay * dt).clamp(0.0, 1.0),
            ParallaxSmoothing::WallpaperEngine => {
                if delay <= 0.0 {
                    1.0
                } else {
                    (1.0 - (-dt.max(0.0) / delay).exp()).clamp(0.0, 1.0)
                }
            }
        };
        self.displacement += (target - self.displacement) * k;
    }
}

/// Per-image parallax translation `t` of spec 04 §7.3, in RC units.
/// `W` (scene width) is the reference for both axes.
#[must_use]
pub fn parallax_translation(
    depth: Vec2,
    amount: f32,
    displacement: Vec2,
    scene_width: f32,
) -> Vec3 {
    // parity: W is used for both axes (spec 04 §7.3).
    Vec3::new(
        (depth.x + amount) * displacement.x * scene_width,
        (depth.y + amount) * displacement.y * scene_width,
        0.0,
    )
}

/// Local transform of a scene node (spec 04 §8.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeTransform {
    pub origin: Vec3,
    pub scale: Vec3,
    /// `angles.z`, radians (0 for text nodes).
    pub angle: f32,
}

impl Default for NodeTransform {
    fn default() -> Self {
        Self {
            origin: Vec3::ZERO,
            scale: Vec3::ONE,
            angle: 0.0,
        }
    }
}

/// Maximum hierarchy depth walked by the reference (spec 04 §8.1).
pub const MAX_HIERARCHY_DEPTH: usize = 32;

/// Compose a parent chain, root first, leaf last (spec 04 §8.1).
/// Only the first [`MAX_HIERARCHY_DEPTH`] nodes closest to the leaf count.
#[must_use]
pub fn resolve_hierarchy(root_to_leaf: &[NodeTransform]) -> NodeTransform {
    let start = root_to_leaf.len().saturating_sub(MAX_HIERARCHY_DEPTH);
    let chain = &root_to_leaf[start..];
    let Some((first, rest)) = chain.split_first() else {
        return NodeTransform::default();
    };
    let mut acc = *first;
    for c in rest {
        let (s, co) = acc.angle.sin_cos();
        let lx = c.origin.x * acc.scale.x;
        let ly = c.origin.y * acc.scale.y;
        let off = Vec2::new(co * lx - s * ly, s * lx + co * ly);
        acc = NodeTransform {
            origin: Vec3::new(
                acc.origin.x + off.x,
                acc.origin.y + off.y,
                acc.origin.z + c.origin.z * acc.scale.z,
            ),
            scale: c.scale * acc.scale,
            angle: c.angle + acc.angle,
        };
    }
    acc
}

/// Vertical part of an alignment string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VAlign {
    Top,
    #[default]
    Center,
    Bottom,
}

/// Horizontal part of an alignment string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HAlign {
    Left,
    #[default]
    Center,
    Right,
}

/// Parsed `horizontalalign` / `alignment` value (spec 04 §8.3). Parse once at
/// load: per-frame code never touches strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Alignment {
    pub v: VAlign,
    pub h: HAlign,
}

impl Alignment {
    /// Substring tests, in the spec order (`top` before `bottom`, `left` before `right`).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        let v = if s.contains("top") {
            VAlign::Top
        } else if s.contains("bottom") {
            VAlign::Bottom
        } else {
            VAlign::Center
        };
        let h = if s.contains("left") {
            HAlign::Left
        } else if s.contains("right") {
            HAlign::Right
        } else {
            HAlign::Center
        };
        Self { v, h }
    }
}

/// Image rectangle in the centred frame RC (spec 04 §8.3): x right, y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageRect {
    pub x0: f32,
    pub x1: f32,
    /// Top edge `Yh` (smallest Y in RC for a positive scale).
    pub y_top: f32,
    /// Bottom edge `Yb`.
    pub y_bottom: f32,
}

impl ImageRect {
    /// Rotation pivot: centre of the rectangle, after alignment (spec 04 §8.4).
    #[must_use]
    pub fn center(&self) -> Vec2 {
        Vec2::new(
            (self.x0 + self.x1) * 0.5,
            (self.y_top + self.y_bottom) * 0.5,
        )
    }
}

/// Rectangle of an image from its resolved origin `o`, scale `e`, size `s`,
/// alignment and the projection size `scene` (spec 04 §8.3).
#[must_use]
pub fn image_rect(o: Vec3, e: Vec3, s: Vec2, align: Alignment, scene: Vec2) -> ImageRect {
    let sw = s.x * e.x;
    let sh = s.y * e.y;
    let (mut x0, mut x1) = (o.x - sw * 0.5, o.x + sw * 0.5);
    let (mut y0, mut y1) = (o.y - sh * 0.5, o.y + sh * 0.5);
    match align.v {
        VAlign::Top => {
            y0 -= sh * 0.5;
            y1 -= sh * 0.5;
        }
        VAlign::Bottom => {
            y0 += sh * 0.5;
            y1 += sh * 0.5;
        }
        VAlign::Center => {}
    }
    match align.h {
        HAlign::Left => {
            x0 += sw * 0.5;
            x1 += sw * 0.5;
        }
        HAlign::Right => {
            x0 -= sw * 0.5;
            x1 -= sw * 0.5;
        }
        HAlign::Center => {}
    }
    ImageRect {
        x0: x0 - scene.x * 0.5,
        x1: x1 - scene.x * 0.5,
        y_top: scene.y * 0.5 - y1,
        y_bottom: scene.y * 0.5 - y0,
    }
}

/// `R_image = T(c) · Rz(-θ) · T(-c)` in RC (spec 04 §8.4).
#[must_use]
pub fn image_rotation(rect: &ImageRect, angle: f32) -> Mat4 {
    if angle == 0.0 {
        return Mat4::IDENTITY;
    }
    let c = rect.center().extend(0.0);
    Mat4::from_translation(c) * Mat4::from_rotation_z(-angle) * Mat4::from_translation(-c)
}

/// `M_ecran = P · V · R_image · T(t)` (spec 04 §8.6).
#[must_use]
pub fn screen_matrix(view_projection: Mat4, rect: &ImageRect, angle: f32, parallax: Vec3) -> Mat4 {
    view_projection * image_rotation(rect, angle) * Mat4::from_translation(parallax)
}

/// `M_copie = ortho2(0, S.x, 0, S.y)`, also `g_ModelMatrix` of every pass of an image.
#[must_use]
pub fn copy_matrix(size: Vec2) -> Mat4 {
    ortho2(0.0, size.x, 0.0, size.y)
}

/// `(uMax, vMax)` of the initial copy (spec 04 §8.7): the useful content of a
/// power-of-two padded texture sits in its top-left corner.
#[must_use]
pub fn copy_uv_max(
    storage: (u32, u32),
    content: (u32, u32),
    animated: bool,
    passthrough: bool,
) -> Vec2 {
    if animated || passthrough || storage == content || storage.0 == 0 || storage.1 == 0 {
        return Vec2::ONE;
    }
    Vec2::new(
        content.0 as f32 / storage.0 as f32,
        content.1 as f32 / storage.1 as f32,
    )
}

/// Text quad matrix (spec 04 §18.4).
///
/// Deviation (spec 04 Q20): the reference counts the text ordinate from the
/// top (`Yc = origin.y - H/2`); Lumenwall uses the scene frame shared by every
/// other object in Wallpaper Engine (`Yc = H/2 - origin.y`).
#[must_use]
pub fn text_matrix(view_projection: Mat4, origin: Vec3, scale: Vec3, scene: Vec2) -> Mat4 {
    let t = Vec3::new(origin.x - scene.x * 0.5, scene.y * 0.5 - origin.y, origin.z);
    view_projection * Mat4::from_translation(t) * Mat4::from_scale(scale)
}

/// Vertex fed to every image, effect and copy pass: `a_Position`, `a_TexCoord`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
}

/// Size of [`Vertex`] in bytes.
pub const VERTEX_SIZE: u32 = std::mem::size_of::<Vertex>() as u32;

/// Skinning attributes of one puppet vertex (spec 08 §9.8), in a static buffer next to the
/// [`Vertex`] stream: `a_BlendIndices` (integer, offset 0), `a_BlendWeights` (16),
/// `a_Normal` (32) and `a_Tangent4` (48).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkinVertex {
    pub joints: [u32; 4],
    pub weights: [f32; 4],
    pub normal: [f32; 3],
    pad: f32,
    pub tangent: [f32; 4],
}

impl SkinVertex {
    #[must_use]
    pub fn new(joints: [u32; 4], weights: [f32; 4], normal: [f32; 3], tangent: [f32; 4]) -> Self {
        Self {
            joints,
            weights,
            normal,
            pad: 0.0,
            tangent,
        }
    }
}

/// Size of [`SkinVertex`] in bytes.
pub const SKIN_VERTEX_SIZE: u32 = std::mem::size_of::<SkinVertex>() as u32;

/// Two non-indexed triangles in the spec order (spec 04 §10.2):
/// bottom-left, top-left, bottom-right; bottom-right, top-left, top-right.
///
/// `y_top` is the edge that lands on row 0 of the destination; texture
/// coordinate (0, 0) is always the top-left corner, `(u_max, v_max)` the
/// bottom-right one.
#[must_use]
pub fn quad(x0: f32, x1: f32, y_top: f32, y_bottom: f32, u_max: f32, v_max: f32) -> [Vertex; 6] {
    let bl = Vertex {
        position: [x0, y_bottom, 0.0],
        uv: [0.0, v_max],
    };
    let tl = Vertex {
        position: [x0, y_top, 0.0],
        uv: [0.0, 0.0],
    };
    let br = Vertex {
        position: [x1, y_bottom, 0.0],
        uv: [u_max, v_max],
    };
    let tr = Vertex {
        position: [x1, y_top, 0.0],
        uv: [u_max, 0.0],
    };
    [bl, tl, br, br, tl, tr]
}

/// Local copy geometry `[0, S.x] x [0, S.y]` with copy texture coordinates.
#[must_use]
pub fn copy_quad(size: Vec2, uv_max: Vec2) -> [Vertex; 6] {
    quad(0.0, size.x, 0.0, size.y, uv_max.x, uv_max.y)
}

/// Full-frame geometry `[-1, 1]²` with `[0, 1]²` texture coordinates.
#[must_use]
pub fn unit_quad() -> [Vertex; 6] {
    quad(-1.0, 1.0, -1.0, 1.0, 1.0, 1.0)
}

/// Image rectangle in RC with the given texture window.
#[must_use]
pub fn rect_quad(rect: &ImageRect, uv_max: Vec2) -> [Vertex; 6] {
    quad(
        rect.x0,
        rect.x1,
        rect.y_top,
        rect.y_bottom,
        uv_max.x,
        uv_max.y,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-5 * (1.0 + a.abs().max(b.abs()))
    }

    fn mat_close(a: Mat4, b: Mat4) -> bool {
        a.to_cols_array()
            .iter()
            .zip(b.to_cols_array().iter())
            .all(|(x, y)| close(*x, *y))
    }

    #[test]
    fn ortho_matches_spec_rows_and_gl_convention() {
        let m = ortho(-2.0, 6.0, -1.0, 3.0, 0.5, 10.0);
        let rows = m.transpose();
        assert!(close(rows.x_axis.x, 2.0 / 8.0));
        assert!(close(rows.x_axis.w, -(6.0 - 2.0) / 8.0));
        assert!(close(rows.y_axis.y, 2.0 / 4.0));
        assert!(close(rows.y_axis.w, -(3.0 - 1.0) / 4.0));
        assert!(close(rows.z_axis.z, -2.0 / 9.5));
        assert!(close(rows.z_axis.w, -10.5 / 9.5));
        assert!(mat_close(
            m,
            Mat4::orthographic_rh_gl(-2.0, 6.0, -1.0, 3.0, 0.5, 10.0)
        ));
        // ortho2 only negates z.
        let o2 = ortho2(0.0, 4.0, 0.0, 2.0);
        assert_eq!(o2.transform_point3(Vec3::new(1.0, 1.0, 0.5)).z, -0.5);
    }

    #[test]
    fn copy_matrix_matches_spec_03() {
        let m = copy_matrix(Vec2::new(200.0, 100.0));
        let r = m.transpose();
        assert!(close(r.x_axis.x, 2.0 / 200.0) && close(r.x_axis.w, -1.0));
        assert!(close(r.y_axis.y, 2.0 / 100.0) && close(r.y_axis.w, -1.0));
        assert!(close(r.z_axis.z, -1.0) && close(r.z_axis.w, 0.0));
    }

    #[test]
    fn look_at_matches_spec_definition() {
        let eye = Vec3::new(1.0, 2.0, 5.0);
        let v = look_at(eye, Vec3::new(1.0, 2.0, 0.0), Vec3::Y);
        assert!(mat_close(v, Mat4::from_translation(-eye)));
        let v2 = look_at(
            Vec3::new(3.0, -1.0, 2.0),
            Vec3::new(0.0, 0.5, -1.0),
            Vec3::new(0.1, 1.0, 0.0),
        );
        let g = Mat4::look_at_rh(
            Vec3::new(3.0, -1.0, 2.0),
            Vec3::new(0.0, 0.5, -1.0),
            Vec3::new(0.1, 1.0, 0.0),
        );
        assert!(mat_close(v2, g));
    }

    #[test]
    fn canonical_camera_collapses_to_plain_ortho() {
        let mut cam = OrthoCamera::canonical(1920.0, 1080.0);
        cam.eye = Vec3::new(0.0, 0.0, 7.0);
        cam.center = Vec3::new(0.0, 0.0, 2.0);
        let vp = cam.view_projection();
        assert!(mat_close(
            vp,
            ortho(-960.0, 960.0, -540.0, 540.0, 0.0, 1000.0)
        ));
        // A z = 0 vertex lands exactly on z_ndc = -1, then on 0 after the clip fix.
        let p = vp * Vec4::new(10.0, 20.0, 0.0, 1.0);
        assert_eq!(p.z, -1.0);
        assert_eq!(clip_fix(p).z, 0.0);
        // RC top edge (Y = -H/2) is row 0: y_ndc = -1 upstream, +1 on wgpu.
        let top = vp * Vec4::new(0.0, -540.0, 0.0, 1.0);
        assert!(close(top.y, -1.0));
        assert!(close(clip_fix(top).y, 1.0));
    }

    #[test]
    fn auto_projection() {
        let s = auto_projection_size(
            [
                (Vec2::new(-100.0, 50.0), Vec2::new(40.0, 20.0)),
                (Vec2::new(10.0, -5.0), Vec2::new(300.0, 30.0)),
            ],
            Vec2::new(1.0, 1.0),
        );
        assert_eq!(s, Vec2::new(2.0 * 160.0, 2.0 * 60.0));
        // Q3: a full-frame image at (W/2, H/2) gives 2W x 2H.
        let s = auto_projection_size(
            [(Vec2::new(960.0, 540.0), Vec2::new(1920.0, 1080.0))],
            Vec2::ONE,
        );
        assert_eq!(s, Vec2::new(3840.0, 2160.0));
        let s = auto_projection_size(
            [(Vec2::ZERO, Vec2::new(0.0, 10.0))],
            Vec2::new(800.0, 600.0),
        );
        assert_eq!(s, Vec2::new(800.0, 600.0));
    }

    #[test]
    fn pointer_mapping() {
        let vp = Viewport {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let uv = UvRange {
            u0: 0.0,
            u1: 1.0,
            v0: 0.21875,
            v1: 0.78125,
        };
        // Bottom-left of the output → (u0, v1) for g_PointerPosition.
        let p = pointer_position(Vec2::ZERO, vp, uv);
        assert_eq!(p.position, Vec2::new(0.0, 0.78125));
        assert_eq!(p.normalized, Vec2::new(0.0, 0.21875));
        // Top-right, clamped.
        let p = pointer_position(Vec2::new(5000.0, 5000.0), vp, uv);
        assert_eq!(p.position, Vec2::new(1.0, 0.21875));
        // First frame: UV window still (0,0,0,0) → pointer (0, 0).
        let p = pointer_position(Vec2::new(500.0, 500.0), vp, UvRange::default());
        assert_eq!(p.position, Vec2::ZERO);
        assert_eq!(pointer_disabled(UvRange::FULL).position, Vec2::splat(0.5));
    }

    #[test]
    fn parallax_laws() {
        let mut p = Parallax::default();
        p.update(
            Vec2::new(1.0, 1.0),
            1.0,
            1.0,
            0.0,
            1.0 / 30.0,
            ParallaxSmoothing::Reference,
        );
        assert_eq!(p.displacement, Vec2::ZERO, "reference: delay 0 never moves");
        p.update(
            Vec2::new(1.0, 0.0),
            2.0,
            0.5,
            3.0,
            0.1,
            ParallaxSmoothing::Reference,
        );
        assert!(close(p.displacement.x, 0.5 * 0.3) && close(p.displacement.y, -0.5 * 0.3));
        let mut q = Parallax::default();
        q.update(
            Vec2::new(1.0, 1.0),
            1.0,
            1.0,
            0.0,
            1.0 / 30.0,
            ParallaxSmoothing::WallpaperEngine,
        );
        assert_eq!(
            q.displacement,
            Vec2::splat(0.5),
            "WE: delay 0 follows instantly"
        );
        // Frame-rate independence: two half steps equal one full step.
        let (mut a, mut b) = (Parallax::default(), Parallax::default());
        a.update(
            Vec2::ONE,
            1.0,
            1.0,
            0.5,
            0.2,
            ParallaxSmoothing::WallpaperEngine,
        );
        b.update(
            Vec2::ONE,
            1.0,
            1.0,
            0.5,
            0.1,
            ParallaxSmoothing::WallpaperEngine,
        );
        b.update(
            Vec2::ONE,
            1.0,
            1.0,
            0.5,
            0.1,
            ParallaxSmoothing::WallpaperEngine,
        );
        assert!(close(a.displacement.x, b.displacement.x));
        let t = parallax_translation(Vec2::new(0.5, 1.0), 1.0, Vec2::new(0.1, -0.2), 1000.0);
        assert!(close(t.x, 150.0) && close(t.y, -400.0) && t.z == 0.0);
    }

    #[test]
    fn hierarchy_composition() {
        let parent = NodeTransform {
            origin: Vec3::new(100.0, 50.0, 1.0),
            scale: Vec3::new(2.0, 3.0, 4.0),
            angle: std::f32::consts::FRAC_PI_2,
        };
        let child = NodeTransform {
            origin: Vec3::new(10.0, 0.0, 2.0),
            scale: Vec3::new(0.5, 0.5, 1.0),
            angle: 0.25,
        };
        let r = resolve_hierarchy(&[parent, child]);
        // off = Rot(90°) · (10·2, 0·3) = (0, 20)
        assert!(close(r.origin.x, 100.0) && close(r.origin.y, 70.0) && close(r.origin.z, 9.0));
        assert_eq!(r.scale, Vec3::new(1.0, 1.5, 4.0));
        assert!(close(r.angle, std::f32::consts::FRAC_PI_2 + 0.25));
        assert_eq!(resolve_hierarchy(&[]), NodeTransform::default());
        assert_eq!(resolve_hierarchy(&[child]), child);
        // Depth limit: only the 32 nodes closest to the leaf count.
        let mut long = vec![
            NodeTransform {
                origin: Vec3::new(1000.0, 0.0, 0.0),
                ..NodeTransform::default()
            };
            1
        ];
        long.extend(std::iter::repeat_n(NodeTransform::default(), 32));
        assert_eq!(resolve_hierarchy(&long).origin, Vec3::ZERO);
    }

    #[test]
    fn rect_and_alignment() {
        let scene = Vec2::new(1920.0, 1080.0);
        let o = Vec3::new(960.0, 540.0, 0.0);
        let r = image_rect(
            o,
            Vec3::ONE,
            Vec2::new(200.0, 100.0),
            Alignment::parse("center"),
            scene,
        );
        assert_eq!(
            r,
            ImageRect {
                x0: -100.0,
                x1: 100.0,
                y_top: -50.0,
                y_bottom: 50.0
            }
        );
        let a = Alignment::parse("topleft");
        assert_eq!(
            a,
            Alignment {
                v: VAlign::Top,
                h: HAlign::Left
            }
        );
        let r = image_rect(o, Vec3::ONE, Vec2::new(200.0, 100.0), a, scene);
        // Origin is the top-left corner: the image extends right and down (WE y up).
        assert_eq!((r.x0, r.x1, r.y_top, r.y_bottom), (0.0, 200.0, 0.0, 100.0));
        assert_eq!(
            Alignment::parse("bottomright"),
            Alignment {
                v: VAlign::Bottom,
                h: HAlign::Right
            }
        );
        // Scale applies to the size; negative scale mirrors the rectangle.
        let r = image_rect(
            o,
            Vec3::new(-1.0, 2.0, 1.0),
            Vec2::new(200.0, 100.0),
            Alignment::default(),
            scene,
        );
        assert_eq!(
            (r.x0, r.x1, r.y_top, r.y_bottom),
            (100.0, -100.0, -100.0, 100.0)
        );
    }

    #[test]
    fn rotation_is_counter_clockwise_on_screen() {
        let rect = ImageRect {
            x0: 0.0,
            x1: 20.0,
            y_top: 0.0,
            y_bottom: 20.0,
        };
        let r = image_rotation(&rect, std::f32::consts::FRAC_PI_2);
        // Right-middle point of the image (screen right of the pivot)...
        let p = r.transform_point3(Vec3::new(20.0, 10.0, 0.0));
        // ...goes above the pivot on screen (smaller RC Y, since RC Y points down).
        assert!(close(p.x, 10.0) && close(p.y, 0.0));
        assert_eq!(image_rotation(&rect, 0.0), Mat4::IDENTITY);
    }

    #[test]
    fn uv_max_padding() {
        assert_eq!(
            copy_uv_max((256, 128), (200, 100), false, false),
            Vec2::new(200.0 / 256.0, 100.0 / 128.0)
        );
        assert_eq!(copy_uv_max((256, 128), (200, 100), true, false), Vec2::ONE);
        assert_eq!(copy_uv_max((256, 128), (200, 100), false, true), Vec2::ONE);
        assert_eq!(copy_uv_max((64, 64), (64, 64), false, false), Vec2::ONE);
    }

    #[test]
    fn quad_order_and_winding() {
        let q = quad(0.0, 2.0, 0.0, 1.0, 0.5, 0.25);
        assert_eq!(q[0].position, [0.0, 1.0, 0.0]); // bottom-left
        assert_eq!(q[1].position, [0.0, 0.0, 0.0]); // top-left
        assert_eq!(q[2].position, [2.0, 1.0, 0.0]); // bottom-right
        assert_eq!(q[3], q[2]);
        assert_eq!(q[4], q[1]);
        assert_eq!(q[5].position, [2.0, 0.0, 0.0]); // top-right
        assert_eq!(q[1].uv, [0.0, 0.0]);
        assert_eq!(q[2].uv, [0.5, 0.25]);
        // Clockwise when the y axis (rows) points down: positive signed area.
        for tri in q.chunks(3) {
            let (a, b, c) = (tri[0].position, tri[1].position, tri[2].position);
            let cross = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            assert!(
                cross > 0.0,
                "triangle must be clockwise in storage coordinates"
            );
        }
        assert_eq!(unit_quad()[1].position, [-1.0, -1.0, 0.0]);
    }

    #[test]
    fn copy_geometry_puts_texel_row_zero_on_target_row_zero() {
        let s = Vec2::new(64.0, 32.0);
        let m = copy_matrix(s);
        let q = copy_quad(s, Vec2::ONE);
        // Top-left vertex has uv (0,0) and must land on row 0: y_ndc = -1 upstream.
        let tl = m * Vec4::new(q[1].position[0], q[1].position[1], 0.0, 1.0);
        assert!(close(tl.y, -1.0) && q[1].uv == [0.0, 0.0]);
        // After the wgpu fix row 0 is y_ndc = +1.
        assert!(close(clip_fix(tl).y, 1.0));
    }

    #[test]
    fn text_matrix_uses_scene_frame() {
        let vp = OrthoCamera::canonical(1000.0, 500.0).view_projection();
        let m = text_matrix(
            vp,
            Vec3::new(500.0, 450.0, 0.0),
            Vec3::ONE,
            Vec2::new(1000.0, 500.0),
        );
        // origin.y = 450 (near the top in WE coordinates) → near row 0 (y_ndc ≈ -1).
        let p = m * Vec4::new(0.0, 0.0, 0.0, 1.0);
        assert!(close(p.y, -0.8));
    }

    #[test]
    fn screen_matrix_composition() {
        let cam = OrthoCamera::canonical(100.0, 100.0);
        let rect = ImageRect {
            x0: -10.0,
            x1: 10.0,
            y_top: -10.0,
            y_bottom: 10.0,
        };
        let m = screen_matrix(cam.view_projection(), &rect, 0.0, Vec3::new(5.0, 0.0, 0.0));
        let p = m * Vec4::new(0.0, 0.0, 0.0, 1.0);
        assert!(close(p.x, 0.1));
        assert_eq!(scene_logical_size(1919.9, 1080.2), (1919, 1080));
        assert_eq!(trunc_u32(-3.0), 0);
    }
}

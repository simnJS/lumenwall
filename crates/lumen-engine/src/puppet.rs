//! Puppet warp animation (spec 08 §9): layer clocks, sampling, layer blending, bone and
//! skinning matrices, attachment frames, and the two ways of applying them (GPU `g_Bones`
//! palette or CPU skinning).
//!
//! Pure CPU math over [`lumen_formats::mdl::PuppetModel`] data, independent of the scene
//! model and of the renderer, so the spec vectors are unit tested here. Everything a frame
//! touches is allocated when the puppet is created: [`Puppet::advance`],
//! [`Puppet::evaluate`], [`Puppet::write_palette`] and [`Puppet::skin_vertices`] allocate
//! nothing.

use glam::{EulerRot, Mat4, Quat, Vec2, Vec3};
use lumen_formats::mdl::{Animation, AnimationMode, BoneSample, PuppetModel};
use lumen_render::{SkinVertex, Vertex};

/// Largest skeleton skinned on the GPU: `BONECOUNT` sizes a uniform array of 64-byte
/// matrices in every pass block (QO-P14). Bigger skeletons are skinned on the CPU.
pub const MAX_GPU_BONES: usize = 128;

/// Longest clock step of one frame, in seconds. The engine clock keeps running while an
/// output is paused; capping the step keeps the pause out of the layer clocks (QO-P18)
/// without slowing animations down above 4 fps.
pub const MAX_STEP: f64 = 0.25;

/// Local pose of a bone (spec 08 §9.1): position, Euler angles (x, y, z) in radians, scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pos: Vec3,
    pub ang: Vec3,
    pub scl: Vec3,
}

impl Pose {
    fn from_sample(s: &BoneSample) -> Self {
        Self {
            pos: Vec3::from_array(s.position),
            ang: Vec3::from_array(s.angles),
            scl: Vec3::from_array(s.scale),
        }
    }

    /// Component-wise interpolation; angles are not wrapped (spec 08 §9.4).
    fn lerp(a: &BoneSample, b: &BoneSample, t: f32) -> Self {
        let l = |x: [f32; 3], y: [f32; 3]| Vec3::from_array(x).lerp(Vec3::from_array(y), t);
        Self {
            pos: l(a.position, b.position),
            ang: l(a.angles, b.angles),
            scl: l(a.scale, b.scale),
        }
    }

    /// `L = T(pos) · Rz(z) · Ry(y) · Rx(x) · Sc(scl)` (spec 08 §9.6).
    #[must_use]
    pub fn matrix(&self) -> Mat4 {
        let r = Quat::from_euler(EulerRot::ZYX, self.ang.z, self.ang.y, self.ang.x);
        Mat4::from_scale_rotation_translation(self.scl, r, self.pos)
    }

    /// Decomposition of a rest transform (spec 08 §9.1).
    fn from_matrix(m: &Mat4) -> Self {
        let (scl, r, pos) = m.to_scale_rotation_translation();
        let (z, y, x) = r.to_euler(EulerRot::ZYX);
        Self {
            pos,
            ang: Vec3::new(x, y, z),
            scl,
        }
    }
}

/// Skeleton, attachment points and animations, prepared once (spec 08 §9.1).
#[derive(Clone, Debug)]
pub struct Rig {
    parents: Box<[Option<usize>]>,
    rest_local: Box<[Mat4]>,
    rest_pose: Box<[Pose]>,
    inverse_bind: Box<[Mat4]>,
    attachments: Vec<(String, usize, Mat4)>,
    animations: Vec<Animation>,
}

impl Rig {
    /// `None` when the model has no valid skeleton (rest pose, spec 08 §8).
    #[must_use]
    pub fn new(model: &PuppetModel) -> Option<Self> {
        let skeleton = model.skeleton.as_ref()?;
        Some(Self::from_parts(
            skeleton.bones.iter().map(|b| (b.parent, b.local)).collect(),
            model
                .attachments
                .iter()
                .map(|a| (a.name.clone(), a.bone, a.transform))
                .collect(),
            model.animations.clone(),
        ))
    }

    /// Bones as `(parent, local rest matrix)` in file order (parents first).
    #[must_use]
    pub fn from_parts(
        bones: Vec<(Option<usize>, [f32; 16])>,
        attachments: Vec<(String, usize, [f32; 16])>,
        animations: Vec<Animation>,
    ) -> Self {
        let n = bones.len();
        let parents: Box<[Option<usize>]> = bones
            .iter()
            .enumerate()
            .map(|(i, (p, _))| p.filter(|&p| p < i))
            .collect();
        let rest_local: Box<[Mat4]> = bones
            .iter()
            .map(|(_, m)| Mat4::from_cols_array(m))
            .collect();
        let mut world = vec![Mat4::IDENTITY; n];
        for b in 0..n {
            world[b] = match parents[b] {
                Some(p) => world[p] * rest_local[b],
                None => rest_local[b],
            };
        }
        Self {
            rest_pose: rest_local.iter().map(Pose::from_matrix).collect(),
            inverse_bind: world.iter().map(Mat4::inverse).collect(),
            parents,
            rest_local,
            attachments: attachments
                .into_iter()
                .filter(|a| a.1 < n)
                .map(|(name, bone, m)| (name, bone, Mat4::from_cols_array(&m)))
                .collect(),
            animations,
        }
    }

    #[must_use]
    pub fn bone_count(&self) -> usize {
        self.parents.len()
    }

    /// First attachment with this name (case-sensitive, spec 08 §5).
    #[must_use]
    pub fn attachment(&self, name: &str) -> Option<usize> {
        self.attachments.iter().position(|a| a.0 == name)
    }

    /// Index of the animation with this `id`.
    #[must_use]
    pub fn animation_index(&self, id: u32) -> Option<usize> {
        self.animations.iter().position(|a| a.id == id)
    }

    #[must_use]
    pub fn animations(&self) -> &[Animation] {
        &self.animations
    }
}

/// One animation layer (spec 08 §9.2, §9.3).
#[derive(Clone, Debug)]
pub struct Layer {
    /// Index of the animation in the rig; `None` = inactive layer.
    pub animation: Option<usize>,
    /// Live values (refreshed by the caller from their settings).
    pub rate: f32,
    pub blend: f32,
    pub visible: bool,
    pub additive: bool,
    pub blend_in: bool,
    pub blend_out: bool,
    pub blend_time: f32,
    /// `τ`, seconds of animation time.
    time: f64,
    /// Real time since creation or the last `play()` (ramp-in base).
    play_time: f64,
    playing: bool,
    ended: bool,
}

impl Layer {
    /// A layer playing `animation` from τ = 0 with the defaults of spec 08 §9.2.
    #[must_use]
    pub fn new(animation: Option<usize>) -> Self {
        Self {
            animation,
            rate: 1.0,
            blend: 1.0,
            visible: true,
            additive: false,
            blend_in: false,
            blend_out: false,
            blend_time: 0.5,
            time: 0.0,
            play_time: 0.0,
            playing: true,
            ended: false,
        }
    }

    /// `τ` in seconds.
    #[must_use]
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Whether the clock runs (a `single` layer stops on its last frame).
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Set once when a `single` layer reaches its last frame (end callbacks, spec 08 §9.3).
    #[must_use]
    pub fn has_ended(&self) -> bool {
        self.ended
    }

    /// Advances the clock by `dt` real seconds (spec 08 §9.3), also when the layer itself is
    /// not visible.
    fn advance(&mut self, dt: f64, anim: Option<&Animation>) {
        self.play_time += dt;
        if !self.playing {
            return;
        }
        self.time += dt * f64::from(self.rate);
        if let Some(a) = anim
            && a.mode == AnimationMode::Single
            && a.fps > 0.0
            && a.frames > 0
            && self.time * f64::from(a.fps) >= f64::from(a.frames)
        {
            // Halts on frame N (end callbacks are called once).
            self.time = f64::from(a.frames) / f64::from(a.fps);
            self.playing = false;
            self.ended = true;
        }
    }

    /// Fractional frame position `φ ∈ [0, N]` (spec 08 §9.3).
    #[must_use]
    pub fn phase(&self, a: &Animation) -> f64 {
        if a.frames == 0 || a.fps.is_nan() || a.fps <= 0.0 {
            return 0.0;
        }
        let n = f64::from(a.frames);
        let f = self.time * f64::from(a.fps);
        if !f.is_finite() {
            return 0.0;
        }
        let wrap = |g: f64, m: f64| {
            let r = g.rem_euclid(m);
            if r >= m { 0.0 } else { r }
        };
        match a.mode {
            AnimationMode::Loop | AnimationMode::Unknown => wrap(f, n),
            AnimationMode::Mirror => {
                let g = wrap(f, 2.0 * n);
                if g <= n { g } else { 2.0 * n - g }
            }
            AnimationMode::Single => f.clamp(0.0, n),
        }
    }

    /// Ramp factor (spec 08 §9.5.1).
    fn ramp(&self, a: &Animation, phase: f64) -> f32 {
        let bt = f64::from(self.blend_time);
        if bt <= 0.0 || bt.is_nan() {
            return 1.0;
        }
        let mut r = 1.0;
        if self.blend_in {
            r *= (self.play_time / bt).clamp(0.0, 1.0);
        }
        if self.blend_out && a.mode == AnimationMode::Single && a.fps > 0.0 {
            r *= ((a.duration() - phase / f64::from(a.fps)) / bt).clamp(0.0, 1.0);
        }
        r as f32
    }
}

/// Value of a track at `phase` (spec 08 §9.4): linear interpolation between the precomputed
/// frames, indices clamped to the samples available. `None` for an empty track.
fn sample(track: &[BoneSample], phase: f64, frames: u32) -> Option<Pose> {
    let last = track.len().checked_sub(1)?;
    if frames == 0 {
        return Some(Pose::from_sample(&track[0]));
    }
    let i = (phase.floor().max(0.0) as u64).min(u64::from(frames) - 1) as usize;
    let alpha = (phase - i as f64) as f32;
    Some(Pose::lerp(
        &track[i.min(last)],
        &track[(i + 1).min(last)],
        alpha,
    ))
}

/// Frame of an attachment decomposed into a transform node (spec 08 §9.10).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttachmentNode {
    pub origin: Vec3,
    pub angle: f32,
    pub scale: Vec3,
}

/// `a`, `φ = atan2(A[1], A[0])`, `σ` with `σ.y < 0` when the 2×2 part mirrors.
#[must_use]
pub fn decompose_attachment(a: &Mat4) -> AttachmentNode {
    let m = a.to_cols_array();
    let det = m[0] * m[5] - m[4] * m[1];
    let mut scale = Vec3::new(
        a.x_axis.truncate().length(),
        a.y_axis.truncate().length(),
        a.z_axis.truncate().length(),
    );
    if det < 0.0 {
        scale.y = -scale.y;
    }
    AttachmentNode {
        origin: Vec3::new(m[12], m[13], m[14]),
        angle: m[1].atan2(m[0]),
        scale,
    }
}

/// `C = T(hx, hy, 0) · Sc(1, −1, 1)`: mesh space → local image pixels (spec 08 §9.8).
fn to_pixels(half: Vec2) -> Mat4 {
    Mat4::from_translation(half.extend(0.0)) * Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0))
}

/// Animated state of one puppet image.
#[derive(Clone, Debug)]
pub struct Puppet {
    rig: Rig,
    pub layers: Vec<Layer>,
    pose: Box<[Pose]>,
    touched: Box<[bool]>,
    world: Box<[Mat4]>,
    skin: Box<[Mat4]>,
}

impl Puppet {
    /// Rest pose; call [`Self::evaluate`] once the layers are set.
    #[must_use]
    pub fn new(rig: Rig, layers: Vec<Layer>) -> Self {
        let n = rig.bone_count();
        let mut world = vec![Mat4::IDENTITY; n].into_boxed_slice();
        for b in 0..n {
            world[b] = match rig.parents[b] {
                Some(p) => world[p] * rig.rest_local[b],
                None => rig.rest_local[b],
            };
        }
        Self {
            pose: rig.rest_pose.clone(),
            touched: vec![false; n].into_boxed_slice(),
            world,
            skin: vec![Mat4::IDENTITY; n].into_boxed_slice(),
            layers,
            rig,
        }
    }

    #[must_use]
    pub fn rig(&self) -> &Rig {
        &self.rig
    }

    /// Whether some layer can change the pose over time.
    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.layers.iter().any(|l| {
            l.animation
                .and_then(|a| self.rig.animations.get(a))
                .is_some_and(|a| a.frames > 0 && a.fps > 0.0 && !a.tracks.is_empty())
        })
    }

    /// Step 1 of spec 08 §9.11: advances every layer clock by `dt` seconds.
    pub fn advance(&mut self, dt: f64) {
        for l in &mut self.layers {
            let anim = l.animation.and_then(|a| self.rig.animations.get(a));
            l.advance(dt, anim);
        }
    }

    /// Step 2 of spec 08 §9.11: blends the layers into the local poses (§9.5), then computes
    /// the world (`W`) and skinning (`K = W · I`) matrices (§9.6). Returns whether a skinning
    /// matrix changed.
    pub fn evaluate(&mut self) -> bool {
        let rig = &self.rig;
        let n = rig.bone_count();
        self.pose.copy_from_slice(&rig.rest_pose);
        self.touched.fill(false);
        for layer in &self.layers {
            if !layer.visible {
                continue;
            }
            let Some(anim) = layer.animation.and_then(|a| rig.animations.get(a)) else {
                continue;
            };
            let phase = layer.phase(anim);
            let w = layer.blend * layer.ramp(anim, phase);
            if w.is_nan() || w <= 0.0 {
                continue;
            }
            for (b, track) in anim.tracks.iter().enumerate().take(n) {
                let Some(s) = sample(track, phase, anim.frames) else {
                    continue;
                };
                let p = &mut self.pose[b];
                if layer.additive {
                    // Offset from the rest pose, additive scale (QO-P8).
                    let r = &rig.rest_pose[b];
                    p.pos += w * (s.pos - r.pos);
                    p.ang += w * (s.ang - r.ang);
                    p.scl += w * (s.scl - r.scl);
                } else {
                    p.pos += w * (s.pos - p.pos);
                    p.ang += w * (s.ang - p.ang);
                    p.scl += w * (s.scl - p.scl);
                }
                self.touched[b] = true;
            }
        }
        let mut changed = false;
        for b in 0..n {
            let local = if self.touched[b] {
                self.pose[b].matrix()
            } else {
                rig.rest_local[b]
            };
            self.world[b] = match rig.parents[b] {
                Some(p) => self.world[p] * local,
                None => local,
            };
            let k = self.world[b] * rig.inverse_bind[b];
            if k != self.skin[b] {
                self.skin[b] = k;
                changed = true;
            }
        }
        changed
    }

    /// `W_b` of the current pose (mesh space).
    #[must_use]
    pub fn world(&self, bone: usize) -> Mat4 {
        self.world.get(bone).copied().unwrap_or(Mat4::IDENTITY)
    }

    /// `K_b` of the current pose.
    #[must_use]
    pub fn skinning(&self) -> &[Mat4] {
        &self.skin
    }

    /// `A(t) = W_bone · M_att` (spec 08 §9.10).
    #[must_use]
    pub fn attachment_frame(&self, index: usize) -> Mat4 {
        self.rig
            .attachments
            .get(index)
            .map_or(Mat4::IDENTITY, |(_, bone, m)| self.world(*bone) * *m)
    }

    /// The `g_Bones` palette for positions in local pixels: `K' = C · K · C⁻¹`, 12 floats
    /// per bone (the 3 first rows, column-major), into `out` (spec 08 §9.8).
    pub fn write_palette(&self, half: Vec2, out: &mut [f32]) {
        let c = to_pixels(half);
        let ci = c.inverse();
        for (k, dst) in self.skin.iter().zip(out.as_chunks_mut::<12>().0) {
            let m = c * *k * ci;
            for (col, d) in [m.x_axis, m.y_axis, m.z_axis, m.w_axis]
                .iter()
                .zip(dst.as_chunks_mut::<3>().0)
            {
                d.copy_from_slice(&col.truncate().to_array());
            }
        }
    }

    /// CPU skinning (spec 08 §9.7) of `rest` (local pixels, see [`to_pixels`]) into `out`:
    /// `v' = (Σ w_i · K'_{j_i}) · v`, no renormalisation, zero weights skipped.
    pub fn skin_vertices(
        &self,
        half: Vec2,
        rest: &[Vertex],
        skin: &[SkinVertex],
        out: &mut [Vertex],
    ) {
        let c = to_pixels(half);
        let ci = c.inverse();
        for ((v, s), o) in rest.iter().zip(skin).zip(out.iter_mut()) {
            let mut m = Mat4::ZERO;
            for (&j, &w) in s.joints.iter().zip(&s.weights) {
                if w != 0.0
                    && let Some(k) = self.skin.get(j as usize)
                {
                    m += *k * w;
                }
            }
            let p = (c * m * ci).transform_point3(Vec3::from_array(v.position));
            *o = Vertex {
                position: p.to_array(),
                uv: v.uv,
            };
        }
    }
}

#[cfg(test)]
// Spec vectors are copied with the digits the spec prints.
#[allow(clippy::approx_constant, clippy::excessive_precision)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, FRAC_PI_8};

    const EPS: f32 = 1e-4;

    fn t(x: f32, y: f32, z: f32) -> [f32; 16] {
        Mat4::from_translation(Vec3::new(x, y, z)).to_cols_array()
    }

    fn sample(z: f32) -> BoneSample {
        BoneSample {
            position: [10.0, 0.0, 0.0],
            angles: [0.0, 0.0, z],
            scale: [1.0; 3],
        }
    }

    /// F-P1 (spec 08 §10) as a rig: one root bone at T(10, 0, 0), attachment `att` at
    /// T(0, 5, 0), animation 7 rotating it from 0 to π/2 over one frame at 1 fps.
    fn fp1(mode: AnimationMode) -> Rig {
        Rig::from_parts(
            vec![(None, t(10.0, 0.0, 0.0))],
            vec![("att".into(), 0, t(0.0, 5.0, 0.0))],
            vec![Animation {
                id: 7,
                name: "A".into(),
                mode_name: String::new(),
                mode,
                fps: 1.0,
                frames: 1,
                tracks: vec![vec![sample(0.0), sample(FRAC_PI_2)]],
            }],
        )
    }

    fn puppet(mode: AnimationMode, layers: &[(f32, f32, bool)]) -> Puppet {
        let rig = fp1(mode);
        let layers = layers
            .iter()
            .map(|&(rate, blend, additive)| Layer {
                rate,
                blend,
                additive,
                ..Layer::new(rig.animation_index(7))
            })
            .collect();
        Puppet::new(rig, layers)
    }

    fn run(p: &mut Puppet, seconds: f64) {
        p.advance(seconds);
        p.evaluate();
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    fn skinned(p: &Puppet, v: [f32; 2]) -> [f32; 2] {
        let r = p.skinning()[0].transform_point3(Vec3::new(v[0], v[1], 0.0));
        [r.x, r.y]
    }

    /// θ of bone 0, the three vertices of F-P1 and the attachment node.
    fn check(p: &Puppet, theta: f32, s: [[f32; 2]; 3], att: ([f32; 2], f32)) {
        let k = p.skinning()[0];
        let (c, si) = (theta.cos(), theta.sin());
        let expected = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0))
            * Mat4::from_rotation_z(theta)
            * Mat4::from_translation(Vec3::new(-10.0, 0.0, 0.0));
        assert!(k.abs_diff_eq(expected, EPS), "K = {k:?}, θ = {theta}");
        assert!(close(k.x_axis.x, c) && close(k.x_axis.y, si));
        for (v, e) in [[0.0, 0.0], [20.0, 0.0], [0.0, 20.0]].iter().zip(s) {
            let r = skinned(p, *v);
            assert!(
                close(r[0], e[0]) && close(r[1], e[1]),
                "{v:?} → {r:?}, expected {e:?}"
            );
        }
        let n = decompose_attachment(&p.attachment_frame(0));
        assert!(
            close(n.origin.x, att.0[0]) && close(n.origin.y, att.0[1]) && close(n.angle, att.1),
            "{n:?}"
        );
        assert!(n.scale.abs_diff_eq(Vec3::ONE, EPS));
    }

    const V1: [[f32; 2]; 3] = [[0.0, 0.0], [20.0, 0.0], [0.0, 20.0]];
    const V2: [[f32; 2]; 3] = [[2.9289, -7.0711], [17.0711, 7.0711], [-11.2132, 7.0711]];
    const V2_ATT: ([f32; 2], f32) = ([6.4645, 3.5355], FRAC_PI_4);
    const V7: [[f32; 2]; 3] = [[0.7612, -3.8268], [19.2388, 3.8268], [-6.8925, 14.6508]];
    const V7_ATT: ([f32; 2], f32) = ([8.0866, 4.6194], FRAC_PI_8);

    #[test]
    fn v1_rest_pose() {
        let mut p = Puppet::new(fp1(AnimationMode::Loop), Vec::new());
        assert!(!p.evaluate(), "rest pose: K stays the identity");
        assert_eq!(p.skinning()[0], Mat4::IDENTITY);
        check(&p, 0.0, V1, ([10.0, 5.0], 0.0));
    }

    #[test]
    fn v2_v3_loop() {
        let mut p = puppet(AnimationMode::Loop, &[(1.0, 1.0, true)]);
        run(&mut p, 0.5);
        check(&p, FRAC_PI_4, V2, V2_ATT);
        // V3: t = 1 s wraps to frame 0; sample N is never held in loop mode.
        run(&mut p, 0.5);
        check(&p, 0.0, V1, ([10.0, 5.0], 0.0));
    }

    #[test]
    fn v4_rate() {
        let mut p = puppet(AnimationMode::Loop, &[(2.0, 1.0, true)]);
        run(&mut p, 0.25);
        check(&p, FRAC_PI_4, V2, V2_ATT);
    }

    #[test]
    fn v5_mirror() {
        let mut p = puppet(AnimationMode::Mirror, &[(1.0, 1.0, true)]);
        run(&mut p, 1.5);
        assert!((p.layers[0].phase(&p.rig.animations[0]) - 0.5).abs() < 1e-9);
        check(&p, FRAC_PI_4, V2, V2_ATT);
        run(&mut p, 0.25);
        assert!((p.layers[0].phase(&p.rig.animations[0]) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn v6_single_halts_on_the_last_frame() {
        let mut p = puppet(AnimationMode::Single, &[(1.0, 1.0, true)]);
        run(&mut p, 0.5);
        assert!(p.layers[0].is_playing() && !p.layers[0].has_ended());
        run(&mut p, 2.5);
        assert!(!p.layers[0].is_playing() && p.layers[0].has_ended());
        assert_eq!(p.layers[0].time(), 1.0);
        check(
            &p,
            FRAC_PI_2,
            [[10.0, -10.0], [10.0, 10.0], [-10.0, -10.0]],
            ([5.0, 0.0], FRAC_PI_2),
        );
        let k = p.skinning()[0];
        assert!(close(k.w_axis.x, 10.0) && close(k.w_axis.y, -10.0));
    }

    #[test]
    fn v7_additive_half_weight() {
        let mut p = puppet(AnimationMode::Loop, &[(1.0, 0.5, true)]);
        run(&mut p, 0.5);
        check(&p, FRAC_PI_8, V7, V7_ATT);
    }

    #[test]
    fn v8_two_additive_layers_sum() {
        let mut p = puppet(AnimationMode::Loop, &[(1.0, 1.0, true), (1.0, 0.5, true)]);
        run(&mut p, 0.5);
        let theta = 3.0 * FRAC_PI_8;
        let n = decompose_attachment(&p.attachment_frame(0));
        assert!(close(n.angle, theta));
        let e = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0))
            * Mat4::from_rotation_z(theta)
            * Mat4::from_translation(Vec3::new(-10.0, 0.0, 0.0));
        assert!(p.skinning()[0].abs_diff_eq(e, EPS));
    }

    #[test]
    fn v9_replacing_layers() {
        // Layer 1: π/4; layer 2 (rate 2, f = 1 wraps): 0, mixed at 0.5 → π/8.
        let mut p = puppet(AnimationMode::Loop, &[(1.0, 1.0, false), (2.0, 0.5, false)]);
        run(&mut p, 0.5);
        check(&p, FRAC_PI_8, V7, V7_ATT);
    }

    #[test]
    fn v10_invisible_layer() {
        let mut p = puppet(AnimationMode::Loop, &[(1.0, 1.0, true)]);
        p.layers[0].visible = false;
        run(&mut p, 0.5);
        check(&p, 0.0, V1, ([10.0, 5.0], 0.0));
        // The clock still moved (spec 08 §9.3).
        assert_eq!(p.layers[0].time(), 0.5);
        p.layers[0].visible = true;
        assert!(p.evaluate());
        check(&p, FRAC_PI_4, V2, V2_ATT);
    }

    #[test]
    fn gpu_palette_for_local_pixels() {
        let mut p = puppet(AnimationMode::Loop, &[(1.0, 1.0, true)]);
        run(&mut p, 0.5);
        let mut out = [0.0f32; 12];
        p.write_palette(Vec2::new(20.0, 20.0), &mut out);
        let expected = [
            0.707107, -0.707107, 0.0, 0.707107, 0.707107, 0.0, 0.0, 0.0, 1.0, -5.355339, 27.071068,
            0.0,
        ];
        for (a, b) in out.iter().zip(expected) {
            assert!(close(*a, b), "{out:?}");
        }
        // C(s1) = (40, 20) becomes (37.0711, 12.9289) = C(s1').
        let rest = [Vertex {
            position: [40.0, 20.0, 0.0],
            uv: [1.0, 1.0],
        }];
        let skin = [SkinVertex::new(
            [0; 4],
            [1.0, 0.0, 0.0, 0.0],
            [0.0; 3],
            [0.0; 4],
        )];
        let mut cpu = [Vertex::default()];
        p.skin_vertices(Vec2::new(20.0, 20.0), &rest, &skin, &mut cpu);
        assert!(close(cpu[0].position[0], 37.0711) && close(cpu[0].position[1], 12.9289));
        assert_eq!(cpu[0].uv, [1.0, 1.0]);
    }

    #[test]
    fn two_bone_blend_is_not_renormalised() {
        // K0 = identity, K1 = T(0, 10, 0), vertex (5, 5) (no pixel conversion: half = 0
        // flips y, so compare in mesh space through C·K·C⁻¹ with half = 0).
        let rig = Rig::from_parts(
            vec![(None, t(0.0, 0.0, 0.0)), (Some(0), t(0.0, 0.0, 0.0))],
            Vec::new(),
            vec![Animation {
                id: 1,
                name: String::new(),
                mode_name: String::new(),
                mode: AnimationMode::Loop,
                fps: 1.0,
                frames: 1,
                tracks: vec![
                    vec![
                        BoneSample {
                            scale: [1.0; 3],
                            ..Default::default()
                        };
                        2
                    ],
                    vec![
                        BoneSample {
                            position: [0.0, 10.0, 0.0],
                            scale: [1.0; 3],
                            ..Default::default()
                        };
                        2
                    ],
                ],
            }],
        );
        let mut p = Puppet::new(rig, vec![Layer::new(Some(0))]);
        p.evaluate();
        assert!(
            p.skinning()[1].abs_diff_eq(Mat4::from_translation(Vec3::new(0.0, 10.0, 0.0)), EPS)
        );
        let to_px = |x: f32, y: f32| [x, -y, 0.0];
        let rest = [Vertex {
            position: to_px(5.0, 5.0),
            uv: [0.0; 2],
        }; 2];
        let skin = [
            SkinVertex::new([0, 1, 0, 0], [0.25, 0.75, 0.0, 0.0], [0.0; 3], [0.0; 4]),
            SkinVertex::new([0, 1, 0, 0], [0.25, 0.25, 0.0, 0.0], [0.0; 3], [0.0; 4]),
        ];
        let mut out = [Vertex::default(); 2];
        p.skin_vertices(Vec2::ZERO, &rest, &skin, &mut out);
        assert!(close(out[0].position[0], 5.0) && close(-out[0].position[1], 12.5));
        assert!(close(out[1].position[0], 2.5) && close(-out[1].position[1], 5.0));
    }

    #[test]
    fn attached_child_composition() {
        // Spec 08 §10 "Enfant attaché": parent origin (100, 200), scale 2, angle 0.
        use lumen_scene::Transform;
        let mut p = puppet(AnimationMode::Loop, &[(1.0, 1.0, true)]);
        let parent = Transform {
            origin: Vec3::new(100.0, 200.0, 0.0),
            scale: Vec3::new(2.0, 2.0, 1.0),
            angle: 0.0,
        };
        let child = Transform {
            origin: Vec3::new(3.0, 4.0, 0.0),
            ..Transform::default()
        };
        let resolve = |p: &Puppet| {
            let n = decompose_attachment(&p.attachment_frame(0));
            let node = parent.then(&Transform {
                origin: n.origin,
                scale: n.scale,
                angle: n.angle,
            });
            (node, node.then(&child))
        };
        p.evaluate();
        let (node, c) = resolve(&p);
        assert!(node.origin.abs_diff_eq(Vec3::new(120.0, 210.0, 0.0), EPS));
        assert!(c.origin.abs_diff_eq(Vec3::new(126.0, 218.0, 0.0), EPS));
        run(&mut p, 0.5);
        let (node, c) = resolve(&p);
        assert!(
            node.origin
                .abs_diff_eq(Vec3::new(112.928932, 207.071068, 0.0), EPS),
            "{node:?}"
        );
        assert!(close(node.angle, FRAC_PI_4));
        assert!(node.scale.abs_diff_eq(Vec3::new(2.0, 2.0, 1.0), EPS));
        assert!(
            c.origin
                .abs_diff_eq(Vec3::new(111.514718, 216.970563, 0.0), EPS),
            "{c:?}"
        );
        assert!(close(c.angle, FRAC_PI_4));
    }

    #[test]
    fn euler_order_and_rest_decomposition() {
        let pose = Pose {
            pos: Vec3::new(1.0, 2.0, 3.0),
            ang: Vec3::new(0.3, -0.2, 0.7),
            scl: Vec3::new(2.0, 0.5, 1.5),
        };
        let expected = Mat4::from_translation(pose.pos)
            * Mat4::from_rotation_z(0.7)
            * Mat4::from_rotation_y(-0.2)
            * Mat4::from_rotation_x(0.3)
            * Mat4::from_scale(pose.scl);
        assert!(pose.matrix().abs_diff_eq(expected, 1e-5));
        let back = Pose::from_matrix(&expected);
        assert!(back.pos.abs_diff_eq(pose.pos, 1e-5));
        assert!(back.ang.abs_diff_eq(pose.ang, 1e-5), "{back:?}");
        assert!(back.scl.abs_diff_eq(pose.scl, 1e-5));
    }

    #[test]
    fn ramps_and_edge_cases() {
        let mut p = puppet(AnimationMode::Single, &[(1.0, 1.0, true)]);
        p.layers[0].blend_in = true;
        p.layers[0].blend_time = 0.5;
        run(&mut p, 0.25);
        // φ = 0.25 → θ = π/8, ramp-in 0.5 → π/16.
        let n = decompose_attachment(&p.attachment_frame(0));
        assert!(close(n.angle, FRAC_PI_8 / 2.0), "{n:?}");
        p.layers[0].blend_in = false;
        p.layers[0].blend_out = true;
        run(&mut p, 0.5);
        // φ = 0.75: remaining 0.25 s of a 0.5 s ramp-out → 0.5 · 3π/8.
        let n = decompose_attachment(&p.attachment_frame(0));
        assert!(close(n.angle, 0.5 * 3.0 * FRAC_PI_8), "{n:?}");
        // fps 0 / N 0: sample 0; empty track: rest.
        let mut a = fp1(AnimationMode::Loop).animations[0].clone();
        a.fps = 0.0;
        let l = Layer::new(Some(0));
        assert_eq!(l.phase(&a), 0.0);
        assert!(super::sample(&[], 0.5, 1).is_none());
        // Negative rate plays backwards.
        let mut p = puppet(AnimationMode::Loop, &[(-1.0, 1.0, true)]);
        run(&mut p, 0.25);
        assert!((p.layers[0].phase(&p.rig.animations[0]) - 0.75).abs() < 1e-9);
    }
}

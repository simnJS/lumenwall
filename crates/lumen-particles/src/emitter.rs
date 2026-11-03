//! Emitters (spec 05 §6, amendments A6.1-c and A6.4).

use crate::defaults as d;
use crate::initializer::{self, InitializerRt, SpawnCtx, SpawnInfo};
use crate::model::{EmitterDef, EmitterKind};
use crate::pool::{BaseValues, Pool};
use crate::rng::Rng;
use glam::{Mat4, Vec3, Vec3A};
use std::f32::consts::TAU;

/// Emission bitmap of a `layerimage` emitter (A6.4): the texels whose alpha is at least 0.5,
/// computed once at load, and the mapping from the layer's texture coordinates to the scene.
#[derive(Clone, Debug, PartialEq)]
pub struct EmitterImage {
    width: u32,
    height: u32,
    /// Retained texels, `y * width + x`.
    texels: Vec<u32>,
    uv_to_scene: Mat4,
}

impl EmitterImage {
    /// Builds the bitmap from the alpha channel (`width × height` bytes, row 0 at the top, `v`
    /// down) of the source layer's base texture. `uv_to_scene` maps `(u, v, 0)` with `u, v ∈ [0, 1]`
    /// to the centred scene space (Y up), with the same transform as the layer's own rendering.
    /// Larger bitmaps are reduced to at most 256×256 texels (nearest sample).
    pub fn from_alpha(width: u32, height: u32, alpha: &[u8], uv_to_scene: Mat4) -> Self {
        let (sw, sh) = (
            width.min(d::LAYER_MAX_BITMAP),
            height.min(d::LAYER_MAX_BITMAP),
        );
        let mut texels = Vec::new();
        if alpha.len() >= (width as usize) * (height as usize) && sw > 0 && sh > 0 {
            for y in 0..sh {
                let src_y = (u64::from(y) * u64::from(height) / u64::from(sh)) as usize;
                for x in 0..sw {
                    let src_x = (u64::from(x) * u64::from(width) / u64::from(sw)) as usize;
                    if alpha[src_y * width as usize + src_x] >= d::LAYER_ALPHA_THRESHOLD {
                        texels.push(y * sw + x);
                    }
                }
            }
        }
        Self {
            width: sw,
            height: sh,
            texels,
            uv_to_scene,
        }
    }

    /// Number of emitting texels.
    pub fn len(&self) -> usize {
        self.texels.len()
    }

    /// True when no texel can emit.
    pub fn is_empty(&self) -> bool {
        self.texels.is_empty()
    }

    /// Random point of a random retained texel, in centred scene space.
    fn sample(&self, rng: &mut Rng) -> Vec3 {
        let t = self.texels[rng.below(self.texels.len() as u32) as usize];
        let (x, y) = (t % self.width, t / self.width);
        let u = (x as f32 + rng.uniform(0.0, 1.0)) / self.width as f32;
        let v = (y as f32 + rng.uniform(0.0, 1.0)) / self.height as f32;
        self.uv_to_scene.transform_point3(Vec3::new(u, v, 0.0))
    }
}

/// Timers and accumulators of an emitter (§6.2, §6.3, A6.1-c).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct EmitterState {
    pub acc: f32,
    pub delay_left: f32,
    pub duration_timer: f32,
    pub periodic_timer: f32,
    pub periodic_duration: f32,
    pub periodic_delay: f32,
    pub emitting: bool,
    /// Box / layer: the `instantaneous` burst was emitted.
    pub burst_done: bool,
    /// Sphere: burst still to emit.
    pub burst_left: u32,
    /// Sphere: base lifetime frozen at creation (`1 × override.lifetime`).
    pub lifetime: f32,
    /// Particles emitted in the current periodic window (`maxtoemitperperiod`).
    pub period_emitted: u32,
    /// Incremented each time a periodic window starts.
    pub epoch: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct EmitterRt {
    pub def: EmitterDef,
    /// Control point index actually used (§6.1).
    pub cp: i32,
    /// `rate × override.rate`, read once.
    pub rate_eff: f32,
    pub state: EmitterState,
    /// `layerimage` emission bitmap (A6.4).
    pub image: Option<Box<EmitterImage>>,
}

impl EmitterRt {
    /// `first_cp_follows_mouse`: the first `controlpoint` entry of the file has bit 1.
    pub fn new(
        def: EmitterDef,
        first_cp_follows_mouse: bool,
        rate_mul: f32,
        lifetime_mul: f32,
    ) -> Self {
        // parity: an explicit -1 maps to control point 0 only if the file's *first* entry follows the mouse.
        let cp = if def.control_point == -1 && first_cp_follows_mouse {
            0
        } else {
            def.control_point
        };
        let mut rt = Self {
            rate_eff: def.rate * rate_mul,
            def,
            cp,
            state: EmitterState::default(),
            image: None,
        };
        rt.reset(lifetime_mul);
        rt
    }

    /// Back to the creation state (child instances are recycled).
    pub fn reset(&mut self, lifetime_mul: f32) {
        self.state = EmitterState {
            delay_left: self.def.delay,
            burst_left: self.def.instantaneous,
            lifetime: lifetime_mul,
            ..EmitterState::default()
        };
    }

    /// False once the emitter can no longer emit (instance end, A6.5 item 7).
    pub fn can_emit(&self) -> bool {
        let (def, st) = (&self.def, &self.state);
        if def.duration > 0.0 && st.duration_timer >= def.duration {
            return false;
        }
        match def.kind {
            EmitterKind::SphereRandom => st.burst_left > 0 || self.rate_eff > 0.0,
            EmitterKind::LayerImage if self.image.as_ref().is_none_or(|i| i.is_empty()) => false,
            _ => def.rate > 0.0 || (def.instantaneous > 0 && !st.burst_done),
        }
    }

    /// Runs the emitter for one frame (§6.1–§6.3, A6.1-c, A6.4).
    pub fn emit(
        &mut self,
        index: u32,
        dt: f32,
        pool: &mut Pool,
        inits: &mut [InitializerRt],
        ctx: &SpawnCtx<'_>,
        rng: &mut Rng,
    ) {
        // parity: a full pool freezes the emitter entirely, timers and accumulators included.
        if pool.full() {
            return;
        }
        let def = &self.def;
        let st = &mut self.state;
        if !gate(st, def, dt, rng) {
            return;
        }
        let mut count = match def.kind {
            EmitterKind::SphereRandom => sphere_count(st, def, self.rate_eff, dt),
            _ => box_count(st, def, self.rate_eff, dt),
        };
        let periodic = def.flags & EmitterDef::FLAG_PERIODIC != 0;
        if periodic && def.max_to_emit_per_period > 0 {
            count = count.min(u64::from(
                def.max_to_emit_per_period.saturating_sub(st.period_emitted),
            ));
        }
        if count == 0 {
            return;
        }
        let image = match def.kind {
            EmitterKind::LayerImage => match self.image.as_deref() {
                Some(img) if !img.is_empty() => Some(img),
                _ => return,
            },
            _ => None,
        };
        // parity: the default control point is 0, so every emitter is offset by CP0's position.
        let mut e = Vec3::new(def.origin.x, -def.origin.y, def.origin.z);
        if let Some(cp) = usize::try_from(self.cp).ok().and_then(|i| ctx.cps.get(i)) {
            e += cp.position;
        }
        let over = ctx.over;
        let base = BaseValues {
            color: over.colorn.vec3().into(),
            alpha: over.alpha.f32(),
            size: 20.0 * over.size.f32(),
            lifetime: match def.kind {
                EmitterKind::SphereRandom => st.lifetime,
                _ => over.lifetime.f32(),
            },
        };
        let info = SpawnInfo {
            spawn_point: e,
            emitter: index,
            epoch: st.epoch,
        };
        for _ in 0..count {
            if pool.full() {
                break; // parity: what does not fit (burst remainder included) is lost.
            }
            let i = pool.next_slot();
            let (p, v) = match (def.kind, image) {
                (EmitterKind::SphereRandom, _) => {
                    let p = sphere_offset(def, ctx.perspective, rng);
                    (e + p, sphere_velocity(def, p, rng))
                }
                (EmitterKind::LayerImage, Some(img)) => {
                    // Layer texel in scene space, brought to local space by subtracting O.
                    let mut p = img.sample(rng) - ctx.origin;
                    p.z = 0.0;
                    (p, Vec3::ZERO)
                }
                _ => (e + box_offset(def, rng), Vec3::ZERO),
            };
            pool.spawn(i, Vec3A::from(p), Vec3A::from(v), &base);
            pool.len += 1;
            st.period_emitted = st.period_emitted.saturating_add(1);
            initializer::apply_all(inits, pool, i, ctx, &info, rng);
        }
    }
}

/// Delay, duration and periodic windows (§6.2 steps 1–3; A6.1-c for the sphere). Returns whether
/// the emitter emits this frame.
fn gate(st: &mut EmitterState, def: &EmitterDef, dt: f32, rng: &mut Rng) -> bool {
    if st.delay_left > 0.0 {
        // parity: the frame where the delay runs out emits nothing; the excess is lost.
        st.delay_left -= dt;
        return false;
    }
    if def.duration > 0.0 {
        st.duration_timer += dt;
        if st.duration_timer >= def.duration {
            return false;
        }
    }
    if def.flags & EmitterDef::FLAG_PERIODIC != 0 {
        st.periodic_timer += dt;
        if st.emitting {
            if st.periodic_timer >= st.periodic_duration {
                st.emitting = false;
                st.periodic_timer = 0.0;
                st.periodic_delay = rng.uniform(def.min_periodic_delay, def.max_periodic_delay);
                return false;
            }
        } else if st.periodic_timer >= st.periodic_delay {
            st.emitting = true;
            st.periodic_timer = 0.0;
            st.periodic_duration =
                rng.uniform(def.min_periodic_duration, def.max_periodic_duration);
            st.period_emitted = 0;
            st.epoch = st.epoch.wrapping_add(1);
        } else {
            return false;
        }
    }
    true
}

/// Box (and layer image) particle count for this frame (§6.2 step 4).
fn box_count(st: &mut EmitterState, def: &EmitterDef, rate_eff: f32, dt: f32) -> u64 {
    let mut k = 0u64;
    if def.instantaneous > 0 && !st.burst_done {
        k = u64::from(def.instantaneous);
        st.burst_done = true;
    }
    if def.rate > 0.0 {
        st.acc += dt * rate_eff;
        let m = st.acc.trunc();
        st.acc -= m;
        let mut m = m.max(0.0) as u64;
        if def.flags & EmitterDef::FLAG_ONE_PER_FRAME != 0 && m > 1 {
            m = 1; // parity: the excess is dropped, not carried over.
        }
        k += m;
    }
    k
}

/// Sphere emitter particle count for this frame (§6.3 steps 1–2).
fn sphere_count(st: &mut EmitterState, def: &EmitterDef, rate_eff: f32, dt: f32) -> u64 {
    st.acc += dt * rate_eff;
    let m = st.acc.trunc();
    st.acc -= m;
    let mut k = m.max(0.0) as u64;
    if def.flags & EmitterDef::FLAG_ONE_PER_FRAME != 0 && k > 1 {
        k = 1;
    }
    if st.burst_left > 0 {
        // parity: the burst replaces the rate-based count of that frame instead of adding to it.
        k = u64::from(st.burst_left);
        st.burst_left = 0;
    }
    k
}

/// Box position offset (§6.2 step 5): two draws per axis, symmetric around the origin.
fn box_offset(def: &EmitterDef, rng: &mut Rng) -> Vec3 {
    let mut d = Vec3::ZERO;
    for a in 0..3 {
        let mut v = rng.uniform(def.distance_min[a], def.distance_max[a]);
        if rng.uniform(0.0, 1.0) < 0.5 {
            v = -v;
        }
        d[a] = v;
    }
    // parity: box directions have Y flipped (sphere directions do not).
    d * Vec3::new(def.directions.x, -def.directions.y, def.directions.z)
}

/// Applies a per-axis `sign` (1: positive, -1: negative, other: unchanged).
pub(crate) fn apply_sign(mut p: Vec3, sign: glam::IVec3) -> Vec3 {
    for a in 0..3 {
        match sign[a] {
            1 => p[a] = p[a].abs(),
            -1 => p[a] = -p[a].abs(),
            _ => {}
        }
    }
    p
}

/// Sphere position offset `p` (§6.3 step 3), before adding the origin.
fn sphere_offset(def: &EmitterDef, perspective: bool, rng: &mut Rng) -> Vec3 {
    let rmin = def.distance_min.x;
    let rmax = def.distance_max.x;
    let p = if perspective {
        let phi = rng.uniform(0.0, TAU);
        let c = rng.uniform(-1.0, 1.0);
        let s = (1.0 - c * c).sqrt();
        let u = Vec3::new(s * phi.cos(), s * phi.sin(), c);
        let rho = rng.uniform(rmin * rmin * rmin, rmax * rmax * rmax).cbrt();
        u * rho * def.directions
    } else {
        let theta = rng.uniform(0.0, TAU);
        let rho = rng.uniform(rmin * rmin, rmax * rmax).sqrt();
        let z = rng.uniform(-rmax, rmax);
        // parity: sphere directions are not Y-flipped.
        Vec3::new(rho * theta.cos(), rho * theta.sin(), z) * def.directions
    };
    apply_sign(p, def.sign)
}

/// Sphere initial velocity (§6.3): radial, not multiplied by `override.speed`.
fn sphere_velocity(def: &EmitterDef, p: Vec3, rng: &mut Rng) -> Vec3 {
    if def.speed_max > 0.0 || def.speed_min != 0.0 {
        let dir = if p == Vec3::ZERO {
            Vec3::Y
        } else {
            p.normalize()
        };
        dir * rng.uniform(def.speed_min, def.speed_max)
    } else {
        Vec3::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::InstanceOverride;
    use crate::system::ControlPoint;
    use serde_json::json;

    fn def(j: serde_json::Value) -> EmitterDef {
        EmitterDef::from_obj(j.as_object().unwrap()).unwrap()
    }

    fn run(
        e: &mut EmitterRt,
        pool: &mut Pool,
        cps: &[ControlPoint; 8],
        dt: f32,
        rng: &mut Rng,
    ) -> usize {
        let over = InstanceOverride::default();
        let ctx = SpawnCtx::basic(cps, &over, 1.0, false);
        let before = pool.len;
        e.emit(0, dt, pool, &mut [], &ctx, rng);
        pool.len - before
    }

    #[test]
    fn box_rate_accumulates_and_positions_are_symmetric() {
        let cps = [ControlPoint::default(); 8];
        let d = def(
            json!({"name": "boxrandom", "rate": 10, "distancemin": "5 10 0", "distancemax": "5 10 0",
                           "directions": "1 2 0", "origin": "100 50 0"}),
        );
        let mut e = EmitterRt::new(d, false, 1.0, 1.0);
        let mut pool = Pool::new(100, false, false, false);
        let mut rng = Rng::seeded(7);
        // 10/s × 0.05 s = 0.5 → nothing, then 1.0 → one particle.
        assert_eq!(run(&mut e, &mut pool, &cps, 0.05, &mut rng), 0);
        assert_eq!(run(&mut e, &mut pool, &cps, 0.05, &mut rng), 1);
        assert_eq!(run(&mut e, &mut pool, &cps, 0.25, &mut rng), 2);
        for i in 0..pool.len {
            let p = pool.pos[i];
            // E = (100, -50, 0); offset = (±5 × 1, ±10 × -2, 0).
            assert_eq!((p.x - 100.0).abs(), 5.0);
            assert_eq!((p.y + 50.0).abs(), 20.0);
            assert_eq!(p.z, 0.0);
            assert_eq!(pool.vel[i], Vec3A::ZERO);
            assert_eq!(pool.size[i], 20.0);
            assert_eq!(pool.lifetime[i], 1.0);
            assert_eq!(pool.frame[i], -1.0);
        }
    }

    #[test]
    fn box_burst_one_per_frame_and_full_pool() {
        let cps = [ControlPoint::default(); 8];
        let d = def(json!({"name": "boxrandom", "rate": 100, "instantaneous": 3, "flags": 2}));
        let mut e = EmitterRt::new(d, false, 1.0, 1.0);
        let mut pool = Pool::new(5, false, false, false);
        let mut rng = Rng::seeded(1);
        // Burst 3 + rate 100 × 0.1 = 10 → capped to 1 (excess dropped).
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 4);
        let st = &e.state;
        assert_eq!(st.acc, 0.0);
        // Pool of 5: one more fits, then frozen.
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 1);
        let st = e.state.clone();
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 0);
        assert_eq!(e.state, st, "timers frozen while the pool is full");
    }

    #[test]
    fn box_delay_and_duration() {
        let cps = [ControlPoint::default(); 8];
        let d = def(json!({"name": "boxrandom", "rate": 10, "delay": 0.15, "duration": 0.35}));
        let mut e = EmitterRt::new(d, false, 1.0, 1.0);
        let mut pool = Pool::new(100, false, false, false);
        let mut rng = Rng::seeded(1);
        // delay: 0.15 → 0.05 → -0.05 (that frame emits nothing).
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 0);
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 0);
        // duration timer 0.1, 0.2, 0.3: emits 1 per frame; 0.4 ≥ 0.35 stops for good.
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 1);
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 1);
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 1);
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 0);
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 0);
    }

    #[test]
    fn box_periodic() {
        let cps = [ControlPoint::default(); 8];
        let d = def(json!({"name": "boxrandom", "rate": 10, "flags": 4,
            "minperiodicduration": 0.25, "maxperiodicduration": 0.25,
            "minperiodicdelay": 0.15, "maxperiodicdelay": 0.15}));
        let mut e = EmitterRt::new(d, false, 1.0, 1.0);
        let mut pool = Pool::new(100, false, false, false);
        let mut rng = Rng::seeded(1);
        // Starts immediately (delay 0) and emits that frame.
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 1);
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 1); // timer 0.1
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 1); // timer 0.2
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 0); // 0.3 ≥ 0.25 → stop, delay 0.15
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 0); // 0.1 < 0.15
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 1); // 0.2 ≥ 0.15 → emit
    }

    #[test]
    fn sphere_burst_replaces_rate_and_cp_offset() {
        let mut cps = [ControlPoint::default(); 8];
        cps[0].position = Vec3::new(10.0, 20.0, 0.0);
        let d = def(
            json!({"name": "sphererandom", "rate": 30, "instantaneous": 2,
            "distancemin": 10, "distancemax": 10, "speedmin": 5, "speedmax": 5, "origin": "0 4 0"}),
        );
        let mut e = EmitterRt::new(d, false, 1.0, 2.5);
        let mut pool = Pool::new(100, false, false, false);
        let mut rng = Rng::seeded(3);
        // Rate gives 3, burst replaces it with 2.
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 2);
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 3);
        let centre = Vec3A::new(10.0, 16.0, 0.0);
        for i in 0..pool.len {
            let r = pool.pos[i] - centre;
            assert!((r.length() - 10.0).abs() < 1e-3, "ring radius");
            assert_eq!(r.z, 0.0, "directions (1,1,0) cancel z");
            assert!((pool.vel[i].length() - 5.0).abs() < 1e-4);
            assert!((pool.vel[i].normalize() - r.normalize()).length() < 1e-4);
            assert_eq!(pool.lifetime[i], 2.5, "frozen lifetime");
        }
    }

    #[test]
    fn sphere_sign_and_perspective() {
        let cps = [ControlPoint::default(); 8];
        let d = def(
            json!({"name": "sphererandom", "instantaneous": 50, "distancemin": 0,
            "distancemax": 20, "directions": "1 1 1", "sign": [1, -1, 0]}),
        );
        let mut e = EmitterRt::new(d, false, 1.0, 1.0);
        let mut pool = Pool::new(100, false, false, false);
        let mut rng = Rng::seeded(3);
        let over = InstanceOverride::default();
        let ctx = SpawnCtx::basic(&cps, &over, 0.0, true);
        e.emit(0, 0.01, &mut pool, &mut [], &ctx, &mut rng);
        assert_eq!(pool.len, 50);
        let mut any_neg_z = false;
        for i in 0..50 {
            let p = pool.pos[i];
            assert!(p.x >= 0.0 && p.y <= 0.0 && p.length() <= 20.0 + 1e-3);
            any_neg_z |= p.z < 0.0;
            assert_eq!(pool.vel[i], Vec3A::ZERO);
        }
        assert!(any_neg_z);
    }

    #[test]
    fn explicit_minus_one_control_point() {
        let d = def(json!({"name": "boxrandom", "controlpoint": -1}));
        assert_eq!(EmitterRt::new(d.clone(), true, 1.0, 1.0).cp, 0);
        assert_eq!(EmitterRt::new(d, false, 1.0, 1.0).cp, -1);
        let r = EmitterRt::new(
            def(json!({"name": "boxrandom", "rate": 4})),
            false,
            2.5,
            1.0,
        );
        assert_eq!(r.rate_eff, 10.0);
    }

    #[test]
    fn sphere_gets_delay_duration_and_periodic_cap() {
        let cps = [ControlPoint::default(); 8];
        // A6.1-c: delay then duration apply to the sphere too.
        let d = def(json!({"name": "sphererandom", "rate": 10, "delay": 0.15, "duration": 0.35}));
        let mut e = EmitterRt::new(d, false, 1.0, 1.0);
        let mut pool = Pool::new(100, false, false, false);
        let mut rng = Rng::seeded(1);
        let got: Vec<usize> = (0..7)
            .map(|_| run(&mut e, &mut pool, &cps, 0.1, &mut rng))
            .collect();
        assert_eq!(got, [0, 0, 1, 1, 1, 0, 0]);
        assert!(!e.can_emit());
        // maxtoemitperperiod: windows of 1 s (rate 100) capped to 32 particles.
        let d = def(
            json!({"name": "boxrandom", "rate": 100, "flags": 4, "maxtoemitperperiod": 32,
            "minperiodicduration": 1, "maxperiodicduration": 1, "minperiodicdelay": 1, "maxperiodicdelay": 1}),
        );
        let mut e = EmitterRt::new(d, false, 1.0, 1.0);
        let mut pool = Pool::new(1000, false, false, false);
        let total: usize = (0..10)
            .map(|_| run(&mut e, &mut pool, &cps, 0.1, &mut rng))
            .sum();
        assert_eq!(total, 32, "one window of 1 s, capped");
        assert_eq!(e.state.epoch, 1);
    }

    #[test]
    fn layer_image_emitter() {
        let cps = [ControlPoint::default(); 8];
        let d = def(json!({"name": "layerimage", "rate": 100}));
        let mut e = EmitterRt::new(d, false, 1.0, 1.0);
        let mut pool = Pool::new(100, false, false, false);
        let mut rng = Rng::seeded(1);
        // Without an image: nothing, and the emitter cannot emit.
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 0);
        assert!(!e.can_emit());
        // 4×2 bitmap where only texel (3, 1) is opaque; uv mapped to a 400×200 rect at (1000, 500).
        let mut alpha = [0u8; 8];
        alpha[7] = 255;
        let uv_to_scene = glam::Mat4::from_translation(Vec3::new(1000.0, 500.0, 0.0))
            * glam::Mat4::from_scale(Vec3::new(400.0, 200.0, 1.0));
        let img = EmitterImage::from_alpha(4, 2, &alpha, uv_to_scene);
        assert_eq!(img.len(), 1);
        e.image = Some(Box::new(img));
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 10);
        for i in 0..pool.len {
            let p = pool.pos[i];
            assert!(
                (1300.0..1400.0).contains(&p.x) && (600.0..700.0).contains(&p.y) && p.z == 0.0,
                "{p}"
            );
        }
    }
}

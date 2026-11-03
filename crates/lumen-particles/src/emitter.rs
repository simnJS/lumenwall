//! Emitters (spec 05 §6).

use crate::initializer::{self, InitializerRt, SpawnCtx};
use crate::model::{EmitterDef, EmitterKind};
use crate::pool::{BaseValues, Pool};
use crate::rng::Rng;
use glam::{Vec3, Vec3A};
use std::f32::consts::TAU;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BoxState {
    pub acc: f32,
    pub delay_left: f32,
    pub duration_timer: f32,
    pub periodic_timer: f32,
    pub periodic_duration: f32,
    pub periodic_delay: f32,
    pub emitting: bool,
    pub burst_done: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SphereState {
    pub acc: f32,
    pub burst_left: u32,
    /// Base lifetime frozen at creation (`1 × override.lifetime`).
    pub lifetime: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EmitterState {
    Box(BoxState),
    Sphere(SphereState),
}

#[derive(Clone, Debug)]
pub(crate) struct EmitterRt {
    pub def: EmitterDef,
    /// Control point index actually used (§6.1).
    pub cp: i32,
    /// `rate × override.rate`, read once.
    pub rate_eff: f32,
    pub state: EmitterState,
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
        let state = match def.kind {
            EmitterKind::BoxRandom => EmitterState::Box(BoxState {
                delay_left: def.delay,
                ..BoxState::default()
            }),
            EmitterKind::SphereRandom => EmitterState::Sphere(SphereState {
                acc: 0.0,
                burst_left: def.instantaneous,
                lifetime: lifetime_mul,
            }),
        };
        Self {
            rate_eff: def.rate * rate_mul,
            def,
            cp,
            state,
        }
    }

    /// Runs the emitter for one frame (§6.1–§6.3).
    pub fn emit(
        &mut self,
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
        let count = match &mut self.state {
            EmitterState::Box(st) => box_count(st, def, self.rate_eff, dt, rng),
            EmitterState::Sphere(st) => sphere_count(st, def, self.rate_eff, dt),
        };
        if count == 0 {
            return;
        }
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
            lifetime: match &self.state {
                EmitterState::Box(_) => over.lifetime.f32(),
                EmitterState::Sphere(st) => st.lifetime,
            },
        };
        for _ in 0..count {
            if pool.full() {
                break; // parity: what does not fit (burst remainder included) is lost.
            }
            let i = pool.next_slot();
            let (p, v) = match def.kind {
                EmitterKind::BoxRandom => (e + box_offset(def, rng), Vec3::ZERO),
                EmitterKind::SphereRandom => {
                    let p = sphere_offset(def, ctx.perspective, rng);
                    (e + p, sphere_velocity(def, p, rng))
                }
            };
            pool.spawn(i, Vec3A::from(p), Vec3A::from(v), &base);
            pool.len += 1;
            initializer::apply_all(inits, pool, i, ctx, rng);
        }
    }
}

/// Box emitter timers and particle count for this frame (§6.2 steps 1–4).
fn box_count(st: &mut BoxState, def: &EmitterDef, rate_eff: f32, dt: f32, rng: &mut Rng) -> u64 {
    if st.delay_left > 0.0 {
        // parity: the frame where the delay runs out emits nothing; the excess is lost.
        st.delay_left -= dt;
        return 0;
    }
    if def.duration > 0.0 {
        st.duration_timer += dt;
        if st.duration_timer >= def.duration {
            return 0;
        }
    }
    if def.flags & EmitterDef::FLAG_PERIODIC != 0 {
        st.periodic_timer += dt;
        if st.emitting {
            if st.periodic_timer >= st.periodic_duration {
                st.emitting = false;
                st.periodic_timer = 0.0;
                st.periodic_delay = rng.uniform(def.min_periodic_delay, def.max_periodic_delay);
                return 0;
            }
        } else if st.periodic_timer >= st.periodic_delay {
            st.emitting = true;
            st.periodic_timer = 0.0;
            st.periodic_duration =
                rng.uniform(def.min_periodic_duration, def.max_periodic_duration);
        } else {
            return 0;
        }
    }
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
fn sphere_count(st: &mut SphereState, def: &EmitterDef, rate_eff: f32, dt: f32) -> u64 {
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

/// Sphere position offset `p` (§6.3 step 3), before adding the origin.
fn sphere_offset(def: &EmitterDef, perspective: bool, rng: &mut Rng) -> Vec3 {
    let rmin = def.distance_min.x;
    let rmax = def.distance_max.x;
    let mut p = if perspective {
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
    for a in 0..3 {
        match def.sign[a] {
            1 => p[a] = p[a].abs(),
            -1 => p[a] = -p[a].abs(),
            _ => {}
        }
    }
    p
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
        let ctx = SpawnCtx {
            cps,
            over: &over,
            time: 1.0,
            perspective: false,
        };
        let before = pool.len;
        e.emit(dt, pool, &mut [], &ctx, rng);
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
        let EmitterState::Box(st) = &e.state else {
            unreachable!()
        };
        assert_eq!(st.acc, 0.0);
        // Pool of 5: one more fits, then frozen.
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 1);
        let EmitterState::Box(st) = e.state.clone() else {
            unreachable!()
        };
        assert_eq!(run(&mut e, &mut pool, &cps, 0.1, &mut rng), 0);
        assert_eq!(
            e.state,
            EmitterState::Box(st),
            "timers frozen while the pool is full"
        );
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
        let ctx = SpawnCtx {
            cps: &cps,
            over: &over,
            time: 0.0,
            perspective: true,
        };
        e.emit(0.01, &mut pool, &mut [], &ctx, &mut rng);
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
}

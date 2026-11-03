//! Operators (spec 05 §8). Each operator reads its [U] parameters once per frame, then runs one
//! monomorphic loop over the live particles.

use crate::model::{OperatorDef, OscillateDef, RampDef, TurbulenceDef, VortexDef};
use crate::noise::curl;
use crate::pool::{Live, Osc, OscPos, Pool};
use crate::rng::Rng;
use crate::system::ControlPoint;
use glam::{Vec3, Vec3A};
use std::f32::consts::{PI, TAU};

/// Per-frame operator context.
pub(crate) struct OpCtx<'a> {
    pub dt: f32,
    /// `t_g`, seconds.
    pub time: f32,
    /// `override.speed` (`S`).
    pub speed: f32,
    pub cps: &'a [ControlPoint; 8],
}

#[derive(Clone, Debug)]
pub(crate) struct OperatorRt {
    pub def: OperatorDef,
    /// `turbulence`: phase and speed drawn once at creation.
    pub turb_phase: f32,
    pub turb_speed: f32,
    /// `vortex` with `audioprocessingmode > 0` at creation.
    pub inactive: bool,
}

impl OperatorRt {
    /// Creates the runtime operator; `turbulence` draws its phase then its speed here (§8.7).
    pub fn new(def: OperatorDef, rng: &mut Rng) -> Self {
        let mut rt = Self {
            def,
            turb_phase: 0.0,
            turb_speed: 0.0,
            inactive: false,
        };
        match &rt.def {
            OperatorDef::Turbulence(t) => {
                // parity: phase and speed are drawn once for the whole system, not per particle.
                rt.turb_phase = rng.uniform(t.phase_min.f32(), t.phase_max.f32());
                rt.turb_speed = rng.uniform(t.speed_min.f32(), t.speed_max.f32());
            }
            OperatorDef::Vortex(v) => {
                // parity: audio-reactive vortex is fully inactive (no audio amplitude).
                rt.inactive = v.audio_processing_mode.f32() > 0.0;
            }
            _ => {}
        }
        rt
    }

    pub fn run(&mut self, pool: &mut Pool, ctx: &OpCtx<'_>, rng: &mut Rng) {
        if pool.len == 0 {
            return;
        }
        let l = pool.live();
        let n = l.pos.len();
        let (dt, s) = (ctx.dt, ctx.speed);
        match &self.def {
            OperatorDef::Movement { drag, gravity } => {
                let g = gravity.vec3();
                let g = Vec3A::new(g.x, -g.y, g.z) * dt * s;
                let damp = (1.0 - drag.f32() * dt).max(0.0);
                for (p, v) in l.pos.iter_mut().zip(l.vel.iter_mut()) {
                    // Position uses the previous frame's velocity; S does not scale this step.
                    *p += *v * dt;
                    *v = (*v + g) * damp;
                }
            }
            OperatorDef::AngularMovement { drag, force } => {
                let f = Vec3A::from(force.vec3()) * dt * s;
                let damp = (1.0 - drag.f32() * dt).max(0.0);
                for (r, w) in l.rot.iter_mut().zip(l.angvel.iter_mut()) {
                    let nr = *r + *w * dt * s;
                    *w = (*w + f) * damp;
                    // Fast path (one SIMD compare) when no component needs wrapping.
                    *r = if nr.abs().cmple(Vec3A::splat(PI)).all() {
                        nr
                    } else {
                        Vec3A::new(wrap_pi(nr.x), wrap_pi(nr.y), wrap_pi(nr.z))
                    };
                }
            }
            OperatorDef::AlphaFade {
                fade_in_time,
                fade_out_time,
            } => {
                let (fin, fout) = (fade_in_time.f32(), fade_out_time.f32());
                for i in 0..n {
                    let lf = life(l.age[i], l.lifetime[i]);
                    let ia = l.init_alpha[i];
                    l.alpha[i] = if lf <= fin {
                        ia * ramp(lf, 0.0, fin, 0.0, 1.0)
                    } else if lf > fout {
                        ia * (1.0 - ramp(lf, fout, 1.0, 0.0, 1.0))
                    } else {
                        ia
                    };
                }
                set_base(l.osc_alpha, l.alpha);
            }
            OperatorDef::SizeChange(r) => {
                let [t0, t1, v0, v1] = ramp_params(r);
                for i in 0..n {
                    let lf = life(l.age[i], l.lifetime[i]);
                    l.size[i] = l.init_size[i] * ramp(lf, t0, t1, v0, v1);
                }
                set_base(l.osc_size, l.size);
            }
            OperatorDef::AlphaChange(r) => {
                let [t0, t1, v0, v1] = ramp_params(r);
                for i in 0..n {
                    let lf = life(l.age[i], l.lifetime[i]);
                    l.alpha[i] = l.init_alpha[i] * ramp(lf, t0, t1, v0, v1);
                }
                set_base(l.osc_alpha, l.alpha);
            }
            OperatorDef::ColorChange(r) => {
                let (t0, t1) = (r.start_time.f32(), r.end_time.f32());
                // [U] float vec3: no 0–255 conversion.
                let (v0, v1) = (r.start_value.vec3(), r.end_value.vec3());
                for i in 0..n {
                    let lf = life(l.age[i], l.lifetime[i]);
                    let m = Vec3A::new(
                        ramp(lf, t0, t1, v0.x, v1.x),
                        ramp(lf, t0, t1, v0.y, v1.y),
                        ramp(lf, t0, t1, v0.z, v1.z),
                    );
                    l.color[i] = l.init_color[i] * m;
                }
            }
            OperatorDef::Turbulence(t) => turbulence(t, self.turb_phase, self.turb_speed, l, ctx),
            OperatorDef::Vortex(v) => {
                if !self.inactive {
                    vortex(v, l, ctx);
                }
            }
            OperatorDef::ControlPointAttract {
                control_point,
                origin,
                scale,
                threshold,
            } => {
                let Some(cp) = usize::try_from(*control_point)
                    .ok()
                    .and_then(|i| ctx.cps.get(i))
                else {
                    return;
                };
                let c = Vec3A::from(cp.position + origin.vec3());
                let limit = threshold.f32() / 2.0;
                let k = scale.f32() * dt * s;
                for (p, v) in l.pos.iter().zip(l.vel.iter_mut()) {
                    let delta = c - *p;
                    let d = delta.length();
                    if d > 0.001 && d < limit {
                        *v += delta / d * k;
                    }
                }
            }
            OperatorDef::OscillateAlpha(o) => oscillate_scalar(o, l.osc_alpha, l.alpha, l.age, rng),
            OperatorDef::OscillateSize(o) => oscillate_scalar(o, l.osc_size, l.size, l.age, rng),
            OperatorDef::OscillatePosition(o) => {
                oscillate_position(o, l.osc_pos, l.pos, l.age, ctx, rng)
            }
        }
    }
}

/// Relative position in the particle's life (§3).
#[inline(always)]
pub(crate) fn life(age: f32, lifetime: f32) -> f32 {
    if lifetime > 0.0 { age / lifetime } else { 1.0 }
}

/// `rampe(life, t0, t1, v0, v1)` (§8.1).
#[inline(always)]
pub(crate) fn ramp(life: f32, t0: f32, t1: f32, v0: f32, v1: f32) -> f32 {
    // parity: `<= t0` is tested first, so t1 < t0 jumps from v0 to v1 without interpolation.
    if life <= t0 {
        v0
    } else if life >= t1 {
        v1
    } else {
        v0 + (life - t0) / (t1 - t0) * (v1 - v0)
    }
}

fn ramp_params(r: &RampDef) -> [f32; 4] {
    [
        r.start_time.f32(),
        r.end_time.f32(),
        r.start_value.f32(),
        r.end_value.f32(),
    ]
}

/// Updates the oscillator base when the oscillator exists (empty slice otherwise).
#[inline]
fn set_base(osc: &mut [Osc], values: &[f32]) {
    for (o, v) in osc.iter_mut().zip(values) {
        o.base = *v;
    }
}

/// Wraps an angle to `[-π, π]` by repeated additions/subtractions of 2π.
#[inline]
fn wrap_pi(mut r: f32) -> f32 {
    if !r.is_finite() {
        return r; // lumen: repeated subtraction would never terminate on ±inf/NaN.
    }
    if r.abs() > 1.0e4 {
        r -= TAU * (r / TAU).round(); // lumen: repeated subtraction stalls once 2π is below one ulp.
    }
    while r > PI {
        r -= TAU;
    }
    while r < -PI {
        r += TAU;
    }
    r
}

fn turbulence(t: &TurbulenceDef, phase: f32, speed: f32, l: Live<'_>, ctx: &OpCtx<'_>) {
    if speed <= 1e-4 {
        return;
    }
    let k = t.scale.f32() * 2.0;
    let shift = phase + t.time_scale.f32() * ctx.time;
    let mask = t.mask.vec3();
    let ds = ctx.dt * ctx.speed;
    for (p, v) in l.pos.iter().zip(l.vel.iter_mut()) {
        let mut q = Vec3::from(*p);
        q.x += shift;
        q *= k;
        let mut c = curl(q);
        let len = c.length();
        if len > 1e-4 {
            c = c / len * speed;
        }
        *v += Vec3A::from(c * mask * ds);
    }
}

fn vortex(v: &VortexDef, l: Live<'_>, ctx: &OpCtx<'_>) {
    let offset = v.offset.vec3();
    let centre = Vec3A::from(
        usize::try_from(v.control_point)
            .ok()
            .and_then(|i| ctx.cps.get(i))
            .map_or(offset, |cp| cp.position + offset),
    );
    let axis = v.axis.vec3();
    let a = Vec3A::from(if axis == Vec3::ZERO {
        Vec3::Z
    } else {
        axis.normalize()
    });
    let infinite = v.flags & VortexDef::FLAG_INFINITE_AXIS != 0;
    let center_force = v.flags & VortexDef::FLAG_CENTER_FORCE != 0;
    let ring = v.flags & VortexDef::FLAG_RING != 0;
    let (d_in, d_out) = (v.distance_inner.f32(), v.distance_outer.f32());
    let (s_in, s_out) = (v.speed_inner.f32(), v.speed_outer.f32());
    let cforce = v.center_force.f32();
    let (rr, rw) = (v.ring_radius.f32(), v.ring_width.f32());
    let (rpd, rpf) = (v.ring_pull_distance.f32(), v.ring_pull_force.f32());
    let ds = ctx.dt * ctx.speed;
    // parity: +0.1 in the transition width.
    let m = d_out - d_in + 0.1;
    let (r_in, r_out) = (rr - rw / 2.0, rr + rw / 2.0);
    for (p, vel) in l.pos.iter().zip(l.vel.iter_mut()) {
        let w = *p - centre;
        let rho = if infinite { w - a * w.dot(a) } else { w };
        let d = rho.length();
        let tau = a.cross(rho);
        let tl = tau.length();
        if tl <= 0.001 {
            continue;
        }
        let tau = tau / tl;
        let mut force = Vec3A::ZERO;
        let speed = if ring {
            if d < r_in {
                0.0
            } else if d <= r_out {
                s_in + (d - r_in) / rw * (s_out - s_in)
            } else if d <= r_out + rpd {
                let pull = (d - r_out) / rpd;
                if d > 0.001 {
                    // parity: the pull grows with the distance from the ring.
                    force = -(rho / d) * rpf * pull;
                }
                s_out * (1.0 - pull)
            } else {
                0.0
            }
        } else if m < 0.0 || d < d_in {
            s_in
        } else if d > d_out {
            s_out
        } else {
            s_in + (d - d_in) / m * (s_out - s_in)
        };
        *vel += tau * speed * ds;
        *vel += force * ds;
        if center_force && d > 0.001 {
            *vel += -(rho / d) * cforce * ds;
        }
    }
}

struct OscParams {
    fmin: f32,
    fmax: f32,
    smin: f32,
    smax: f32,
    pmin: f32,
    pmax: f32,
}

fn osc_params(o: &OscillateDef) -> OscParams {
    OscParams {
        fmin: o.frequency_min.f32(),
        fmax: o.frequency_max.f32(),
        smin: o.scale_min.f32(),
        smax: o.scale_max.f32(),
        pmin: o.phase_min.f32(),
        // parity: the phase is drawn in [phasemin, phasemax + 2π).
        pmax: o.phase_max.f32() + TAU,
    }
}

fn oscillate_scalar(
    o: &OscillateDef,
    osc: &mut [Osc],
    values: &mut [f32],
    age: &[f32],
    rng: &mut Rng,
) {
    let p = osc_params(o);
    for ((st, val), &age) in osc.iter_mut().zip(values.iter_mut()).zip(age) {
        if !st.init {
            st.freq = rng.uniform(p.fmin, p.fmax);
            // parity: a per-particle scale is drawn but never used.
            let _unused_scale = rng.uniform(p.smin, p.smax);
            st.phase = rng.uniform(p.pmin, p.pmax);
            st.base = *val;
            st.init = true;
        }
        // parity: the frequency is a pulsation (rad/s), not Hz.
        let c = ((st.freq * age + st.phase).cos() + 1.0) / 2.0;
        *val = st.base * (p.smin + c * (p.smax - p.smin));
    }
}

fn oscillate_position(
    o: &OscillateDef,
    osc: &mut [OscPos],
    pos: &mut [Vec3A],
    age: &[f32],
    ctx: &OpCtx<'_>,
    rng: &mut Rng,
) {
    let p = osc_params(o);
    let mask = o.mask.vec3();
    let k = mask * (ctx.dt * ctx.speed);
    // lumen: axes with a zero mask are skipped (their term is 0 for any finite amplitude/frequency).
    let active = [mask.x != 0.0, mask.y != 0.0, mask.z != 0.0];
    for ((st, pos), &age) in osc.iter_mut().zip(pos.iter_mut()).zip(age) {
        if !st.init {
            for a in 0..3 {
                st.freq[a] = rng.uniform(p.fmin, p.fmax);
                st.amp[a] = rng.uniform(p.smin, p.smax);
                st.phase[a] = rng.uniform(p.pmin, p.pmax);
            }
            st.init = true;
        }
        for a in 0..3 {
            if active[a] {
                // Euler step of d/dt[A cos(f t + φ)].
                let f = st.freq[a];
                pos[a] += -st.amp[a] * f * (f * age + st.phase[a]).sin() * k[a];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::BaseValues;
    use serde_json::json;

    fn pool_with(n: usize, osc: bool) -> Pool {
        let mut pool = Pool::new(n.max(1), osc, osc, osc);
        let base = BaseValues {
            color: Vec3A::ONE,
            alpha: 1.0,
            size: 20.0,
            lifetime: 1.0,
        };
        for i in 0..n {
            pool.spawn(i, Vec3A::ZERO, Vec3A::ZERO, &base);
        }
        pool.len = n;
        pool
    }

    fn op(j: serde_json::Value, rng: &mut Rng) -> OperatorRt {
        OperatorRt::new(OperatorDef::from_obj(j.as_object().unwrap()).unwrap(), rng)
    }

    fn ctx(cps: &[ControlPoint; 8], dt: f32) -> OpCtx<'_> {
        OpCtx {
            dt,
            time: 3.0,
            speed: 1.0,
            cps,
        }
    }

    const CPS: [ControlPoint; 8] = [ControlPoint {
        position: Vec3::ZERO,
        offset: Vec3::ZERO,
        follows_mouse: false,
        world_space: false,
    }; 8];

    #[test]
    fn ramp_function() {
        assert_eq!(ramp(0.0, 0.0, 1.0, 2.0, 4.0), 2.0);
        assert_eq!(ramp(0.25, 0.0, 1.0, 2.0, 4.0), 2.5);
        assert_eq!(ramp(1.5, 0.0, 1.0, 2.0, 4.0), 4.0);
        // t1 < t0: jumps.
        assert_eq!(ramp(0.5, 0.6, 0.2, 2.0, 4.0), 2.0);
        assert_eq!(ramp(0.7, 0.6, 0.2, 2.0, 4.0), 4.0);
        assert_eq!(life(0.5, 2.0), 0.25);
        assert_eq!(life(0.5, 0.0), 1.0);
    }

    #[test]
    fn movement_order_gravity_drag() {
        let mut rng = Rng::seeded(1);
        let mut pool = pool_with(1, false);
        pool.vel[0] = Vec3A::new(10.0, 0.0, 0.0);
        let mut m = op(
            json!({"name": "movement", "gravity": "0 100 0", "drag": 2}),
            &mut rng,
        );
        m.run(
            &mut pool,
            &OpCtx {
                dt: 0.1,
                time: 0.0,
                speed: 2.0,
                cps: &CPS,
            },
            &mut rng,
        );
        // 1. pos += v·dt (previous velocity, not × S) = (1, 0, 0)
        assert_eq!(pool.pos[0], Vec3A::new(1.0, 0.0, 0.0));
        // 2. v += (0, -100, 0)·0.1·2 = (10, -20, 0); 3. × max(0, 1 - 0.2) = (8, -16, 0)
        let v = pool.vel[0];
        assert!((v - Vec3A::new(8.0, -16.0, 0.0)).length() < 1e-5, "{v}");
        // Drag clamps at 0.
        let mut m = op(json!({"name": "movement", "drag": 50}), &mut rng);
        m.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        assert_eq!(pool.vel[0], Vec3A::ZERO);
    }

    #[test]
    fn angular_movement_wraps() {
        let mut rng = Rng::seeded(1);
        let mut pool = pool_with(1, false);
        pool.rot[0] = Vec3A::new(3.0, 0.0, -3.0);
        pool.angvel[0] = Vec3A::new(2.0, 0.0, -2.0);
        let mut m = op(
            json!({"name": "angularmovement", "force": "0 10 0"}),
            &mut rng,
        );
        m.run(
            &mut pool,
            &OpCtx {
                dt: 0.1,
                time: 0.0,
                speed: 2.0,
                cps: &CPS,
            },
            &mut rng,
        );
        // 3 + 2·0.1·2 = 3.4 > π → 3.4 - 2π
        let r = pool.rot[0];
        assert!(
            (r.x - (3.4 - TAU)).abs() < 1e-5 && (r.z - (TAU - 3.4)).abs() < 1e-5,
            "{r}"
        );
        // Force: ω += (0,10,0)·0.1·2, no Y flip.
        assert!((pool.angvel[0].y - 2.0).abs() < 1e-6);
        assert!(wrap_pi(1.0e9).abs() <= PI);
        assert!(wrap_pi(f32::INFINITY).is_infinite());
    }

    #[test]
    fn alpha_fade() {
        let mut rng = Rng::seeded(1);
        let mut pool = pool_with(4, true);
        for (i, age) in [0.1, 0.5, 0.7, 0.95].into_iter().enumerate() {
            pool.age[i] = age;
            pool.init_alpha[i] = 0.8;
        }
        let mut m = op(
            json!({"name": "alphafade", "fadeintime": 0.2, "fadeouttime": 0.6}),
            &mut rng,
        );
        m.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        let a = &pool.alpha;
        assert!((a[0] - 0.8 * 0.5).abs() < 1e-6);
        assert_eq!(a[1], 0.8);
        assert!((a[2] - 0.8 * (1.0 - 0.25)).abs() < 1e-6);
        assert!((a[3] - 0.8 * (1.0 - 0.875)).abs() < 1e-6);
        assert_eq!(pool.osc_alpha[2].base, a[2]);
    }

    #[test]
    fn size_alpha_color_change() {
        let mut rng = Rng::seeded(1);
        let mut pool = pool_with(2, true);
        pool.age[0] = 0.25;
        pool.age[1] = 0.75;
        pool.init_size[0] = 10.0;
        pool.init_size[1] = 10.0;
        let mut m = op(
            json!({"name": "sizechange", "startvalue": 2, "endvalue": 0}),
            &mut rng,
        );
        m.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        assert_eq!(pool.size[0], 15.0);
        assert_eq!(pool.size[1], 5.0);
        assert_eq!(pool.osc_size[0].base, 15.0);
        let mut m = op(
            json!({"name": "alphachange", "starttime": 0.5, "endtime": 1, "startvalue": 1, "endvalue": 0}),
            &mut rng,
        );
        m.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        assert_eq!(pool.alpha[0], 1.0);
        assert_eq!(pool.alpha[1], 0.5);
        pool.init_color[0] = Vec3A::new(0.5, 0.5, 0.5);
        let mut m = op(
            json!({"name": "colorchange", "startvalue": "255 0 1", "endvalue": "1 1 1"}),
            &mut rng,
        );
        m.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        // life 0.25: per channel 255 + 0.25·(1-255) = 191.5; 0 + 0.25 = 0.25; 1.
        let c = pool.color[0];
        assert!(
            (c - Vec3A::new(0.5 * 191.5, 0.5 * 0.25, 0.5)).length() < 1e-4,
            "{c}"
        );
    }

    #[test]
    fn turbulence_draws_once_and_adds_velocity() {
        let mut rng = Rng::seeded(11);
        let mut expect = rng.clone();
        let t = op(
            json!({"name": "turbulence", "phasemin": 1, "phasemax": 2, "speedmin": 300, "speedmax": 400}),
            &mut rng,
        );
        assert_eq!(t.turb_phase, expect.uniform(1.0, 2.0));
        assert_eq!(t.turb_speed, expect.uniform(300.0, 400.0));
        let mut pool = pool_with(2, false);
        pool.pos[0] = Vec3A::new(120.0, -40.0, 0.0);
        pool.pos[1] = Vec3A::new(-300.0, 75.0, 0.0);
        let mut t = t;
        t.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        for i in 0..2 {
            // |c| normalised to V, masked (1,1,0), × dt.
            let v = pool.vel[i];
            assert_eq!(v.z, 0.0);
            assert!(v.length() <= t.turb_speed * 0.1 + 1e-3 && v.length() > 0.0);
        }
        // Hand check of one particle against the formula.
        let p = Vec3::new(120.0, -40.0, 0.0);
        let mut q = p;
        q.x += t.turb_phase + 0.01 * 3.0;
        q *= 0.005 * 2.0;
        let c = curl(q);
        let expected = c / c.length() * t.turb_speed * Vec3::new(1.0, 1.0, 0.0) * 0.1;
        assert!((Vec3::from(pool.vel[0]) - expected).length() < 1e-3);
        // V ≤ 1e-4 → no-op.
        let mut z = op(
            json!({"name": "turbulence", "speedmin": 0, "speedmax": 0}),
            &mut rng,
        );
        let before = pool.vel[0];
        z.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        assert_eq!(pool.vel[0], before);
    }

    #[test]
    fn vortex_standard_mode() {
        let mut rng = Rng::seeded(1);
        let mut pool = pool_with(3, false);
        pool.pos[0] = Vec3A::new(100.0, 0.0, 0.0); // d < inner → speedinner
        pool.pos[1] = Vec3A::new(0.0, 575.05, 0.0); // halfway in the transition
        pool.pos[2] = Vec3A::ZERO; // on the axis → ignored
        let mut v = op(
            json!({"name": "vortex", "flags": 2, "speedinner": 1000, "speedouter": 0}),
            &mut rng,
        );
        v.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        // Counter-clockwise: tangent at +X is +Y; centripetal: -X × 1 × 0.1.
        let v0 = pool.vel[0];
        assert!((v0 - Vec3A::new(-0.1, 100.0, 0.0)).length() < 1e-4, "{v0}");
        // m = 650 - 500 + 0.1 = 150.1; d = 575.05 → t = 0.5 → 500; tangent at +Y is -X.
        let v1 = pool.vel[1];
        assert!((v1 - Vec3A::new(-50.0, -0.1, 0.0)).length() < 1e-2, "{v1}");
        assert_eq!(pool.vel[2], Vec3A::ZERO);
    }

    #[test]
    fn vortex_ring_mode_and_audio() {
        let mut rng = Rng::seeded(1);
        let mut pool = pool_with(4, false);
        pool.pos[0] = Vec3A::new(200.0, 0.0, 0.0); // d < 275 → 0
        pool.pos[1] = Vec3A::new(287.5, 0.0, 0.0); // ring: lerp(1000, 200, 0.25) = 800
        pool.pos[2] = Vec3A::new(345.0, 0.0, 0.0); // pull: p = 0.4, v = 200·0.6 = 120, F = -X·10·0.4
        pool.pos[3] = Vec3A::new(400.0, 0.0, 0.0); // beyond → 0
        let mut v = op(
            json!({"name": "vortex", "flags": 4, "speedinner": 1000, "speedouter": 200}),
            &mut rng,
        );
        v.run(&mut pool, &ctx(&CPS, 1.0), &mut rng);
        assert_eq!(pool.vel[0], Vec3A::ZERO);
        assert!((pool.vel[1] - Vec3A::new(0.0, 800.0, 0.0)).length() < 1e-3);
        assert!(
            (pool.vel[2] - Vec3A::new(-4.0, 120.0, 0.0)).length() < 1e-3,
            "{}",
            pool.vel[2]
        );
        assert_eq!(pool.vel[3], Vec3A::ZERO);
        let mut dead = op(
            json!({"name": "vortex", "audioprocessingmode": 1}),
            &mut rng,
        );
        assert!(dead.inactive);
        dead.run(&mut pool, &ctx(&CPS, 1.0), &mut rng);
        assert_eq!(pool.vel[0], Vec3A::ZERO);
    }

    #[test]
    fn vortex_infinite_axis_projects() {
        let mut rng = Rng::seeded(1);
        let mut pool = pool_with(1, false);
        pool.pos[0] = Vec3A::new(100.0, 0.0, 900.0);
        let mut v = op(
            json!({"name": "vortex", "flags": 1, "speedinner": 10}),
            &mut rng,
        );
        v.run(&mut pool, &ctx(&CPS, 1.0), &mut rng);
        assert!((pool.vel[0] - Vec3A::new(0.0, 10.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn control_point_attract() {
        let mut rng = Rng::seeded(1);
        let mut cps = CPS;
        cps[1].position = Vec3::new(100.0, 0.0, 0.0);
        let mut pool = pool_with(3, false);
        pool.pos[1] = Vec3A::new(100.0, 600.0, 0.0); // d ≈ 608 > 500 → untouched
        pool.pos[2] = Vec3A::new(110.0, 0.0, 0.0); // repelled when scale < 0? (scale > 0 here: attracted to -X)
        let mut a = op(
            json!({"name": "controlpointattract", "controlpoint": 1, "origin": "0 0 0", "scale": 50}),
            &mut rng,
        );
        a.run(
            &mut pool,
            &OpCtx {
                dt: 0.1,
                time: 0.0,
                speed: 2.0,
                cps: &cps,
            },
            &mut rng,
        );
        assert!((pool.vel[0] - Vec3A::new(10.0, 0.0, 0.0)).length() < 1e-5);
        assert_eq!(pool.vel[1], Vec3A::ZERO);
        assert!((pool.vel[2] - Vec3A::new(-10.0, 0.0, 0.0)).length() < 1e-5);
        let mut off = op(
            json!({"name": "controlpointattract", "controlpoint": 8}),
            &mut rng,
        );
        off.run(&mut pool, &ctx(&cps, 0.1), &mut rng);
        assert!((pool.vel[0] - Vec3A::new(10.0, 0.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn oscillate_alpha_and_size() {
        let mut rng = Rng::seeded(2);
        let mut pool = pool_with(1, true);
        pool.alpha[0] = 0.6;
        pool.age[0] = 0.3;
        let mut expect = rng.clone();
        let mut o = op(
            json!({"name": "oscillatealpha", "frequencymin": 2, "frequencymax": 4, "scalemin": 0.5, "scalemax": 1.0}),
            &mut rng,
        );
        o.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        let f = expect.uniform(2.0, 4.0);
        let _ = expect.uniform(0.5, 1.0);
        let phase = expect.uniform(0.0, TAU + TAU);
        let st = pool.osc_alpha[0];
        assert!(st.init && st.freq == f && st.phase == phase && st.base == 0.6);
        let c = ((f * 0.3 + phase).cos() + 1.0) / 2.0;
        assert!((pool.alpha[0] - 0.6 * (0.5 + c * 0.5)).abs() < 1e-6);
        // Second frame: no new draws, base kept, current scale bounds.
        pool.age[0] = 0.4;
        let mut o2 = op(
            json!({"name": "oscillatealpha", "frequencymin": 2, "frequencymax": 4, "scalemin": 0.0, "scalemax": 2.0}),
            &mut rng,
        );
        o2.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        let c = ((f * 0.4 + phase).cos() + 1.0) / 2.0;
        assert!((pool.alpha[0] - 0.6 * (c * 2.0)).abs() < 1e-6);
        // Size uses the size oscillator.
        let mut s = op(
            json!({"name": "oscillatesize", "frequencymin": 0, "frequencymax": 0, "phasemin": 0, "phasemax": -TAU}),
            &mut rng,
        );
        s.run(&mut pool, &ctx(&CPS, 0.1), &mut rng);
        // f = 0, phase ∈ [0, 0) → 0: c = 1, m = scalemax 1.2.
        assert!((pool.size[0] - 20.0 * 1.2).abs() < 1e-5);
    }

    #[test]
    fn oscillate_position() {
        let mut rng = Rng::seeded(2);
        let mut pool = pool_with(1, true);
        pool.age[0] = 0.5;
        let mut expect = rng.clone();
        let mut o = op(json!({"name": "oscillateposition"}), &mut rng);
        o.run(
            &mut pool,
            &OpCtx {
                dt: 0.1,
                time: 0.0,
                speed: 2.0,
                cps: &CPS,
            },
            &mut rng,
        );
        let mut d = [0.0f32; 3];
        for (a, slot) in d.iter_mut().enumerate() {
            let f = expect.uniform(0.0, 5.0);
            let amp = expect.uniform(0.0, 10.0);
            let ph = expect.uniform(0.0, TAU + TAU);
            let mask = if a == 2 { 0.0 } else { 1.0 };
            *slot = -amp * f * (f * 0.5 + ph).sin() * 0.1 * mask * 2.0;
        }
        let p = pool.pos[0];
        assert!((p - Vec3A::from_array(d)).length() < 1e-4, "{p} vs {d:?}");
    }
}

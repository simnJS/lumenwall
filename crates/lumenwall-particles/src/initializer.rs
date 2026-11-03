//! Initializers (spec 05 §7 and amendments A5, A6.1-b, A6.2), applied to each new particle in
//! file order.

use crate::defaults as d;
use crate::emitter::apply_sign;
use crate::event::{ParentParticle, apply_inherit};
use crate::model::{
    HsvColorDef, InitializerDef, InstanceOverride, LimitBehavior, PositionOffsetDef,
    SequenceBetweenDef, TurbulentVelocityDef,
};
use crate::noise::{curl, fbm3};
use crate::pool::Pool;
use crate::remap::{RemapEnv, RemapParams, remap_particle};
use crate::rng::Rng;
use crate::system::ControlPoint;
use glam::{Vec3, Vec3A};
use std::f32::consts::{PI, TAU};

/// Spawn-time context shared by emitters and initializers.
pub(crate) struct SpawnCtx<'a> {
    pub cps: &'a [ControlPoint; 8],
    /// A6.1-e: control point velocities.
    pub cp_velocity: &'a [Vec3; 8],
    pub over: &'a InstanceOverride,
    /// `t_g`, seconds.
    pub time: f32,
    /// Σ of the simulated `dt` (A6.3.1 `particlesystemtime`).
    pub system_time: f32,
    pub time_of_day: f32,
    pub perspective: bool,
    /// `O`.
    pub origin: Vec3,
    /// Parent particle of a child instance (A6.5).
    pub parent: Option<&'a ParentParticle>,
}

#[cfg(test)]
static ZERO_VELOCITIES: [Vec3; 8] = [Vec3::ZERO; 8];

impl<'a> SpawnCtx<'a> {
    #[cfg(test)]
    pub fn basic(
        cps: &'a [ControlPoint; 8],
        over: &'a InstanceOverride,
        time: f32,
        perspective: bool,
    ) -> Self {
        Self {
            cps,
            cp_velocity: &ZERO_VELOCITIES,
            over,
            time,
            system_time: 0.0,
            time_of_day: 0.0,
            perspective,
            origin: Vec3::ZERO,
            parent: None,
        }
    }
}

/// Per-emission data: the spawn point `E` of §6.1 and the periodic window of the emitter.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SpawnInfo {
    pub spawn_point: Vec3,
    pub emitter: u32,
    pub epoch: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct InitializerRt {
    pub def: InitializerDef,
    /// Sequence counter of the `mapsequence*` initializers, persistent for the system's life.
    pub counter: i64,
    /// Last periodic window seen (`mapsequencebetweencontrolpoints` bit 16).
    pub last_window: Option<(u32, u32)>,
}

impl InitializerRt {
    pub fn new(def: InitializerDef) -> Self {
        Self {
            def,
            counter: 0,
            last_window: None,
        }
    }

    pub fn reset(&mut self) {
        self.counter = 0;
        self.last_window = None;
    }
}

pub(crate) fn apply_all(
    inits: &mut [InitializerRt],
    pool: &mut Pool,
    i: usize,
    ctx: &SpawnCtx<'_>,
    info: &SpawnInfo,
    rng: &mut Rng,
) {
    // Weight of the sequence position (A6.2.1), used by later initializers of the chain.
    let mut w_seq = 1.0f32;
    for init in inits {
        apply(init, pool, i, ctx, info, &mut w_seq, rng);
    }
}

/// `U(min, max)`, or with A6.1-b's bias `min + U(0,1)^e × (max − min)` when `e != 1`.
#[inline]
fn draw(rng: &mut Rng, min: f32, max: f32, e: f32) -> f32 {
    if e == 1.0 {
        rng.uniform(min, max)
    } else {
        min + rng.uniform(0.0, 1.0).powf(e) * (max - min)
    }
}

#[inline]
fn draw3(rng: &mut Rng, min: Vec3, max: Vec3, e: f32) -> Vec3 {
    let x = draw(rng, min.x, max.x, e);
    let y = draw(rng, min.y, max.y, e);
    let z = draw(rng, min.z, max.z, e);
    Vec3::new(x, y, z)
}

/// Sequence index `i` of counter `k` over `n` points (A5 step 2) and the next counter value.
pub(crate) fn sequence_index(k: i64, n: i64, limit: LimitBehavior) -> (i64, i64) {
    match limit {
        LimitBehavior::Repeat => (k.rem_euclid(n), (k + 1).rem_euclid(n)),
        LimitBehavior::Mirror if n >= 2 => {
            let p = 2 * (n - 1);
            let m = k.rem_euclid(p);
            (if m < n { m } else { p - m }, (k + 1).rem_euclid(p))
        }
        LimitBehavior::Mirror => (0, 0),
    }
}

fn cp_pos(cps: &[ControlPoint; 8], i: i32) -> Vec3 {
    usize::try_from(i)
        .ok()
        .and_then(|i| cps.get(i))
        .map_or(Vec3::ZERO, |c| c.position)
}

fn apply(
    init: &mut InitializerRt,
    pool: &mut Pool,
    i: usize,
    ctx: &SpawnCtx<'_>,
    info: &SpawnInfo,
    w_seq: &mut f32,
    rng: &mut Rng,
) {
    let over = ctx.over;
    match &init.def {
        InitializerDef::ColorRandom { min, max, exponent } => {
            // Three independent draws, not a lerp on a shared t.
            let c = draw3(rng, min.vec3(), max.vec3(), exponent.f32()) * over.colorn.vec3();
            pool.color[i] = c.into();
            pool.init_color[i] = c.into();
        }
        InitializerDef::SizeRandom { min, max, exponent } => {
            let (lo, hi) = (min.f32(), max.f32());
            let t = rng.uniform(0.0, 1.0);
            // parity: halved here, while the base size (20 × override.size) is not.
            let s = (lo + t.powf(exponent.f32()) * (hi - lo)) * over.size.f32() / 2.0;
            pool.size[i] = s;
            pool.init_size[i] = s;
        }
        InitializerDef::AlphaRandom { min, max, exponent } => {
            let a = draw(rng, min.f32(), max.f32(), exponent.f32()) * over.alpha.f32();
            pool.alpha[i] = a;
            pool.init_alpha[i] = a;
        }
        InitializerDef::LifetimeRandom { min, max, exponent } => {
            pool.lifetime[i] =
                draw(rng, min.f32(), max.f32(), exponent.f32()) * over.lifetime.f32();
        }
        InitializerDef::VelocityRandom { min, max, exponent } => {
            let mut v = draw3(rng, min.vec3(), max.vec3(), exponent.f32()) * over.speed.f32();
            v.y = -v.y;
            pool.vel[i] += Vec3A::from(v);
        }
        InitializerDef::RotationRandom { min, max, exponent } => {
            // parity: the initial rotation is multiplied by override.speed.
            pool.rot[i] =
                Vec3A::from(draw3(rng, min.vec3(), max.vec3(), exponent.f32()) * over.speed.f32());
        }
        InitializerDef::AngularVelocityRandom { min, max, exponent } => {
            let (lo, hi, e) = (min.vec3(), max.vec3(), exponent.f32());
            let mut w = Vec3::ZERO;
            for a in 0..3 {
                let t = rng.uniform(0.0, 1.0).powf(e);
                w[a] = lo[a] + t * (hi[a] - lo[a]);
            }
            pool.angvel[i] = Vec3A::from(w * over.speed.f32());
        }
        InitializerDef::TurbulentVelocityRandom(t) => {
            let p = Vec3::from(pool.pos[i]);
            let v = turbulent_velocity(t, p, ctx.time, ctx.perspective, rng) * over.speed.f32();
            pool.vel[i] += Vec3A::from(v);
        }
        InitializerDef::MapSequenceAroundControlPoint {
            control_point,
            count,
            speed_min,
            speed_max,
            bounds,
            limit_behavior,
            axis,
        } => {
            // parity: count truncated toward zero; <= 0 treated as 1 (A5).
            let n = (count.f32() as i64).max(1);
            let (idx, next) = sequence_index(init.counter, n, *limit_behavior);
            init.counter = next;
            let b = bounds.vec2();
            // parity: f32 division, then × 2π in f32 (bit-identical to the reference with default bounds).
            let f = idx as f32 / n as f32;
            let theta = (b.x + f * (b.y - b.x)) * TAU;
            let cp = control_point.f32() as i64;
            let centre = usize::try_from(cp)
                .ok()
                .and_then(|c| ctx.cps.get(c))
                .map_or(Vec3::ZERO, |c| c.position);
            // Overwrites the emitter position, emitter origin included.
            pool.pos[i] = centre.into();
            let mut s = rng.uniform_vec3(speed_min.vec3(), speed_max.vec3());
            s.y = -s.y;
            let ax = axis.vec3();
            let r = if ax == Vec3::Z || ax == Vec3::ZERO {
                // Clockwise rotation by θ in the XY plane (§7.9 formula, exact for the default axis).
                let (sin, cos) = theta.sin_cos();
                Vec3::new(s.x * cos + s.y * sin, -s.x * sin + s.y * cos, s.z)
            } else {
                rotate(s, ax.normalize(), -theta)
            };
            pool.vel[i] = Vec3A::from(r * over.speed.f32());
        }
        InitializerDef::MapSequenceBetweenControlPoints(sd) => {
            sequence_between(
                sd,
                &mut init.counter,
                &mut init.last_window,
                pool,
                i,
                ctx,
                info,
                w_seq,
            );
        }
        InitializerDef::PositionOffsetRandom(po) => position_offset(po, pool, i, ctx.time, *w_seq),
        InitializerDef::HsvColorRandom(h) => {
            let c = hsv_random(h, rng) * over.colorn.vec3();
            pool.color[i] = c.into();
            pool.init_color[i] = c.into();
        }
        InitializerDef::InheritControlPointVelocity {
            control_point,
            min,
            max,
        } => {
            if let Some(v) = usize::try_from(*control_point)
                .ok()
                .and_then(|c| ctx.cp_velocity.get(c))
            {
                let k = rng.uniform(min.f32(), max.f32()) * over.speed.f32();
                pool.vel[i] += Vec3A::from(*v * k);
            }
        }
        InitializerDef::ColorList { colors } => {
            if !colors.is_empty() {
                let m = colors.len();
                let j = ((rng.uniform(0.0, 1.0) * m as f32) as usize).min(m - 1);
                let c = colors[j].vec3() * over.colorn.vec3();
                pool.color[i] = c.into();
                pool.init_color[i] = c.into();
            }
        }
        InitializerDef::RemapInitialValue(r) => {
            if r.active() {
                let env = RemapEnv {
                    cps: ctx.cps,
                    time: ctx.time,
                    system_time: ctx.system_time,
                    time_of_day: ctx.time_of_day,
                };
                remap_particle(r, &RemapParams::read(r), &env, pool, i, true, rng);
            }
        }
        InitializerDef::InheritInitialValueFromEvent { mode } => {
            if let Some(parent) = ctx.parent {
                apply_inherit(*mode, parent, pool, i, true);
            }
        }
    }
}

/// `mapsequencebetweencontrolpoints` (A6.2.1).
#[allow(clippy::too_many_arguments)]
fn sequence_between(
    sd: &SequenceBetweenDef,
    counter: &mut i64,
    last_window: &mut Option<(u32, u32)>,
    pool: &mut Pool,
    i: usize,
    ctx: &SpawnCtx<'_>,
    info: &SpawnInfo,
    w_seq: &mut f32,
) {
    if sd.flags & d::SEQ_FLAG_PERIODIC_RESTART != 0 {
        let window = Some((info.emitter, info.epoch));
        if *last_window != window {
            *counter = 0;
            *last_window = window;
        }
    }
    let n = (sd.count.f32() as i64).max(1);
    let (idx, next) = sequence_index(*counter, n, sd.limit_behavior);
    *counter = next;
    let u = if n >= 2 {
        idx as f32 / (n - 1) as f32
    } else {
        0.0
    };
    let b = sd.bounds.vec2();
    let s = b.x + u * (b.y - b.x);
    let a = cp_pos(ctx.cps, sd.control_point_start);
    let bp = cp_pos(ctx.cps, sd.control_point_end);
    let mut p = a.lerp(bp, s);
    if sd.flags & d::SEQ_FLAG_ARC != 0 {
        let dv = bp - a;
        if dv.truncate().length() > 1e-4 {
            let perp = Vec3::new(-dv.y, dv.x, 0.0).normalize();
            p += perp * (2.0 * sd.arc_amount.f32() * dv.length() * s * (1.0 - s));
        }
    }
    let w = (PI * s).sin();
    let reduce_pos = sd.flags & d::SEQ_FLAG_REDUCE_POSITION != 0;
    let offset = Vec3::from(pool.pos[i]) - info.spawn_point;
    pool.pos[i] = Vec3A::from(p + offset * if reduce_pos { w } else { 1.0 });
    *w_seq = if reduce_pos { w } else { 1.0 };
    if let Some(slot) = pool.seq_s.get_mut(i) {
        *slot = s;
    }
    if sd.flags & d::SEQ_FLAG_REDUCE_VELOCITY != 0 {
        pool.vel[i] *= w;
    }
    if sd.flags & d::SEQ_FLAG_REDUCE_SIZE != 0 {
        pool.size[i] *= w;
        pool.init_size[i] *= w;
    }
}

/// `positionoffsetrandom` (A6.2.2).
fn position_offset(po: &PositionOffsetDef, pool: &mut Pool, i: usize, time: f32, w_seq: f32) {
    let octaves = (po.octaves.f32() as i64).clamp(1, 16) as u32;
    let q = Vec3::from(pool.pos[i]) * po.scale.f32() + Vec3::splat(time * po.time_scale.f32());
    let delta = apply_sign(
        fbm3(q, octaves) * po.distance.f32() * po.directions,
        po.sign,
    );
    pool.pos[i] += Vec3A::from(delta * w_seq);
}

/// `hsvcolorrandom` (A6.2.3): draws h, s, v in that order, then converts to RGB.
fn hsv_random(h: &HsvColorDef, rng: &mut Rng) -> Vec3 {
    let (hmin, hmax) = (h.hue_min.f32(), h.hue_max.f32());
    let steps = h.hue_steps.f32().trunc();
    let hue = if steps >= 1.0 {
        let j = (rng.uniform(0.0, 1.0) * steps).trunc().min(steps - 1.0);
        hmin + (j / steps) * (hmax - hmin)
    } else {
        rng.uniform(hmin, hmax)
    };
    let s = rng.uniform(h.saturation_min.f32(), h.saturation_max.f32());
    let v = rng.uniform(h.value_min.f32(), h.value_max.f32());
    hsv_to_rgb(hue, s, v)
}

pub(crate) fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Vec3 {
    let hp = (h - h.floor()) * 6.0;
    let sector = hp.floor();
    let f = hp - sector;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match sector as i32 {
        0 => Vec3::new(v, t, p),
        1 => Vec3::new(q, v, p),
        2 => Vec3::new(p, v, t),
        3 => Vec3::new(p, q, v),
        4 => Vec3::new(t, p, v),
        _ => Vec3::new(v, p, q),
    }
}

/// Right-handed rotation of `v` around the unit axis `k` by `angle` (Rodrigues).
#[inline]
pub(crate) fn rotate(v: Vec3, k: Vec3, angle: f32) -> Vec3 {
    let (s, c) = angle.sin_cos();
    v * c + k.cross(v) * s + k * (k.dot(v) * (1.0 - c))
}

fn unit_or(v: Vec3, fallback: Vec3) -> Vec3 {
    let l = v.length();
    if l > 1e-4 { v / l } else { fallback }
}

/// Direction × speed of `turbulentvelocityrandom` (§7.8 steps 1–8), before `× override.speed`.
fn turbulent_velocity(
    t: &TurbulentVelocityDef,
    x: Vec3,
    time: f32,
    perspective: bool,
    rng: &mut Rng,
) -> Vec3 {
    let mut f = t.forward.vec3();
    f.y = -f.y;
    let mut r = t.right.vec3();
    r.y = -r.y;
    let f = unit_or(f, Vec3::Y);
    let r = unit_or(r, Vec3::X);
    let v = rng.uniform(t.speed_min.f32(), t.speed_max.f32());
    let mut q = 0.1 * x + Vec3::splat(time * t.time_scale.f32());
    let phi = rng.uniform(t.phase_min.f32(), t.phase_max.f32());
    q += Vec3::new(phi, 0.7 * phi, 1.3 * phi);
    let c = curl(q);
    let cl = c.length();
    let mut d = if cl < 1e-4 { f } else { c / cl };
    let scale = t.scale.f32();
    if scale < 2.0 {
        let alpha = d.dot(f).clamp(-1.0, 1.0).acos() / PI;
        let alpha_max = scale / 2.0;
        // parity: scale = 0 gives alpha_max = 0, which disables the cone constraint entirely.
        if alpha > alpha_max && alpha_max > 1e-4 {
            let axis = d.cross(f);
            let al = axis.length();
            if al > 1e-4 {
                d = rotate(d, axis / al, (alpha - alpha_max) * PI);
            }
        }
    }
    let offset = t.offset.f32();
    if offset.abs() > 1e-4 {
        d = rotate(d, r, -offset);
    }
    if !perspective {
        d.z = 0.0;
        let l = d.length();
        if l > 1e-4 {
            d /= l;
        }
    }
    d * v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::BaseValues;
    use crate::value::{UserValue, Value};
    use serde_json::json;

    struct Fx {
        pool: Pool,
        cps: [ControlPoint; 8],
        over: InstanceOverride,
    }

    impl Fx {
        fn new() -> Self {
            let mut pool = Pool::new(4, false, false, false);
            let base = BaseValues {
                color: Vec3A::ONE,
                alpha: 1.0,
                size: 20.0,
                lifetime: 1.0,
            };
            pool.spawn(
                0,
                Vec3A::new(3.0, 4.0, 0.0),
                Vec3A::new(1.0, 1.0, 1.0),
                &base,
            );
            pool.len = 1;
            Self {
                pool,
                cps: [ControlPoint::default(); 8],
                over: InstanceOverride::default(),
            }
        }

        fn run(&mut self, j: serde_json::Value, rng: &mut Rng) {
            let def = InitializerDef::from_obj(j.as_object().unwrap()).unwrap();
            let mut inits = [InitializerRt::new(def)];
            self.run_rt(&mut inits, rng);
        }

        fn run_rt(&mut self, inits: &mut [InitializerRt], rng: &mut Rng) {
            let ctx = SpawnCtx::basic(&self.cps, &self.over, 2.0, false);
            apply_all(inits, &mut self.pool, 0, &ctx, &SpawnInfo::default(), rng);
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-5 * (1.0 + b.abs())
    }

    #[test]
    fn color_random_per_channel_times_colorn() {
        let mut fx = Fx::new();
        fx.over.colorn = UserValue::new(Value::Vec3(Vec3::new(0.5, 1.0, 2.0)));
        let mut rng = Rng::seeded(9);
        let mut expect = rng.clone();
        fx.run(
            json!({"name": "colorrandom", "min": "0 51 102", "max": "255 51 204"}),
            &mut rng,
        );
        // min = (0, 0.2, 0.4), max = (1, 0.2, 0.8): three independent draws.
        let r = expect.uniform(0.0, 1.0);
        let b = expect.uniform(0.4, 0.8);
        let c = Vec3::from(fx.pool.color[0]);
        assert!(
            close(c.x, r * 0.5) && close(c.y, 0.2) && close(c.z, b * 2.0),
            "{c}"
        );
        assert_eq!(fx.pool.init_color[0], fx.pool.color[0]);
    }

    #[test]
    fn size_random_is_halved() {
        let mut fx = Fx::new();
        fx.over.size = UserValue::number(3.0);
        let mut rng = Rng::seeded(1);
        // min = max = 10 → (10 + t^e × 0) × 3 / 2 = 15.
        fx.run(
            json!({"name": "sizerandom", "min": 10, "max": 10}),
            &mut rng,
        );
        assert_eq!(fx.pool.size[0], 15.0);
        assert_eq!(fx.pool.init_size[0], 15.0);
        // Exponent: (0 + t² × 20) × 3 / 2.
        let mut expect = rng.clone();
        fx.run(
            json!({"name": "sizerandom", "min": 0, "max": 20, "exponent": 2}),
            &mut rng,
        );
        let t = expect.uniform(0.0, 1.0);
        assert!(close(fx.pool.size[0], t * t * 20.0 * 3.0 / 2.0));
    }

    #[test]
    fn alpha_and_lifetime_random() {
        let mut fx = Fx::new();
        fx.over.alpha = UserValue::number(0.5);
        fx.over.lifetime = UserValue::number(4.0);
        let mut rng = Rng::seeded(1);
        fx.run(
            json!({"name": "alpharandom", "min": 0.6, "max": 0.6}),
            &mut rng,
        );
        assert_eq!(fx.pool.alpha[0], 0.3);
        assert_eq!(fx.pool.init_alpha[0], 0.3);
        fx.run(
            json!({"name": "lifetimerandom", "min": 2.5, "max": 2.5}),
            &mut rng,
        );
        assert_eq!(fx.pool.lifetime[0], 10.0);
        // Defaults: alpha in [0.05, 1) × 0.5.
        fx.run(json!({"name": "alpharandom"}), &mut rng);
        assert!((0.025..0.5).contains(&fx.pool.alpha[0]));
    }

    #[test]
    fn velocity_random_adds_with_y_flip() {
        let mut fx = Fx::new();
        fx.over.speed = UserValue::number(2.0);
        let mut rng = Rng::seeded(1);
        fx.run(
            json!({"name": "velocityrandom", "min": "1 2 3", "max": "1 2 3"}),
            &mut rng,
        );
        // (1,1,1) + (2, -4, 6)
        assert_eq!(fx.pool.vel[0], Vec3A::new(3.0, -3.0, 7.0));
    }

    #[test]
    fn rotation_random_scaled_by_speed() {
        let mut fx = Fx::new();
        fx.over.speed = UserValue::number(0.5);
        let mut rng = Rng::seeded(1);
        fx.run(
            json!({"name": "rotationrandom", "min": "0 0 2", "max": "0 0 2"}),
            &mut rng,
        );
        assert_eq!(fx.pool.rot[0], Vec3A::new(0.0, 0.0, 1.0));
        fx.run(json!({"name": "rotationrandom"}), &mut rng);
        assert!(fx.pool.rot[0].z >= 0.0 && fx.pool.rot[0].z < PI);
    }

    #[test]
    fn angular_velocity_random() {
        let mut fx = Fx::new();
        fx.over.speed = UserValue::number(2.0);
        let mut rng = Rng::seeded(4);
        let mut expect = rng.clone();
        fx.run(
            json!({"name": "angularvelocityrandom", "exponent": 3}),
            &mut rng,
        );
        // Default min (0,0,-5), max (0,0,5); one draw per axis.
        let _tx = expect.uniform(0.0, 1.0);
        let _ty = expect.uniform(0.0, 1.0);
        let tz = expect.uniform(0.0, 1.0).powf(3.0);
        let w = fx.pool.angvel[0];
        assert_eq!((w.x, w.y), (0.0, 0.0));
        assert!(close(w.z, (-5.0 + tz * 10.0) * 2.0));
    }

    #[test]
    fn map_sequence_around_control_point() {
        let mut fx = Fx::new();
        fx.cps[2].position = Vec3::new(50.0, 60.0, 0.0);
        let def = InitializerDef::from_obj(
            json!({"name": "mapsequencearoundcontrolpoint", "controlpoint": 2, "count": 4,
                   "speedmin": "10 0 0", "speedmax": "10 0 0"})
            .as_object()
            .unwrap(),
        )
        .unwrap();
        let mut inits = [InitializerRt::new(def)];
        let mut rng = Rng::seeded(1);
        let expected = [
            (10.0, 0.0),
            (0.0, -10.0),
            (-10.0, 0.0),
            (0.0, 10.0),
            (10.0, 0.0),
        ];
        for (k, (ex, ey)) in expected.into_iter().enumerate() {
            fx.run_rt(&mut inits, &mut rng);
            assert_eq!(fx.pool.pos[0], Vec3A::new(50.0, 60.0, 0.0));
            let v = fx.pool.vel[0];
            assert!(
                (v.x - ex).abs() < 1e-4 && (v.y - ey).abs() < 1e-4,
                "k = {k}: {v}"
            );
        }
        // Y of the drawn speed is flipped before the rotation.
        let def = InitializerDef::from_obj(
            json!({"name": "mapsequencearoundcontrolpoint", "controlpoint": 9, "speedmin": "0 5 1", "speedmax": "0 5 1"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let mut inits = [InitializerRt::new(def)];
        fx.run_rt(&mut inits, &mut rng);
        assert_eq!(
            fx.pool.pos[0],
            Vec3A::ZERO,
            "out-of-range control point → origin"
        );
        assert_eq!(fx.pool.vel[0], Vec3A::new(0.0, -5.0, 1.0));
    }

    #[test]
    fn turbulent_velocity_2d_unit_direction() {
        let mut fx = Fx::new();
        let mut rng = Rng::seeded(5);
        fx.pool.vel[0] = Vec3A::ZERO;
        fx.run(
            json!({"name": "turbulentvelocityrandom", "speedmin": 100, "speedmax": 100}),
            &mut rng,
        );
        let v = fx.pool.vel[0];
        assert!((v.length() - 100.0).abs() < 1e-2, "{v}");
        assert_eq!(v.z, 0.0);
        // Cone of half-angle scale/2 × π around forward (0,-1,0) after the Y flip, scale 0.5 → 45°.
        for seed in 0..50 {
            let mut rng = Rng::seeded(seed);
            fx.pool.vel[0] = Vec3A::ZERO;
            fx.pool.pos[0] = Vec3A::new(seed as f32 * 13.0, 7.0, 0.0);
            fx.run(json!({"name": "turbulentvelocityrandom", "speedmin": 1, "speedmax": 1, "scale": 0.5}), &mut rng);
            let d = Vec3::from(fx.pool.vel[0]);
            let angle = d.dot(Vec3::NEG_Y).clamp(-1.0, 1.0).acos();
            assert!(angle <= PI / 4.0 + 1e-3, "seed {seed}: {d} angle {angle}");
        }
    }

    #[test]
    fn rodrigues() {
        let v = rotate(Vec3::X, Vec3::Z, PI / 2.0);
        assert!((v - Vec3::Y).length() < 1e-6);
    }

    fn init(j: serde_json::Value) -> InitializerRt {
        InitializerRt::new(InitializerDef::from_obj(j.as_object().unwrap()).unwrap())
    }

    #[test]
    fn exponent_biases_draws() {
        let mut fx = Fx::new();
        let mut rng = Rng::seeded(3);
        let mut expect = rng.clone();
        // exponent 1: plain U(min, max).
        fx.run(
            json!({"name": "lifetimerandom", "min": 2, "max": 4}),
            &mut rng,
        );
        assert_eq!(fx.pool.lifetime[0], expect.uniform(2.0, 4.0));
        // exponent 2: min + U(0,1)^2 × (max − min), min/max not swapped.
        fx.run(
            json!({"name": "lifetimerandom", "min": 4, "max": 2, "exponent": 2}),
            &mut rng,
        );
        let t = expect.uniform(0.0, 1.0);
        assert!(close(fx.pool.lifetime[0], 4.0 + t * t * -2.0));
        fx.run(
            json!({"name": "alpharandom", "min": 0, "max": 1, "exponent": 3}),
            &mut rng,
        );
        let t = expect.uniform(0.0, 1.0);
        assert!(close(fx.pool.alpha[0], t * t * t));
    }

    #[test]
    fn sequence_index_repeat_and_mirror() {
        let seq = |n: i64, limit| {
            let mut k = 0;
            (0..8)
                .map(|_| {
                    let (i, next) = sequence_index(k, n, limit);
                    k = next;
                    i
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(seq(3, LimitBehavior::Repeat), [0, 1, 2, 0, 1, 2, 0, 1]);
        assert_eq!(seq(3, LimitBehavior::Mirror), [0, 1, 2, 1, 0, 1, 2, 1]);
        assert_eq!(seq(1, LimitBehavior::Mirror), [0; 8]);
    }

    #[test]
    fn map_sequence_around_bounds_and_axis() {
        let mut fx = Fx::new();
        let mut rng = Rng::seeded(1);
        // count 3.02 → 3 points; bounds (0, 0.5): angles 0, π/3, 2π/3.
        let mut rt = [init(
            json!({"name": "mapsequencearoundcontrolpoint", "count": 3.02, "bounds": "0 0.5",
            "speedmin": "10 0 0", "speedmax": "10 0 0"}),
        )];
        for k in 0..3 {
            fx.run_rt(&mut rt, &mut rng);
            let theta = k as f32 / 3.0 * 0.5 * TAU;
            let v = fx.pool.vel[0];
            assert!(
                (v.x - 10.0 * theta.cos()).abs() < 1e-4 && (v.y + 10.0 * theta.sin()).abs() < 1e-4,
                "{v}"
            );
        }
        // Axis (0, 1, 0): rotation of −θ around Y; s = (10, 0, 0), count 4 → θ = π/2 at k = 1.
        let mut rt = [init(
            json!({"name": "mapsequencearoundcontrolpoint", "count": 4, "axis": "0 1 0",
            "speedmin": "10 0 0", "speedmax": "10 0 0"}),
        )];
        fx.run_rt(&mut rt, &mut rng);
        fx.run_rt(&mut rt, &mut rng);
        let v = fx.pool.vel[0];
        assert!((v - Vec3A::new(0.0, 0.0, 10.0)).length() < 1e-4, "{v}");
    }

    #[test]
    fn map_sequence_between_control_points() {
        let mut fx = Fx::new();
        fx.pool = Pool::new(4, false, false, false);
        fx.pool.seq_s = vec![f32::NAN; 4];
        fx.cps[0].position = Vec3::new(0.0, 0.0, 0.0);
        fx.cps[1].position = Vec3::new(100.0, 0.0, 0.0);
        let base = crate::pool::BaseValues {
            color: Vec3A::ONE,
            alpha: 1.0,
            size: 20.0,
            lifetime: 1.0,
        };
        let mut rng = Rng::seeded(1);
        // count 3: s = 0, 0.5, 1; reduce outer positions (4) + velocities (1) + sizes (2); arc (8).
        let mut rt = [init(
            json!({"name": "mapsequencebetweencontrolpoints", "count": 3, "flags": 15, "arcamount": 0.1}),
        )];
        let ctx = SpawnCtx::basic(&fx.cps, &fx.over, 0.0, false);
        let info = SpawnInfo {
            spawn_point: Vec3::new(5.0, 5.0, 0.0),
            emitter: 0,
            epoch: 0,
        };
        let mut out = vec![];
        for _ in 0..3 {
            // Emitter offset (2, 0, 0) from E, velocity (10, 0, 0).
            fx.pool.spawn(
                0,
                Vec3A::new(7.0, 5.0, 0.0),
                Vec3A::new(10.0, 0.0, 0.0),
                &base,
            );
            apply_all(&mut rt, &mut fx.pool, 0, &ctx, &info, &mut rng);
            out.push((
                fx.pool.pos[0],
                fx.pool.vel[0].x,
                fx.pool.size[0],
                fx.pool.seq_s[0],
            ));
        }
        // Ends: w = sin(0) = 0 → no offset, no velocity, no size.
        assert!(
            (out[0].0 - Vec3A::ZERO).length() < 1e-4
                && out[0].1.abs() < 1e-4
                && out[0].2.abs() < 1e-4
        );
        // Middle: lerp = (50, 0), arc ⊥ = (0, 1) × 2 × 0.1 × 100 × 0.25 = 5, offset (2, 0) × 1.
        assert!(
            (out[1].0 - Vec3A::new(52.0, 5.0, 0.0)).length() < 1e-4,
            "{}",
            out[1].0
        );
        assert_eq!((out[1].1, out[1].2, out[1].3), (10.0, 20.0, 0.5));
        assert!((out[2].0 - Vec3A::new(100.0, 0.0, 0.0)).length() < 1e-3);
    }

    #[test]
    fn position_offset_hsv_colorlist_cp_velocity() {
        let mut fx = Fx::new();
        let mut rng = Rng::seeded(1);
        // scale 0: the offset only depends on time; directions/sign applied.
        fx.pool.pos[0] = Vec3A::new(3.0, 4.0, 0.0);
        fx.run(
            json!({"name": "positionoffsetrandom", "distance": 100, "scale": 0, "timescale": 5,
            "sign": [1, 1, 1], "directions": "1 1 0"}),
            &mut rng,
        );
        let expected =
            crate::noise::fbm3(Vec3::splat(2.0 * 5.0), 3).abs() * Vec3::new(100.0, 100.0, 0.0);
        assert!((Vec3::from(fx.pool.pos[0]) - Vec3::new(3.0, 4.0, 0.0) - expected).length() < 1e-3);
        // HSV: 6 hue steps, s = v = 1 → pure hues.
        for _ in 0..20 {
            fx.run(
                json!({"name": "hsvcolorrandom", "huesteps": 6, "saturationmin": 1, "valuemin": 1}),
                &mut rng,
            );
            let c = fx.pool.color[0];
            let hues = [
                Vec3A::new(1.0, 0.0, 0.0),
                Vec3A::new(1.0, 1.0, 0.0),
                Vec3A::new(0.0, 1.0, 0.0),
                Vec3A::new(0.0, 1.0, 1.0),
                Vec3A::new(0.0, 0.0, 1.0),
                Vec3A::new(1.0, 0.0, 1.0),
            ];
            assert!(hues.iter().any(|h| (*h - c).length() < 1e-5), "{c}");
        }
        assert_eq!(hsv_to_rgb(0.5, 1.0, 1.0), Vec3::new(0.0, 1.0, 1.0));
        fx.run(
            json!({"name": "colorlist", "colors": ["0 0.5 1", "0 0.5 1"]}),
            &mut rng,
        );
        assert_eq!(fx.pool.color[0], Vec3A::new(0.0, 0.5, 1.0));
        // inheritcontrolpointvelocity: vel += v_cp × U(min, max) × S.
        let vel = [
            Vec3::ZERO,
            Vec3::new(30.0, 0.0, 0.0),
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::ZERO,
        ];
        let def = init(
            json!({"name": "inheritcontrolpointvelocity", "controlpoint": 1, "min": 0.5, "max": 0.5}),
        );
        let mut rt = [def];
        let mut ctx = SpawnCtx::basic(&fx.cps, &fx.over, 0.0, false);
        ctx.cp_velocity = &vel;
        fx.pool.vel[0] = Vec3A::ZERO;
        apply_all(
            &mut rt,
            &mut fx.pool,
            0,
            &ctx,
            &SpawnInfo::default(),
            &mut rng,
        );
        assert_eq!(fx.pool.vel[0], Vec3A::new(15.0, 0.0, 0.0));
    }

    #[test]
    fn remap_initial_and_inherit_initial() {
        let mut fx = Fx::new();
        let mut rng = Rng::seeded(1);
        fx.cps[1].position = Vec3::new(3.0, 0.0, 0.0);
        // Distance to CP1 = 4 over [0, 8] → 0.5 → colour (0,0,1)→(1,0,0) remapped to (0.5, 0, 0.5).
        fx.run(json!({"name": "remapinitialvalue", "input": "distancetocontrolpoint", "inputcontrolpoint0": 1,
            "inputrangemax": 8, "output": "color", "outputrangemin": "0 0 1", "outputrangemax": "1 0 0"}), &mut rng);
        assert!((fx.pool.color[0] - Vec3A::new(0.5, 0.0, 0.5)).length() < 1e-6);
        assert_eq!(fx.pool.init_color[0], fx.pool.color[0]);
        // Inherit from the parent particle.
        let parent = ParentParticle {
            color: Vec3::new(0.2, 0.4, 0.6),
            alpha: 0.5,
            ..ParentParticle::default()
        };
        let mut rt = [init(json!({"name": "inheritinitialvaluefromevent"}))];
        let mut ctx = SpawnCtx::basic(&fx.cps, &fx.over, 0.0, false);
        ctx.parent = Some(&parent);
        apply_all(
            &mut rt,
            &mut fx.pool,
            0,
            &ctx,
            &SpawnInfo::default(),
            &mut rng,
        );
        assert_eq!(fx.pool.color[0], Vec3A::new(0.2, 0.4, 0.6));
        let mut rt = [init(
            json!({"name": "inheritinitialvaluefromevent", "input": "multiplyopacity"}),
        )];
        apply_all(
            &mut rt,
            &mut fx.pool,
            0,
            &ctx,
            &SpawnInfo::default(),
            &mut rng,
        );
        assert_eq!(fx.pool.alpha[0], 0.5);
    }
}

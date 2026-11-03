//! Initializers (spec 05 §7), applied to each new particle in file order.

use crate::model::{InitializerDef, InstanceOverride, TurbulentVelocityDef};
use crate::noise::curl;
use crate::pool::Pool;
use crate::rng::Rng;
use crate::system::ControlPoint;
use glam::{Vec3, Vec3A};
use std::f32::consts::{PI, TAU};

/// Spawn-time context shared by emitters and initializers.
pub(crate) struct SpawnCtx<'a> {
    pub cps: &'a [ControlPoint; 8],
    pub over: &'a InstanceOverride,
    /// `t_g`, seconds.
    pub time: f32,
    pub perspective: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct InitializerRt {
    pub def: InitializerDef,
    /// Sequence counter of `mapsequencearoundcontrolpoint`, persistent for the system's life.
    pub counter: i64,
}

impl InitializerRt {
    pub fn new(def: InitializerDef) -> Self {
        Self { def, counter: 0 }
    }
}

pub(crate) fn apply_all(
    inits: &mut [InitializerRt],
    pool: &mut Pool,
    i: usize,
    ctx: &SpawnCtx<'_>,
    rng: &mut Rng,
) {
    for init in inits {
        apply(init, pool, i, ctx, rng);
    }
}

fn apply(init: &mut InitializerRt, pool: &mut Pool, i: usize, ctx: &SpawnCtx<'_>, rng: &mut Rng) {
    let over = ctx.over;
    match &init.def {
        InitializerDef::ColorRandom { min, max } => {
            // Three independent draws, not a lerp on a shared t.
            let c = rng.uniform_vec3(min.vec3(), max.vec3()) * over.colorn.vec3();
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
        InitializerDef::AlphaRandom { min, max } => {
            let a = rng.uniform(min.f32(), max.f32()) * over.alpha.f32();
            pool.alpha[i] = a;
            pool.init_alpha[i] = a;
        }
        InitializerDef::LifetimeRandom { min, max } => {
            pool.lifetime[i] = rng.uniform(min.f32(), max.f32()) * over.lifetime.f32();
        }
        InitializerDef::VelocityRandom { min, max } => {
            let mut v = rng.uniform_vec3(min.vec3(), max.vec3()) * over.speed.f32();
            v.y = -v.y;
            pool.vel[i] += Vec3A::from(v);
        }
        InitializerDef::RotationRandom { min, max } => {
            // parity: the initial rotation is multiplied by override.speed.
            pool.rot[i] = Vec3A::from(rng.uniform_vec3(min.vec3(), max.vec3()) * over.speed.f32());
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
        } => {
            let n = count.f32() as i64;
            // Spec leaves count <= 0 undefined (division/modulo by zero); treated as 1 as recommended.
            let n = if n <= 0 { 1 } else { n };
            let k = init.counter;
            let theta = (k as f32 / n as f32) * TAU;
            init.counter = (k + 1) % n;
            let cp = control_point.f32() as i64;
            let centre = usize::try_from(cp)
                .ok()
                .and_then(|c| ctx.cps.get(c))
                .map_or(Vec3::ZERO, |c| c.position);
            // Overwrites the emitter position, emitter origin included.
            pool.pos[i] = centre.into();
            let mut s = rng.uniform_vec3(speed_min.vec3(), speed_max.vec3());
            s.y = -s.y;
            let (sin, cos) = theta.sin_cos();
            // Clockwise rotation by θ in the XY plane.
            let r = Vec3::new(s.x * cos + s.y * sin, -s.x * sin + s.y * cos, s.z);
            pool.vel[i] = Vec3A::from(r * over.speed.f32());
        }
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
            let ctx = SpawnCtx {
                cps: &self.cps,
                over: &self.over,
                time: 2.0,
                perspective: false,
            };
            apply_all(inits, &mut self.pool, 0, &ctx, rng);
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
}

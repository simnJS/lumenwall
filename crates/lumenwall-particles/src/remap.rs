//! `remapvalue` / `remapinitialvalue` (spec 05 A6.3.1), shared by the operator and the initializer.

use crate::defaults as d;
use crate::model::{RemapDef, RemapInput, RemapOperation, RemapOutput, RemapTransform};
use crate::noise::{fbm3, noise3};
use crate::operator::life;
use crate::pool::Pool;
use crate::rng::Rng;
use crate::system::ControlPoint;
use glam::{Vec3, Vec3A};
use std::f32::consts::TAU;

/// Global inputs of a remap.
pub(crate) struct RemapEnv<'a> {
    pub cps: &'a [ControlPoint; 8],
    /// `t_g`.
    pub time: f32,
    /// Σ of the simulated `dt` since creation (pre-simulation included).
    pub system_time: f32,
    /// Local time of day / 86400.
    pub time_of_day: f32,
}

/// [U] parameters read once per frame (or per spawn).
pub(crate) struct RemapParams {
    in_min: f32,
    in_max: f32,
    out_min: Vec3,
    out_max: Vec3,
    sigma: f32,
}

impl RemapParams {
    pub fn read(r: &RemapDef) -> Self {
        Self {
            in_min: r.input_range_min.f32(),
            in_max: r.input_range_max.f32(),
            out_min: r.output_range_min.vec3(),
            out_max: r.output_range_max.vec3(),
            sigma: r.transform_input_scale.f32(),
        }
    }
}

fn cp_pos(cps: &[ControlPoint; 8], i: i32) -> Vec3 {
    usize::try_from(i)
        .ok()
        .and_then(|i| cps.get(i))
        .map_or(Vec3::ZERO, |c| c.position)
}

#[inline]
fn fract(x: f32) -> f32 {
    x - x.floor()
}

/// Remaps particle `i`. `initializer`: the value in the initializer chain is modified and the
/// initial values follow; operator: size/opacity/colour multiply/add/subtract start from the
/// initial value.
pub(crate) fn remap_particle(
    r: &RemapDef,
    p: &RemapParams,
    env: &RemapEnv<'_>,
    pool: &mut Pool,
    i: usize,
    initializer: bool,
    rng: &mut Rng,
) {
    // 1. Input.
    let pos = Vec3::from(pool.pos[i]);
    let x = match r.input {
        RemapInput::LifetimeFraction => life(pool.age[i], pool.lifetime[i]),
        RemapInput::MaxLifetime => pool.lifetime[i],
        RemapInput::Size => pool.size[i],
        RemapInput::Opacity => pool.alpha[i],
        RemapInput::Speed => pool.vel[i].length(),
        RemapInput::Rotation => pool.rot[i].z,
        RemapInput::AngularSpeed => pool.angvel[i].length(),
        RemapInput::DistanceToControlPoint => {
            (pos - cp_pos(env.cps, r.input_control_point0)).length()
        }
        RemapInput::PositionBetweenTwoControlPoints => {
            let a = cp_pos(env.cps, r.input_control_point0);
            let ab = cp_pos(env.cps, r.input_control_point1) - a;
            let l2 = ab.length_squared();
            if l2 > 0.0 {
                (pos - a).dot(ab) / l2
            } else {
                0.0
            }
        }
        RemapInput::Runtime => env.time,
        RemapInput::SystemTime => env.system_time,
        RemapInput::TimeOfDay => env.time_of_day,
        RemapInput::Unsupported => return,
    };
    let mut n = if p.in_max == p.in_min {
        if x >= p.in_max { 1.0 } else { 0.0 }
    } else {
        (x - p.in_min) / (p.in_max - p.in_min)
    };
    if r.flags & RemapDef::FLAG_CLAMP_INPUT != 0 {
        n = n.clamp(0.0, 1.0);
    }
    // 2. Transform.
    let sn = p.sigma * n;
    let t = match r.transform {
        RemapTransform::None => Vec3::splat(n),
        RemapTransform::Sine => Vec3::splat(0.5 + 0.5 * (TAU * sn).sin()),
        RemapTransform::Square => Vec3::splat(if fract(sn) < 0.5 { 1.0 } else { 0.0 }),
        RemapTransform::Saw => Vec3::splat(fract(sn)),
        RemapTransform::Triangle => Vec3::splat(1.0 - (2.0 * fract(sn) - 1.0).abs()),
        RemapTransform::SimplexNoise | RemapTransform::FbmNoise => {
            let phase = match pool.remap_phase.get_mut(i) {
                Some(ph) => {
                    if ph.is_nan() {
                        *ph = rng.uniform(0.0, d::REMAP_PHASE_RANGE);
                    }
                    *ph
                }
                None => 0.0,
            };
            let q = Vec3::new(sn, phase, 0.0);
            let nv = if r.transform == RemapTransform::FbmNoise {
                fbm3(q, d::REMAP_FBM_OCTAVES)
            } else {
                noise3(q)
            };
            Vec3::splat(0.5) + 0.5 * nv
        }
    };
    // 3. Output range.
    let mut y = p.out_min + t * (p.out_max - p.out_min);
    if r.flags & RemapDef::FLAG_CLAMP_OUTPUT != 0 {
        y = y.clamp(p.out_min.min(p.out_max), p.out_min.max(p.out_max));
    }
    // 4. Application.
    let op = |x: f32, y: f32| match r.operation {
        RemapOperation::Remap => y,
        RemapOperation::Multiply => x * y,
        RemapOperation::Add => x + y,
        RemapOperation::Subtract => x - y,
    };
    let op3 = |x: Vec3A, y: Vec3A| match r.operation {
        RemapOperation::Remap => y,
        RemapOperation::Multiply => x * y,
        RemapOperation::Add => x + y,
        RemapOperation::Subtract => x - y,
    };
    // Vector offsets follow the Y convention of `velocityrandom` / emitter origins; a multiplier
    // is a scale factor and keeps its sign.
    let flip = |y: Vec3| {
        if r.operation == RemapOperation::Multiply {
            y
        } else {
            Vec3::new(y.x, -y.y, y.z)
        }
    };
    match r.output {
        RemapOutput::Size => {
            let base = if initializer || r.operation == RemapOperation::Remap {
                pool.size[i]
            } else {
                pool.init_size[i]
            };
            pool.size[i] = op(base, y.x);
            if initializer {
                pool.init_size[i] = pool.size[i];
            } else if let Some(o) = pool.osc_size.get_mut(i) {
                o.base = pool.size[i];
            }
        }
        RemapOutput::Opacity => {
            let base = if initializer || r.operation == RemapOperation::Remap {
                pool.alpha[i]
            } else {
                pool.init_alpha[i]
            };
            pool.alpha[i] = op(base, y.x);
            if initializer {
                pool.init_alpha[i] = pool.alpha[i];
            } else if let Some(o) = pool.osc_alpha.get_mut(i) {
                o.base = pool.alpha[i];
            }
        }
        RemapOutput::Color => {
            let base = if initializer || r.operation == RemapOperation::Remap {
                pool.color[i]
            } else {
                pool.init_color[i]
            };
            pool.color[i] = op3(base, y.into());
            if initializer {
                pool.init_color[i] = pool.color[i];
            }
        }
        RemapOutput::Velocity => pool.vel[i] = op3(pool.vel[i], flip(y).into()),
        RemapOutput::Speed => {
            let v = pool.vel[i];
            let s = v.length();
            if s >= 1e-6 {
                pool.vel[i] = v / s * op(s, y.x);
            }
        }
        RemapOutput::Rotation => pool.rot[i] = op3(pool.rot[i], y.into()),
        RemapOutput::AngularSpeed => pool.angvel[i] = op3(pool.angvel[i], y.into()),
        RemapOutput::Position => pool.pos[i] = op3(pool.pos[i], flip(y).into()),
        RemapOutput::Unsupported => {}
    }
}

//! `boids` operator (spec 05 A6.3.5) on a hashed uniform grid: no O(n²) loop, and no allocation
//! once the buffers are sized at creation.

use crate::defaults as d;
use crate::model::BoidsDef;
use glam::Vec3A;

/// Scratch buffers of one `boids` operator.
#[derive(Clone, Debug, Default)]
pub(crate) struct BoidsGrid {
    /// Bucket of each live particle.
    bucket_of: Vec<u32>,
    /// Bucket `b` holds `sorted[start[b]..start[b + 1]]`.
    start: Vec<u32>,
    sorted: Vec<u32>,
    delta: Vec<Vec3A>,
}

impl BoidsGrid {
    /// Buffers for up to `slots` particles.
    pub fn with_capacity(slots: usize) -> Self {
        let mut g = Self::default();
        g.ensure(slots);
        g
    }

    fn ensure(&mut self, n: usize) {
        if self.bucket_of.len() < n {
            let buckets = (2 * n).next_power_of_two().max(64);
            self.bucket_of.resize(n, 0);
            self.sorted.resize(n, 0);
            self.delta.resize(n, Vec3A::ZERO);
            self.start.resize(buckets + 1, 0);
        }
    }
}

#[inline]
fn cell(p: Vec3A, inv: f32) -> [i32; 3] {
    [
        (p.x * inv).floor() as i32,
        (p.y * inv).floor() as i32,
        (p.z * inv).floor() as i32,
    ]
}

#[inline]
fn hash(c: [i32; 3], mask: usize) -> usize {
    let h = (c[0] as u32).wrapping_mul(73_856_093)
        ^ (c[1] as u32).wrapping_mul(19_349_663)
        ^ (c[2] as u32).wrapping_mul(83_492_791);
    h as usize & mask
}

/// One application. All variations are computed on the state at the start of the operator, then
/// applied (two passes).
pub(crate) fn run(b: &BoidsDef, grid: &mut BoidsGrid, pos: &[Vec3A], vel: &mut [Vec3A], dt: f32) {
    let n = pos.len();
    let nthr = b.neighbor_threshold.f32();
    let sthr = b.separation_threshold.f32();
    let (fs, fc, fa) = (
        b.separation_factor.f32(),
        b.cohesion_factor.f32(),
        b.alignment_factor.f32(),
    );
    let cell_size = nthr.max(sthr);
    if n == 0 || cell_size.is_nan() || cell_size <= 0.0 {
        return;
    }
    grid.ensure(n);
    let buckets = grid.start.len() - 1;
    let mask = buckets - 1;
    let inv = 1.0 / cell_size;

    // Counting sort of the particles by bucket.
    grid.start.fill(0);
    let (mut zmin, mut zmax) = (i32::MAX, i32::MIN);
    for (i, p) in pos.iter().enumerate() {
        let c = cell(*p, inv);
        zmin = zmin.min(c[2]);
        zmax = zmax.max(c[2]);
        let h = hash(c, mask);
        grid.bucket_of[i] = h as u32;
        grid.start[h] += 1;
    }
    for k in 1..buckets {
        grid.start[k] += grid.start[k - 1];
    }
    grid.start[buckets] = n as u32;
    for i in (0..n).rev() {
        let h = grid.bucket_of[i] as usize;
        grid.start[h] -= 1;
        grid.sorted[grid.start[h] as usize] = i as u32;
    }

    // Pass 1: variations from the frozen state. Particles are visited bucket by bucket so that
    // consecutive particles share their neighbour buckets (cache locality). Squared distances and
    // branch-free accumulation keep the inner loop cheap; the particle itself is counted then
    // removed (distance 0 is always below the threshold).
    let (n2, s2) = (nthr * nthr, sthr * sthr);
    let dz: &[i32] = if zmin == zmax { &[0] } else { &[-1, 0, 1] };
    for idx in 0..n {
        let i = grid.sorted[idx] as usize;
        let pi = pos[i];
        let vi = vel[i];
        let c = cell(pi, inv);
        let mut visited = [usize::MAX; 27];
        let mut nv = 0usize;
        let (mut count, mut sum_p, mut sum_v, mut sep) =
            (0u32, Vec3A::ZERO, Vec3A::ZERO, Vec3A::ZERO);
        for &oz in dz {
            for oy in -1..=1 {
                for ox in -1..=1 {
                    let h = hash([c[0] + ox, c[1] + oy, c[2] + oz], mask);
                    // Two neighbour cells can share a bucket: visit each bucket once.
                    if visited[..nv].contains(&h) {
                        continue;
                    }
                    visited[nv] = h;
                    nv += 1;
                    let range = grid.start[h] as usize..grid.start[h + 1] as usize;
                    for &j in &grid.sorted[range] {
                        let pj = pos[j as usize];
                        let dv = pi - pj;
                        let d2 = dv.length_squared();
                        let near = d2 < n2;
                        let m = f32::from(u8::from(near));
                        count += u32::from(near);
                        sum_p += pj * m;
                        sum_v += vel[j as usize] * m;
                        if d2 < s2 && d2 > 1e-8 {
                            let dist = d2.sqrt();
                            sep += dv / dist * (sthr - dist);
                        }
                    }
                }
            }
        }
        if n2 > 0.0 {
            // Remove the particle itself.
            count -= 1;
            sum_p -= pi;
            sum_v -= vi;
        }
        grid.delta[i] = if count == 0 {
            Vec3A::ZERO
        } else {
            let k = 1.0 / count as f32;
            let cohesion = sum_p * k - pi;
            let alignment = sum_v * k - vi;
            (fc * cohesion + fa * alignment + fs * sep) * dt
        };
    }

    // Pass 2: apply.
    let clamp = b.flags & d::BOIDS_FLAG_CLAMP_SPEED != 0;
    let max_speed = b.max_speed.f32();
    for (v, dv) in vel.iter_mut().zip(&grid.delta[..n]) {
        *v += *dv;
        if clamp {
            let s = v.length();
            if s > max_speed {
                *v = *v / s * max_speed;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OperatorDef;
    use serde_json::json;

    fn def(j: serde_json::Value) -> BoidsDef {
        match OperatorDef::from_obj(j.as_object().unwrap()).unwrap() {
            OperatorDef::Boids(b) => *b,
            _ => unreachable!(),
        }
    }

    /// Brute-force reference of the same rule.
    fn brute(b: &BoidsDef, pos: &[Vec3A], vel: &[Vec3A], dt: f32) -> Vec<Vec3A> {
        let (nthr, sthr) = (b.neighbor_threshold.f32(), b.separation_threshold.f32());
        (0..pos.len())
            .map(|i| {
                let (mut c, mut sp, mut sv, mut sep) = (0, Vec3A::ZERO, Vec3A::ZERO, Vec3A::ZERO);
                for j in 0..pos.len() {
                    if j == i {
                        continue;
                    }
                    let d = (pos[i] - pos[j]).length();
                    if d < nthr {
                        c += 1;
                        sp += pos[j];
                        sv += vel[j];
                    }
                    if d < sthr && d > 1e-4 {
                        sep += (pos[i] - pos[j]) / d * (sthr - d);
                    }
                }
                if c == 0 {
                    return vel[i];
                }
                let k = 1.0 / c as f32;
                vel[i]
                    + (b.cohesion_factor.f32() * (sp * k - pos[i])
                        + b.alignment_factor.f32() * (sv * k - vel[i])
                        + b.separation_factor.f32() * sep)
                        * dt
            })
            .collect()
    }

    #[test]
    fn grid_matches_brute_force() {
        let b = def(
            json!({"name": "boids", "neighborthreshold": 50, "separationthreshold": 20,
            "separationfactor": 25, "cohesionfactor": 4, "alignmentfactor": 5}),
        );
        let mut rng = crate::rng::Rng::seeded(4);
        for z_spread in [0.0, 80.0] {
            let pos: Vec<Vec3A> = (0..400)
                .map(|_| {
                    Vec3A::new(
                        rng.uniform(-300.0, 300.0),
                        rng.uniform(-200.0, 200.0),
                        rng.uniform(-z_spread, z_spread),
                    )
                })
                .collect();
            let vel: Vec<Vec3A> = (0..400)
                .map(|_| Vec3A::new(rng.uniform(-10.0, 10.0), rng.uniform(-10.0, 10.0), 0.0))
                .collect();
            let expected = brute(&b, &pos, &vel, 0.1);
            let mut v = vel.clone();
            let mut grid = BoidsGrid::with_capacity(400);
            run(&b, &mut grid, &pos, &mut v, 0.1);
            for (a, e) in v.iter().zip(&expected) {
                assert!((*a - *e).length() < 1e-3 * (1.0 + e.length()), "{a} vs {e}");
            }
        }
    }

    #[test]
    fn isolated_particle_unchanged_and_speed_clamp() {
        let b = def(json!({"name": "boids", "flags": 1, "maxspeed": 5, "cohesionfactor": 100}));
        let pos = [
            Vec3A::ZERO,
            Vec3A::new(10.0, 0.0, 0.0),
            Vec3A::new(5000.0, 0.0, 0.0),
        ];
        let mut vel = [Vec3A::ZERO, Vec3A::ZERO, Vec3A::new(1.0, 0.0, 0.0)];
        run(&b, &mut BoidsGrid::default(), &pos, &mut vel, 0.1);
        assert_eq!(vel[2], Vec3A::new(1.0, 0.0, 0.0));
        // Cohesion pulls 0 toward 1 (100 × 10 × 0.1 = 100), clamped to 5.
        assert!((vel[0] - Vec3A::new(5.0, 0.0, 0.0)).length() < 1e-4);
        assert!((vel[1] - Vec3A::new(-5.0, 0.0, 0.0)).length() < 1e-4);
    }
}

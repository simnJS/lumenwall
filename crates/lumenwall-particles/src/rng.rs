//! Per-system pseudo-random generator (spec 05 §4.5, §9.1).
//!
//! wyrand: 64-bit state, one 64×64→128 multiply per draw, passes PractRand/BigCrush. The reference
//! uses a Mersenne Twister; the spec leaves the algorithm free, only the draw order matters for
//! internal reproducibility.

use glam::Vec3;
use std::hash::{BuildHasher, Hasher};

/// Fast seedable generator.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Deterministic generator, for tests and replays.
    pub fn seeded(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Generator seeded from process entropy (std's randomly keyed hasher) mixed with the clock,
    /// like the reference's OS-entropy seed: runs are not reproducible.
    pub fn from_entropy() -> Self {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            h.write_u128(d.as_nanos());
        }
        let local = 0u8;
        h.write_usize(std::ptr::from_ref(&local) as usize);
        Self { state: h.finish() }
    }

    /// Next 64 random bits.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0xa076_1d64_78bd_642f);
        let t = u128::from(self.state) * u128::from(self.state ^ 0xe703_7ed1_a0b4_28db);
        ((t >> 64) as u64) ^ (t as u64)
    }

    /// Uniform `f32` in `[0, 1)` (24 random mantissa bits).
    #[inline]
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 * (1.0 / 16_777_216.0)
    }

    /// `U(a, b)`: uniform in `[a, b)`; bounds swapped if `b < a`; returns `a` if `a == b`.
    #[inline]
    pub fn uniform(&mut self, a: f32, b: f32) -> f32 {
        let (lo, hi) = if b < a { (b, a) } else { (a, b) };
        if lo == hi {
            return lo;
        }
        let v = lo + (hi - lo) * self.next_f32();
        // Rounding can land exactly on `hi`; keep the documented half-open interval.
        if v < hi { v } else { hi.next_down() }
    }

    /// Component-wise `U`, drawn in x, y, z order.
    #[inline]
    pub fn uniform_vec3(&mut self, a: Vec3, b: Vec3) -> Vec3 {
        let x = self.uniform(a.x, b.x);
        let y = self.uniform(a.y, b.y);
        let z = self.uniform(a.z, b.z);
        Vec3::new(x, y, z)
    }

    /// Uniform integer in `[0, n)`, `n > 0`.
    #[inline]
    pub fn below(&mut self, n: u32) -> u32 {
        (((self.next_u64() >> 32) * u64::from(n)) >> 32) as u32
    }
}

/// SplitMix64 finaliser, used for per-slot hashes.
#[inline]
pub(crate) fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_bounds_and_swap() {
        let mut r = Rng::seeded(1);
        for _ in 0..10_000 {
            let v = r.uniform(2.0, 5.0);
            assert!((2.0..5.0).contains(&v));
            let w = r.uniform(5.0, 2.0);
            assert!((2.0..5.0).contains(&w));
        }
        assert_eq!(r.uniform(3.0, 3.0), 3.0);
        let f = r.next_f32();
        assert!((0.0..1.0).contains(&f));
    }

    #[test]
    fn seeded_is_deterministic_and_mean_is_sane() {
        let mut a = Rng::seeded(42);
        let mut b = Rng::seeded(42);
        let mut sum = 0.0f64;
        for _ in 0..100_000 {
            let x = a.next_f32();
            assert_eq!(x, b.next_f32());
            sum += f64::from(x);
        }
        assert!((sum / 100_000.0 - 0.5).abs() < 0.01);
        let mut c = Rng::seeded(3);
        let mut hist = [0u32; 7];
        for _ in 0..70_000 {
            hist[c.below(7) as usize] += 1;
        }
        assert!(hist.iter().all(|&h| (9_000..11_000).contains(&h)));
    }
}

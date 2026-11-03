//! Particle pool (spec 05 §3, §4.1, §11): structure of arrays.
//!
//! Each attribute is a `Vec` allocated once; the live particles occupy the window
//! `[head, head + len)` in spawn order (first = oldest). Operators loop over one or two attribute
//! slices at a time, so each loop only touches the memory it needs; vector attributes use `Vec3A`
//! (16-byte SIMD lanes). Oscillator states are only allocated when an oscillate operator exists.
//!
//! Stable compaction packs the survivors toward the *end* of the window, so only the survivors
//! older than the last death move (deaths cluster among the oldest particles) and a dead prefix
//! costs nothing. The buffer has `N_pool / 4` spare slots after the window; when a spawn reaches
//! the end of the buffer the window is moved back to slot 0 (amortised over many frames).
//!
//! Acceleration and angular acceleration (always zero, never read) and the initial lifetime (never
//! read) are not stored.

use glam::{Vec3, Vec3A};
use std::ops::Range;

/// Scalar oscillator state (`oscillatealpha` / `oscillatesize`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Osc {
    pub freq: f32,
    pub phase: f32,
    pub base: f32,
    pub init: bool,
}

impl Osc {
    pub const RESET: Osc = Osc {
        freq: 0.0,
        phase: 0.0,
        base: 1.0,
        init: false,
    };
}

/// Position oscillator state (`oscillateposition`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OscPos {
    pub freq: Vec3,
    pub amp: Vec3,
    pub phase: Vec3,
    pub init: bool,
}

impl OscPos {
    pub const RESET: OscPos = OscPos {
        freq: Vec3::ZERO,
        amp: Vec3::ONE,
        phase: Vec3::ZERO,
        init: false,
    };
}

/// Values written at spawn before the initializers (§3).
#[derive(Clone, Copy, Debug)]
pub(crate) struct BaseValues {
    pub color: Vec3A,
    pub alpha: f32,
    pub size: f32,
    pub lifetime: f32,
}

#[derive(Debug)]
pub(crate) struct Pool {
    /// First live slot.
    pub head: usize,
    /// Number of live particles (`n`).
    pub len: usize,
    /// `N_pool`: maximum number of live particles.
    pub cap: usize,
    pub pos: Vec<Vec3A>,
    pub vel: Vec<Vec3A>,
    pub rot: Vec<Vec3A>,
    pub angvel: Vec<Vec3A>,
    pub color: Vec<Vec3A>,
    pub init_color: Vec<Vec3A>,
    pub alpha: Vec<f32>,
    pub init_alpha: Vec<f32>,
    pub size: Vec<f32>,
    pub init_size: Vec<f32>,
    pub lifetime: Vec<f32>,
    pub age: Vec<f32>,
    pub frame: Vec<f32>,
    /// Empty unless an `oscillatealpha` operator exists.
    pub osc_alpha: Vec<Osc>,
    /// Empty unless an `oscillatesize` operator exists.
    pub osc_size: Vec<Osc>,
    /// Empty unless an `oscillateposition` operator exists.
    pub osc_pos: Vec<OscPos>,
    /// Spawn-ordered particle ids; empty unless child events need to track particles (A6.5).
    pub ids: Vec<u64>,
    /// Sequence position `s` (A6.2.1 / A6.3.3), NaN when unset; empty unless needed.
    pub seq_s: Vec<f32>,
    /// Noise phase of `remapvalue` transforms (A6.3.1), NaN until drawn; empty unless needed.
    pub remap_phase: Vec<f32>,
    /// Next particle id.
    pub next_id: u64,
}

/// Mutable view of the live window.
pub(crate) struct Live<'a> {
    pub pos: &'a mut [Vec3A],
    pub vel: &'a mut [Vec3A],
    pub rot: &'a mut [Vec3A],
    pub angvel: &'a mut [Vec3A],
    pub color: &'a mut [Vec3A],
    pub init_color: &'a [Vec3A],
    pub alpha: &'a mut [f32],
    pub init_alpha: &'a [f32],
    pub size: &'a mut [f32],
    pub init_size: &'a [f32],
    pub lifetime: &'a [f32],
    pub age: &'a mut [f32],
    pub frame: &'a mut [f32],
    pub osc_alpha: &'a mut [Osc],
    pub osc_size: &'a mut [Osc],
    pub osc_pos: &'a mut [OscPos],
    pub seq_s: &'a mut [f32],
}

/// Shared view of the live window.
pub(crate) struct LiveRef<'a> {
    pub pos: &'a [Vec3A],
    pub vel: &'a [Vec3A],
    pub rot: &'a [Vec3A],
    pub angvel: &'a [Vec3A],
    pub color: &'a [Vec3A],
    pub alpha: &'a [f32],
    pub size: &'a [f32],
    pub lifetime: &'a [f32],
    pub age: &'a [f32],
    pub frame: &'a [f32],
}

impl Pool {
    pub fn new(cap: usize, osc_alpha: bool, osc_size: bool, osc_pos: bool) -> Self {
        let slots = cap + (cap / 4).max(16);
        let v3 = || vec![Vec3A::ZERO; slots];
        let f = || vec![0.0f32; slots];
        Self {
            head: 0,
            len: 0,
            cap,
            pos: v3(),
            vel: v3(),
            rot: v3(),
            angvel: v3(),
            color: v3(),
            init_color: v3(),
            alpha: f(),
            init_alpha: f(),
            size: f(),
            init_size: f(),
            lifetime: f(),
            age: f(),
            frame: f(),
            osc_alpha: if osc_alpha {
                vec![Osc::RESET; slots]
            } else {
                Vec::new()
            },
            osc_size: if osc_size {
                vec![Osc::RESET; slots]
            } else {
                Vec::new()
            },
            osc_pos: if osc_pos {
                vec![OscPos::RESET; slots]
            } else {
                Vec::new()
            },
            ids: Vec::new(),
            seq_s: Vec::new(),
            remap_phase: Vec::new(),
            next_id: 0,
        }
    }

    /// Allocates the optional per-particle attributes.
    pub fn with_extras(mut self, ids: bool, seq_s: bool, remap_phase: bool) -> Self {
        let slots = self.pos.len();
        if ids {
            self.ids = vec![0; slots];
        }
        if seq_s {
            self.seq_s = vec![f32::NAN; slots];
        }
        if remap_phase {
            self.remap_phase = vec![f32::NAN; slots];
        }
        self
    }

    /// Number of slots of the buffer (window + spare).
    pub fn slots(&self) -> usize {
        self.pos.len()
    }

    /// Empties the pool (child instances are recycled); ids keep increasing.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Live window `[head, head + len)`.
    #[inline]
    pub fn range(&self) -> Range<usize> {
        self.head..self.head + self.len
    }

    /// True when `n >= N_pool`.
    #[inline]
    pub fn full(&self) -> bool {
        self.len >= self.cap
    }

    pub fn live(&mut self) -> Live<'_> {
        let r = self.range();
        Live {
            pos: &mut self.pos[r.clone()],
            vel: &mut self.vel[r.clone()],
            rot: &mut self.rot[r.clone()],
            angvel: &mut self.angvel[r.clone()],
            color: &mut self.color[r.clone()],
            init_color: &self.init_color[r.clone()],
            alpha: &mut self.alpha[r.clone()],
            init_alpha: &self.init_alpha[r.clone()],
            size: &mut self.size[r.clone()],
            init_size: &self.init_size[r.clone()],
            lifetime: &self.lifetime[r.clone()],
            age: &mut self.age[r.clone()],
            frame: &mut self.frame[r.clone()],
            osc_alpha: self.osc_alpha.get_mut(r.clone()).unwrap_or_default(),
            osc_size: self.osc_size.get_mut(r.clone()).unwrap_or_default(),
            osc_pos: self.osc_pos.get_mut(r.clone()).unwrap_or_default(),
            seq_s: self.seq_s.get_mut(r).unwrap_or_default(),
        }
    }

    pub fn live_ref(&self) -> LiveRef<'_> {
        let r = self.range();
        LiveRef {
            pos: &self.pos[r.clone()],
            vel: &self.vel[r.clone()],
            rot: &self.rot[r.clone()],
            angvel: &self.angvel[r.clone()],
            color: &self.color[r.clone()],
            alpha: &self.alpha[r.clone()],
            size: &self.size[r.clone()],
            lifetime: &self.lifetime[r.clone()],
            age: &self.age[r.clone()],
            frame: &self.frame[r],
        }
    }

    /// Slot of the next particle (the caller checked `!full()`). Moves the window back to slot 0
    /// when it reached the end of the buffer.
    #[inline]
    pub fn next_slot(&mut self) -> usize {
        let end = self.head + self.len;
        if end == self.pos.len() {
            self.move_range(self.range(), 0);
            self.head = 0;
            return self.len;
        }
        end
    }

    /// Writes the spawn state of slot `i` (§3).
    #[inline]
    pub fn spawn(&mut self, i: usize, pos: Vec3A, vel: Vec3A, b: &BaseValues) {
        self.pos[i] = pos;
        self.vel[i] = vel;
        self.rot[i] = Vec3A::ZERO;
        self.angvel[i] = Vec3A::ZERO;
        self.color[i] = b.color;
        self.init_color[i] = b.color;
        self.alpha[i] = b.alpha;
        self.init_alpha[i] = b.alpha;
        self.size[i] = b.size;
        self.init_size[i] = b.size;
        self.lifetime[i] = b.lifetime;
        self.age[i] = 0.0;
        self.frame[i] = -1.0;
        if let Some(o) = self.osc_alpha.get_mut(i) {
            *o = Osc::RESET;
        }
        if let Some(o) = self.osc_size.get_mut(i) {
            *o = Osc::RESET;
        }
        if let Some(o) = self.osc_pos.get_mut(i) {
            *o = OscPos::RESET;
        }
        if let Some(id) = self.ids.get_mut(i) {
            *id = self.next_id;
            self.next_id += 1;
        }
        if let Some(s) = self.seq_s.get_mut(i) {
            *s = f32::NAN;
        }
        if let Some(p) = self.remap_phase.get_mut(i) {
            *p = f32::NAN;
        }
    }

    /// Live index of the particle with id `id` (ids increase in spawn order).
    pub fn find_id(&self, id: u64) -> Option<usize> {
        let ids = self.ids.get(self.range())?;
        ids.binary_search(&id).ok()
    }

    /// Moves slots `src` to `dst..` in every attribute (`memmove`).
    fn move_range(&mut self, src: Range<usize>, dst: usize) {
        fn mv<T: Copy>(v: &mut [T], src: Range<usize>, dst: usize) {
            if !v.is_empty() {
                v.copy_within(src, dst);
            }
        }
        if src.start == dst || src.is_empty() {
            return;
        }
        mv(&mut self.pos, src.clone(), dst);
        mv(&mut self.vel, src.clone(), dst);
        mv(&mut self.rot, src.clone(), dst);
        mv(&mut self.angvel, src.clone(), dst);
        mv(&mut self.color, src.clone(), dst);
        mv(&mut self.init_color, src.clone(), dst);
        mv(&mut self.alpha, src.clone(), dst);
        mv(&mut self.init_alpha, src.clone(), dst);
        mv(&mut self.size, src.clone(), dst);
        mv(&mut self.init_size, src.clone(), dst);
        mv(&mut self.lifetime, src.clone(), dst);
        mv(&mut self.age, src.clone(), dst);
        mv(&mut self.frame, src.clone(), dst);
        mv(&mut self.osc_alpha, src.clone(), dst);
        mv(&mut self.osc_size, src.clone(), dst);
        mv(&mut self.osc_pos, src.clone(), dst);
        mv(&mut self.ids, src.clone(), dst);
        mv(&mut self.seq_s, src.clone(), dst);
        mv(&mut self.remap_phase, src, dst);
    }

    /// Removes every particle with `age >= lifetime`, keeping the survivors in spawn order
    /// (stable compaction, §5 step 10). Survivors are packed toward the end of the window: the
    /// ones after the last death do not move; earlier runs move with one `memmove` per attribute.
    pub fn compact(&mut self) {
        let start = self.head;
        let end = self.head + self.len;
        let age = &self.age;
        let lifetime = &self.lifetime;
        let dead = |i: usize| age[i] >= lifetime[i];
        let mut j = end;
        while j > start && !dead(j - 1) {
            j -= 1;
        }
        if j == start {
            return;
        }
        let mut w = j;
        while j > start {
            let (age, lifetime) = (&self.age, &self.lifetime);
            let dead = |i: usize| age[i] >= lifetime[i];
            while j > start && dead(j - 1) {
                j -= 1;
            }
            let run_end = j;
            while j > start && !dead(j - 1) {
                j -= 1;
            }
            let l = run_end - j;
            if l > 0 {
                self.move_range(j..run_end, w - l);
                w -= l;
            }
        }
        self.len = end - w;
        self.head = if self.len == 0 { 0 } else { w };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(n: usize) -> Pool {
        let mut p = Pool::new(n, true, false, false);
        let base = BaseValues {
            color: Vec3A::ONE,
            alpha: 1.0,
            size: 1.0,
            lifetime: 1.0,
        };
        for i in 0..n {
            let s = p.next_slot();
            p.spawn(s, Vec3A::splat(i as f32), Vec3A::ZERO, &base);
            p.osc_alpha[s].base = i as f32;
            p.len += 1;
        }
        p
    }

    fn xs(p: &Pool) -> Vec<f32> {
        p.live_ref().pos.iter().map(|v| v.x).collect()
    }

    #[test]
    fn stable_compaction() {
        let mut p = filled(8);
        // Kill 0, 2, 3, 7.
        for i in [0, 2, 3, 7] {
            p.age[i] = 1.0;
        }
        p.compact();
        assert_eq!(p.len, 4);
        assert_eq!(xs(&p), [1.0, 4.0, 5.0, 6.0]);
        let r = p.range();
        let bases: Vec<f32> = p.osc_alpha[r].iter().map(|o| o.base).collect();
        assert_eq!(bases, [1.0, 4.0, 5.0, 6.0]);
        // Nothing dead: unchanged.
        p.compact();
        assert_eq!(xs(&p), [1.0, 4.0, 5.0, 6.0]);
        // NaN lifetime is never removed (`age >= NaN` is false).
        let h = p.head;
        p.lifetime[h] = f32::NAN;
        p.age[h + 1] = 5.0;
        p.compact();
        assert_eq!(xs(&p), [1.0, 5.0, 6.0]);
        // Everything dead.
        let r = p.range();
        p.age[r].fill(9.0);
        p.lifetime[p.head] = 1.0;
        p.compact();
        assert_eq!((p.len, p.head), (0, 0));
    }

    #[test]
    fn dead_prefix_moves_nothing_and_window_wraps() {
        let mut p = filled(8);
        for i in 0..3 {
            p.age[i] = 2.0;
        }
        p.compact();
        assert_eq!(p.head, 3);
        assert_eq!(xs(&p), [3.0, 4.0, 5.0, 6.0, 7.0]);
        // Spawn until the buffer end is reached: the window moves back to slot 0, order kept.
        let base = BaseValues {
            color: Vec3A::ONE,
            alpha: 1.0,
            size: 1.0,
            lifetime: 1.0,
        };
        let slots = p.pos.len();
        let mut next = 8.0;
        while !p.full() {
            let s = p.next_slot();
            assert!(s < slots);
            p.spawn(s, Vec3A::splat(next), Vec3A::ZERO, &base);
            p.len += 1;
            next += 1.0;
        }
        assert_eq!(xs(&p), [3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        for i in 0..20 {
            // Kill the oldest, spawn one: the window slides and wraps.
            let h = p.head;
            p.age[h] = 5.0;
            p.compact();
            let s = p.next_slot();
            p.spawn(s, Vec3A::splat(next), Vec3A::ZERO, &base);
            p.len += 1;
            next += 1.0;
            let v = xs(&p);
            assert_eq!(v.len(), 8, "iteration {i}");
            assert!(
                v.windows(2).all(|w| w[1] == w[0] + 1.0),
                "order kept: {v:?}"
            );
        }
    }
}

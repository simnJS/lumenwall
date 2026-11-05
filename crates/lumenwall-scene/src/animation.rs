//! Property animations: the `animation` timeline of a setting (`{"animation": {"c0": [keyframes],
//! "options": {...}}, "value": ..}`), played by scripts through `IAnimation` (spec 06).
//!
//! A timeline has one keyframe list per component (`c0` … `c3`), each keyframe a frame number, a
//! value and two bezier handles (offsets in frames and in value). Between two keyframes the
//! value follows the cubic bezier whose inner control points are the front handle of the first
//! and the back handle of the second; a keyframe without an enabled handle on a side gives a
//! straight segment on that side.

use crate::raw::{Json, RawAnimation, RawHandle, RawKeyframe};

/// Index of a timeline in [`crate::Settings`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnimationId(pub(crate) u32);

impl AnimationId {
    /// Position in the store.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One keyframe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Keyframe {
    /// Frame number.
    pub frame: f32,
    /// Value at that frame.
    pub value: f32,
    /// Handle towards the previous keyframe: `(frames, value)` offset, if enabled.
    pub back: Option<[f32; 2]>,
    /// Handle towards the next keyframe, if enabled.
    pub front: Option<[f32; 2]>,
}

/// What a timeline does when it reaches its last frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationMode {
    /// Plays once and stops on the last frame.
    #[default]
    Single,
    /// Starts again from the first frame.
    Loop,
    /// Plays backwards, then forwards again.
    Mirror,
}

/// A timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyAnimation {
    /// Keyframes of each component, at least one list.
    pub channels: Vec<Vec<Keyframe>>,
    /// Frames per second.
    pub fps: f32,
    /// Length in frames.
    pub length: f32,
    /// End behaviour.
    pub mode: AnimationMode,
    /// Starts paused (a script plays it).
    pub start_paused: bool,
}

fn number(j: &Json<'_>) -> Option<f32> {
    match j {
        Json::Int(i) => Some(*i as f32),
        Json::UInt(u) => Some(*u as f32),
        Json::Float(f) => Some(*f as f32),
        _ => None,
    }
}

fn handle(h: &RawHandle<'_>) -> Option<[f32; 2]> {
    let enabled = matches!(h.enabled, Json::Bool(true));
    enabled.then(|| [number(&h.x).unwrap_or(0.0), number(&h.y).unwrap_or(0.0)])
}

fn keyframes(raw: &[RawKeyframe<'_>]) -> Vec<Keyframe> {
    let mut out: Vec<Keyframe> = raw
        .iter()
        .filter_map(|k| {
            Some(Keyframe {
                frame: number(&k.frame)?,
                value: number(&k.value)?,
                back: handle(&k.back),
                front: handle(&k.front),
            })
        })
        .collect();
    out.sort_by(|a, b| a.frame.total_cmp(&b.frame));
    out
}

impl PropertyAnimation {
    /// Reads a timeline; `None` when it has no keyframe at all.
    pub(crate) fn from_raw(raw: &RawAnimation<'_>) -> Option<Self> {
        let mut channels: Vec<Vec<Keyframe>> = [&raw.c0, &raw.c1, &raw.c2, &raw.c3]
            .into_iter()
            .map(|c| keyframes(&c.items))
            .collect();
        while channels.len() > 1 && channels.last().is_some_and(Vec::is_empty) {
            channels.pop();
        }
        if channels.iter().all(Vec::is_empty) {
            return None;
        }
        let last = channels
            .iter()
            .flatten()
            .map(|k| k.frame)
            .fold(0.0f32, f32::max);
        let o = &raw.options;
        let fps = number(&o.fps).filter(|f| *f > 0.0).unwrap_or(30.0);
        let length = number(&o.length).filter(|l| *l > 0.0).unwrap_or(last);
        let mode = match &o.mode {
            Json::Str(s) if s.eq_ignore_ascii_case("loop") => AnimationMode::Loop,
            Json::Str(s) if s.eq_ignore_ascii_case("mirror") => AnimationMode::Mirror,
            _ => AnimationMode::Single,
        };
        Some(PropertyAnimation {
            channels,
            fps,
            length,
            mode,
            start_paused: matches!(o.startpaused, Json::Bool(true)),
        })
    }

    /// Duration in seconds.
    pub fn duration(&self) -> f32 {
        self.length / self.fps
    }

    /// Values of the components at `frame` (components without keyframes are 0).
    pub fn value_at(&self, frame: f32) -> [f32; 4] {
        let mut out = [0.0; 4];
        for (o, c) in out.iter_mut().zip(&self.channels) {
            *o = channel_at(c, frame);
        }
        out
    }
}

fn channel_at(keys: &[Keyframe], frame: f32) -> f32 {
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
        return 0.0;
    };
    if frame <= first.frame {
        return first.value;
    }
    if frame >= last.frame {
        return last.value;
    }
    let i = keys.partition_point(|k| k.frame <= frame).max(1) - 1;
    let (a, b) = (keys[i], keys[i + 1]);
    let span = b.frame - a.frame;
    if span <= f32::EPSILON {
        return b.value;
    }
    if a.front.is_none() && b.back.is_none() {
        return a.value + (b.value - a.value) * (frame - a.frame) / span;
    }
    // Control points; a missing handle is the straight third of the segment. The handle's x is
    // kept inside the segment so that x(t) stays monotonic.
    let third = [span / 3.0, (b.value - a.value) / 3.0];
    let f = a.front.unwrap_or(third);
    let k = b.back.unwrap_or([-third[0], -third[1]]);
    let p1 = (a.frame + f[0].clamp(0.0, span), a.value + f[1]);
    let p2 = (b.frame + k[0].clamp(-span, 0.0), b.value + k[1]);
    let bez = |t: f32, p0: f32, p1: f32, p2: f32, p3: f32| {
        let u = 1.0 - t;
        u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if bez(mid, a.frame, p1.0, p2.0, b.frame) < frame {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(0.5 * (lo + hi), a.value, p1.1, p2.1, b.value)
}

impl PropertyAnimation {
    /// Maps the elapsed `time` in frames (from the start, unbounded) to the frame shown and
    /// whether a `Single` timeline has ended: `Single` stops on the last frame, `Loop` wraps,
    /// `Mirror` goes back and forth.
    pub fn frame_at(&self, elapsed: f32) -> (f32, bool) {
        let len = self.length.max(f32::EPSILON);
        match self.mode {
            AnimationMode::Single => (elapsed.min(len), elapsed >= len),
            AnimationMode::Loop => (elapsed.rem_euclid(len), false),
            AnimationMode::Mirror => {
                let m = elapsed.rem_euclid(2.0 * len);
                (if m > len { 2.0 * len - m } else { m }, false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(frame: f32, value: f32, h: bool) -> Keyframe {
        Keyframe {
            frame,
            value,
            back: h.then_some([-1.0, 0.0]),
            front: h.then_some([1.0, 0.0]),
        }
    }

    fn anim(keys: Vec<Keyframe>, mode: AnimationMode) -> PropertyAnimation {
        PropertyAnimation {
            channels: vec![keys],
            fps: 15.0,
            length: 15.0,
            mode,
            start_paused: true,
        }
    }

    #[test]
    fn straight_and_eased_segments() {
        let a = anim(
            vec![key(0.0, 1.0, false), key(15.0, 0.0, false)],
            AnimationMode::Single,
        );
        assert!((a.value_at(7.5)[0] - 0.5).abs() < 1e-6);
        assert_eq!(a.value_at(-3.0)[0], 1.0);
        assert_eq!(a.value_at(40.0)[0], 0.0);
        // Horizontal handles of one frame: slow at both ends, still 0.5 in the middle.
        let e = anim(
            vec![key(0.0, 1.0, true), key(15.0, 0.0, true)],
            AnimationMode::Single,
        );
        assert!((e.value_at(7.5)[0] - 0.5).abs() < 1e-3);
        assert!(e.value_at(1.0)[0] > 0.9, "{}", e.value_at(1.0)[0]);
        assert!(e.value_at(14.0)[0] < 0.1);
        let mut prev = 1.0;
        for f in 0..=150 {
            let v = e.value_at(f as f32 / 10.0)[0];
            assert!(v <= prev + 1e-5, "monotonic at {f}");
            prev = v;
        }
    }

    #[test]
    fn end_modes() {
        let k = vec![key(0.0, 0.0, false), key(15.0, 1.0, false)];
        let s = anim(k.clone(), AnimationMode::Single);
        assert_eq!(s.frame_at(20.0), (15.0, true));
        assert_eq!(s.frame_at(5.0), (5.0, false));
        let l = anim(k.clone(), AnimationMode::Loop);
        assert_eq!(l.frame_at(20.0), (5.0, false));
        let m = anim(k, AnimationMode::Mirror);
        assert_eq!(m.frame_at(20.0), (10.0, false));
        assert_eq!(m.frame_at(35.0), (5.0, false));
    }
}

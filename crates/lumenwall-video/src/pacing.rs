//! Playback timing, independent of FFmpeg and of the GPU (pure, unit tested).
//!
//! - [`LoopTimeline`]: stream timestamps → one continuous media time across loops, so the
//!   first frame of a loop follows the last frame of the previous one by exactly one frame
//!   duration (no visible jump, spec 07 §14.1 "boucle infinie").
//! - [`Decimator`]: drops, on the decoding thread, the frames that would never be shown
//!   when the video is faster than the frame cap (60 fps video, 30 fps cap): a frame is kept
//!   when it is the newest one at some point of the display grid `k / cap`. Dropped frames
//!   are never copied or uploaded.
//! - [`PlayClock`]: media time of the wall clock, frozen while paused (the reference pauses
//!   its video player, spec 07 §9.4), resynchronised when decoding falls behind.

use std::time::{Duration, Instant};

/// Continuous media time across loops.
#[derive(Clone, Debug, Default)]
pub struct LoopTimeline {
    /// Stream timestamp of time 0 in every loop (seconds).
    start: Option<f64>,
    /// Media time of the current loop's start.
    offset: f64,
    /// End of the latest frame of the current loop, relative to the loop start.
    end: f64,
    /// Media time of the last frame returned (for frames without a timestamp).
    last: Option<f64>,
    /// Loops completed.
    loops: u64,
}

impl LoopTimeline {
    /// `start`: the stream's declared start time, if any (seconds).
    pub fn new(start: Option<f64>) -> Self {
        Self {
            start,
            ..Self::default()
        }
    }

    /// Media time of a frame with stream time `pts` (seconds; `None` = unknown) lasting
    /// `duration` seconds.
    pub fn frame(&mut self, pts: Option<f64>, duration: f64) -> f64 {
        let duration = if duration.is_finite() && duration > 0.0 {
            duration
        } else {
            0.0
        };
        let local = match pts {
            Some(p) => {
                let start = *self.start.get_or_insert(p);
                (p - start).max(0.0)
            }
            // No timestamp: right after the previous frame of this loop.
            None => self.end,
        };
        self.end = self.end.max(local + duration);
        let t = self.offset + local;
        self.last = Some(t);
        t
    }

    /// The demuxer wrapped around: the next frame starts a new loop.
    pub fn wrap(&mut self) {
        self.offset += self.end;
        self.end = 0.0;
        self.loops += 1;
    }

    /// Loops completed.
    pub fn loops(&self) -> u64 {
        self.loops
    }
}

/// Keeps the frames that some display instant `k / cap` would show.
#[derive(Debug)]
pub struct Decimator<T> {
    pending: Option<(f64, T)>,
}

impl<T> Default for Decimator<T> {
    fn default() -> Self {
        Self { pending: None }
    }
}

/// Tolerance on frame times (float error, timestamp rounding).
const EPSILON: f64 = 1e-4;

/// Whether a display instant `k / cap` lies in `[from, to)` (both media times).
fn grid_point_in(from: f64, to: f64, cap: u32) -> bool {
    let cap = f64::from(cap);
    let k = ((from - EPSILON) * cap).ceil();
    k / cap < to - EPSILON
}

impl<T> Decimator<T> {
    /// Feeds the next decoded frame (media time `t`); returns the frame to keep, if any.
    /// `cap` = 0 keeps everything. A one-frame lookahead decides whether the previous frame
    /// is the newest one at a display instant.
    pub fn push(&mut self, t: f64, item: T, cap: u32) -> Option<(f64, T)> {
        if cap == 0 {
            return match self.pending.replace((t, item)) {
                Some(p) => Some(p),
                None => self.pending.take(),
            };
        }
        let prev = self.pending.replace((t, item))?;
        // Frames out of order or duplicated: keep the earlier one only if it is shown.
        if t <= prev.0 || grid_point_in(prev.0, t, cap) {
            Some(prev)
        } else {
            None
        }
    }

    /// Frame held back by the lookahead (end of stream, flush).
    pub fn take(&mut self) -> Option<(f64, T)> {
        self.pending.take()
    }
}

/// Media time of the wall clock, with pause and resynchronisation.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlayClock {
    /// Wall-clock instant of media time 0 (shifted by pauses).
    origin: Option<Instant>,
    paused_at: Option<Instant>,
}

/// A frame later than this (and no newer one available) resynchronises the clock: decoding
/// is slower than real time, play slower instead of skipping or bursting.
pub const MAX_LAG: Duration = Duration::from_millis(250);

impl PlayClock {
    /// Starts so that media time `t` is now.
    pub fn start(&mut self, t: f64, now: Instant) {
        self.origin = Some(now.checked_sub(secs(t)).unwrap_or(now));
        if self.paused_at.is_some() {
            self.paused_at = Some(now);
        }
    }

    pub fn started(&self) -> bool {
        self.origin.is_some()
    }

    /// Media time at `now` (frozen while paused).
    pub fn time(&self, now: Instant) -> Option<f64> {
        let origin = self.origin?;
        let at = self.paused_at.unwrap_or(now);
        Some(at.saturating_duration_since(origin).as_secs_f64())
    }

    /// Wall-clock instant of media time `t` (`None` while paused or before the start).
    pub fn instant_of(&self, t: f64) -> Option<Instant> {
        if self.paused_at.is_some() {
            return None;
        }
        Some(self.origin? + secs(t))
    }

    pub fn pause(&mut self, now: Instant) {
        if self.paused_at.is_none() {
            self.paused_at = Some(now);
        }
    }

    pub fn resume(&mut self, now: Instant) {
        if let (Some(p), Some(o)) = (self.paused_at.take(), self.origin.as_mut()) {
            *o += now.saturating_duration_since(p);
        }
    }

    pub fn paused(&self) -> bool {
        self.paused_at.is_some()
    }
}

fn secs(t: f64) -> Duration {
    Duration::from_secs_f64(t.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_is_continuous_across_loops() {
        let mut tl = LoopTimeline::new(None);
        let d = 1.0 / 60.0;
        // A stream whose timestamps start at 0.5 s, 4 frames.
        let first: Vec<f64> = (0..4)
            .map(|i| tl.frame(Some(0.5 + f64::from(i) * d), d))
            .collect();
        tl.wrap();
        let second: Vec<f64> = (0..4)
            .map(|i| tl.frame(Some(0.5 + f64::from(i) * d), d))
            .collect();
        assert!(first[0].abs() < 1e-12);
        // Loop 2 starts one frame after the last frame of loop 1.
        assert!((second[0] - (first[3] + d)).abs() < 1e-9);
        assert!((second[3] - 7.0 * d).abs() < 1e-9);
        assert_eq!(tl.loops(), 1);
    }

    #[test]
    fn timeline_without_timestamps() {
        let mut tl = LoopTimeline::new(Some(0.0));
        assert_eq!(tl.frame(None, 0.1), 0.0);
        assert!((tl.frame(None, 0.1) - 0.1).abs() < 1e-12);
    }

    fn kept(fps: f64, cap: u32, n: u32) -> Vec<u32> {
        let mut d = Decimator::default();
        let mut out = Vec::new();
        for i in 0..n {
            if let Some((_, k)) = d.push(f64::from(i) / fps, i, cap) {
                out.push(k);
            }
        }
        out
    }

    #[test]
    fn sixty_fps_at_thirty_keeps_every_other_frame() {
        assert_eq!(kept(60.0, 30, 9), vec![0, 2, 4, 6]);
    }

    #[test]
    fn slower_video_keeps_every_frame() {
        assert_eq!(kept(24.0, 30, 6), vec![0, 1, 2, 3, 4]);
        assert_eq!(kept(30.0, 30, 6), vec![0, 1, 2, 3, 4]);
        // No cap: everything, without lookahead.
        assert_eq!(kept(240.0, 0, 4), vec![0, 1, 2, 3]);
    }

    #[test]
    fn fifty_fps_at_thirty_keeps_three_of_five() {
        // Grid 0, 33.3, 66.7, 100 ms; frames every 20 ms: 0, 20, 60, 100 are shown.
        assert_eq!(kept(50.0, 30, 7), vec![0, 1, 3, 5]);
    }

    #[test]
    fn clock_pause_freezes_media_time() {
        let t0 = Instant::now();
        let mut c = PlayClock::default();
        assert_eq!(c.time(t0), None);
        c.start(0.0, t0);
        let t1 = t0 + Duration::from_millis(500);
        assert!((c.time(t1).unwrap() - 0.5).abs() < 1e-6);
        c.pause(t1);
        let t2 = t1 + Duration::from_secs(10);
        assert!((c.time(t2).unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(c.instant_of(1.0), None);
        c.resume(t2);
        assert!((c.time(t2 + Duration::from_millis(100)).unwrap() - 0.6).abs() < 1e-6);
        let due = c.instant_of(1.0).unwrap();
        assert!((due.duration_since(t2).as_secs_f64() - 0.5).abs() < 1e-6);
    }
}

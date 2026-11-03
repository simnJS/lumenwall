//! Frame scheduling and pause bookkeeping shared by every backend (pure logic, testable without an OS).
//!
//! A backend keeps one [`FrameSlot`] per output. The caller asks for a frame with `request_frame`; the backend
//! emits [`Event::Frame`] once the output is running, the compositor is ready (Wayland frame callback; always
//! true elsewhere) and the frame cap allows it. The earliest pending deadline becomes the timeout of the
//! blocking wait, so a paused or idle output never arms a timer.

use std::time::{Duration, Instant};

use crate::types::{Event, OutputId, PauseReasons};

/// Frame-rate cap with drift-free cadence.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameClock {
    interval: Duration,
    last: Option<Instant>,
}

impl FrameClock {
    pub(crate) fn new(max_fps: u32) -> Self {
        Self {
            interval: interval_for(max_fps),
            last: None,
        }
    }

    pub(crate) fn set_max_fps(&mut self, max_fps: u32) {
        self.interval = interval_for(max_fps);
    }

    /// Earliest instant at which the next frame may start.
    pub(crate) fn earliest(&self, now: Instant) -> Instant {
        match self.last {
            Some(last) => (last + self.interval).max(now),
            None => now,
        }
    }

    /// Records that a frame starts at `now`. When the frame is on time (less than one interval late) the
    /// cadence is kept on the ideal grid, so timer jitter does not accumulate into a lower frame rate.
    pub(crate) fn mark(&mut self, now: Instant) {
        let next = match self.last {
            Some(last) => {
                let ideal = last + self.interval;
                if ideal <= now && now - ideal < self.interval {
                    ideal
                } else {
                    now
                }
            }
            None => now,
        };
        self.last = Some(next);
    }
}

fn interval_for(max_fps: u32) -> Duration {
    if max_fps == 0 {
        Duration::ZERO
    } else {
        Duration::from_nanos(1_000_000_000 / u64::from(max_fps))
    }
}

/// Per-output frame request state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameSlot {
    pub(crate) requested: bool,
    pub(crate) clock: FrameClock,
}

impl FrameSlot {
    pub(crate) fn new(max_fps: u32) -> Self {
        Self {
            requested: false,
            clock: FrameClock::new(max_fps),
        }
    }

    /// When a frame should be emitted, if one is wanted and possible.
    pub(crate) fn due(&self, now: Instant, paused: bool, ready: bool) -> Option<Instant> {
        (self.requested && !paused && ready).then(|| self.clock.earliest(now))
    }

    /// Emits the frame if it is due at `now`; returns `true` when it was emitted.
    pub(crate) fn fire_if_due(&mut self, now: Instant, paused: bool, ready: bool) -> bool {
        match self.due(now, paused, ready) {
            Some(due) if due <= now => {
                self.requested = false;
                self.clock.mark(now);
                true
            }
            _ => false,
        }
    }
}

/// Event to emit when the pause reasons of an output change from `old` to `new`.
pub(crate) fn pause_transition(
    output: OutputId,
    old: PauseReasons,
    new: PauseReasons,
) -> Option<Event> {
    if old == new {
        None
    } else if new.is_empty() {
        Some(Event::Resumed(output))
    } else {
        Some(Event::Paused {
            output,
            reasons: new,
        })
    }
}

/// Earliest of two optional deadlines.
pub(crate) fn min_deadline(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

/// Converts an absolute deadline into a timeout for a blocking call (`None` = wait forever).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn timeout_until(deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    deadline.map(|d| d.saturating_duration_since(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn clock_caps_rate() {
        let t0 = Instant::now();
        let mut c = FrameClock::new(30);
        assert_eq!(c.earliest(t0), t0);
        c.mark(t0);
        let due = c.earliest(t0 + MS);
        assert_eq!(due, t0 + Duration::from_nanos(33_333_333));
    }

    #[test]
    fn clock_keeps_cadence_despite_jitter() {
        let t0 = Instant::now();
        let mut c = FrameClock::new(50); // 20 ms
        c.mark(t0);
        // Woken 2 ms late: the next deadline stays on the 20 ms grid.
        c.mark(t0 + 22 * MS);
        assert_eq!(c.earliest(t0 + 23 * MS), t0 + 40 * MS);
        // Very late (after a pause): restart from now.
        c.mark(t0 + 500 * MS);
        assert_eq!(c.earliest(t0 + 500 * MS), t0 + 520 * MS);
    }

    #[test]
    fn uncapped_clock() {
        let t0 = Instant::now();
        let mut c = FrameClock::new(0);
        c.mark(t0);
        assert_eq!(c.earliest(t0), t0);
    }

    #[test]
    fn slot_only_fires_when_requested_running_and_ready() {
        let t0 = Instant::now();
        let mut s = FrameSlot::new(30);
        assert!(!s.fire_if_due(t0, false, true));
        s.requested = true;
        assert_eq!(
            s.due(t0, true, true),
            None,
            "paused outputs never arm a deadline"
        );
        assert_eq!(
            s.due(t0, false, false),
            None,
            "not ready: waiting for the compositor"
        );
        assert!(s.fire_if_due(t0, false, true));
        assert!(!s.requested);
        s.requested = true;
        assert!(!s.fire_if_due(t0 + MS, false, true), "frame cap");
        assert!(s.fire_if_due(t0 + 34 * MS, false, true));
    }

    #[test]
    fn transitions() {
        let o = OutputId(3);
        let fs = PauseReasons::FULLSCREEN;
        assert_eq!(
            pause_transition(o, PauseReasons::NONE, PauseReasons::NONE),
            None
        );
        assert_eq!(
            pause_transition(o, PauseReasons::NONE, fs),
            Some(Event::Paused {
                output: o,
                reasons: fs
            })
        );
        let both = fs | PauseReasons::LOCKED;
        assert_eq!(
            pause_transition(o, fs, both),
            Some(Event::Paused {
                output: o,
                reasons: both
            })
        );
        assert_eq!(
            pause_transition(o, both, PauseReasons::NONE),
            Some(Event::Resumed(o))
        );
    }

    #[test]
    fn deadlines() {
        let t0 = Instant::now();
        assert_eq!(min_deadline(None, None), None);
        assert_eq!(min_deadline(Some(t0 + MS), Some(t0)), Some(t0));
        assert_eq!(min_deadline(None, Some(t0)), Some(t0));
        assert_eq!(timeout_until(Some(t0), t0 + MS), Some(Duration::ZERO));
        assert_eq!(timeout_until(None, t0), None);
    }
}

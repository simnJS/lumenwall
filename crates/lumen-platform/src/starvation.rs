//! Hidden-window detection for X11 windows shown through XWayland (pure logic, testable without a display).
//!
//! On a Wayland session (GNOME/Mutter has no layer-shell, so Lumen falls back on X11 desktop windows) the
//! X11 window tree only contains XWayland clients: a native Wayland window covering the desktop, the lock
//! screen or a powered-off monitor produce no X11 event at all. But each X11 window is a Wayland surface of
//! XWayland, and the compositor stops sending frame callbacks to surfaces it does not paint. XWayland then
//! completes `PresentPixmap` requests on a fallback timer (once per second) instead of at the compositor's
//! frame rate. The X11 backend selects `PresentCompleteNotify` on its windows (the renderer's own presents
//! are reported to every client that selected them) and feeds this tracker:
//!
//! * **running**: a present left unanswered for [`STARVED_AFTER`] means the window is hidden. Event times
//!   come from the completion's UST, so a render thread that was itself blocked waiting for that completion
//!   (FIFO swapchain) still measures the real latency, and a thread busy elsewhere does not invent one.
//! * **hidden**: the renderer is paused; one probe present (a 1×1 copy of the window's own pixel, so nothing
//!   visible changes) stays in flight. XWayland answers it on its fallback timer while the window stays
//!   hidden (one wake-up per second) and within a frame once the compositor paints the window again;
//!   [`FAST_PROBES`] prompt answers in a row resume the output.
//!
//! Measured on GNOME 50 / XWayland (Ubuntu 26.04): visible 2–6 ms per answer, hidden exactly 1000 ms.

use std::time::{Duration, Instant};

/// A present unanswered for this long means the compositor stopped painting the window.
pub(crate) const STARVED_AFTER: Duration = Duration::from_millis(750);
/// A probe answered faster than this was answered by a frame callback, not by the fallback timer.
pub(crate) const FAST: Duration = Duration::from_millis(250);
/// Prompt probe answers in a row needed to call the window visible again.
pub(crate) const FAST_PROBES: u8 = 2;
/// A probe still unanswered after this is replaced (lost event, or a server without fallback timer).
pub(crate) const PROBE_BACKSTOP: Duration = Duration::from_secs(2);
/// Replaced probes in a row after which the detection is abandoned for the window.
pub(crate) const GIVE_UP_AFTER: u8 = 5;

/// Outcome of a tracker update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// Nothing changed.
    None,
    /// The window just became hidden.
    Hidden,
    /// The window is visible again.
    Visible,
    /// Probes are never answered: detection disabled for this window (it counts as visible).
    GaveUp,
}

#[derive(Clone, Copy, Debug)]
struct Probe {
    serial: u32,
    sent: Instant,
}

/// Frame-callback starvation tracker of one window.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Starvation {
    /// Issue time of the oldest renderer present not answered yet.
    waiting_since: Option<Instant>,
    probe: Option<Probe>,
    fast: u8,
    unanswered: u8,
    hidden: bool,
    disabled: bool,
}

impl Starvation {
    /// True while the compositor does not paint the window.
    pub(crate) fn hidden(&self) -> bool {
        self.hidden
    }

    /// The renderer is about to present a frame.
    pub(crate) fn presented(&mut self, now: Instant) {
        if !self.disabled && !self.hidden {
            self.waiting_since.get_or_insert(now);
        }
    }

    /// The output is paused for another reason: forget the pending present (it may legitimately never be
    /// answered, and must not count once the output runs again).
    pub(crate) fn suspend(&mut self) {
        self.waiting_since = None;
    }

    /// A `PresentCompleteNotify` (pixmap kind) arrived for the window. `at` is the event time (from its UST).
    pub(crate) fn completed(&mut self, serial: u32, skipped: bool, at: Instant) -> Change {
        if self.disabled {
            return Change::None;
        }
        if let Some(p) = self.probe
            && p.serial == serial
        {
            self.probe = None;
            self.unanswered = 0;
            if !self.hidden {
                return Change::None;
            }
            let fast = !skipped && at.saturating_duration_since(p.sent) < FAST;
            self.fast = if fast { self.fast + 1 } else { 0 };
            if self.fast >= FAST_PROBES {
                self.hidden = false;
                self.fast = 0;
                self.waiting_since = None;
                return Change::Visible;
            }
            return Change::None;
        }
        // Renderer completions only matter while running; skipped presents were never shown.
        if skipped || self.hidden {
            return Change::None;
        }
        match self.waiting_since {
            // Completion of a present issued before the one we wait for.
            Some(since) if at < since => Change::None,
            Some(since) => {
                self.waiting_since = None;
                if at - since >= STARVED_AFTER {
                    self.enter_hidden();
                    Change::Hidden
                } else {
                    Change::None
                }
            }
            None => Change::None,
        }
    }

    /// Deadline check, after the pending events were read: an unanswered present or probe.
    pub(crate) fn check(&mut self, now: Instant) -> Change {
        if self.disabled {
            return Change::None;
        }
        if !self.hidden {
            if self
                .waiting_since
                .is_some_and(|s| now.saturating_duration_since(s) >= STARVED_AFTER)
            {
                self.enter_hidden();
                return Change::Hidden;
            }
            return Change::None;
        }
        if let Some(p) = self.probe
            && now.saturating_duration_since(p.sent) >= PROBE_BACKSTOP
        {
            self.probe = None;
            self.fast = 0;
            self.unanswered += 1;
            if self.unanswered >= GIVE_UP_AFTER {
                self.disabled = true;
                self.hidden = false;
                self.waiting_since = None;
                return Change::GaveUp;
            }
        }
        Change::None
    }

    /// True when a probe should be sent now.
    pub(crate) fn wants_probe(&self) -> bool {
        !self.disabled && self.hidden && self.probe.is_none()
    }

    /// A probe present with `serial` was sent at `now`.
    pub(crate) fn probe_sent(&mut self, serial: u32, now: Instant) {
        self.probe = Some(Probe { serial, sent: now });
    }

    /// When [`check`](Self::check) must run next, if ever.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        if self.disabled {
            None
        } else if self.hidden {
            self.probe.map(|p| p.sent + PROBE_BACKSTOP)
        } else {
            self.waiting_since.map(|s| s + STARVED_AFTER)
        }
    }

    fn enter_hidden(&mut self) {
        self.hidden = true;
        self.fast = 0;
        self.waiting_since = None;
    }
}

/// Local time of an event stamped `ust` (µs of `CLOCK_MONOTONIC`, the server's clock), read at `now` when
/// that clock reads `now_us`. Implausible stamps (another clock, zero, in the future) fall back on `now`.
pub(crate) fn event_time(now: Instant, now_us: u64, ust: u64) -> Instant {
    match now_us.checked_sub(ust) {
        Some(age) if age < 60_000_000 => now.checked_sub(Duration::from_micros(age)).unwrap_or(now),
        _ => now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn visible_renderer_never_starves() {
        let t0 = Instant::now();
        let mut s = Starvation::default();
        for i in 0..100u32 {
            let t = t0 + MS * (33 * i);
            s.presented(t);
            assert_eq!(s.deadline(), Some(t + STARVED_AFTER));
            assert_eq!(s.completed(i, false, t + MS * 5), Change::None);
            assert_eq!(s.check(t + MS * 30), Change::None);
            assert_eq!(s.deadline(), None, "nothing pending: no timer");
        }
        assert!(!s.hidden());
    }

    #[test]
    fn late_completion_means_hidden_even_when_read_late() {
        let t0 = Instant::now();
        let mut s = Starvation::default();
        s.presented(t0);
        s.presented(t0 + MS * 33); // the oldest pending present counts
        // FIFO: the render thread was blocked until the fallback timer answered.
        assert_eq!(s.completed(7, false, t0 + MS * 1000), Change::Hidden);
        assert!(s.hidden());
        assert!(s.wants_probe());
        s.presented(t0 + MS * 1010); // a frame already in flight: ignored while hidden
        assert_eq!(s.deadline(), None);
    }

    #[test]
    fn busy_thread_does_not_fake_starvation() {
        let t0 = Instant::now();
        let mut s = Starvation::default();
        s.presented(t0);
        // Read 3 s later (scene loading), but the event time says it was answered in 4 ms.
        assert_eq!(s.completed(1, false, t0 + MS * 4), Change::None);
        assert_eq!(s.check(t0 + MS * 3000), Change::None);
        assert!(!s.hidden());
    }

    #[test]
    fn older_completion_does_not_answer_a_newer_present() {
        let t0 = Instant::now();
        let mut s = Starvation::default();
        s.presented(t0 + MS * 100);
        assert_eq!(s.completed(1, false, t0 + MS * 90), Change::None);
        assert_eq!(s.deadline(), Some(t0 + MS * 100 + STARVED_AFTER));
        // Skipped presents prove nothing either.
        assert_eq!(s.completed(2, true, t0 + MS * 120), Change::None);
        assert_eq!(s.check(t0 + MS * 900), Change::Hidden);
    }

    #[test]
    fn probes_resume_after_two_prompt_answers() {
        let t0 = Instant::now();
        let mut s = Starvation::default();
        s.presented(t0);
        assert_eq!(s.check(t0 + STARVED_AFTER), Change::Hidden);
        // Still hidden: the fallback timer answers after 1 s.
        let mut t = t0 + STARVED_AFTER;
        for serial in 0..3 {
            assert!(s.wants_probe());
            s.probe_sent(serial, t);
            assert!(!s.wants_probe());
            assert_eq!(s.deadline(), Some(t + PROBE_BACKSTOP));
            t += MS * 1000;
            assert_eq!(s.completed(serial, false, t), Change::None);
            assert!(s.hidden());
        }
        // Uncovered mid-cycle: first answer after 600 ms (frame callback), then prompt ones.
        s.probe_sent(10, t);
        assert_eq!(s.completed(10, false, t + MS * 600), Change::None);
        s.probe_sent(11, t + MS * 600);
        assert_eq!(s.completed(11, false, t + MS * 603), Change::None);
        s.probe_sent(12, t + MS * 603);
        // A renderer completion with another serial does not count as a probe answer.
        assert_eq!(s.completed(99, false, t + MS * 604), Change::None);
        assert_eq!(s.completed(12, false, t + MS * 606), Change::Visible);
        assert!(!s.hidden());
        assert!(!s.wants_probe());
    }

    #[test]
    fn slow_answer_resets_the_streak() {
        let t0 = Instant::now();
        let mut s = Starvation::default();
        s.presented(t0);
        s.check(t0 + STARVED_AFTER);
        s.probe_sent(1, t0);
        assert_eq!(s.completed(1, false, t0 + MS * 5), Change::None);
        s.probe_sent(2, t0 + MS * 5);
        assert_eq!(s.completed(2, false, t0 + MS * 1005), Change::None);
        s.probe_sent(3, t0 + MS * 1005);
        assert_eq!(s.completed(3, false, t0 + MS * 1010), Change::None);
        assert!(s.hidden(), "one prompt answer is not enough");
        s.probe_sent(4, t0 + MS * 1010);
        assert_eq!(s.completed(4, true, t0 + MS * 1011), Change::None);
        assert!(s.hidden(), "a skipped probe is not an answer");
    }

    #[test]
    fn lost_probes_are_replaced_then_detection_gives_up() {
        let t0 = Instant::now();
        let mut s = Starvation::default();
        s.presented(t0);
        s.check(t0 + STARVED_AFTER);
        let mut t = t0 + STARVED_AFTER;
        for i in 0..GIVE_UP_AFTER {
            assert!(s.wants_probe());
            s.probe_sent(u32::from(i), t);
            t += PROBE_BACKSTOP;
            let c = s.check(t);
            if i + 1 < GIVE_UP_AFTER {
                assert_eq!(c, Change::None);
                assert!(s.hidden());
            } else {
                assert_eq!(c, Change::GaveUp);
            }
        }
        assert!(!s.hidden());
        assert!(!s.wants_probe());
        s.presented(t);
        assert_eq!(s.deadline(), None, "disabled: never arms a timer again");
    }

    #[test]
    fn suspend_forgets_the_pending_present() {
        let t0 = Instant::now();
        let mut s = Starvation::default();
        s.presented(t0);
        s.suspend();
        assert_eq!(s.deadline(), None);
        assert_eq!(s.check(t0 + MS * 5000), Change::None);
    }

    #[test]
    fn event_time_from_ust() {
        let now = Instant::now();
        assert_eq!(event_time(now, 10_000_000, 9_000_000), now - MS * 1000);
        assert_eq!(event_time(now, 10_000_000, 10_000_000), now);
        assert_eq!(event_time(now, 10_000_000, 11_000_000), now, "future stamp");
        assert_eq!(event_time(now, 100_000_000_000, 0), now, "no stamp");
    }
}

//! Time of one scene: `g_Time`, `engine.runtime`, `engine.frametime`, script timers, particles,
//! puppets, animated textures and camera paths all read it through [`crate::scene::FrameState`].
//!
//! Wallpaper Engine's public SceneScript reference defines `runtime` as the time the wallpaper has
//! been running and `frametime` as the duration of the last frame. The reference reimplementation
//! starts its clock with the process, which runs one wallpaper; the daemon outlives its
//! wallpapers, so each scene gets its own clock, started on its first frame:
//!
//! - a wallpaper loaded after hours of uptime (IPC `apply`, GPU restore) starts at one frame, not at
//!   the daemon's age, and its first `frametime` is one frame, not the whole uptime (Dino Run
//!   moved its dino by `speed × frametime` pixels: 5000 s of uptime put it 50 000 px away);
//! - a step longer than [`SceneClock::MAX_STEP`] (paused output, display off, a stalled frame) is
//!   cut to that step: the pause is not part of the running time (spec 06 §6.1: `t_g` does not
//!   advance during a pause) and no script integrates a pause-long `frametime`;
//! - the time stays in `f64` until it is uploaded (spec 04 Q24), and since it only counts what was
//!   shown since the wallpaper was (re)built, the `f32` copy keeps sub-millisecond resolution for
//!   the first 2.2 hours on screen and stays finer than a 144 fps frame for 18 hours.

/// Scene clock: running time and frame step from the monotonic engine clock.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SceneClock {
    /// Engine clock (seconds) at the previous tick; `None` before the first frame.
    last: Option<f64>,
    /// Running time of the scene, seconds.
    time: f64,
}

impl SceneClock {
    /// Longest step of one frame at frame rates of 4 fps and above, seconds (puppets cap their
    /// clocks the same way, spec 08 QO-P18).
    pub(crate) const MAX_STEP: f64 = 0.25;

    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Advances to `now` (engine clock, seconds) and returns `(time, dt)`. `frame` is the nominal
    /// frame period (`1 / fps`): the step of the first frame, and twice it bounds a step when the
    /// frame rate is below 8 fps.
    pub(crate) fn tick(&mut self, now: f64, frame: f64) -> (f64, f32) {
        let frame = if frame.is_finite() && frame > 0.0 {
            frame
        } else {
            1.0 / 30.0
        };
        let step = match self.last {
            None => frame,
            Some(last) => (now - last).clamp(0.0, Self::MAX_STEP.max(2.0 * frame)),
        };
        self.last = Some(now);
        self.time += step;
        (self.time, step as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_is_one_frame_whatever_the_uptime() {
        let mut c = SceneClock::new();
        let (t, dt) = c.tick(5000.0, 1.0 / 144.0);
        assert_eq!(dt, (1.0f64 / 144.0) as f32);
        assert_eq!(t, 1.0 / 144.0);
        let (t, dt) = c.tick(5000.0 + 1.0 / 144.0, 1.0 / 144.0);
        assert!((t - 2.0 / 144.0).abs() < 1e-9);
        assert!((f64::from(dt) - 1.0 / 144.0).abs() < 1e-6);
    }

    #[test]
    fn pauses_and_stalls_are_cut() {
        let mut c = SceneClock::new();
        c.tick(10.0, 1.0 / 30.0);
        let (t0, _) = c.tick(10.0 + 1.0 / 30.0, 1.0 / 30.0);
        // Ten minutes paused: one capped step.
        let (t1, dt) = c.tick(610.0, 1.0 / 30.0);
        assert_eq!(f64::from(dt), SceneClock::MAX_STEP);
        assert!((t1 - t0 - SceneClock::MAX_STEP).abs() < 1e-9);
        // Clock going backwards (should not happen): no step.
        let (t2, dt) = c.tick(600.0, 1.0 / 30.0);
        assert_eq!((t2, dt), (t1, 0.0));
    }

    #[test]
    fn low_frame_rates_keep_real_time() {
        let mut c = SceneClock::new();
        c.tick(0.0, 1.0);
        let (_, dt) = c.tick(1.0, 1.0);
        assert_eq!(dt, 1.0);
        // Not finite or zero period: 30 fps.
        let mut c = SceneClock::new();
        assert_eq!(c.tick(3.0, 0.0).1, (1.0f64 / 30.0) as f32);
    }

    /// The same seconds at 30 and 144 fps give the same running time.
    #[test]
    fn running_time_does_not_depend_on_the_frame_rate() {
        let run = |fps: f64| {
            let mut c = SceneClock::new();
            let mut sum = 0.0f64;
            let n = (10.0 * fps).round() as u32;
            let mut t = 0.0;
            for i in 0..=n {
                let (time, dt) = c.tick(42.0 + f64::from(i) / fps, 1.0 / fps);
                sum += f64::from(dt);
                t = time;
            }
            assert!((t - sum).abs() < 1e-3);
            t
        };
        let (a, b) = (run(30.0), run(144.0));
        assert!((a - 10.0 - 1.0 / 30.0).abs() < 1e-9, "{a}");
        assert!((b - 10.0 - 1.0 / 144.0).abs() < 1e-9, "{b}");
    }
}

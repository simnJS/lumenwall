//! When to take the `--screenshot` capture (spec 07 §18.2). Validation of the path is in
//! [`crate::settings::ScreenshotFormat`]; reading back and encoding belong to the renderer.

/// One capture per arming; the application keeps running afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenshotTrigger {
    delay_frames: u32,
    due: Option<u64>,
}

impl ScreenshotTrigger {
    pub const fn new(delay_frames: u32) -> Self {
        Self {
            delay_frames,
            due: None,
        }
    }

    /// Call whenever a wallpaper is loaded (startup and playlist changes).
    pub fn arm(&mut self, frame: u64) {
        self.due = Some(frame + u64::from(self.delay_frames));
    }

    /// Call at the end of each iteration that was not paused; true exactly once per arming,
    /// at the first frame count reaching the deadline.
    pub fn should_capture(&mut self, frame: u64) -> bool {
        match self.due {
            Some(due) if frame >= due => {
                self.due = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_per_arming() {
        let mut trigger = ScreenshotTrigger::new(5);
        assert!(!trigger.should_capture(100));
        trigger.arm(0);
        assert!((0..5).all(|f| !trigger.should_capture(f)));
        assert!(trigger.should_capture(5));
        assert!(!trigger.should_capture(6));
        trigger.arm(40);
        assert!(!trigger.should_capture(44));
        assert!(trigger.should_capture(50));
    }

    #[test]
    fn zero_delay_fires_on_the_arming_frame() {
        let mut trigger = ScreenshotTrigger::new(0);
        trigger.arm(7);
        assert!(trigger.should_capture(7));
    }
}

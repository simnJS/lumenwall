//! Audio data for `wallpaperRegisterAudioListener`.
//!
//! Public WE contract: an array of 128 numbers, 0–63 left channel, 64–127 right channel, bass
//! first, ≈ 0–1 (may exceed 1), about 30 calls per second, only while a listener is registered.

use std::time::{Duration, Instant};

/// Number of values passed to the page's audio listener.
pub const AUDIO_FRAME_LEN: usize = 128;

/// Interval between two audio callbacks (≈ 30 per second).
pub const AUDIO_INTERVAL: Duration = Duration::from_micros(33_333);

/// After this many consecutive all-zero frames the pacer stops sending until sound returns.
/// One second lets pages finish decay animations driven by the callback.
pub const SILENT_FRAMES_BEFORE_IDLE: u32 = 30;

/// Builds the 128-value array from two 64-band spectra (spec 07 §13, `cible64`).
///
/// Values are clamped to ≥ 0: the scene spectrum can be negative (spec 07 §13.4), the web
/// contract documents 0–1 with occasional overshoot.
pub fn audio_frame(left: &[f32; 64], right: &[f32; 64]) -> [f32; AUDIO_FRAME_LEN] {
    let mut out = [0.0; AUDIO_FRAME_LEN];
    for (dst, src) in out[..64].iter_mut().zip(left) {
        *dst = src.max(0.0);
    }
    for (dst, src) in out[64..].iter_mut().zip(right) {
        *dst = src.max(0.0);
    }
    out
}

/// Decides when the main process forwards an audio frame to the page.
#[derive(Debug, Clone)]
pub struct AudioPacer {
    next: Option<Instant>,
    silent_run: u32,
}

impl Default for AudioPacer {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioPacer {
    pub fn new() -> Self {
        Self {
            next: None,
            silent_run: 0,
        }
    }

    /// Returns true when `frame` should be sent at `now`. Call at most once per rendered frame.
    pub fn should_send(&mut self, now: Instant, frame: &[f32; AUDIO_FRAME_LEN]) -> bool {
        if self.next.is_some_and(|next| now < next) {
            return false;
        }
        let silent = frame.iter().all(|v| *v == 0.0);
        if silent {
            if self.silent_run >= SILENT_FRAMES_BEFORE_IDLE {
                return false;
            }
            self.silent_run += 1;
        } else {
            self.silent_run = 0;
        }
        // Fixed cadence without drift; resynchronise after a long gap.
        self.next = Some(match self.next {
            Some(deadline) if deadline + AUDIO_INTERVAL > now => deadline + AUDIO_INTERVAL,
            _ => now + AUDIO_INTERVAL,
        });
        true
    }

    /// Forget timing state (after a pause or a listener change).
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_layout_and_clamp() {
        let mut l = [0.5f32; 64];
        l[0] = -0.2;
        let r = [1.3f32; 64];
        let f = audio_frame(&l, &r);
        assert_eq!(f[0], 0.0);
        assert_eq!(f[1], 0.5);
        assert_eq!(f[64], 1.3);
        assert_eq!(f[127], 1.3);
    }

    #[test]
    fn pacing_is_about_30_hz() {
        let mut p = AudioPacer::new();
        let start = Instant::now();
        let frame = [0.5f32; AUDIO_FRAME_LEN];
        let mut sent = 0;
        // 144 Hz render loop for one second.
        for i in 0..144u32 {
            let now = start + Duration::from_secs_f64(f64::from(i) / 144.0);
            if p.should_send(now, &frame) {
                sent += 1;
            }
        }
        assert!((29..=31).contains(&sent), "sent {sent}");
    }

    #[test]
    fn silence_goes_idle_and_resumes() {
        let mut p = AudioPacer::new();
        let start = Instant::now();
        let zero = [0.0f32; AUDIO_FRAME_LEN];
        let mut sent = 0;
        for i in 0..120u32 {
            if p.should_send(start + AUDIO_INTERVAL * i, &zero) {
                sent += 1;
            }
        }
        assert_eq!(sent, SILENT_FRAMES_BEFORE_IDLE);
        let loud = [0.1f32; AUDIO_FRAME_LEN];
        assert!(p.should_send(start + AUDIO_INTERVAL * 121, &loud));
    }
}

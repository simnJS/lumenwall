//! Output mixing (spec 07 §11.1). Runs in the device callback: no allocation, no I/O.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use rtrb::Consumer;

/// State of one voice shared between the control thread and the device callback.
pub(crate) struct VoiceShared {
    /// Object gain (Wallpaper Engine `volume`), f32 bits.
    gain: AtomicU32,
    /// Object playing (not stopped/paused by a script, not `startsilent`).
    pub(crate) active: AtomicBool,
    /// The decoder produced its last sample (single play ended).
    pub(crate) finished: AtomicBool,
    /// `finished` and every buffered sample was played.
    pub(crate) drained: AtomicBool,
}

impl VoiceShared {
    pub(crate) fn new(gain: f32, active: bool) -> Arc<Self> {
        Arc::new(Self {
            gain: AtomicU32::new(gain.to_bits()),
            active: AtomicBool::new(active),
            finished: AtomicBool::new(false),
            drained: AtomicBool::new(false),
        })
    }

    pub(crate) fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }
}

/// Callback side of a voice.
pub(crate) struct VoiceOut {
    pub(crate) consumer: Consumer<f32>,
    pub(crate) shared: Arc<VoiceShared>,
    /// Refill is requested below this many buffered samples.
    pub(crate) low_water: usize,
}

/// Everything the device callback mixes.
pub(crate) struct MixState {
    pub(crate) voices: Vec<VoiceOut>,
    pub(crate) channels: usize,
}

impl MixState {
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            voices: Vec::with_capacity(32),
            channels: channels.max(1),
        }
    }

    /// Mixes all active voices into `out` (interleaved, `channels` per frame) with the global
    /// gain `master` (= volume/128). Returns `true` when the control thread should refill or
    /// notice a finished voice.
    pub(crate) fn mix(&mut self, out: &mut [f32], master: f32) -> bool {
        out.fill(0.0);
        let mut wake = false;
        for v in &mut self.voices {
            if !v.shared.active.load(Ordering::Relaxed) {
                continue;
            }
            let gain = master * v.shared.gain();
            let mut n = v.consumer.slots().min(out.len());
            n -= n % self.channels;
            // A voice with nothing buffered is skipped for this callback (§11.1) and its
            // decoder is woken below.
            if n > 0
                && let Ok(chunk) = v.consumer.read_chunk(n)
            {
                let (a, b) = chunk.as_slices();
                for (o, &s) in out.iter_mut().zip(a.iter().chain(b.iter())) {
                    // parity: saturating add per stream with gain volume/128 (§11.1).
                    *o = (*o + s * gain).clamp(-1.0, 1.0);
                }
                chunk.commit_all();
            }
            let left = v.consumer.slots();
            if v.shared.finished.load(Ordering::Acquire) {
                if left == 0 && !v.shared.drained.swap(true, Ordering::AcqRel) {
                    wake = true;
                }
            } else if left < v.low_water {
                wake = true;
            }
        }
        wake
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    fn voice(samples: &[f32], gain: f32) -> (VoiceOut, rtrb::Producer<f32>) {
        let (mut p, c) = RingBuffer::new(64);
        for &s in samples {
            p.push(s).unwrap();
        }
        (
            VoiceOut {
                consumer: c,
                shared: VoiceShared::new(gain, true),
                low_water: 8,
            },
            p,
        )
    }

    #[test]
    fn saturating_add_in_stream_order() {
        let mut m = MixState::new(2);
        let (a, _pa) = voice(&[0.9, 0.9], 1.0);
        let (b, _pb) = voice(&[0.5, 0.5], 1.0);
        let (c, _pc) = voice(&[-0.5, -0.5], 1.0);
        m.voices.extend([a, b, c]);
        let mut out = [0.0f32; 2];
        m.mix(&mut out, 1.0);
        // Sequential clamping: min(0.9 + 0.5, 1) − 0.5 = 0.5, not 0.9.
        assert_eq!(out, [0.5, 0.5]);
    }

    #[test]
    fn gain_is_volume_over_128_times_object_gain() {
        let mut m = MixState::new(1);
        let (a, _p) = voice(&[1.0], 0.5);
        m.voices.push(a);
        let mut out = [0.0f32; 1];
        m.mix(&mut out, 15.0 / 128.0);
        assert!((out[0] - 0.5 * 15.0 / 128.0).abs() < 1e-7);
    }

    #[test]
    fn inactive_voices_are_not_consumed() {
        let mut m = MixState::new(1);
        let (a, _p) = voice(&[0.25, 0.25], 1.0);
        a.shared.active.store(false, Ordering::Relaxed);
        m.voices.push(a);
        let mut out = [0.0f32; 2];
        m.mix(&mut out, 1.0);
        assert_eq!(out, [0.0, 0.0]);
        assert_eq!(m.voices[0].consumer.slots(), 2);
    }

    #[test]
    fn partial_data_mixes_whole_frames_and_requests_refill() {
        let mut m = MixState::new(2);
        let (a, _p) = voice(&[0.1, 0.2, 0.3], 1.0);
        m.voices.push(a);
        let mut out = [9.0f32; 4];
        assert!(m.mix(&mut out, 1.0));
        assert_eq!(out, [0.1, 0.2, 0.0, 0.0]);
        assert_eq!(m.voices[0].consumer.slots(), 1);
    }

    #[test]
    fn drained_voice_reported_once() {
        let mut m = MixState::new(1);
        let (a, _p) = voice(&[], 1.0);
        a.shared.finished.store(true, Ordering::Relaxed);
        m.voices.push(a);
        let mut out = [0.0f32; 4];
        assert!(m.mix(&mut out, 1.0));
        assert!(!m.mix(&mut out, 1.0));
        assert!(m.voices[0].shared.drained.load(Ordering::Relaxed));
    }
}

//! Conversion of native capture data to the reference capture format (spec 07 §13.2, §22).
//!
//! The reference analyses 8-bit unsigned mono PCM at 44 100 Hz produced by its sound server.
//! When a backend receives another format (WASAPI loopback, PipeWire graph format: usually
//! float 48 kHz stereo), this module reproduces the server-side conversion: channel average →
//! band-limited resampling to 44.1 kHz → 8-bit quantisation. Only bins up to ~5.4 kHz feed the
//! bands, so a short polyphase filter is exact well below the 8-bit noise floor.

use crate::resample::{Quality, Resampler};
use crate::spectrum::REFERENCE_RATE;

/// Frames processed per internal block (bounds scratch memory; no allocation per call).
const BLOCK_FRAMES: usize = 1024;

/// Quantises one normalised sample to unsigned 8-bit, the inverse of `x = (b − 128)/128`.
#[inline]
pub fn quantize_u8(x: f32) -> u8 {
    // parity: 8-bit quantisation of the reference capture format (rounding to nearest).
    let q = (x * 128.0).round().clamp(-128.0, 127.0);
    (q as i32 + 128) as u8
}

/// Sample types accepted by [`ReferenceConverter`].
pub trait CaptureSample: Copy {
    /// Normalised value in `[-1, 1]`.
    fn to_f32(self) -> f32;
}

impl CaptureSample for f32 {
    #[inline]
    fn to_f32(self) -> f32 {
        self
    }
}

impl CaptureSample for f64 {
    #[inline]
    fn to_f32(self) -> f32 {
        self as f32
    }
}

impl CaptureSample for i16 {
    #[inline]
    fn to_f32(self) -> f32 {
        f32::from(self) / 32768.0
    }
}

impl CaptureSample for i32 {
    #[inline]
    fn to_f32(self) -> f32 {
        self as f32 / 2_147_483_648.0
    }
}

impl CaptureSample for u8 {
    #[inline]
    fn to_f32(self) -> f32 {
        (f32::from(self) - 128.0) / 128.0
    }
}

impl CaptureSample for u16 {
    #[inline]
    fn to_f32(self) -> f32 {
        (f32::from(self) - 32768.0) / 32768.0
    }
}

/// Converts interleaved PCM of any rate/channel count into reference-format bytes.
pub struct ReferenceConverter {
    channels: usize,
    in_rate: u32,
    resampler: Resampler,
    mono: Vec<f32>,
    bytes: Vec<u8>,
}

impl ReferenceConverter {
    /// Converter for `channels` interleaved channels at `in_rate` Hz.
    pub fn new(in_rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        Self {
            channels,
            in_rate,
            resampler: Resampler::new(in_rate, REFERENCE_RATE, 1, Quality::Analysis),
            mono: Vec::with_capacity(BLOCK_FRAMES),
            bytes: Vec::with_capacity(BLOCK_FRAMES * 2),
        }
    }

    /// Input sample rate.
    pub fn in_rate(&self) -> u32 {
        self.in_rate
    }

    /// Input channel count.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Converts `interleaved` input; `emit` receives reference-format bytes in blocks.
    pub fn process<S: CaptureSample>(&mut self, interleaved: &[S], emit: &mut impl FnMut(&[u8])) {
        let c = self.channels;
        let scale = 1.0 / c as f32;
        let Self {
            resampler,
            mono,
            bytes,
            ..
        } = self;
        for block in interleaved.chunks(BLOCK_FRAMES * c) {
            mono.clear();
            for frame in block.chunks_exact(c) {
                // Channel average: the server's downmix to mono.
                let sum: f32 = frame.iter().map(|s| s.to_f32()).sum();
                mono.push(sum * scale);
            }
            resampler.process(mono, &mut |out: &[f32]| {
                bytes.clear();
                bytes.extend(out.iter().map(|&x| quantize_u8(x)));
                emit(bytes);
            });
        }
    }

    /// Forgets filter history (use after a capture gap).
    pub fn reset(&mut self) {
        self.resampler.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum::{SpectrumAnalyzer, WINDOW_SIZE};

    fn convert_all<S: CaptureSample>(conv: &mut ReferenceConverter, input: &[S]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in input.chunks(960 * conv.channels()) {
            conv.process(chunk, &mut |b: &[u8]| out.extend_from_slice(b));
        }
        out
    }

    #[test]
    fn quantize_is_inverse_of_reference_decoding() {
        for b in 0..=255u8 {
            let x = (f32::from(b) - 128.0) / 128.0;
            assert_eq!(quantize_u8(x), b);
        }
        assert_eq!(quantize_u8(1.0), 255);
        assert_eq!(quantize_u8(-1.5), 0);
    }

    #[test]
    fn silence_stays_silent() {
        let mut conv = ReferenceConverter::new(48_000, 2);
        let out = convert_all(&mut conv, &vec![0.0f32; 48_000 * 2]);
        assert!(out.iter().all(|&b| b == 128));
        assert!((out.len() as i64 - 44_100).abs() <= 16, "{}", out.len());
    }

    #[test]
    fn opposite_channels_cancel() {
        let mut conv = ReferenceConverter::new(48_000, 2);
        let input: Vec<f32> = (0..9600)
            .flat_map(|n| {
                let s = (n as f32 * 0.05).sin() * 0.8;
                [s, -s]
            })
            .collect();
        assert!(convert_all(&mut conv, &input).iter().all(|&b| b == 128));
    }

    /// A 48 kHz float stereo tone converted to the reference format gives the same band
    /// values as the same tone generated directly at 44.1 kHz 8-bit mono.
    #[test]
    fn converted_tone_matches_native_reference_tone() {
        for &bin in &[4usize, 20, 46, 90, 126] {
            let freq = bin as f64 * REFERENCE_RATE as f64 / WINDOW_SIZE as f64;
            let amp = 0.25;
            let mut conv = ReferenceConverter::new(48_000, 2);
            let input: Vec<f32> = (0..48_000 / 4)
                .flat_map(|n| {
                    let s = (amp * (2.0 * std::f64::consts::PI * freq * n as f64 / 48_000.0).sin())
                        as f32;
                    [s, s]
                })
                .collect();
            let converted = convert_all(&mut conv, &input);
            // Pick a window well after the filter start-up; align its phase to the native tone.
            let start = 4096;
            let mut window = [0u8; WINDOW_SIZE];
            window.copy_from_slice(&converted[start..start + WINDOW_SIZE]);
            let mut native = [0u8; WINDOW_SIZE];
            for (n, b) in native.iter_mut().enumerate() {
                let t = (start + n) as f64 / REFERENCE_RATE as f64;
                *b = quantize_u8((amp * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32);
            }
            let mut a = SpectrumAnalyzer::new(30.0);
            let mut b = SpectrumAnalyzer::new(30.0);
            a.analyze(&window);
            b.analyze(&native);
            let k = bin / 2;
            let (ta, tb) = (a.targets().b64[k], b.targets().b64[k]);
            assert!(
                (ta - tb).abs() < 0.01,
                "bin {bin}: converted {ta} vs native {tb}"
            );
            // Sample-level agreement within one LSB on almost all samples.
            let off = window
                .iter()
                .zip(&native)
                .filter(|(x, y)| x.abs_diff(**y) > 1)
                .count();
            assert!(
                off < 8,
                "bin {bin}: {off} samples differ by more than 1 LSB"
            );
        }
    }

    #[test]
    fn integer_formats_are_normalised() {
        let mut conv = ReferenceConverter::new(44_100, 1);
        let out = convert_all(&mut conv, &[i16::MIN, 0, 16384, -16384]);
        assert_eq!(out, vec![0, 128, 192, 64]);
    }
}

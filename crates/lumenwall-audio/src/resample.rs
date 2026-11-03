//! Streaming polyphase resampler (Kaiser-windowed sinc), interleaved, any channel count.
//!
//! The ratio is kept as an exact fraction (`out/in` reduced by their gcd) so common rates
//! (44.1 k ↔ 48 k, 96 k, 22.05 k…) use exact phases; rates whose reduced numerator exceeds
//! [`MAX_PHASES`] round the phase to the nearest of `MAX_PHASES` (error far below 16-bit noise).
//! All buffers are allocated in [`Resampler::new`]; processing never allocates.

/// Upper bound on the phase table size.
pub const MAX_PHASES: usize = 1024;
/// Input frames processed per internal block.
const BLOCK_FRAMES: usize = 1024;
/// Output frames buffered before calling the sink.
const OUT_FRAMES: usize = 1024;

/// Quality preset of a [`Resampler`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// 16 taps: flat to ~10 kHz at 48 kHz, used for spectrum capture (only 0–5.4 kHz matter).
    Analysis,
    /// 32 taps: flat to ~17 kHz, used for scene sound playback.
    Playback,
}

impl Quality {
    fn taps(self) -> usize {
        match self {
            Quality::Analysis => 16,
            Quality::Playback => 32,
        }
    }

    fn beta(self) -> f64 {
        match self {
            Quality::Analysis => 7.0,
            Quality::Playback => 8.5,
        }
    }

    fn cutoff_margin(self) -> f64 {
        match self {
            Quality::Analysis => 0.92,
            Quality::Playback => 0.94,
        }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Modified Bessel function of the first kind, order 0 (series form).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..64 {
        term *= half / k as f64;
        let t2 = term * term;
        sum += t2;
        if t2 < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// `Σ h[k]·x[k·stride]` with four independent accumulators (instruction-level parallelism;
/// the kernel is short and the order of the sum is irrelevant at this precision).
#[inline]
fn dot_strided(h: &[f32], x: &[f32], stride: usize) -> f32 {
    let mut acc = [0.0f32; 4];
    let (quads, rest) = h.as_chunks::<4>();
    let mut i = 0;
    for q in quads {
        acc[0] += q[0] * x[i];
        acc[1] += q[1] * x[i + stride];
        acc[2] += q[2] * x[i + 2 * stride];
        acc[3] += q[3] * x[i + 3 * stride];
        i += 4 * stride;
    }
    for &hk in rest {
        acc[0] += hk * x[i];
        i += stride;
    }
    (acc[0] + acc[1]) + (acc[2] + acc[3])
}

/// Streaming sample-rate converter.
pub struct Resampler {
    channels: usize,
    taps: usize,
    up: usize,
    down: usize,
    phases: usize,
    table: Vec<f32>,
    hist: Vec<f32>,
    hist_frames: usize,
    start: usize,
    acc: usize,
    out: Vec<f32>,
    passthrough: bool,
}

impl Resampler {
    /// Creates a converter from `in_rate` to `out_rate` for `channels` interleaved channels.
    pub fn new(in_rate: u32, out_rate: u32, channels: usize, quality: Quality) -> Self {
        let channels = channels.max(1);
        let in_rate = u64::from(in_rate.max(1));
        let out_rate = u64::from(out_rate.max(1));
        let g = gcd(in_rate, out_rate);
        let up = (out_rate / g) as usize;
        let down = (in_rate / g) as usize;
        let passthrough = up == down;

        // Low-pass at the lower Nyquist frequency; longer kernel when decimating.
        let ratio = up as f64 / down as f64;
        let taps = if ratio < 1.0 {
            let t = (quality.taps() as f64 / ratio).ceil() as usize;
            t + (t & 1)
        } else {
            quality.taps()
        };
        let fc = 0.5 * ratio.min(1.0) * quality.cutoff_margin();
        let phases = up.min(MAX_PHASES);
        let beta = quality.beta();
        let i0_beta = bessel_i0(beta);
        let half = taps as f64 / 2.0;

        let mut table = vec![0.0f32; if passthrough { 0 } else { phases * taps }];
        if !passthrough {
            for p in 0..phases {
                let frac = p as f64 / phases as f64;
                let row = &mut table[p * taps..(p + 1) * taps];
                let mut coefs = [0.0f64; 512];
                let coefs = &mut coefs[..taps];
                let mut sum = 0.0;
                for (k, c) in coefs.iter_mut().enumerate() {
                    let t = frac + half - 1.0 - k as f64;
                    let x = 2.0 * fc * t;
                    let sinc = if x.abs() < 1e-12 {
                        1.0
                    } else {
                        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                    };
                    let r = (t / half).clamp(-1.0, 1.0);
                    let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;
                    *c = 2.0 * fc * sinc * w;
                    sum += *c;
                }
                // Unity DC gain per phase.
                for (dst, c) in row.iter_mut().zip(coefs.iter()) {
                    *dst = (c / sum) as f32;
                }
            }
        }

        let mut r = Self {
            channels,
            taps,
            up,
            down,
            phases,
            table,
            hist: vec![0.0; (taps + BLOCK_FRAMES) * channels],
            hist_frames: 0,
            start: 0,
            acc: 0,
            out: Vec::with_capacity(OUT_FRAMES * channels),
            passthrough,
        };
        r.reset();
        r
    }

    /// Number of interleaved channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Clears the history (next output starts a new signal).
    pub fn reset(&mut self) {
        // Pre-pad so that output 0 is centred on input frame 0.
        let pad = self.taps / 2 - 1;
        self.hist[..pad * self.channels].fill(0.0);
        self.hist_frames = pad;
        self.start = 0;
        self.acc = 0;
        self.out.clear();
    }

    /// Feeds interleaved input; `sink` receives interleaved output in blocks.
    pub fn process(&mut self, input: &[f32], sink: &mut impl FnMut(&[f32])) {
        let c = self.channels;
        if self.passthrough {
            for block in input.chunks(BLOCK_FRAMES * c) {
                sink(block);
            }
            return;
        }
        for block in input.chunks(BLOCK_FRAMES * c) {
            let at = self.hist_frames * c;
            self.hist[at..at + block.len()].copy_from_slice(block);
            self.hist_frames += block.len() / c;
            self.run(sink);
        }
    }

    /// Pushes trailing zeros so that every input frame has produced its outputs.
    pub fn flush(&mut self, sink: &mut impl FnMut(&[f32])) {
        if self.passthrough {
            return;
        }
        let c = self.channels;
        let pad = self.taps / 2 + 1;
        let at = self.hist_frames * c;
        self.hist[at..at + pad * c].fill(0.0);
        self.hist_frames += pad;
        self.run(sink);
        self.reset();
    }

    fn run(&mut self, sink: &mut impl FnMut(&[f32])) {
        let c = self.channels;
        let taps = self.taps;
        while self.start + taps <= self.hist_frames {
            let p = if self.phases == self.up {
                self.acc
            } else {
                self.acc * self.phases / self.up
            };
            let coefs = &self.table[p * taps..(p + 1) * taps];
            let base = self.start * c;
            let window = &self.hist[base..base + taps * c];
            for ch in 0..c {
                self.out.push(dot_strided(coefs, &window[ch..], c));
            }
            if self.out.len() >= OUT_FRAMES * c {
                sink(&self.out);
                self.out.clear();
            }
            self.acc += self.down;
            while self.acc >= self.up {
                self.acc -= self.up;
                self.start += 1;
            }
        }
        if !self.out.is_empty() {
            sink(&self.out);
            self.out.clear();
        }
        // Drop consumed history.
        if self.start > 0 {
            let keep_from = self.start.min(self.hist_frames);
            self.hist
                .copy_within(keep_from * c..self.hist_frames * c, 0);
            self.hist_frames -= keep_from;
            self.start -= keep_from;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, frames: usize, channels: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * channels);
        for n in 0..frames {
            let s = (2.0 * std::f64::consts::PI * freq * n as f64 / rate as f64).sin() as f32;
            for _ in 0..channels {
                v.push(s * 0.5);
            }
        }
        v
    }

    fn run_all(r: &mut Resampler, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            r.process(c, &mut |b: &[f32]| out.extend_from_slice(b));
        }
        out
    }

    #[test]
    fn output_length_follows_ratio() {
        let mut r = Resampler::new(48_000, 44_100, 1, Quality::Analysis);
        let out = run_all(&mut r, &vec![0.0; 48_000], 480);
        // Latency of taps/2 frames at most.
        assert!((out.len() as i64 - 44_100).abs() <= 40, "{}", out.len());
    }

    #[test]
    fn chunking_does_not_change_output() {
        let input = sine(48_000, 1000.0, 9_000, 2);
        let mut a = Resampler::new(48_000, 44_100, 2, Quality::Playback);
        let mut b = Resampler::new(48_000, 44_100, 2, Quality::Playback);
        let oa = run_all(&mut a, &input, 18_000);
        let ob = run_all(&mut b, &input, 2 * 37);
        assert_eq!(oa.len(), ob.len());
        for (x, y) in oa.iter().zip(&ob) {
            assert!((x - y).abs() < 1e-6);
        }
    }

    #[test]
    fn preserves_in_band_sine_amplitude_and_frequency() {
        for &(from, to) in &[
            (48_000u32, 44_100u32),
            (44_100, 48_000),
            (96_000, 44_100),
            (22_050, 48_000),
        ] {
            let f = 3000.0;
            let input = sine(from, f, from as usize / 2, 1);
            let mut r = Resampler::new(from, to, 1, Quality::Playback);
            let out = run_all(&mut r, &input, 1000);
            // Compare with the ideal resampled sine after the filter latency (output 0 = input 0).
            let skip = 200;
            let mut err = 0.0f64;
            for (n, &y) in out.iter().enumerate().skip(skip).take(out.len() - 2 * skip) {
                let ideal = 0.5 * (2.0 * std::f64::consts::PI * f * n as f64 / to as f64).sin();
                err = err.max((y as f64 - ideal).abs());
            }
            assert!(err < 2e-3, "{from}->{to}: max err {err}");
        }
    }

    #[test]
    fn passthrough_is_identity() {
        let input = sine(44_100, 440.0, 5000, 2);
        let mut r = Resampler::new(44_100, 44_100, 2, Quality::Analysis);
        assert_eq!(run_all(&mut r, &input, 333), input);
    }
}

//! Spectrum parity tests (spec 07 §13.3–§13.6, §21.2) with synthetic signals.
//!
//! Expected values come from a literal f64 transcription of the spec formula (naive DFT,
//! sequential writes into the 32/16 arrays) and from hand-computed constants.

use std::f64::consts::PI;

use crate::spectrum::{
    Bands, FRAGMENT_BYTES, Fragmenter, STALE_INPUT_SECONDS, SpectrumAnalyzer, SpectrumFeed,
    WINDOW_SIZE, WindowAccumulator,
};

/// Literal transcription of §13.4 in f64: returns (targets, per-band power).
fn reference(samples: &[f64; WINDOW_SIZE]) -> (Bands, [f64; 64]) {
    let mut t = Bands::ZERO;
    let mut powers = [0.0; 64];
    for (b, power) in powers.iter_mut().enumerate() {
        let k = 2 * b;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, &x) in samples.iter().enumerate() {
            let a = -2.0 * PI * (k * n % WINDOW_SIZE) as f64 / WINDOW_SIZE as f64;
            re += x * a.cos();
            im += x * a.sin();
        }
        let p = re * re + im * im;
        *power = p;
        let l = if p > 0.0 { 0.35 * p.log10() } else { 0.0 };
        let bf = b as f64;
        t.b64[b] = (l * (2.0 - (0.5 - bf / 63.0).exp())).min(1.0) as f32;
        t.b32[b / 2] = (l * (2.0 - (0.5 - bf / 31.0).exp())).min(1.0) as f32;
        t.b16[b / 4] = (l * (2.0 - (0.5 - bf / 15.0).exp())).min(1.0) as f32;
    }
    (t, powers)
}

fn decode(bytes: &[u8; WINDOW_SIZE]) -> [f64; WINDOW_SIZE] {
    let mut x = [0.0; WINDOW_SIZE];
    for (d, &b) in x.iter_mut().zip(bytes) {
        *d = (f64::from(b) - 128.0) / 128.0;
    }
    x
}

/// Compares implementation targets with the f64 reference. Bands whose power is zero in exact
/// arithmetic (reference power < 1e-6) only need to be non-positive in both: their exact value
/// depends on FFT rounding and is not reproducible across implementations.
fn assert_matches_reference(bytes: &[u8; WINDOW_SIZE]) {
    let mut a = SpectrumAnalyzer::new(30.0);
    a.analyze(bytes);
    let (expected, powers) = reference(&decode(bytes));
    let got = a.targets();
    let check = |name: &str, i: usize, b: usize, g: f32, e: f32| {
        if powers[b] < 1e-6 {
            assert!(
                g <= 0.0 && e <= 0.0,
                "{name}[{i}] (b={b}): near-zero power, got {g}, expected {e}"
            );
        } else {
            assert!(
                (g - e).abs() < 1e-4,
                "{name}[{i}] (b={b}): got {g}, expected {e}"
            );
        }
    };
    for b in 0..64 {
        check("b64", b, b, got.b64[b], expected.b64[b]);
    }
    for j in 0..32 {
        check("b32", j, 2 * j + 1, got.b32[j], expected.b32[j]);
    }
    for j in 0..16 {
        check("b16", j, 4 * j + 3, got.b16[j], expected.b16[j]);
    }
}

fn sine_bytes(bin: usize, amplitude: f64, phase: f64) -> [u8; WINDOW_SIZE] {
    let mut w = [0u8; WINDOW_SIZE];
    for (n, b) in w.iter_mut().enumerate() {
        let x = amplitude * (2.0 * PI * bin as f64 * n as f64 / WINDOW_SIZE as f64 + phase).sin();
        *b = ((x * 128.0).round().clamp(-128.0, 127.0) as i32 + 128) as u8;
    }
    w
}

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn noise_bytes(seed: u64, spread: u8) -> [u8; WINDOW_SIZE] {
    let mut rng = XorShift(seed);
    let mut w = [0u8; WINDOW_SIZE];
    for b in w.iter_mut() {
        let r = (rng.next() % (2 * u64::from(spread) + 1)) as i32 - i32::from(spread);
        *b = (128 + r) as u8;
    }
    w
}

fn all(b: &Bands) -> impl Iterator<Item = f32> + '_ {
    b.b16.iter().chain(&b.b32).chain(&b.b64).copied()
}

#[test]
fn silence_gives_zero() {
    let mut a = SpectrumAnalyzer::new(30.0);
    a.analyze(&[128; WINDOW_SIZE]);
    assert!(all(a.targets()).all(|v| v == 0.0));
    a.update_reference_step();
    assert!(all(a.values()).all(|v| v == 0.0));
}

#[test]
fn dc_level_hand_computed() {
    // x = 0.5 everywhere: X[0] = 512, p = 512², L = 0.35·log10(262144), weight 2 − e^0.5.
    let mut a = SpectrumAnalyzer::new(30.0);
    a.analyze(&[192; WINDOW_SIZE]);
    assert!(
        (a.targets().b64[0] - 0.666_196_2).abs() < 1e-5,
        "{}",
        a.targets().b64[0]
    );
}

#[test]
fn exact_sine_hand_computed() {
    // Float sine of amplitude 1/64 on bin k = 22 (b = 11): |X| = 8, p = 64, L = 0.35·log10(64).
    let mut x = [0.0f32; WINDOW_SIZE];
    for (n, v) in x.iter_mut().enumerate() {
        *v = ((2.0 * PI * 22.0 * n as f64 / WINDOW_SIZE as f64).sin() / 64.0) as f32;
    }
    let mut a = SpectrumAnalyzer::new(30.0);
    a.analyze_normalized(&x);
    let t = a.targets();
    assert!(
        (t.b64[11] - 0.389_045_76).abs() < 1e-5,
        "b64[11] = {}",
        t.b64[11]
    );
    // b = 11 = 2·5+1 → 32-band slot 5; 11 = 4·2+3 → 16-band slot 2.
    assert!(
        (t.b32[5] - 0.533_402_7).abs() < 1e-5,
        "b32[5] = {}",
        t.b32[5]
    );
    assert!(
        (t.b16[2] - 0.763_722_7).abs() < 1e-5,
        "b16[2] = {}",
        t.b16[2]
    );
    // Same signal on b = 10 instead: weight 2 − e^(0.5 − 10/63).
    for (n, v) in x.iter_mut().enumerate() {
        *v = ((2.0 * PI * 20.0 * n as f64 / WINDOW_SIZE as f64).sin() / 64.0) as f32;
    }
    a.analyze_normalized(&x);
    assert!((a.targets().b64[10] - 0.375_041_58).abs() < 1e-5);
}

#[test]
fn quantized_sines_peak_on_their_band_and_match_reference() {
    for &b in &[1usize, 5, 10, 21, 37, 50, 63] {
        let bytes = sine_bytes(2 * b, 0.3, 0.37);
        assert_matches_reference(&bytes);
        let mut a = SpectrumAnalyzer::new(30.0);
        a.analyze(&bytes);
        let t = a.targets();
        let peak = (0..64)
            .max_by(|&i, &j| t.b64[i].total_cmp(&t.b64[j]))
            .unwrap();
        assert_eq!(
            peak,
            b,
            "sine on bin {} must peak on band {b}: {:?}",
            2 * b,
            t.b64
        );
    }
}

#[test]
fn noise_matches_reference() {
    for seed in 1..6u64 {
        assert_matches_reference(&noise_bytes(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), 60));
    }
}

#[test]
fn saturation_clamps_to_one() {
    // Full-scale square wave of period 16 samples: strong odd harmonics on bins 64, 192, ...
    let mut w = [0u8; WINDOW_SIZE];
    for (n, b) in w.iter_mut().enumerate() {
        *b = if (n / 8) % 2 == 0 { 255 } else { 0 };
    }
    let mut a = SpectrumAnalyzer::new(30.0);
    a.analyze(&w);
    assert_eq!(a.targets().b64[32], 1.0);
    assert!(all(a.targets()).all(|v| v <= 1.0));
    // A loud broadband signal saturates some bands and never exceeds 1.
    a.analyze(&noise_bytes(7, 127));
    assert!(a.targets().b64.contains(&1.0));
    assert!(all(a.targets()).all(|v| v <= 1.0));
}

#[test]
fn weak_signal_gives_negative_values() {
    let w = noise_bytes(11, 1);
    assert_matches_reference(&w);
    let mut a = SpectrumAnalyzer::new(30.0);
    a.analyze(&w);
    assert!(a.targets().b64.iter().any(|&v| v < 0.0));
    assert!(a.targets().b16.iter().any(|&v| v < 0.0));
}

#[test]
fn bands_32_and_16_keep_last_written_bin() {
    // Energy only on b = 10 (k = 20): b32[5] is taken from b = 11, which is empty, so the
    // 32-band slot must not show the peak; b16[2] (b = 11) neither.
    let bytes = sine_bytes(20, 0.9, 0.1);
    let mut a = SpectrumAnalyzer::new(30.0);
    a.analyze(&bytes);
    let t = a.targets().clone();
    assert_eq!(t.b64[10], 1.0);
    assert!(t.b32[5] < 0.5, "b32[5] = {}", t.b32[5]);
    assert!(t.b16[2] < 0.5, "b16[2] = {}", t.b16[2]);
    // Energy on b = 11 shows in all three.
    let bytes = sine_bytes(22, 0.9, 0.1);
    a.analyze(&bytes);
    let t = a.targets();
    assert_eq!((t.b64[11], t.b32[5], t.b16[2]), (1.0, 1.0, 1.0));
}

#[test]
fn left_equals_right() {
    let feed_bytes = noise_bytes(3, 40);
    let mut a = SpectrumAnalyzer::new(30.0);
    let feed = a.feed().unwrap();
    feed.publish(&feed_bytes);
    for _ in 0..5 {
        a.update_reference_step();
    }
    assert_eq!(a.left16(), a.right16());
    assert_eq!(a.left32(), a.right32());
    assert_eq!(a.left64(), a.right64());
    assert!(a.left64().iter().any(|&v| v != 0.0));
}

#[test]
fn smoothing_slews_by_point_three_per_reference_frame() {
    let mut a = SpectrumAnalyzer::new(30.0);
    let feed = a.feed().unwrap();
    feed.publish(&[192; WINDOW_SIZE]); // b64[0] target ≈ 0.6662
    // Analysis happens after smoothing: the first update only computes targets.
    a.update_reference_step();
    assert_eq!(a.values().b64[0], 0.0);
    let target = a.targets().b64[0];
    a.update_reference_step();
    assert!((a.values().b64[0] - 0.3).abs() < 1e-6);
    a.update_reference_step();
    assert!((a.values().b64[0] - 0.6).abs() < 1e-6);
    a.update_reference_step();
    assert_eq!(a.values().b64[0], target, "snaps when within one step");
    // Back to silence, downward slope; negative targets are approached the same way.
    feed.publish(&[128; WINDOW_SIZE]);
    a.update_reference_step();
    a.update_reference_step();
    assert!((a.values().b64[0] - (target - 0.3)).abs() < 1e-6);
}

#[test]
fn smoothing_from_elapsed_time_matches_reference_rate() {
    let mut a = SpectrumAnalyzer::new(30.0);
    let mut b = SpectrumAnalyzer::new(30.0);
    let mut c = SpectrumAnalyzer::new(30.0);
    for an in [&mut a, &mut b, &mut c] {
        an.analyze(&[255; WINDOW_SIZE]);
    }
    a.update_reference_step();
    b.update(1.0 / 30.0);
    // Two frames at 60 fps = one reference frame.
    c.update(1.0 / 60.0);
    c.update(1.0 / 60.0);
    assert!((a.values().b64[0] - 0.3).abs() < 1e-6);
    assert!((b.values().b64[0] - 0.3).abs() < 1e-5);
    assert!((c.values().b64[0] - 0.3).abs() < 1e-5);
}

#[test]
fn stale_input_decays_to_silence() {
    let mut a = SpectrumAnalyzer::new(30.0);
    let feed = a.feed().unwrap();
    feed.publish(&noise_bytes(5, 80));
    a.update(0.0);
    assert!(a.targets().b64.iter().any(|&v| v != 0.0));
    let mut t = 0.0;
    while t < STALE_INPUT_SECONDS - 0.05 {
        a.update(1.0 / 30.0);
        t += 1.0 / 30.0;
    }
    assert!(a.targets().b64.iter().any(|&v| v != 0.0));
    for _ in 0..3 {
        a.update(1.0 / 30.0);
    }
    assert!(all(a.targets()).all(|v| v == 0.0));
}

#[test]
fn disabled_analyzer_stays_zero() {
    let mut a = SpectrumAnalyzer::disabled();
    assert!(a.feed().is_none());
    a.update(1.0);
    assert!(all(a.values()).all(|v| v == 0.0));
}

#[test]
fn feed_delivers_latest_window_once() {
    let feed = SpectrumFeed::new();
    let mut out = [0u8; WINDOW_SIZE];
    assert!(!feed.take(&mut out));
    feed.publish(&[1; WINDOW_SIZE]);
    feed.publish(&[2; WINDOW_SIZE]);
    assert!(feed.take(&mut out));
    assert_eq!(out[0], 2);
    assert!(!feed.take(&mut out));
}

fn ramp(len: usize, start: u8) -> Vec<u8> {
    (0..len).map(|i| start.wrapping_add(i as u8)).collect()
}

#[test]
fn window_accumulation_rules() {
    // Two half packets form one window.
    let mut acc = WindowAccumulator::new();
    assert!(acc.push(&ramp(512, 0)).is_none());
    let w = *acc.push(&ramp(512, 100)).unwrap();
    assert_eq!(&w[..512], &ramp(512, 0)[..]);
    assert_eq!(&w[512..], &ramp(512, 100)[..]);
    assert_eq!(acc.fill(), 0);

    // A packet holding the completion plus a full extra window → its last 1024 bytes.
    let p = ramp(3000, 7);
    let w = *acc.push(&p).unwrap();
    assert_eq!(&w[..], &p[3000 - 1024..]);
    assert_eq!(acc.fill(), 0);

    // Exactly two windows: still "at least one more full window" → last 1024 bytes.
    let p = ramp(2048, 3);
    assert_eq!(&acc.push(&p).unwrap()[..], &p[1024..]);

    // Completion without a full extra window → completed window; excess dropped.
    let p = ramp(1500, 9);
    assert_eq!(&acc.push(&p).unwrap()[..], &p[..1024]);
    assert_eq!(
        acc.fill(),
        0,
        "excess bytes are discarded, not carried over"
    );

    // Partially filled window then a packet with a full extra window after completion.
    assert!(acc.push(&ramp(1000, 50)).is_none());
    let p = ramp(24 + 1024, 200);
    assert_eq!(&acc.push(&p).unwrap()[..], &p[24..]);

    // Partially filled window then a packet just short of an extra window.
    let first = ramp(1000, 60);
    assert!(acc.push(&first).is_none());
    let p = ramp(24 + 1023, 1);
    let w = *acc.push(&p).unwrap();
    assert_eq!(&w[..1000], &first[..]);
    assert_eq!(&w[1000..], &p[..24]);
    assert_eq!(acc.fill(), 0);
}

#[test]
fn fragmenter_regroups_reference_fragments() {
    let mut f = Fragmenter::new();
    let data = ramp(FRAGMENT_BYTES * 3 + 100, 0);
    let mut frags: Vec<Vec<u8>> = Vec::new();
    for chunk in data.chunks(941) {
        f.push(chunk, |frag| frags.push(frag.to_vec()));
    }
    assert_eq!(frags.len(), 3);
    for (i, frag) in frags.iter().enumerate() {
        assert_eq!(
            &frag[..],
            &data[i * FRAGMENT_BYTES..(i + 1) * FRAGMENT_BYTES]
        );
    }
    // Through a capture sink, each fragment yields exactly one window: its last 1024 bytes.
    let feed = SpectrumFeed::new();
    let mut sink = crate::capture::CaptureSink::new(feed.clone());
    let mut out = [0u8; WINDOW_SIZE];
    for chunk in data.chunks(333) {
        sink.push_reference_stream(chunk);
    }
    assert!(feed.take(&mut out));
    assert_eq!(
        &out[..],
        &data[3 * FRAGMENT_BYTES - WINDOW_SIZE..3 * FRAGMENT_BYTES]
    );
}

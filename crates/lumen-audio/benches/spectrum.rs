//! Cost of the audio-reactive path: one analysis (u8 → FFT → 112 bands), one frame update
//! (feed + analysis + smoothing), and conversion of 100 ms of 48 kHz float stereo.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lumen_audio::convert::ReferenceConverter;
use lumen_audio::spectrum::{SpectrumAnalyzer, WINDOW_SIZE};

fn noise_window() -> [u8; WINDOW_SIZE] {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut w = [0u8; WINDOW_SIZE];
    for b in w.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = (state >> 56) as u8;
    }
    w
}

fn spectrum(c: &mut Criterion) {
    let window = noise_window();
    let mut analyzer = SpectrumAnalyzer::new(30.0);
    c.bench_function("analyze_window", |b| {
        b.iter(|| {
            analyzer.analyze(black_box(&window));
            black_box(analyzer.targets().b64[10])
        })
    });

    let mut analyzer = SpectrumAnalyzer::new(30.0);
    let feed = analyzer.feed().expect("enabled analyzer");
    c.bench_function("frame_update_with_new_window", |b| {
        b.iter(|| {
            feed.publish(black_box(&window));
            analyzer.update(black_box(1.0 / 60.0));
            black_box(analyzer.left64()[3])
        })
    });

    c.bench_function("frame_update_no_new_window", |b| {
        b.iter(|| {
            analyzer.update(black_box(1.0 / 60.0));
            black_box(analyzer.left64()[3])
        })
    });

    let input: Vec<f32> = (0..4800 * 2)
        .map(|i| ((i as f32) * 0.013).sin() * 0.5)
        .collect();
    let mut conv = ReferenceConverter::new(48_000, 2);
    c.bench_function("convert_100ms_48k_stereo_to_reference", |b| {
        b.iter(|| {
            let mut sum = 0u32;
            conv.process(black_box(&input[..]), &mut |bytes: &[u8]| {
                sum += u32::from(bytes[0])
            });
            black_box(sum)
        })
    });
}

criterion_group!(benches, spectrum);
criterion_main!(benches);

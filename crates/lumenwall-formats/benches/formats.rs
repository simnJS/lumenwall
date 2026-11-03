//! Benchmarks: opening a 5 000-entry `.pkg`, and parsing (with LZ4 decompression) a
//! 2048×2048 RGBA `.tex`. Inputs are synthetic.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use lumenwall_formats::pkg::{PkgArchive, build_pkg};
use lumenwall_formats::tex::{self, UploadOptions};

const ENTRIES: usize = 5_000;
const SIZE: u32 = 2048;

fn synthetic_pkg() -> (Vec<String>, Vec<u8>) {
    let names: Vec<String> = (0..ENTRIES)
        .map(|i| match i % 4 {
            0 => format!("materials/workshop/item_{i}/diffuse_{i}.tex"),
            1 => format!("shaders/effects/effect_{i}.frag"),
            2 => format!("models/layer_{i}.json"),
            _ => format!("particles/presets/system_{i}.json"),
        })
        .collect();
    let payload = vec![0xA5u8; 256];
    let bytes = build_pkg("PKGV0019", names.iter().map(|n| (n.as_str(), &payload[..])));
    (names, bytes)
}

fn bench_pkg(c: &mut Criterion) {
    let (names, bytes) = synthetic_pkg();
    let mut group = c.benchmark_group("pkg");

    let leaked: &'static [u8] = Box::leak(bytes.clone().into_boxed_slice());
    group.throughput(Throughput::Elements(ENTRIES as u64));
    group.bench_function("parse_and_index_5000_entries", |b| {
        b.iter(|| PkgArchive::from_static(black_box(leaked)).unwrap())
    });

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scene.pkg");
    std::fs::write(&path, &bytes).unwrap();
    group.bench_function("open_mmap_5000_entries", |b| {
        b.iter(|| PkgArchive::open(black_box(&path)).unwrap())
    });

    let pkg = PkgArchive::from_static(leaked).unwrap();
    group.bench_function("lookup_all_5000_entries", |b| {
        b.iter(|| {
            for name in &names {
                black_box(pkg.get(black_box(name)).unwrap());
            }
        })
    });
    group.finish();
}

fn tex_header(format: u32, mip_count: u32) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"TEXV0005\0TEXI0001\0");
    t.extend_from_slice(&format.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes()); // flags
    for _ in 0..4 {
        t.extend_from_slice(&SIZE.to_le_bytes());
    }
    t.extend_from_slice(&0u32.to_le_bytes()); // reserved
    t.extend_from_slice(b"TEXB0003\0");
    t.extend_from_slice(&1u32.to_le_bytes()); // images
    t.extend_from_slice(&u32::MAX.to_le_bytes()); // no embedded image
    t.extend_from_slice(&mip_count.to_le_bytes());
    t
}

fn push_mip(t: &mut Vec<u8>, size: u32, compression: u32, decompressed: usize, data: &[u8]) {
    t.extend_from_slice(&size.to_le_bytes());
    t.extend_from_slice(&size.to_le_bytes());
    t.extend_from_slice(&compression.to_le_bytes());
    t.extend_from_slice(&(decompressed as i32).to_le_bytes());
    t.extend_from_slice(&(data.len() as i32).to_le_bytes());
    t.extend_from_slice(data);
}

/// 2048×2048 RGBA8, LZ4-compressed: gradients plus noise, roughly like a photo.
fn synthetic_lz4_tex() -> (Vec<u8>, usize) {
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let mut state = 0x9E37_79B9u32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let noise = (state & 0x07) as u8;
            pixels.extend_from_slice(&[
                (x >> 3) as u8 ^ noise,
                (y >> 3) as u8,
                ((x + y) >> 4) as u8,
                0xFF,
            ]);
        }
    }
    let compressed = lz4_flex::block::compress(&pixels);
    let mut t = tex_header(0, 1);
    push_mip(&mut t, SIZE, 1, pixels.len(), &compressed);
    (t, pixels.len())
}

/// 2048×2048 DXT5 with a full raw mip chain (borrowed, never copied).
fn synthetic_dxt5_tex() -> Vec<u8> {
    let mut t = tex_header(4, 12);
    for level in 0..12 {
        let size = (SIZE >> level).max(1);
        let blocks = (size.div_ceil(4) * size.div_ceil(4)) as usize;
        let data = vec![0x5Au8; blocks * 16];
        push_mip(&mut t, size, 0, data.len(), &data);
    }
    t
}

fn bench_tex(c: &mut Criterion) {
    let (lz4, raw_len) = synthetic_lz4_tex();
    let mut group = c.benchmark_group("tex");
    group.throughput(Throughput::Bytes(raw_len as u64));
    group.bench_function("parse_lz4_rgba_2048", |b| {
        b.iter(|| tex::parse(black_box(&lz4)).unwrap())
    });
    group.bench_function("parse_and_prepare_lz4_rgba_2048", |b| {
        b.iter(|| {
            tex::parse(black_box(&lz4))
                .unwrap()
                .into_upload(&UploadOptions::default())
                .unwrap()
        })
    });
    let dxt5 = synthetic_dxt5_tex();
    group.throughput(Throughput::Bytes(dxt5.len() as u64));
    group.bench_function("parse_and_prepare_dxt5_2048_mips_zero_copy", |b| {
        b.iter(|| {
            tex::parse(black_box(&dxt5))
                .unwrap()
                .into_upload(&UploadOptions::default())
                .unwrap()
        })
    });
    group.finish();
}

criterion_group!(benches, bench_pkg, bench_tex);
criterion_main!(benches);

//! GPU tests on the reference video wallpaper (docs/TESTING.md, second target 3655057993).
//!
//! `LUMENWALL_GPU_TESTS=1 LUMENWALL_WE_WORKSHOP=/abs/path/431960 cargo test -p lumenwall-video
//! --features ffmpeg --release -- --ignored gpu`
//!
//! Without the content the tests print why and return. Without an adapter they fail only
//! when `LUMENWALL_GPU_TESTS=1` (an adapter is then guaranteed, docs/CI.md).
#![cfg(feature = "ffmpeg")]

use std::path::PathBuf;

use lumenwall_render::{ContextOptions, GpuContext};
use lumenwall_video::{DecodePath, HwDecode, VideoInput, VideoOptions, VideoPlayer};

fn video() -> Option<PathBuf> {
    let root = std::env::var_os("LUMENWALL_WE_WORKSHOP")?;
    let p = PathBuf::from(root).join("3655057993/loop_boykisser.mp4");
    p.is_file().then_some(p)
}

/// Render device; `WGPU_BACKEND=dx12|vulkan` restricts the backend (docs/CI.md).
fn context() -> Option<GpuContext> {
    match GpuContext::new_headless(&ContextOptions {
        backends: wgpu::Backends::from_env(),
        video_decode: true,
        ..ContextOptions::default()
    }) {
        Ok(c) => Some(c),
        Err(e) if std::env::var_os("LUMENWALL_GPU_TESTS").is_some_and(|v| v == "1") => {
            panic!("LUMENWALL_GPU_TESTS=1 but no adapter: {e}")
        }
        Err(e) => {
            eprintln!("skipped: no GPU adapter ({e})");
            None
        }
    }
}

/// RGBA pixels of the frame shown at media time `t`.
fn frame_at(
    ctx: &GpuContext,
    input: &VideoInput,
    hwdec: HwDecode,
    t: f64,
) -> (DecodePath, Vec<u8>) {
    let opts = VideoOptions {
        hwdec,
        max_fps: 0,
        queue_frames: 3,
    };
    let mut player = VideoPlayer::open_input(ctx, input, &opts, None).expect("open");
    player.update_at(ctx, t);
    let mut enc = ctx.device().create_command_encoder(&Default::default());
    let pre = player.encode(ctx, &mut enc);
    ctx.queue().submit(pre.into_iter().chain([enc.finish()]));
    let px = lumenwall_render::headless::read_texture_rgba8(ctx, player.output().texture())
        .expect("readback");
    assert_eq!(player.size(), (3840, 2160));
    (player.decode_path(), px)
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let sum: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    sum as f64 / a.len() as f64
}

fn channel_means(px: &[u8]) -> [f64; 3] {
    let n = (px.len() / 4) as f64;
    let mut m = [0.0; 3];
    for p in px.as_chunks::<4>().0 {
        for (acc, v) in m.iter_mut().zip(p) {
            *acc += f64::from(*v);
        }
    }
    m.map(|v| v / n)
}

#[test]
#[ignore = "GPU + Wallpaper Engine content: LUMENWALL_GPU_TESTS=1 LUMENWALL_WE_WORKSHOP=... cargo test -- --ignored gpu"]
fn gpu_decode_paths_agree() {
    let Some(path) = video() else {
        eprintln!("skipped: set LUMENWALL_WE_WORKSHOP (3655057993 not found)");
        return;
    };
    let Some(ctx) = context() else {
        return;
    };
    let file = VideoInput::File(path.clone());
    let (hw_path, hw) = frame_at(&ctx, &file, HwDecode::Auto, 6.0);
    let (sw_path, sw) = frame_at(&ctx, &file, HwDecode::No, 6.0);
    let memory = VideoInput::Memory {
        name: "embedded.mp4".into(),
        data: lumenwall_video::map_file(&path).expect("map"),
    };
    let (_, mem) = frame_at(&ctx, &memory, HwDecode::No, 6.0);
    eprintln!("hardware path: {hw_path}; software path: {sw_path}");
    assert_eq!(sw_path, DecodePath::Software);
    // H.264 decoding is bit-exact: the NV12 (hardware) and I420 (software) planes go through
    // two different upload/sampling paths of the converter and must give the same picture.
    let d = mean_abs_diff(&hw, &sw);
    assert!(d < 0.5, "hardware and software frames differ by {d}");
    assert_eq!(mean_abs_diff(&sw, &mem), 0.0, "in-memory input differs");
    // A snowy, blue-tinted picture: no green or magenta cast (wrong matrix or plane order).
    let [r, g, b] = channel_means(&sw);
    assert!(
        b >= r && b >= g - 2.0 && (r - g).abs() < 20.0,
        "means {r} {g} {b}"
    );
}

#[test]
#[ignore = "GPU + Wallpaper Engine content: LUMENWALL_GPU_TESTS=1 LUMENWALL_WE_WORKSHOP=... cargo test -- --ignored gpu"]
fn gpu_loop_continues_the_timeline() {
    let Some(path) = video() else {
        eprintln!("skipped: set LUMENWALL_WE_WORKSHOP (3655057993 not found)");
        return;
    };
    let Some(ctx) = context() else {
        return;
    };
    let opts = VideoOptions {
        hwdec: HwDecode::Auto,
        max_fps: 30,
        queue_frames: 3,
    };
    let mut player = VideoPlayer::open(&ctx, &path, &opts, None).expect("open");
    let duration = player.info().duration;
    assert!(duration > 12.0);
    // Last frames of loop 1 and first frames of loop 2, at the 30 fps cap.
    let mut t = duration - 0.2;
    while t < duration + 0.3 {
        player.update_at(&ctx, t);
        let mut enc = ctx.device().create_command_encoder(&Default::default());
        let pre = player.encode(&ctx, &mut enc);
        ctx.queue().submit(pre.into_iter().chain([enc.finish()]));
        t += 1.0 / 30.0;
    }
    ctx.wait_idle();
    let s = player.stats();
    assert!(s.loops >= 1, "{s:?}");
    // 60 fps video capped at 30: every other frame is dropped before any copy.
    assert!(s.skipped * 3 >= s.decoded, "{s:?}");
}

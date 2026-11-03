//! Decodes a video off screen, writes frames as PNG and prints decoding statistics.
//!
//! ```text
//! cargo run -p lumen-video --features ffmpeg --release --example video_png -- [options] <file>
//!   --hwdec auto|nvdec-copy|no   decoder choice (default auto)
//!   --times a,b,c                media times written as PNG, seconds (default 0)
//!   --out DIR                    output folder (default target/agent-video/shots)
//!   --bench SECONDS              decode SECONDS of video as fast as possible at --fps
//!   --fps N                      display cap for --bench (default 30)
//!   --low-power                  render on the integrated GPU
//! ```
//! `LUMEN_VIDEO_DECODERS=zerocopy,cuda,vulkan,vaapi` overrides the decoder order of `auto`.

use std::path::PathBuf;
use std::time::Instant;

use lumen_render::{ContextOptions, GpuContext};
use lumen_video::{HwDecode, VideoOptions, VideoPlayer};

fn frame(ctx: &GpuContext, player: &mut VideoPlayer) {
    let mut enc = ctx.device().create_command_encoder(&Default::default());
    let pre = player.encode(ctx, &mut enc);
    ctx.queue().submit(pre.into_iter().chain([enc.finish()]));
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut opts = VideoOptions {
        max_fps: 0,
        ..VideoOptions::default()
    };
    let mut times = vec![0.0f64];
    let mut out = PathBuf::from("target/agent-video/shots");
    let mut bench: Option<f64> = None;
    let mut fps = 30u32;
    let mut low_power = false;
    let mut file = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_default();
        match a.as_str() {
            "--hwdec" => {
                opts.hwdec = match val().as_str() {
                    "no" => HwDecode::No,
                    "nvdec-copy" => HwDecode::NvdecCopy,
                    _ => HwDecode::Auto,
                }
            }
            "--times" => {
                times = val()
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
            }
            "--out" => out = PathBuf::from(val()),
            "--bench" => bench = val().parse().ok(),
            "--fps" => fps = val().parse().unwrap_or(30),
            "--low-power" => low_power = true,
            other => file = Some(PathBuf::from(other)),
        }
    }
    let Some(file) = file else {
        eprintln!("usage: video_png [--hwdec auto|nvdec-copy|no] [--times a,b] [--out DIR] <file>");
        std::process::exit(2);
    };
    let ctx = GpuContext::new_headless(&ContextOptions {
        power_preference: if low_power {
            wgpu::PowerPreference::LowPower
        } else {
            wgpu::PowerPreference::None
        },
        video_decode: true,
        ..ContextOptions::default()
    })
    .expect("GPU");
    println!("adapter: {}", ctx.adapter().get_info().name);
    if let Some(seconds) = bench {
        opts.max_fps = fps;
        let mut player = VideoPlayer::open(&ctx, &file, &opts, None).expect("open");
        println!("decode path: {}", player.decode_path());
        let start = Instant::now();
        let mut t = 0.0;
        let step = 1.0 / f64::from(fps);
        let mut frames = 0u32;
        while t < seconds {
            player.update_at(&ctx, t);
            frame(&ctx, &mut player);
            frames += 1;
            t += step;
        }
        ctx.wait_idle();
        let el = start.elapsed().as_secs_f64();
        let s = player.stats();
        println!(
            "{frames} frames shown for {seconds:.1} s of video in {el:.2} s ({:.1}x real time): decoded {}, skipped {}, uploaded {}, upload {:.2} ms/frame, loops {}",
            seconds / el,
            s.decoded,
            s.skipped,
            s.shown,
            s.upload.as_secs_f64() * 1000.0 / s.shown.max(1) as f64,
            s.loops
        );
        return;
    }
    let mut player = VideoPlayer::open(&ctx, &file, &opts, None).expect("open");
    println!("decode path: {}", player.decode_path());
    println!("info: {:?}", player.info());
    std::fs::create_dir_all(&out).expect("output folder");
    let stem = file
        .file_stem()
        .map_or_else(|| "video".into(), |s| s.to_string_lossy().into_owned());
    let tag = match opts.hwdec {
        HwDecode::No => "sw",
        HwDecode::NvdecCopy => "nvdec",
        HwDecode::Auto => "auto",
    };
    for t in times {
        player.update_at(&ctx, t);
        frame(&ctx, &mut player);
        let rt = player.output();
        let px = lumen_render::headless::read_texture_rgba8(&ctx, rt.texture()).expect("readback");
        let (w, h) = rt.size();
        let path = out.join(format!("{stem}-{tag}-{t:06.2}.png"));
        lumen_render::headless::write_png(&path, w, h, &px).expect("png");
        println!("{} ({w}x{h})", path.display());
    }
    println!("stats: {:?}", player.stats());
}

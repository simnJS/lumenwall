//! Off-screen rendering of a wallpaper to PNG files (visual checks, tests, tools).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use glam::Vec2;
use lumenwall_render::compose::{ComposeSettings, Compositor};
use lumenwall_render::math::{Pointer, UvRange, pointer_disabled};
use lumenwall_render::stats::GpuTimer;
use lumenwall_render::{ClampMode, ContextOptions, GpuContext, HeadlessOutput, ScalingMode};

use crate::plan::DebugPlan;
use crate::scene::{FrameState, SceneOptions, SceneStats, ScreenState, build_scene};
use crate::shaders::default_provider;
use crate::wallpaper::Wallpaper;

/// Settings of [`render_headless`].
#[derive(Clone, Debug)]
pub struct HeadlessOptions {
    pub width: u32,
    pub height: u32,
    /// Frames simulated.
    pub frames: u32,
    /// Simulated frame rate (time step `1/fps`).
    pub fps: f32,
    /// `g_Time` of the first frame.
    pub start_time: f64,
    /// Frames written as PNG (indices into `0..frames`); empty = only the last one.
    pub save: Vec<u32>,
    pub out_dir: PathBuf,
    /// File name prefix (`<prefix>-<frame>.png`).
    pub prefix: String,
    pub scaling: ScalingMode,
    pub clamp: ClampMode,
    pub particles: bool,
    pub properties: BTreeMap<String, String>,
    /// Pointer in output pixels from the top-left corner; `None` = mouse disabled (centre).
    pub pointer: Option<Vec2>,
    pub debug: DebugPlan,
    /// Use a CPU adapter (lavapipe).
    pub software: bool,
    /// Video decoder choice (`--hwdec`).
    pub hwdec: lumenwall_video::HwDecode,
    /// Constant level fed to every audio spectrum band (audio-reactive checks; 0 = silence).
    pub audio_level: f32,
}

impl Default for HeadlessOptions {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            frames: 1,
            fps: 30.0,
            start_time: 1.0,
            save: Vec::new(),
            out_dir: PathBuf::from("."),
            prefix: "frame".into(),
            scaling: ScalingMode::Default,
            clamp: ClampMode::Clamp,
            particles: true,
            properties: BTreeMap::new(),
            pointer: None,
            debug: DebugPlan::default(),
            software: false,
            hwdec: lumenwall_video::HwDecode::Auto,
            audio_level: 0.0,
        }
    }
}

/// What a headless run produced.
#[derive(Clone, Debug, Default)]
pub struct HeadlessReport {
    pub adapter: String,
    pub scene_size: (u32, u32),
    pub stats: SceneStats,
    pub load_time: Duration,
    pub frames: u32,
    /// Mean CPU time of update + encode + submit.
    pub cpu_frame: Duration,
    /// Mean GPU time of a frame (timestamp queries), when available.
    pub gpu_frame: Option<Duration>,
    pub gpu_memory: u64,
    pub live_particles: usize,
    pub pngs: Vec<PathBuf>,
}

/// Renders `frames` frames of the wallpaper at `wallpaper` and writes the requested PNGs.
pub fn render_headless(
    wallpaper: &Path,
    assets: &Path,
    opts: &HeadlessOptions,
) -> Result<HeadlessReport, String> {
    let t0 = Instant::now();
    let mut wp = Wallpaper::load(wallpaper, assets, &opts.properties)?;
    wp.check_supported()?;
    let video = wp.kind() == lumenwall_scene::ProjectKind::Video;
    let ctx = GpuContext::new_headless(&ContextOptions {
        prefer_software: opts.software,
        profiling: true,
        video_decode: video && opts.hwdec == lumenwall_video::HwDecode::Auto,
        ..ContextOptions::default()
    })
    .map_err(|e| e.to_string())?;
    if video {
        return crate::video::render_headless_video(&ctx, &wp, opts, t0);
    }
    let mut shaders = default_provider();
    let mut rt = build_scene(
        &ctx,
        &mut wp.project,
        &wp.vfs,
        shaders.as_mut(),
        &SceneOptions {
            output_size: (opts.width, opts.height),
            clamp: opts.clamp,
            particles: opts.particles,
            parallax: true,
            debug: opts.debug.clone(),
        },
    )?;
    let load_time = t0.elapsed();
    let out = HeadlessOutput::new(&ctx, opts.width, opts.height);
    let mut compositor = Compositor::new(&ctx);
    let mut composer = compositor.create_output(
        &ctx,
        HeadlessOutput::FORMAT,
        rt.scene.targets().get(rt.full),
        ComposeSettings {
            scaling: opts.scaling,
            clamp: opts.clamp,
            flip_vertical: false,
        },
    );
    let mut timer = GpuTimer::new(&ctx);
    let mut report = HeadlessReport {
        adapter: ctx.adapter().get_info().name,
        scene_size: rt.logical,
        load_time,
        ..HeadlessReport::default()
    };
    std::fs::create_dir_all(&opts.out_dir)
        .map_err(|e| format!("{}: {e}", opts.out_dir.display()))?;
    let level = opts.audio_level;
    let (a16, a32, a64) = ([level; 16], [level; 32], [level; 64]);
    let mut uv = UvRange::FULL;
    let mut cpu = Duration::ZERO;
    let mut gpu_total = Duration::ZERO;
    let mut gpu_samples = 0u32;
    // Frames that differ from the previous one (the live engine skips the others).
    let mut changed_frames = 0u32;
    let dt = 1.0 / opts.fps.max(1.0);
    let last = opts.frames.saturating_sub(1);
    for i in 0..opts.frames {
        let time = opts.start_time + f64::from(i) * f64::from(dt);
        let cursor = opts.pointer.map_or(
            Vec2::new(opts.width as f32, opts.height as f32) * 0.5,
            |p| Vec2::new(p.x, opts.height as f32 - p.y),
        );
        let pointer = match opts.pointer {
            Some(_) => lumenwall_render::math::pointer_position(
                cursor,
                lumenwall_render::math::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: opts.width as f32,
                    height: opts.height as f32,
                },
                uv,
            ),
            None => pointer_disabled(uv),
        };
        let start = Instant::now();
        rt.update(
            &ctx,
            &mut wp.project,
            &FrameState {
                time,
                dt: if i == 0 { time as f32 } else { dt },
                daytime: 0.5,
                pointer: Pointer {
                    position: pointer.position,
                    normalized: pointer.normalized,
                },
                audio16: &a16,
                audio32: &a32,
                audio64: &a64,
                screen: ScreenState {
                    size: (opts.width, opts.height),
                    cursor,
                    left_down: false,
                },
            },
        );
        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumenwall.headless"),
            });
        if let Some(t) = &mut timer {
            t.begin(&mut enc);
        }
        changed_frames += u32::from(rt.scene.needs_redraw());
        rt.scene.encode(&ctx, &mut enc);
        uv = composer.update(&ctx, out.size(), rt.logical);
        compositor.encode(
            &mut enc,
            &composer,
            out.view(),
            [0.0, 0.0, opts.width as f32, opts.height as f32],
            Some(wgpu::Color::BLACK),
        );
        if let Some(t) = &mut timer {
            t.end(&mut enc);
        }
        ctx.queue().submit(std::iter::once(enc.finish()));
        if let Some(t) = &mut timer {
            t.after_submit();
        }
        cpu += start.elapsed();
        let save = if opts.save.is_empty() {
            i == last
        } else {
            opts.save.contains(&i)
        };
        if save {
            let path = opts.out_dir.join(format!("{}-{i:04}.png", opts.prefix));
            out.write_png(&ctx, &path).map_err(|e| e.to_string())?;
            report.pngs.push(path);
        }
        if let Some(t) = &mut timer {
            ctx.wait_idle();
            if let Some(g) = t.poll(&ctx) {
                gpu_total += g;
                gpu_samples += 1;
            }
        }
    }
    report.frames = opts.frames;
    report.cpu_frame = cpu / opts.frames.max(1);
    report.gpu_frame = (gpu_samples > 0).then(|| gpu_total / gpu_samples);
    report.gpu_memory = ctx.memory().bytes();
    report.live_particles = rt.live_particles();
    report.stats = rt.stats;
    tracing::debug!(
        "{changed_frames}/{} frame(s) needed a redraw (animated: {})",
        opts.frames,
        rt.animated
    );
    if let Some((s, tick)) = rt.script_stats() {
        tracing::info!(
            "scripts: {} loaded, {} active, {} per-frame update(s), {} timer(s), {:.1} µs per frame, {:.0} KiB JS heap; {changed_frames}/{} frame(s) needed a redraw",
            s.scripts,
            s.active,
            s.per_frame,
            s.timers,
            tick.as_secs_f64() * 1e6,
            s.memory_used as f64 / 1024.0,
            opts.frames,
        );
    }
    rt.shutdown_scripts(&mut wp.project);
    Ok(report)
}

//! Video wallpapers (spec 07 §14.1): one `lumen-video` player per target, its sound track
//! through the scene sound player, pause and wake-ups driven by the engine loop.
//!
//! The player decodes on its own thread into a small queue; the loop sleeps until the next
//! queued frame is due (`Wait::Until`) or until the decoder signals a frame landing in an
//! empty queue (engine waker). While every output of a video is paused, the clock stops,
//! the decoder blocks on its full queue and nothing wakes up.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lumen_audio::playback::ObjectId;
use lumen_render::compose::{ComposeSettings, Compositor};
use lumen_render::stats::GpuTimer;
use lumen_render::{GpuContext, HeadlessOutput};
use lumen_video::{Notify, VideoOptions, VideoPlayer};

use crate::audio::Audio;
use crate::headless::{HeadlessOptions, HeadlessReport};
use crate::plan::EnginePlan;
use crate::wallpaper::Wallpaper;

/// Decoded frames kept ahead of display (one 4K NV12 frame = 12 MiB of system memory on the
/// copy paths, a reference to a GPU image on the zero-copy path).
const QUEUE_FRAMES: usize = 3;

/// A running video wallpaper.
pub(crate) struct VideoSlot {
    pub player: VideoPlayer,
    path: PathBuf,
    sound: Option<ObjectId>,
    /// Loop iteration of the last update (outputs sharing the video update it once).
    pub updated_at: u64,
    /// Incremented each time a new frame is converted; an output presents when its last
    /// presented serial differs.
    pub serial: u64,
    paused: bool,
}

impl VideoSlot {
    /// Opens the video of `wp` (no sound: see [`VideoSlot::attach_sound`]).
    pub fn open(
        ctx: &GpuContext,
        wp: &Wallpaper,
        plan: &EnginePlan,
        notify: Option<Notify>,
    ) -> Result<Self, String> {
        let path = wp.video_path()?;
        let player = VideoPlayer::open(
            ctx,
            &path,
            &VideoOptions {
                hwdec: plan.hwdec,
                max_fps: plan.fps,
                queue_frames: QUEUE_FRAMES,
            },
            notify,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            player,
            path,
            sound: None,
            updated_at: u64::MAX,
            serial: 0,
            paused: false,
        })
    }

    /// Starts the sound track in `audio` (a new player after an audio change), unless the
    /// plan is silent or the video has none. Keeps the pause state.
    pub fn attach_sound(&mut self, audio: &mut Audio, plan: &EnginePlan) {
        self.sound = if self.player.info().has_audio && !plan.audio.silent {
            add_sound(audio, &self.path, plan.audio.volume)
        } else {
            None
        };
        if self.paused
            && let Some(id) = self.sound
        {
            audio.set_object_playing(id, false);
        }
    }

    /// Global volume changed: the track carries the `(v/128)²` part of the cubic curve.
    pub fn set_volume(&self, audio: &Audio, volume: u8) {
        if let Some(id) = self.sound {
            let v = f32::from(volume.min(128)) / 128.0;
            audio.set_object_volume(id, v * v);
        }
    }

    /// Pauses or resumes the picture and the sound together.
    pub fn set_paused(&mut self, paused: bool, audio: &Audio) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        self.player.set_paused(paused, Instant::now());
        if let Some(id) = self.sound {
            audio.set_object_playing(id, !paused);
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Logs the decoding path and counters (end of run).
    pub fn log_stats(&self) {
        let s = self.player.stats();
        tracing::info!(
            "video: {}; {} frames decoded, {} skipped (above the fps cap), {} shown, {} late, {} loop(s), upload {:.2} ms/frame, GPU {:.1} MiB",
            self.player.decode_path(),
            s.decoded,
            s.skipped,
            s.shown,
            s.late,
            s.loops,
            s.upload.as_secs_f64() * 1000.0 / s.shown.max(1) as f64,
            self.player.gpu_bytes() as f64 / 1048576.0,
        );
    }
}

fn add_sound(audio: &mut Audio, path: &Path, volume: u8) -> Option<ObjectId> {
    let name = path
        .file_name()
        .map_or_else(|| "video.mp4".into(), |n| n.to_string_lossy().into_owned());
    match lumen_video::map_file(path) {
        Ok(data) => audio.add_video_sound(&name, data, volume),
        Err(e) => {
            tracing::warn!("video sound track: {}: {e}", path.display());
            None
        }
    }
}

/// [`crate::render_headless`] for video wallpapers: frame `i` shows media time
/// `start_time + i / fps`.
pub(crate) fn render_headless_video(
    ctx: &GpuContext,
    wp: &Wallpaper,
    opts: &HeadlessOptions,
    t0: Instant,
) -> Result<HeadlessReport, String> {
    let path = wp.video_path()?;
    let mut player = VideoPlayer::open(
        ctx,
        &path,
        &VideoOptions {
            hwdec: opts.hwdec,
            max_fps: 0,
            queue_frames: QUEUE_FRAMES,
        },
        None,
    )
    .map_err(|e| e.to_string())?;
    let load_time = t0.elapsed();
    let out = HeadlessOutput::new(ctx, opts.width, opts.height);
    let mut compositor = Compositor::new(ctx);
    let mut composer = compositor.create_output(
        ctx,
        HeadlessOutput::FORMAT,
        player.output(),
        ComposeSettings {
            scaling: opts.scaling,
            clamp: opts.clamp,
            flip_vertical: false,
        },
    );
    let mut generation = player.generation();
    let mut timer = GpuTimer::new(ctx);
    let mut report = HeadlessReport {
        adapter: ctx.adapter().get_info().name,
        scene_size: player.size(),
        load_time,
        ..HeadlessReport::default()
    };
    std::fs::create_dir_all(&opts.out_dir)
        .map_err(|e| format!("{}: {e}", opts.out_dir.display()))?;
    let dt = 1.0 / f64::from(opts.fps.max(1.0));
    let last = opts.frames.saturating_sub(1);
    let (mut cpu, mut gpu_total, mut gpu_samples) = (Duration::ZERO, Duration::ZERO, 0u32);
    for i in 0..opts.frames {
        let start = Instant::now();
        player.update_at(ctx, opts.start_time + f64::from(i) * dt);
        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumen.headless.video"),
            });
        if let Some(t) = &mut timer {
            t.begin(&mut enc);
        }
        let pre = player.encode(ctx, &mut enc);
        if generation != player.generation() {
            generation = player.generation();
            let settings = composer.settings();
            compositor.rebind(ctx, &mut composer, player.output(), settings);
        }
        composer.update(ctx, out.size(), player.size());
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
        ctx.queue()
            .submit(pre.into_iter().chain(std::iter::once(enc.finish())));
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
            let png = opts.out_dir.join(format!("{}-{i:04}.png", opts.prefix));
            out.write_png(ctx, &png).map_err(|e| e.to_string())?;
            report.pngs.push(png);
        }
        if let Some(t) = &mut timer {
            ctx.wait_idle();
            if let Some(g) = t.poll(ctx) {
                gpu_total += g;
                gpu_samples += 1;
            }
        }
    }
    tracing::info!("video: {}", player.decode_path());
    report.frames = opts.frames;
    report.cpu_frame = cpu / opts.frames.max(1);
    report.gpu_frame = (gpu_samples > 0).then(|| gpu_total / gpu_samples);
    report.gpu_memory = ctx.memory().bytes();
    Ok(report)
}

/// Engine waker as a decoder notification.
pub(crate) fn notify_from(waker: crate::EngineWaker) -> Notify {
    Arc::new(move || waker.wake())
}

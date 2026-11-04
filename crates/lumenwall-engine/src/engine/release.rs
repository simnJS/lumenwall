//! GPU release after a long pause ([`EnginePlan::gpu_release`](crate::EnginePlan)).
//!
//! A paused wallpaper renders nothing, but the GPU device it keeps costs on its own: the
//! NVIDIA driver runs threads for every device (≈ 115 wake-ups/s in total, measured), and the
//! textures, render targets, swap chains and decoded video frames stay in VRAM. Once every
//! output has been paused for the configured delay, the engine drops the renderers (scenes,
//! videos, web pages), the output swap chains, the device and the instance; it keeps the
//! platform surfaces (the windows, which report pause and resume), the parsed wallpapers
//! (memory-mapped `.pkg`, scene model, property values), the audio player and the clock.
//!
//! When an output resumes (or the user unpauses), a new device is created and every target is
//! rebuilt like at start-up, from the translated-shader cache and the on-disk pipeline cache.
//! What a rebuild resets: particle systems start over (with their own `starttime` warm-up),
//! scripts run `init` again (their in-memory `localStorage` is lost), videos restart from the
//! beginning with their sound track, web pages reload.
//!
//! The same release and rebuild serve a video applied to a device created without Vulkan
//! Video queues (hot switch from a scene, [`Local::recreate_for_video`]): the device is
//! recreated with them right away, the instance kept, so the video decodes with zero copy.

use std::time::Instant;

use lumenwall_platform::{Event, OutputId, Wait, WallpaperSurface};
use lumenwall_render::GpuInstance;
use lumenwall_render::compose::Compositor;
use lumenwall_render::stats::GpuTimer;

use super::{Local, Running, Stats, note_known, target_for};
use crate::audio::Audio;
use crate::monitor::Change;
use crate::plan::Stopped;
use lumenwall_platform::Config;
use lumenwall_platform::mouse::PointerTracker;

/// Why the GPU is recreated.
#[derive(Clone, Copy)]
enum Restore {
    /// An output shows a wallpaper again (or a control message needs a frame).
    Resume,
    /// A video was applied to a device without decode queues ([`Local::recreate_for_video`]).
    VideoDevice,
}

/// What a released engine keeps: everything but the GPU.
pub(super) struct Dormant {
    surfaces: Box<dyn WallpaperSurface>,
    platform: Config,
    pointer: PointerTracker,
    audio: Audio,
    clock: Instant,
    iteration: u64,
    stats: Stats,
    screenshot: Option<(std::path::PathBuf, u32)>,
    rendered: u64,
    /// When the GPU was released.
    since: Instant,
}

impl Dormant {
    pub(super) fn surfaces(&self) -> &dyn WallpaperSurface {
        self.surfaces.as_ref()
    }

    pub(super) fn surfaces_mut(&mut self) -> &mut dyn WallpaperSurface {
        self.surfaces.as_mut()
    }

    pub(super) fn audio(&mut self) -> &mut Audio {
        &mut self.audio
    }
}

/// No output needs the GPU: each one is paused by the platform, or held by a user pause with
/// nothing left to show. An engine without any output also qualifies.
pub(super) fn gpu_unused(r: &Running) -> bool {
    r.outputs.iter().all(|o| o.paused || (o.held && !o.dirty))
}

impl Local {
    /// When the GPU of `r` is due for release, if it is unused and release is enabled.
    pub(super) fn release_deadline(&self, r: &Running) -> Option<Instant> {
        Some(r.idle_since? + self.plan.gpu_release?)
    }

    /// Before each wait of the loop: starts or stops the idle clock, releases the GPU when
    /// due. Returns true when `running` was released into `self.dormant`.
    pub(super) fn check_release(&mut self, running: &mut Option<Running>) -> bool {
        let Some(r) = running.as_mut() else {
            return false;
        };
        let now = Instant::now();
        if !gpu_unused(r) {
            r.idle_since = None;
            return false;
        }
        r.idle_since.get_or_insert(now);
        if self.release_deadline(r).is_none_or(|d| now < d) {
            return false;
        }
        let r = running.take().expect("checked above");
        self.release_gpu(r);
        true
    }

    /// Drops every GPU object of `r`; keeps the platform, the audio and the wallpapers.
    fn release_gpu(&mut self, r: Running) {
        let t0 = Instant::now();
        let (vram, _) = self.release_running(r, false);
        super::trim_heap();
        super::trim_resident();
        tracing::info!(
            "every output paused for {} s: GPU released ({:.1} MiB) in {:.0} ms",
            self.plan.gpu_release.map_or(0, |d| d.as_secs()),
            vram as f64 / 1048576.0,
            t0.elapsed().as_secs_f64() * 1000.0
        );
        self.publish(None, Change::State);
    }

    /// A video was applied to a device created without Vulkan Video queues (a scene was shown
    /// until now): its frames would go through a hardware decoder and a copy (measured with
    /// the 4K reference video: 8 % CPU, 1480 wake-ups/s, +330 MiB RSS and +460 MiB VRAM
    /// against zero copy). The device is released and recreated with the decode queues by the
    /// GPU release mechanism (the Vulkan instance is kept), and every target rebuilt (≈ 0.2 s).
    /// When no output can show a wallpaper yet, the engine stays released and the loop
    /// restores it later.
    pub(super) fn recreate_for_video(&mut self, running: &mut Option<Running>) {
        let Some(r) = running.take() else {
            return;
        };
        let (_, instance) = self.release_running(r, true);
        match self.restore(Restore::VideoDevice, instance) {
            Ok(r) => *running = Some(r),
            Err(e) => tracing::debug!("render device not recreated yet: {e}"),
        }
    }

    /// Whether one of `r`'s outputs is about to show a new video of `plan` (`new_videos`:
    /// target indices) on a Vulkan device created without decode queues, while zero copy is
    /// allowed: the video would decode through a copy unless the device is recreated.
    pub(super) fn wants_video_device(
        r: &Running,
        plan: &crate::plan::EnginePlan,
        mut new_videos: impl Iterator<Item = usize>,
    ) -> bool {
        if plan.hwdec != crate::plan::HwDecode::Auto
            || r.ctx.video_decode_requested()
            || r.ctx.adapter().get_info().backend != wgpu::Backend::Vulkan
            || !lumenwall_video::select::zero_copy_allowed()
        {
            return false;
        }
        new_videos.any(|i| {
            r.outputs
                .iter()
                .any(|o| target_for(plan, &o.name) == Some(i))
        })
    }

    /// Drops every GPU object of `r` into [`Self::dormant`]; returns the VRAM it held, and the
    /// instance when `keep_instance`.
    fn release_running(
        &mut self,
        mut r: Running,
        keep_instance: bool,
    ) -> (u64, Option<GpuInstance>) {
        let video_bytes: u64 = r
            .videos
            .iter()
            .flatten()
            .map(|v| v.player.gpu_bytes())
            .sum();
        let vram = r.ctx.memory().bytes() + video_bytes;
        for (slot, wp) in r.scenes.iter_mut().zip(&mut self.wallpapers) {
            if let Some(slot) = slot {
                slot.runtime.shutdown_scripts(&mut wp.project);
            }
        }
        for v in r.videos.iter().flatten() {
            v.log_stats();
        }
        for w in r.webs.iter().flatten() {
            w.log_stats();
        }
        r.ctx.wait_idle();
        // Video sound tracks restart with their pictures: a sound player without them.
        if r.videos.iter().any(Option::is_some) {
            r.videos.iter_mut().for_each(|v| *v = None);
            self.rebuild_audio(&mut r);
        }
        let Running {
            outputs,
            scenes,
            videos,
            webs,
            compositor,
            timer,
            ctx,
            surfaces,
            platform,
            pointer,
            audio,
            clock,
            iteration,
            stats,
            screenshot,
            rendered,
            ..
        } = r;
        // Swap chains first, then what renders into them, then the device.
        drop(outputs);
        drop((scenes, videos, webs, compositor, timer));
        let instance = keep_instance.then(|| ctx.into_instance());
        self.dormant = Some(Dormant {
            surfaces,
            platform,
            pointer,
            audio,
            clock,
            iteration,
            stats,
            screenshot,
            rendered,
            since: Instant::now(),
        });
        self.monitor.set_gpu_memory(0);
        (vram, instance)
    }

    /// Whether an output shows a wallpaper again (resumed by the platform and not held by
    /// the user).
    pub(super) fn dormant_should_wake(&self) -> bool {
        !self.user_paused && self.dormant_visible()
    }

    /// Whether the platform shows an output of a released engine (it may still be held by a
    /// user pause).
    pub(super) fn dormant_visible(&self) -> bool {
        let Some(d) = &self.dormant else {
            return false;
        };
        d.surfaces.outputs().iter().any(|o| {
            target_for(&self.plan, &o.name).is_some() && d.surfaces.pause_reasons(o.id).is_empty()
        })
    }

    /// Blocks on the platform while released and handles its events without the GPU.
    pub(super) fn wait_dormant(&mut self) -> Result<Option<Stopped>, String> {
        let Self { dormant, known, .. } = self;
        let Some(d) = dormant.as_mut() else {
            return Ok(None);
        };
        d.surfaces.wait(Wait::Block).map_err(|e| e.to_string())?;
        d.stats.wakeups += 1;
        let mut change = None;
        while let Some(ev) = d.surfaces.next_event() {
            match ev {
                Event::CloseRequested => return Ok(Some(Stopped::Driver)),
                Event::OutputAdded(id)
                | Event::Resized { output: id, .. }
                | Event::ScaleChanged { output: id, .. }
                | Event::Moved { output: id, .. } => {
                    note_known(known, d.surfaces.output(id));
                    change = Some(Change::Outputs);
                }
                Event::OutputRemoved(id) => {
                    d.pointer.remove_output(id);
                    known.retain(|k| !(k.live && k.info.id == id));
                    change = Some(Change::Outputs);
                }
                Event::Paused { .. } | Event::Resumed(_) => {
                    change.get_or_insert(Change::State);
                }
                Event::Pointer(p) => {
                    let h = d
                        .surfaces
                        .output(p.output)
                        .map_or(0.0, |o| f64::from(o.physical_size.1));
                    d.pointer.handle(&p, h);
                }
                Event::Frame(_) | Event::Wake => {}
            }
        }
        if let Some(change) = change {
            self.publish(None, change);
        }
        Ok(None)
    }

    /// Recreates the device and every renderer (see the module documentation). On error the
    /// engine stays released.
    pub(super) fn restore_gpu(&mut self) -> Result<Running, String> {
        self.restore(Restore::Resume, None)
    }

    fn restore(&mut self, why: Restore, instance: Option<GpuInstance>) -> Result<Running, String> {
        let t0 = Instant::now();
        let d = self.dormant.as_ref().ok_or("GPU not released")?;
        let first = first_output(self, d).ok_or("no output to show")?;
        let (first_surface, ctx) = self.open_gpu(d.surfaces.as_ref(), first, instance)?;
        let device_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let Dormant {
            surfaces,
            platform,
            pointer,
            audio,
            clock,
            iteration,
            stats,
            screenshot,
            rendered,
            since,
        } = self.dormant.take().expect("checked above");
        let timer = if stats.enabled {
            GpuTimer::new(&ctx)
        } else {
            None
        };
        let n = self.wallpapers.len();
        let mut r = Running {
            outputs: Vec::new(),
            scenes: (0..n).map(|_| None).collect(),
            videos: (0..n).map(|_| None).collect(),
            webs: (0..n).map(|_| None).collect(),
            compositor: Compositor::new(&ctx),
            timer_tried: stats.enabled,
            timer,
            ctx,
            surfaces,
            platform,
            pointer,
            audio,
            clock,
            iteration,
            stats,
            screenshot,
            rendered,
            idle_since: None,
            first_frame_due: Some((t0, "the GPU restore began")),
            trim_due: None,
        };
        let mut first_surface = Some((first, first_surface));
        let ids: Vec<OutputId> = r.surfaces.outputs().iter().map(|o| o.id).collect();
        for id in ids {
            if let Err(e) = self.add_output(&mut r, id, &mut first_surface) {
                tracing::error!("{e}");
            }
        }
        // Video sound tracks were attached afresh by `add_output`, in step with their videos.
        if !self.user_paused && r.outputs.iter().any(|o| !o.paused) {
            r.audio.resume();
        }
        match why {
            Restore::Resume => tracing::info!(
                "GPU restored after {:.0} s released: device {device_ms:.0} ms, wallpapers ready in {:.0} ms",
                since.elapsed().as_secs_f64(),
                t0.elapsed().as_secs_f64() * 1000.0
            ),
            Restore::VideoDevice => tracing::info!(
                "render device recreated with video decode queues: device {device_ms:.0} ms, wallpapers ready in {:.0} ms",
                t0.elapsed().as_secs_f64() * 1000.0
            ),
        }
        self.publish(Some(&r), Change::State);
        Ok(r)
    }

    /// Restores the GPU for a control message that needs it (a new plan).
    pub(super) fn wake_for_control(&mut self, running: &mut Option<Running>) -> Result<(), String> {
        if running.is_some() || self.dormant.is_none() {
            return Ok(());
        }
        *running = Some(self.restore_gpu()?);
        Ok(())
    }

    /// Releases the platform of a released engine (stop, or a plan without wallpaper).
    pub(super) fn close_dormant(&mut self) {
        if let Some(d) = self.dormant.take() {
            super::log_summary(&d.stats);
            self.waker.detach();
            drop(d);
            for k in &mut self.known {
                k.live = false;
            }
        }
    }
}

/// The output whose surface picks the adapter: one that shows a wallpaper, unpaused first.
fn first_output(engine: &Local, d: &Dormant) -> Option<OutputId> {
    let outputs = d.surfaces.outputs();
    let shown = |o: &&lumenwall_platform::OutputInfo| target_for(&engine.plan, &o.name).is_some();
    outputs
        .iter()
        .filter(shown)
        .find(|o| d.surfaces.pause_reasons(o.id).is_empty())
        .or_else(|| outputs.iter().find(shown))
        .map(|o| o.id)
}

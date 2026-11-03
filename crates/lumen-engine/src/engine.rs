//! The live engine: platform surfaces, one GPU device, one scene per target, the event loop.
//!
//! The loop blocks in the platform's `wait` (no polling). A frame is requested only while the
//! scene changes over time (time-driven shaders, particles, animated textures, spectrum,
//! accumulation settling) or after an input/property/resize event; a static scene renders once
//! and then sleeps. Paused outputs (fullscreen, covered, locked) receive no frame at all.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glam::Vec2;
use lumen_platform::mouse::PointerTracker;
use lumen_platform::{
    BackendChoice, Config, Event, FullscreenPolicy, Mode, OutputId, OutputSelection, PauseScope,
    PointerMode, Wait, WallpaperSurface, WindowGeometry,
};
use lumen_render::compose::{ComposeSettings, Compositor, OutputComposer};
use lumen_render::math::{UvRange, Viewport, pointer_disabled, pointer_position};
use lumen_render::stats::GpuTimer;
use lumen_render::{ContextOptions, GpuContext, GpuInstance, OutputSurface};

use crate::audio::Audio;
use crate::plan::{EngineControl, EnginePlan, OutputMode, Stopped};
use crate::scene::{FrameState, SceneOptions, SceneRuntime, ScreenState, build_scene};
use crate::shaders::{ShaderProvider, default_provider};
use crate::video::{VideoSlot, notify_from};
use crate::wallpaper::Wallpaper;
use crate::{WallpaperResolver, cache_dir};

/// Wakes the engine loop from any thread; usable before the platform exists.
#[derive(Clone, Default)]
pub struct EngineWaker {
    inner: Arc<WakerInner>,
}

#[derive(Default)]
struct WakerInner {
    platform: Mutex<Option<lumen_platform::Waker>>,
    pending: AtomicBool,
}

impl EngineWaker {
    pub fn wake(&self) {
        let guard = self
            .inner
            .platform
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*guard {
            Some(w) => w.wake(),
            None => self.inner.pending.store(true, Ordering::Release),
        }
    }

    fn attach(&self, w: lumen_platform::Waker) {
        let mut guard = self
            .inner
            .platform
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.pending.swap(false, Ordering::AcqRel) {
            w.wake();
        }
        *guard = Some(w);
    }
}

struct Output {
    id: OutputId,
    /// Index into `Running::scenes`.
    target: usize,
    surface: OutputSurface,
    composer: OutputComposer,
    size: (u32, u32),
    uv: UvRange,
    paused: bool,
    /// The surface must be presented again (resize, first frame).
    dirty: bool,
    frame_requested: bool,
    /// Video wallpapers: serial of the last presented frame, output image generation bound.
    shown_serial: u64,
    video_generation: u64,
}

struct SceneSlot {
    runtime: SceneRuntime,
    /// Loop iteration of the last update/encode (shared by span outputs).
    updated_at: u64,
    last_time: f64,
}

/// Debug counters (enabled with `LUMEN_STATS=1`).
#[derive(Default)]
struct Stats {
    enabled: bool,
    since: Option<Instant>,
    frames: u64,
    cpu: Duration,
    gpu: Duration,
    gpu_samples: u32,
    total_frames: u64,
    total_cpu: Duration,
    total_gpu: Duration,
    total_gpu_samples: u64,
    started: Option<Instant>,
    wakeups: u64,
}

struct Running {
    // Declaration order is drop order: GPU surfaces go before the native windows.
    outputs: Vec<Output>,
    scenes: Vec<Option<SceneSlot>>,
    videos: Vec<Option<VideoSlot>>,
    compositor: Compositor,
    timer: Option<GpuTimer>,
    ctx: GpuContext,
    surfaces: Box<dyn WallpaperSurface>,
    pointer: PointerTracker,
    audio: Audio,
    shaders: Box<dyn ShaderProvider>,
    clock: Instant,
    iteration: u64,
    stats: Stats,
    screenshot: Option<(PathBuf, u32)>,
    rendered: u64,
}

/// The wallpaper engine: load, start, run.
pub struct Engine {
    plan: EnginePlan,
    resolver: WallpaperResolver,
    wallpapers: Vec<Wallpaper>,
    waker: EngineWaker,
    running: Option<Running>,
}

impl Engine {
    pub fn new(resolver: WallpaperResolver) -> Self {
        Self {
            plan: EnginePlan::default(),
            resolver,
            wallpapers: Vec::new(),
            waker: EngineWaker::default(),
            running: None,
        }
    }

    pub fn waker(&self) -> EngineWaker {
        self.waker.clone()
    }

    /// Reads every wallpaper of the plan (no window, no GPU, no audio).
    pub fn load(&mut self, plan: &EnginePlan) -> Result<(), String> {
        self.plan = plan.clone();
        let assets = self.resolver.assets_dir(plan.assets_dir.as_deref())?;
        tracing::info!("assets folder: {}", assets.display());
        self.wallpapers.clear();
        for t in &plan.targets {
            let wp = Wallpaper::load(&t.wallpaper, &assets, &plan.properties)?;
            tracing::info!(
                "{}: '{}' ({:?}, {} propert{})",
                t.key,
                wp.project.title,
                wp.project.kind,
                wp.project.properties.len(),
                if wp.project.properties.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            );
            self.wallpapers.push(wp);
        }
        Ok(())
    }

    /// `--list-properties`.
    pub fn write_properties(&self, out: &mut dyn std::io::Write) -> Result<(), String> {
        for wp in &self.wallpapers {
            out.write_all(wp.project.list_properties().as_bytes())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// `--dump-structure`.
    pub fn write_structure(&self, out: &mut dyn std::io::Write) -> Result<(), String> {
        for wp in &self.wallpapers {
            out.write_all(wp.project.dump().as_bytes())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn platform_config(&self) -> Config {
        let plan = &self.plan;
        let mode = match plan.mode {
            OutputMode::Window => Mode::Window(None),
            OutputMode::ExplicitWindow {
                x,
                y,
                width,
                height,
            } => Mode::Window(Some(WindowGeometry {
                x,
                y,
                width,
                height,
            })),
            OutputMode::Desktop => Mode::Desktop,
        };
        let names: Vec<String> = plan
            .targets
            .iter()
            .flat_map(|t| t.outputs.iter().cloned())
            .collect();
        Config {
            mode,
            backend: BackendChoice::Auto,
            outputs: if names.is_empty() {
                OutputSelection::All
            } else {
                OutputSelection::Named(names)
            },
            layer: plan.layer,
            max_fps: plan.fps,
            pointer: if plan.mouse {
                PointerMode::Motion
            } else {
                PointerMode::Disabled
            },
            fullscreen: FullscreenPolicy {
                enabled: plan.pause.enabled,
                only_active: plan.pause.only_active,
                ignore_app_ids: plan.pause.ignore_app_ids.clone(),
                scope: PauseScope::AllOutputs,
            },
            pause_when_covered: true,
            identity: "lumen".into(),
        }
    }

    /// Opens the surfaces, the GPU device, the audio, and builds the scenes.
    pub fn start(&mut self) -> Result<(), String> {
        for wp in &self.wallpapers {
            wp.check_supported()?;
        }
        let mut config = self.platform_config();
        let mut surfaces = match lumen_platform::open(&config) {
            Ok(s) => s,
            Err(e) if config.mode == Mode::Desktop && std::env::var_os("DISPLAY").is_some() => {
                // GNOME/Mutter has no layer-shell: fall back on desktop windows through XWayland.
                tracing::warn!("desktop surfaces unavailable ({e}); trying X11 desktop windows");
                config.backend = BackendChoice::X11;
                lumen_platform::open(&config).map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        };
        tracing::info!("platform backend: {:?}", surfaces.backend());
        self.waker.attach(surfaces.waker());

        // Outputs arrive as events: wait for the first one (its surface picks the adapter).
        let mut early = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !early.iter().any(|e| matches!(e, Event::OutputAdded(_))) {
            if Instant::now() >= deadline {
                return Err("no output appeared within 10 s".into());
            }
            surfaces
                .wait(Wait::Until(deadline))
                .map_err(|e| e.to_string())?;
            while let Some(ev) = surfaces.next_event() {
                if ev == Event::CloseRequested {
                    return Err("the output closed during start-up".into());
                }
                early.push(ev);
            }
        }
        let stats_enabled = std::env::var_os("LUMEN_STATS").is_some_and(|v| v != "0");
        let instance = GpuInstance::new(None);
        let first = early
            .iter()
            .find_map(|e| match e {
                Event::OutputAdded(id) => Some(*id),
                _ => None,
            })
            .ok_or("no output")?;
        let first_surface = create_wgpu_surface(&instance, surfaces.as_ref(), first)?;
        let ctx = instance
            .into_context(
                &ContextOptions {
                    pipeline_cache_dir: cache_dir().map(|d| d.join("pipelines")),
                    profiling: stats_enabled,
                    // Video wallpapers: decode queues on the render device (zero copy).
                    video_decode: self.plan.hwdec == crate::plan::HwDecode::Auto
                        && self
                            .wallpapers
                            .iter()
                            .any(|w| w.kind() == lumen_scene::ProjectKind::Video),
                    ..ContextOptions::default()
                },
                &[&first_surface],
            )
            .map_err(|e| e.to_string())?;
        let spectrum = self
            .wallpapers
            .iter()
            .any(|w| w.project.supports_audio_processing);
        let mut audio = Audio::new(&self.plan.audio, self.plan.fps, spectrum);
        for wp in &self.wallpapers {
            let objects = wp.raw_objects();
            audio.add_scene_sounds(&wp.vfs, objects.iter());
        }
        audio.resume();
        let timer = if stats_enabled {
            GpuTimer::new(&ctx)
        } else {
            None
        };
        let mut running = Running {
            outputs: Vec::new(),
            scenes: (0..self.wallpapers.len()).map(|_| None).collect(),
            videos: (0..self.wallpapers.len()).map(|_| None).collect(),
            compositor: Compositor::new(&ctx),
            timer,
            ctx,
            surfaces,
            pointer: PointerTracker::new(self.plan.mouse),
            audio,
            shaders: default_provider(),
            clock: Instant::now(),
            iteration: 0,
            stats: Stats {
                enabled: stats_enabled,
                started: Some(Instant::now()),
                ..Stats::default()
            },
            screenshot: self.plan.screenshot.clone(),
            rendered: 0,
        };
        let mut first_surface = Some((first, first_surface));
        for ev in early {
            self.handle_event(&mut running, ev, &mut first_surface)?;
        }
        if let Err(e) = running.ctx.save_pipeline_cache() {
            tracing::debug!("pipeline cache not saved: {e}");
        }
        self.running = Some(running);
        Ok(())
    }

    /// Main loop: blocks on OS events; `control` is polled when woken.
    pub fn run(
        &mut self,
        control: &mut dyn FnMut() -> Option<EngineControl>,
    ) -> Result<Stopped, String> {
        let mut running = self.running.take().ok_or("engine not started")?;
        let result = loop {
            if let Some(stop) = self.drain_control(&mut running, control) {
                break Ok(stop);
            }
            let wait = video_deadline(&running).map_or(Wait::Block, Wait::Until);
            if let Err(e) = running.surfaces.wait(wait) {
                break Err(e.to_string());
            }
            running.iteration += 1;
            running.stats.wakeups += 1;
            let mut stop = None;
            while let Some(ev) = running.surfaces.next_event() {
                if ev == Event::CloseRequested {
                    stop = Some(Stopped::Driver);
                    break;
                }
                if let Err(e) = self.handle_event(&mut running, ev, &mut None) {
                    tracing::error!("{e}");
                }
            }
            if let Some(s) = stop {
                break Ok(s);
            }
            request_due_videos(&mut running);
        };
        log_summary(&running.stats);
        for (slot, wp) in running.scenes.iter_mut().zip(&mut self.wallpapers) {
            if let Some(slot) = slot {
                slot.runtime.shutdown_scripts(&mut wp.project);
            }
        }
        for v in running.videos.iter().flatten() {
            v.log_stats();
        }
        running.ctx.wait_idle();
        drop(running);
        result
    }

    fn drain_control(
        &mut self,
        running: &mut Running,
        control: &mut dyn FnMut() -> Option<EngineControl>,
    ) -> Option<Stopped> {
        while let Some(msg) = control() {
            match msg {
                EngineControl::Stop(Some(sig)) => return Some(Stopped::Signal(sig)),
                EngineControl::Stop(None) => return Some(Stopped::Requested),
                EngineControl::SetFps(fps) => {
                    self.plan.fps = fps.max(1);
                    running.surfaces.set_max_fps(self.plan.fps);
                    for v in running.videos.iter().flatten() {
                        v.player.set_max_fps(self.plan.fps);
                    }
                }
                EngineControl::SetProperties(props) => {
                    for (i, wp) in self.wallpapers.iter_mut().enumerate() {
                        let changed = wp.set_properties(&props);
                        if let Some(slot) = running.scenes.get_mut(i).and_then(Option::as_mut) {
                            for name in &changed {
                                slot.runtime.property_changed(&wp.project, name);
                            }
                        }
                        if !changed.is_empty() {
                            tracing::info!("properties changed: {}", changed.join(", "));
                        }
                    }
                    for o in &mut running.outputs {
                        o.dirty = true;
                        if !o.paused && !o.frame_requested {
                            running.surfaces.request_frame(o.id);
                            o.frame_requested = true;
                        }
                    }
                }
            }
        }
        None
    }

    fn target_for(&self, name: &str) -> Option<usize> {
        match self.plan.mode {
            OutputMode::Window | OutputMode::ExplicitWindow { .. } => Some(0),
            OutputMode::Desktop => self
                .plan
                .targets
                .iter()
                .position(|t| t.outputs.iter().any(|o| o == name))
                .or_else(|| (self.plan.targets.len() == 1).then_some(0)),
        }
    }

    fn handle_event(
        &mut self,
        r: &mut Running,
        ev: Event,
        first_surface: &mut Option<(OutputId, wgpu::Surface<'static>)>,
    ) -> Result<(), String> {
        match ev {
            Event::OutputAdded(id) => self.add_output(r, id, first_surface)?,
            Event::OutputRemoved(id) => {
                r.outputs.retain(|o| o.id != id);
                r.pointer.remove_output(id);
            }
            Event::Resized {
                output,
                physical_size,
                ..
            } => {
                if let Some(o) = r.outputs.iter_mut().find(|o| o.id == output) {
                    o.surface.resize(&r.ctx, physical_size);
                    o.size = physical_size;
                    o.dirty = true;
                    request(r.surfaces.as_mut(), o);
                }
            }
            Event::Paused { output, reasons } => {
                if let Some(o) = r.outputs.iter_mut().find(|o| o.id == output) {
                    if !o.paused {
                        tracing::info!("output {output} paused: {reasons:?}");
                    }
                    o.paused = true;
                }
                if r.outputs.iter().all(|o| o.paused) {
                    r.audio.pause();
                    r.audio.update(0.0, true);
                }
                sync_video_pause(r);
            }
            Event::Resumed(output) => {
                let all_paused = r.outputs.iter().all(|o| o.paused);
                if let Some(o) = r.outputs.iter_mut().find(|o| o.id == output) {
                    tracing::info!("output {output} resumed");
                    o.paused = false;
                    o.dirty = true;
                    request(r.surfaces.as_mut(), o);
                }
                if all_paused {
                    r.audio.resume();
                }
                sync_video_pause(r);
            }
            Event::Frame(id) => self.render(r, id),
            Event::Pointer(p) => {
                let h = r
                    .outputs
                    .iter()
                    .find(|o| o.id == p.output)
                    .map_or(0.0, |o| f64::from(o.size.1));
                r.pointer.handle(&p, h);
                let wallpapers_pointer: Vec<usize> = r
                    .scenes
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.as_ref().is_some_and(|s| s.runtime.uses_pointer))
                    .map(|(i, _)| i)
                    .collect();
                for o in &mut r.outputs {
                    if o.id == p.output && wallpapers_pointer.contains(&o.target) && !o.paused {
                        request(r.surfaces.as_mut(), o);
                    }
                }
            }
            Event::ScaleChanged { .. }
            | Event::Moved { .. }
            | Event::Wake
            | Event::CloseRequested => {}
        }
        Ok(())
    }

    fn add_output(
        &mut self,
        r: &mut Running,
        id: OutputId,
        first_surface: &mut Option<(OutputId, wgpu::Surface<'static>)>,
    ) -> Result<(), String> {
        let Some(info) = r.surfaces.output(id).cloned() else {
            return Ok(());
        };
        let Some(target) = self.target_for(&info.name) else {
            tracing::debug!("output {} has no wallpaper", info.name);
            return Ok(());
        };
        let surface = match first_surface.take() {
            Some((first, s)) if first == id => s,
            other => {
                *first_surface = other;
                create_surface_for(&r.ctx, r.surfaces.as_ref(), id)?
            }
        };
        let size = info.physical_size;
        let surface = OutputSurface::new(&r.ctx, surface, size).map_err(|e| e.to_string())?;
        let video = self.wallpapers[target].kind() == lumen_scene::ProjectKind::Video;
        if video && r.videos[target].is_none() {
            let t0 = Instant::now();
            let slot = VideoSlot::open(
                &r.ctx,
                &self.wallpapers[target],
                &self.plan,
                &mut r.audio,
                Some(notify_from(self.waker.clone())),
            )?;
            tracing::info!(
                "video wallpaper '{}' {}x{} loaded in {:.0} ms; GPU memory tracked {:.1} MiB",
                self.wallpapers[target].project.title,
                slot.player.size().0,
                slot.player.size().1,
                t0.elapsed().as_secs_f64() * 1000.0,
                r.ctx.memory().bytes() as f64 / 1048576.0,
            );
            r.videos[target] = Some(slot);
        }
        if !video && r.scenes[target].is_none() {
            let t0 = Instant::now();
            let tplan = &self.plan.targets[target];
            let wp = &mut self.wallpapers[target];
            let runtime = build_scene(
                &r.ctx,
                &mut wp.project,
                &wp.vfs,
                r.shaders.as_mut(),
                &SceneOptions {
                    output_size: size,
                    clamp: tplan.clamp,
                    particles: self.plan.particles,
                    parallax: self.plan.parallax,
                    debug: self.plan.debug.clone(),
                },
            )?;
            let s = runtime.stats;
            tracing::info!(
                "scene '{}' {}x{} loaded in {:.0} ms: {} image(s), {} pass(es), {} particle system(s), {} texture(s) ({:.1} MiB), {} stand-in shader(s), {} object(s) skipped; GPU memory tracked {:.1} MiB",
                wp.project.title,
                runtime.logical.0,
                runtime.logical.1,
                t0.elapsed().as_secs_f64() * 1000.0,
                s.images,
                s.passes,
                s.particle_systems,
                s.textures,
                s.texture_bytes as f64 / 1048576.0,
                s.stand_in_shaders,
                s.skipped_objects,
                r.ctx.memory().bytes() as f64 / 1048576.0,
            );
            r.scenes[target] = Some(SceneSlot {
                runtime,
                updated_at: u64::MAX,
                last_time: 0.0,
            });
        }
        let source = match (&r.videos[target], &r.scenes[target]) {
            (Some(v), _) => v.player.output(),
            (None, Some(slot)) => slot.runtime.scene.targets().get(slot.runtime.full),
            (None, None) => return Ok(()),
        };
        let video_generation = r.videos[target]
            .as_ref()
            .map_or(0, |v| v.player.generation());
        let tplan = &self.plan.targets[target];
        let composer = r.compositor.create_output(
            &r.ctx,
            surface.format(),
            source,
            ComposeSettings {
                scaling: tplan.scaling,
                clamp: tplan.clamp,
                flip_vertical: false,
            },
        );
        tracing::info!(
            "output {} ({}x{}, scale {}) shows '{}'",
            info.name,
            size.0,
            size.1,
            info.scale,
            self.wallpapers[target].project.title
        );
        let reasons = r.surfaces.pause_reasons(id);
        if !reasons.is_empty() {
            tracing::info!("output {} starts paused: {reasons:?}", info.name);
        }
        let mut out = Output {
            id,
            target,
            surface,
            composer,
            size,
            uv: UvRange::default(),
            paused: !reasons.is_empty(),
            dirty: true,
            frame_requested: false,
            shown_serial: u64::MAX,
            video_generation,
        };
        request(r.surfaces.as_mut(), &mut out);
        r.outputs.push(out);
        sync_video_pause(r);
        Ok(())
    }

    fn render(&mut self, r: &mut Running, id: OutputId) {
        let Some(oi) = r.outputs.iter().position(|o| o.id == id) else {
            return;
        };
        r.outputs[oi].frame_requested = false;
        if r.outputs[oi].paused {
            return;
        }
        let start = Instant::now();
        let target = r.outputs[oi].target;
        if r.videos[target].is_some() {
            render_video(r, oi, start);
            return;
        }
        let Some(slot) = r.scenes[target].as_mut() else {
            return;
        };
        let wp = &mut self.wallpapers[target];
        let fresh = slot.updated_at != r.iteration;
        if fresh {
            let time = r.clock.elapsed().as_secs_f64();
            let dt = if slot.last_time == 0.0 {
                time as f32
            } else {
                (time - slot.last_time) as f32
            };
            slot.last_time = time;
            r.audio.update(dt, false);
            let o = &r.outputs[oi];
            let mut screen = ScreenState {
                size: o.size,
                cursor: Vec2::new(o.size.0 as f32, o.size.1 as f32) * 0.5,
                left_down: false,
            };
            let pointer = if self.plan.mouse {
                let p = r.pointer.state(id, o.size);
                screen.cursor = Vec2::new(p.x as f32, p.y as f32);
                screen.left_down = p.left;
                pointer_position(
                    screen.cursor,
                    Viewport {
                        x: 0.0,
                        y: 0.0,
                        width: o.size.0 as f32,
                        height: o.size.1 as f32,
                    },
                    o.uv,
                )
            } else {
                pointer_disabled(o.uv)
            };
            let (a16, a32, a64) = r.audio.bands();
            slot.runtime.update(
                &r.ctx,
                &mut wp.project,
                &FrameState {
                    time,
                    dt,
                    daytime: daytime(),
                    pointer,
                    audio16: a16,
                    audio32: a32,
                    audio64: a64,
                    screen,
                },
            );
            // Once per iteration even when nothing is redrawn (spec 06 §8.2: scripts tick once
            // per frame and scene, whatever the number of outputs).
            slot.updated_at = r.iteration;
        }
        let o = &mut r.outputs[oi];
        let redraw = slot.runtime.scene.needs_redraw();
        if redraw || o.dirty {
            match o.surface.acquire(&r.ctx) {
                Ok(Some(frame)) => {
                    let mut enc =
                        r.ctx
                            .device()
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("lumen.frame"),
                            });
                    if let Some(t) = &mut r.timer {
                        t.begin(&mut enc);
                    }
                    if fresh || redraw {
                        slot.runtime.scene.encode(&r.ctx, &mut enc);
                        slot.updated_at = r.iteration;
                    }
                    let size = frame.size();
                    o.uv = o.composer.update(&r.ctx, size, slot.runtime.logical);
                    r.compositor.encode(
                        &mut enc,
                        &o.composer,
                        frame.view(),
                        [0.0, 0.0, size.0 as f32, size.1 as f32],
                        Some(wgpu::Color::BLACK),
                    );
                    if let Some(t) = &mut r.timer {
                        t.end(&mut enc);
                    }
                    r.ctx.queue().submit(std::iter::once(enc.finish()));
                    if let Some(t) = &mut r.timer {
                        t.after_submit();
                    }
                    r.surfaces.pre_present(id);
                    frame.present(&r.ctx);
                    o.dirty = false;
                    r.rendered += 1;
                    let cpu = start.elapsed();
                    let gpu = r.timer.as_mut().and_then(|t| t.poll(&r.ctx));
                    record_stats(
                        &mut r.stats,
                        cpu,
                        gpu,
                        &r.ctx,
                        slot.runtime.live_particles(),
                    );
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("output {id}: {e}"),
            }
            if r.screenshot
                .as_ref()
                .is_some_and(|(_, delay)| r.rendered > u64::from(*delay))
                && let Some((path, _)) = r.screenshot.take()
            {
                // spec 07 §18: the wallpaper image (`_rt_FullFrameBuffer`). Lumen writes PNG
                // data whatever the extension (BMP/JPEG encoders are not linked).
                let rt = slot.runtime.scene.targets().get(slot.runtime.full);
                match lumen_render::headless::read_texture_rgba8(&r.ctx, rt.texture()) {
                    Ok(px) => {
                        let (w, h) = rt.size();
                        match lumen_render::headless::write_png(&path, w, h, &px) {
                            Ok(()) => tracing::info!("screenshot written to {}", path.display()),
                            Err(e) => tracing::error!("screenshot: {e}"),
                        }
                    }
                    Err(e) => tracing::error!("screenshot: {e}"),
                }
            }
        }
        let animated = slot.runtime.animated || r.audio.spectrum_enabled();
        if animated || slot.runtime.scene.needs_redraw() {
            request(r.surfaces.as_mut(), o);
        }
    }
}

/// One frame of a video wallpaper on one output: the newest due frame is uploaded (or
/// bound, zero copy) and converted once per loop iteration, then composed like a scene.
fn render_video(r: &mut Running, oi: usize, start: Instant) {
    let id = r.outputs[oi].id;
    let target = r.outputs[oi].target;
    let Some(v) = r.videos[target].as_mut() else {
        return;
    };
    if v.updated_at != r.iteration {
        v.updated_at = r.iteration;
        v.player.update(&r.ctx, Instant::now());
    }
    let o = &mut r.outputs[oi];
    let convert = v.player.needs_redraw();
    if !convert && !o.dirty && o.shown_serial == v.serial {
        return;
    }
    match o.surface.acquire(&r.ctx) {
        Ok(Some(frame)) => {
            let mut enc = r
                .ctx
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("lumen.frame.video"),
                });
            if let Some(t) = &mut r.timer {
                t.begin(&mut enc);
            }
            // Zero copy: `pre` makes the decoded image readable; same submission, first.
            let pre = if convert {
                v.serial += 1;
                v.player.encode(&r.ctx, &mut enc)
            } else {
                None
            };
            if o.video_generation != v.player.generation() {
                o.video_generation = v.player.generation();
                let settings = o.composer.settings();
                r.compositor
                    .rebind(&r.ctx, &mut o.composer, v.player.output(), settings);
            }
            let size = frame.size();
            o.uv = o.composer.update(&r.ctx, size, v.player.size());
            r.compositor.encode(
                &mut enc,
                &o.composer,
                frame.view(),
                [0.0, 0.0, size.0 as f32, size.1 as f32],
                Some(wgpu::Color::BLACK),
            );
            if let Some(t) = &mut r.timer {
                t.end(&mut enc);
            }
            r.ctx
                .queue()
                .submit(pre.into_iter().chain(std::iter::once(enc.finish())));
            if let Some(t) = &mut r.timer {
                t.after_submit();
            }
            r.surfaces.pre_present(id);
            frame.present(&r.ctx);
            o.dirty = false;
            o.shown_serial = v.serial;
            r.rendered += 1;
            let gpu = r.timer.as_mut().and_then(|t| t.poll(&r.ctx));
            record_stats(&mut r.stats, start.elapsed(), gpu, &r.ctx, 0);
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("output {id}: {e}"),
    }
    if r.screenshot
        .as_ref()
        .is_some_and(|(_, delay)| r.rendered > u64::from(*delay))
        && let Some((path, _)) = r.screenshot.take()
    {
        let rt = v.player.output();
        match lumen_render::headless::read_texture_rgba8(&r.ctx, rt.texture()) {
            Ok(px) => {
                let (w, h) = rt.size();
                match lumen_render::headless::write_png(&path, w, h, &px) {
                    Ok(()) => tracing::info!("screenshot written to {}", path.display()),
                    Err(e) => tracing::error!("screenshot: {e}"),
                }
            }
            Err(e) => tracing::error!("screenshot: {e}"),
        }
    }
}

/// A video is paused while every output showing it is paused (spec 07 §9.4).
fn sync_video_pause(r: &mut Running) {
    for (target, slot) in r.videos.iter_mut().enumerate() {
        let Some(v) = slot else {
            continue;
        };
        let mut outputs = r.outputs.iter().filter(|o| o.target == target).peekable();
        let paused = outputs.peek().is_none() || outputs.all(|o| o.paused);
        if paused != v.is_paused() {
            tracing::info!("video {}", if paused { "paused" } else { "resumed" });
            v.set_paused(paused, &r.audio);
        }
    }
}

/// Earliest instant a playing video needs a frame, for videos whose outputs have no frame
/// request pending (a pending request is served by the platform's own pacing).
fn video_deadline(r: &Running) -> Option<Instant> {
    r.videos
        .iter()
        .enumerate()
        .filter_map(|(target, v)| {
            let v = v.as_ref()?;
            let waiting = r
                .outputs
                .iter()
                .any(|o| o.target == target && !o.paused && !o.frame_requested);
            if waiting {
                v.player.next_deadline()
            } else {
                None
            }
        })
        .min()
}

/// Requests a frame for the outputs of every video whose next frame is due.
fn request_due_videos(r: &mut Running) {
    let now = Instant::now();
    for (target, v) in r.videos.iter().enumerate() {
        let Some(v) = v else {
            continue;
        };
        if v.player.next_deadline().is_some_and(|d| d <= now) {
            for o in r
                .outputs
                .iter_mut()
                .filter(|o| o.target == target && !o.paused)
            {
                request(r.surfaces.as_mut(), o);
            }
        }
    }
}

fn request(s: &mut dyn WallpaperSurface, o: &mut Output) {
    if !o.frame_requested {
        s.request_frame(o.id);
        o.frame_requested = true;
    }
}

fn create_wgpu_surface(
    instance: &GpuInstance,
    surfaces: &dyn WallpaperSurface,
    id: OutputId,
) -> Result<wgpu::Surface<'static>, String> {
    let window = surfaces
        .raw_window_handle(id)
        .ok_or("output without window handle")?;
    // SAFETY: the handles stay valid until `OutputRemoved` (window) or the end of the backend
    // (display); `Running` drops every GPU surface before the backend and removes an output's
    // surface as soon as its `OutputRemoved` arrives.
    unsafe { instance.create_surface(surfaces.raw_display_handle(), window) }
        .map_err(|e| e.to_string())
}

fn create_surface_for(
    ctx: &GpuContext,
    surfaces: &dyn WallpaperSurface,
    id: OutputId,
) -> Result<wgpu::Surface<'static>, String> {
    let window = surfaces
        .raw_window_handle(id)
        .ok_or("output without window handle")?;
    let target = wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(surfaces.raw_display_handle()),
        raw_window_handle: window,
    };
    // SAFETY: see `create_wgpu_surface`.
    unsafe { ctx.instance().create_surface_unsafe(target) }.map_err(|e| e.to_string())
}

/// `g_Daytime`: local minutes of the day / 1440 (UTC when the offset is unknown).
fn daytime() -> f32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let minutes = (secs / 60) % 1440;
    minutes as f32 / 1440.0
}

fn rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn record_stats(
    s: &mut Stats,
    cpu: Duration,
    gpu: Option<Duration>,
    ctx: &GpuContext,
    particles: usize,
) {
    s.total_frames += 1;
    s.total_cpu += cpu;
    if let Some(g) = gpu {
        s.total_gpu += g;
        s.total_gpu_samples += 1;
    }
    if !s.enabled {
        return;
    }
    let since = *s.since.get_or_insert_with(Instant::now);
    s.frames += 1;
    s.cpu += cpu;
    if let Some(g) = gpu {
        s.gpu += g;
        s.gpu_samples += 1;
    }
    let elapsed = since.elapsed();
    if elapsed >= Duration::from_secs(5) {
        let fps = s.frames as f64 / elapsed.as_secs_f64();
        let cpu_ms = s.cpu.as_secs_f64() * 1000.0 / s.frames.max(1) as f64;
        let gpu_ms =
            (s.gpu_samples > 0).then(|| s.gpu.as_secs_f64() * 1000.0 / f64::from(s.gpu_samples));
        tracing::info!(
            "stats: {fps:.1} fps, CPU {cpu_ms:.2} ms/frame, GPU {}, GPU memory tracked {:.1} MiB, RSS {}, {particles} live particles, {} wake-ups",
            gpu_ms.map_or("n/a".into(), |g| format!("{g:.2} ms/frame")),
            ctx.memory().bytes() as f64 / 1048576.0,
            rss_bytes().map_or("n/a".into(), |b| format!("{:.1} MiB", b as f64 / 1048576.0)),
            s.wakeups,
        );
        s.since = Some(Instant::now());
        s.frames = 0;
        s.cpu = Duration::ZERO;
        s.gpu = Duration::ZERO;
        s.gpu_samples = 0;
        s.wakeups = 0;
    }
}

fn log_summary(s: &Stats) {
    let Some(started) = s.started else {
        return;
    };
    let secs = started.elapsed().as_secs_f64();
    if s.total_frames == 0 {
        tracing::info!("no frame rendered in {secs:.1} s");
        return;
    }
    tracing::info!(
        "rendered {} frames in {secs:.1} s ({:.1} fps), CPU {:.2} ms/frame{}, RSS {}",
        s.total_frames,
        s.total_frames as f64 / secs,
        s.total_cpu.as_secs_f64() * 1000.0 / s.total_frames as f64,
        if s.total_gpu_samples > 0 {
            format!(
                ", GPU {:.2} ms/frame",
                s.total_gpu.as_secs_f64() * 1000.0 / s.total_gpu_samples as f64
            )
        } else {
            String::new()
        },
        rss_bytes().map_or("n/a".into(), |b| format!("{:.1} MiB", b as f64 / 1048576.0)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waker_before_the_platform_is_remembered() {
        let w = EngineWaker::default();
        w.clone().wake();
        assert!(w.inner.pending.load(Ordering::Acquire));
    }

    #[test]
    fn daytime_is_a_fraction_of_a_day() {
        let d = daytime();
        assert!((0.0..1.0).contains(&d));
    }
}

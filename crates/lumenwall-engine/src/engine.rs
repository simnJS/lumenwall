//! The live engine: platform surfaces, one GPU device, one scene per target, the event loop.
//!
//! The loop blocks in the platform's `wait` (no polling). A frame is requested only while the
//! scene changes over time (time-driven shaders, particles, animated textures, spectrum,
//! accumulation settling) or after an input/property/resize event; a static scene renders once
//! and then sleeps. Paused outputs (fullscreen, covered, locked, user pause) receive no frame.
//!
//! A new plan ([`EngineControl::Reload`]) is applied in place: wallpapers the new plan keeps
//! are reused, new ones are read and built before anything is released (a failure leaves the
//! engine unchanged), outputs are rebound, and the surfaces are reopened only when the layout,
//! layer or pause policy demands it. Without any wallpaper the engine closes its surfaces and
//! GPU device and sleeps until a plan brings one back.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use glam::Vec2;
use lumenwall_platform::mouse::PointerTracker;
use lumenwall_platform::{
    BackendChoice, Config, Event, FullscreenPolicy, Mode, OutputId, OutputInfo, OutputSelection,
    PauseReasons, PauseScope, PointerMode, Wait, WallpaperSurface, WindowGeometry,
};
use lumenwall_render::compose::{ComposeSettings, Compositor, OutputComposer};
use lumenwall_render::math::{UvRange, Viewport, pointer_disabled, pointer_position};
use lumenwall_render::stats::GpuTimer;
use lumenwall_render::targets::RenderTarget;
use lumenwall_render::{ContextOptions, GpuContext, GpuInstance, OutputSurface};

use crate::audio::Audio;
use crate::monitor::{
    Change, EngineMonitor, EngineReport, OutputReport, PauseCause, TargetReport, TargetState,
};
use crate::plan::{EngineControl, EnginePlan, OutputMode, Stopped, TargetPlan};
use crate::scene::{FrameState, SceneOptions, SceneRuntime, ScreenState, build_scene};
use crate::shaders::default_provider;
use crate::video::{VideoSlot, notify_from};
use crate::wallpaper::Wallpaper;
use crate::{WallpaperResolver, cache_dir};

/// Wakes the engine loop from any thread; usable before the platform exists and while the
/// engine is idle (no surfaces).
#[derive(Clone, Default)]
pub struct EngineWaker {
    inner: Arc<WakerInner>,
}

#[derive(Default)]
struct WakerInner {
    state: Mutex<WakerState>,
    /// Signalled by `wake` while no platform is attached (idle engine).
    idle: Condvar,
}

#[derive(Default)]
struct WakerState {
    platform: Option<lumenwall_platform::Waker>,
    pending: bool,
}

impl EngineWaker {
    pub fn wake(&self) {
        let mut state = self.lock();
        match &state.platform {
            Some(w) => w.wake(),
            None => {
                state.pending = true;
                self.inner.idle.notify_all();
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, WakerState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn attach(&self, w: lumenwall_platform::Waker) {
        let mut state = self.lock();
        if std::mem::take(&mut state.pending) {
            w.wake();
        }
        state.platform = Some(w);
    }

    /// Before the platform is dropped: later wakes go to the idle condition variable.
    fn detach(&self) {
        self.lock().platform = None;
    }

    /// Idle engine: sleeps until [`EngineWaker::wake`].
    fn idle_wait(&self) {
        let mut state = self.lock();
        while !state.pending {
            state = self
                .inner
                .idle
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.pending = false;
    }
}

struct Output {
    id: OutputId,
    name: String,
    /// Index into `Running::scenes`.
    target: usize,
    surface: OutputSurface,
    composer: OutputComposer,
    size: (u32, u32),
    uv: UvRange,
    /// Paused by the platform (fullscreen, covered, locked).
    paused: bool,
    reasons: PauseReasons,
    /// Paused by the application ([`EngineControl::SetPaused`]); a dirty output still gets
    /// one frame (property preview while paused).
    held: bool,
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

/// The renderer of one target.
enum Renderer {
    Scene(Box<SceneSlot>),
    Video(Box<VideoSlot>),
}

/// Debug counters (enabled with `LUMENWALL_STATS=1`).
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
    /// A timer was requested (unsupported devices are not asked again every frame).
    timer_tried: bool,
    ctx: GpuContext,
    surfaces: Box<dyn WallpaperSurface>,
    /// Configuration the surfaces were opened with.
    platform: Config,
    pointer: PointerTracker,
    audio: Audio,
    clock: Instant,
    iteration: u64,
    stats: Stats,
    screenshot: Option<(PathBuf, u32)>,
    rendered: u64,
}

/// An output the platform reported, kept across layout changes.
struct Known {
    info: OutputInfo,
    /// Reported by the current surfaces (its id is valid).
    live: bool,
}

/// What a plan change keeps or brings, target by target.
enum Staged {
    /// Index of the current target reused as is.
    Keep(usize),
    New(Box<Wallpaper>, Option<Renderer>),
}

/// The wallpaper engine: load, start, run.
pub struct Engine {
    plan: EnginePlan,
    resolver: WallpaperResolver,
    assets: PathBuf,
    /// One per target of `plan`.
    wallpapers: Vec<Wallpaper>,
    states: Vec<TargetState>,
    waker: EngineWaker,
    running: Option<Running>,
    monitor: EngineMonitor,
    known: Vec<Known>,
    user_paused: bool,
    /// Backend that opened (X11 after a layer-shell failure on GNOME).
    backend: BackendChoice,
}

impl Engine {
    pub fn new(resolver: WallpaperResolver) -> Self {
        Self {
            plan: EnginePlan::default(),
            resolver,
            assets: PathBuf::new(),
            wallpapers: Vec::new(),
            states: Vec::new(),
            waker: EngineWaker::default(),
            running: None,
            monitor: EngineMonitor::default(),
            known: Vec::new(),
            user_paused: false,
            backend: BackendChoice::Auto,
        }
    }

    pub fn waker(&self) -> EngineWaker {
        self.waker.clone()
    }

    /// Status and counters for the application (IPC), readable from any thread.
    pub fn monitor(&self) -> EngineMonitor {
        self.monitor.clone()
    }

    /// Reads every wallpaper of the plan (no window, no GPU, no audio).
    pub fn load(&mut self, plan: &EnginePlan) -> Result<(), String> {
        self.plan = plan.clone();
        let assets = self.resolver.assets_dir(plan.assets_dir.as_deref())?;
        tracing::info!("assets folder: {}", assets.display());
        self.assets = assets;
        self.wallpapers.clear();
        for t in &plan.targets {
            let wp = Wallpaper::load(&t.wallpaper, &self.assets, &plan.properties)?;
            log_loaded(&t.key, &wp);
            self.wallpapers.push(wp);
        }
        self.states = vec![TargetState::Loading; self.wallpapers.len()];
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

    fn platform_config(plan: &EnginePlan) -> Config {
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
        // A target without output names (the default wallpaper) covers every output.
        let all = plan.targets.iter().any(|t| t.outputs.is_empty());
        let names: Vec<String> = plan
            .targets
            .iter()
            .flat_map(|t| t.outputs.iter().cloned())
            .collect();
        Config {
            mode,
            backend: BackendChoice::Auto,
            outputs: if all || names.is_empty() {
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
            // "Other application maximised" follows the fullscreen pause switch.
            pause_when_maximized: plan.pause.enabled,
            identity: "lumenwall".into(),
        }
    }

    /// Opens the platform, falling back on X11 desktop windows when the compositor has no
    /// layer-shell (GNOME/Mutter); the fallback is remembered for later reopenings.
    fn open_platform(&mut self, config: &Config) -> Result<Box<dyn WallpaperSurface>, String> {
        let mut config = config.clone();
        config.backend = self.backend;
        match lumenwall_platform::open(&config) {
            Ok(s) => Ok(s),
            Err(e)
                if config.mode == Mode::Desktop
                    && config.backend == BackendChoice::Auto
                    && std::env::var_os("DISPLAY").is_some() =>
            {
                tracing::warn!("desktop surfaces unavailable ({e}); trying X11 desktop windows");
                config.backend = BackendChoice::X11;
                let s = lumenwall_platform::open(&config).map_err(|e| e.to_string())?;
                self.backend = BackendChoice::X11;
                Ok(s)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Opens the surfaces, the GPU device, the audio, and builds the scenes. Without any
    /// wallpaper nothing is opened: the engine waits for a plan ([`EngineControl::Reload`]).
    pub fn start(&mut self) -> Result<(), String> {
        if self.wallpapers.is_empty() {
            tracing::info!("no wallpaper to show: waiting for one (IPC)");
            self.publish(None, Change::State);
            return Ok(());
        }
        for wp in &self.wallpapers {
            wp.check_supported()?;
        }
        let running = self.open_running()?;
        self.running = Some(running);
        Ok(())
    }

    fn open_running(&mut self) -> Result<Running, String> {
        let config = Self::platform_config(&self.plan);
        let mut surfaces = self.open_platform(&config)?;
        tracing::info!("platform backend: {:?}", surfaces.backend());
        self.waker.attach(surfaces.waker());
        for k in &mut self.known {
            k.live = false;
        }

        // Outputs arrive as events: wait for the first one (its surface picks the adapter).
        let mut early = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !early.iter().any(|e| matches!(e, Event::OutputAdded(_))) {
            if Instant::now() >= deadline {
                self.waker.detach();
                return Err("no output appeared within 10 s".into());
            }
            if let Err(e) = surfaces.wait(Wait::Until(deadline)) {
                self.waker.detach();
                return Err(e.to_string());
            }
            while let Some(ev) = surfaces.next_event() {
                if ev == Event::CloseRequested {
                    self.waker.detach();
                    return Err("the output closed during start-up".into());
                }
                early.push(ev);
            }
        }
        let stats_enabled = std::env::var_os("LUMENWALL_STATS").is_some_and(|v| v != "0");
        let instance = GpuInstance::new(None);
        let first = early
            .iter()
            .find_map(|e| match e {
                Event::OutputAdded(id) => Some(*id),
                _ => None,
            })
            .ok_or("no output")?;
        let built = (|| {
            let first_surface = create_wgpu_surface(&instance, surfaces.as_ref(), first)?;
            let ctx = instance
                .into_context(
                    &ContextOptions {
                        pipeline_cache_dir: cache_dir().map(|d| d.join("pipelines")),
                        // Timestamp queries are allowed so that statistics can time the GPU
                        // on demand; nothing is measured until someone asks.
                        profiling: true,
                        // Video wallpapers: decode queues on the render device (zero copy).
                        video_decode: self.plan.hwdec == crate::plan::HwDecode::Auto
                            && self
                                .wallpapers
                                .iter()
                                .any(|w| w.kind() == lumenwall_scene::ProjectKind::Video),
                        ..ContextOptions::default()
                    },
                    &[&first_surface],
                )
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((first_surface, ctx))
        })();
        let (first_surface, ctx) = match built {
            Ok(b) => b,
            Err(e) => {
                self.waker.detach();
                return Err(e);
            }
        };
        let spectrum = self
            .wallpapers
            .iter()
            .any(|w| w.project.supports_audio_processing);
        let mut audio = Audio::new(&self.plan.audio, self.plan.fps, spectrum);
        for wp in &self.wallpapers {
            let objects = wp.raw_objects();
            audio.add_scene_sounds(&wp.vfs, objects.iter());
        }
        if self.user_paused {
            audio.set_paused(true);
        } else {
            audio.resume();
        }
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
            timer_tried: stats_enabled,
            ctx,
            surfaces,
            platform: config,
            pointer: PointerTracker::new(self.plan.mouse),
            audio,
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
            if let Err(e) = self.handle_event(&mut running, ev, &mut first_surface) {
                self.waker.detach();
                return Err(e);
            }
        }
        if let Err(e) = running.ctx.save_pipeline_cache() {
            tracing::debug!("pipeline cache not saved: {e}");
        }
        self.publish(Some(&running), Change::Outputs);
        Ok(running)
    }

    /// Ends scripts and releases the surfaces and the GPU device.
    fn close_running(&mut self, mut running: Running) {
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
        self.waker.detach();
        drop(running);
        for k in &mut self.known {
            k.live = false;
        }
    }

    /// Main loop: blocks on OS events (or, without any wallpaper, on the waker); `control` is
    /// polled when woken.
    pub fn run(
        &mut self,
        control: &mut dyn FnMut() -> Option<EngineControl>,
    ) -> Result<Stopped, String> {
        let mut running = self.running.take();
        let result = loop {
            if let Some(stop) = self.drain_control(&mut running, control) {
                break Ok(stop);
            }
            let Some(r) = running.as_mut() else {
                self.waker.idle_wait();
                continue;
            };
            let wait = video_deadline(r).map_or(Wait::Block, Wait::Until);
            if let Err(e) = r.surfaces.wait(wait) {
                break Err(e.to_string());
            }
            r.iteration += 1;
            r.stats.wakeups += 1;
            let mut stop = None;
            while let Some(ev) = r.surfaces.next_event() {
                if ev == Event::CloseRequested {
                    stop = Some(Stopped::Driver);
                    break;
                }
                if let Err(e) = self.handle_event(r, ev, &mut None) {
                    tracing::error!("{e}");
                }
            }
            if let Some(s) = stop {
                break Ok(s);
            }
            request_due_videos(r);
        };
        if let Some(r) = running.take() {
            self.close_running(r);
        }
        result
    }

    fn drain_control(
        &mut self,
        running: &mut Option<Running>,
        control: &mut dyn FnMut() -> Option<EngineControl>,
    ) -> Option<Stopped> {
        while let Some(msg) = control() {
            match msg {
                EngineControl::Stop(Some(sig)) => return Some(Stopped::Signal(sig)),
                EngineControl::Stop(None) => return Some(Stopped::Requested),
                EngineControl::SetFps(fps) => self.set_fps(running.as_mut(), fps),
                EngineControl::SetProperties(props) => {
                    self.plan.properties = props;
                    self.apply_properties(running.as_mut());
                }
                EngineControl::SetPaused(paused) => self.set_user_paused(running.as_mut(), paused),
                EngineControl::Reload { plan, done } => {
                    let result = self.reload(running, *plan);
                    if let Err(e) = &result {
                        tracing::error!("new settings not applied: {e}");
                    }
                    if let Some(done) = done {
                        let _ = done.try_send(result);
                    }
                }
            }
        }
        None
    }

    fn set_fps(&mut self, r: Option<&mut Running>, fps: u32) {
        self.plan.fps = fps.max(1);
        if let Some(r) = r {
            r.surfaces.set_max_fps(self.plan.fps);
            for v in r.videos.iter().flatten() {
                v.player.set_max_fps(self.plan.fps);
            }
        }
    }

    /// Gives every wallpaper the plan's property overrides (defaults for the others) and
    /// redraws what changed.
    fn apply_properties(&mut self, mut r: Option<&mut Running>) {
        let mut any = false;
        for (i, wp) in self.wallpapers.iter_mut().enumerate() {
            let changed = wp.set_properties(&self.plan.properties);
            if changed.is_empty() {
                continue;
            }
            any = true;
            tracing::info!("properties changed: {}", changed.join(", "));
            if let Some(slot) = r
                .as_deref_mut()
                .and_then(|r| r.scenes.get_mut(i))
                .and_then(Option::as_mut)
            {
                for name in &changed {
                    slot.runtime.property_changed(&wp.project, name);
                }
            }
        }
        if !any {
            return;
        }
        if let Some(r) = r.as_deref_mut() {
            for o in &mut r.outputs {
                o.dirty = true;
                if !o.paused {
                    request(r.surfaces.as_mut(), o);
                }
            }
        }
        self.publish(r.as_deref(), Change::Properties);
    }

    fn set_user_paused(&mut self, r: Option<&mut Running>, paused: bool) {
        if self.user_paused == paused {
            return;
        }
        self.user_paused = paused;
        tracing::info!("{}", if paused { "paused" } else { "resumed" });
        let Some(r) = r else {
            self.publish(None, Change::State);
            return;
        };
        for o in &mut r.outputs {
            o.held = paused;
        }
        r.audio.set_paused(paused);
        let all_platform_paused = r.outputs.iter().all(|o| o.paused);
        if paused {
            r.audio.pause();
        } else {
            if !all_platform_paused {
                r.audio.resume();
            }
            for o in &mut r.outputs {
                if !o.paused {
                    o.dirty = true;
                    request(r.surfaces.as_mut(), o);
                }
            }
        }
        sync_video_pause(r);
        self.publish(Some(r), Change::State);
    }

    /// Applies a new plan in place (see the module documentation).
    fn reload(&mut self, running: &mut Option<Running>, new: EnginePlan) -> Result<(), String> {
        let t0 = Instant::now();
        let old = &self.plan;
        // Options that shape how a wallpaper is read or built: nothing can be reused.
        let rebuild_all = new.assets_dir != old.assets_dir
            || new.particles != old.particles
            || new.parallax != old.parallax
            || new.debug != old.debug;
        let rebuild_video = new.hwdec != old.hwdec;
        let assets = if new.assets_dir == old.assets_dir {
            self.assets.clone()
        } else {
            self.resolver.assets_dir(new.assets_dir.as_deref())?
        };

        // 1. Read the wallpapers the new plan adds; reuse the others.
        let mut used = vec![false; old.targets.len()];
        let mut staged = Vec::with_capacity(new.targets.len());
        for t in &new.targets {
            let reusable = |j: usize| {
                let o = &old.targets[j];
                !used[j]
                    && !rebuild_all
                    && o.wallpaper == t.wallpaper
                    && o.clamp == t.clamp
                    && !matches!(self.states[j], TargetState::Error(_))
                    && !(rebuild_video
                        && self.wallpapers[j].kind() == lumenwall_scene::ProjectKind::Video)
            };
            let reuse = (0..old.targets.len())
                .find(|&j| reusable(j) && old.targets[j].key == t.key)
                .or_else(|| (0..old.targets.len()).find(|&j| reusable(j)));
            match reuse {
                Some(j) => {
                    used[j] = true;
                    staged.push(Staged::Keep(j));
                }
                None => {
                    let wp = Wallpaper::load(&t.wallpaper, &assets, &new.properties)?;
                    wp.check_supported()?;
                    staged.push(Staged::New(Box::new(wp), None));
                }
            }
        }
        let added = staged
            .iter()
            .filter(|s| matches!(s, Staged::New(..)))
            .count();
        let released = used.iter().filter(|u| !**u).count();

        // 2. Build the new renderers for the outputs already there, and open the new
        // surfaces when the layout changes: a failure here changes nothing.
        let config = Self::platform_config(&new);
        let mut surfaces = None;
        if let Some(r) = running.as_mut() {
            for (i, s) in staged.iter_mut().enumerate() {
                let Staged::New(wp, renderer) = s else {
                    continue;
                };
                let size = r
                    .outputs
                    .iter()
                    .find(|o| target_for(&new, &o.name) == Some(i))
                    .map(|o| o.size);
                if let Some(size) = size {
                    *renderer = Some(build_renderer(
                        r,
                        wp,
                        &new.targets[i],
                        &new,
                        &self.waker,
                        size,
                    )?);
                }
            }
            if needs_reopen(r, &config, &new, &self.known) {
                surfaces = Some(self.open_platform(&config)?);
            }
        }

        // 3. Commit.
        let old_plan = std::mem::replace(&mut self.plan, new);
        let old_states = std::mem::take(&mut self.states);
        let mut old_wallpapers: Vec<Option<Wallpaper>> = std::mem::take(&mut self.wallpapers)
            .into_iter()
            .map(Some)
            .collect();
        let (mut old_scenes, mut old_videos) = match running.as_mut() {
            Some(r) => (std::mem::take(&mut r.scenes), std::mem::take(&mut r.videos)),
            None => (Vec::new(), Vec::new()),
        };
        let mut scenes = Vec::with_capacity(staged.len());
        let mut videos = Vec::with_capacity(staged.len());
        for s in staged {
            match s {
                Staged::Keep(j) => {
                    self.wallpapers
                        .push(old_wallpapers[j].take().expect("each target reused once"));
                    self.states.push(old_states[j].clone());
                    scenes.push(old_scenes.get_mut(j).and_then(Option::take));
                    videos.push(old_videos.get_mut(j).and_then(Option::take));
                }
                Staged::New(wp, renderer) => {
                    log_loaded(&self.plan.targets[self.wallpapers.len()].key, &wp);
                    self.wallpapers.push(*wp);
                    let (scene, video, state) = match renderer {
                        Some(Renderer::Scene(s)) => (Some(*s), None, TargetState::Running),
                        Some(Renderer::Video(v)) => (None, Some(*v), TargetState::Running),
                        None => (None, None, TargetState::Loading),
                    };
                    scenes.push(scene);
                    videos.push(video);
                    self.states.push(state);
                }
            }
        }
        // Release what the new plan does not use.
        for (j, slot) in old_scenes.iter_mut().enumerate() {
            if let (Some(slot), Some(wp)) = (slot.as_mut(), old_wallpapers[j].as_mut()) {
                slot.runtime.shutdown_scripts(&mut wp.project);
            }
        }
        for wp in old_wallpapers.iter().flatten() {
            tracing::info!("'{}' released", wp.project.title);
        }
        drop(old_scenes);
        drop(old_videos);
        drop(old_wallpapers);
        self.assets = assets;
        let targets_changed = added > 0 || released > 0 || old_plan.targets != self.plan.targets;

        if running.is_none() {
            if !self.plan.targets.is_empty() {
                // Idle until now (no wallpaper): open everything.
                match self.open_running() {
                    Ok(r) => *running = Some(r),
                    Err(e) => {
                        // Back to the previous plan, which had no wallpaper.
                        self.plan = old_plan;
                        self.wallpapers.clear();
                        self.states.clear();
                        self.publish(None, Change::State);
                        return Err(e);
                    }
                }
            }
        } else if self.plan.targets.is_empty() {
            if let Some(r) = running.take() {
                self.close_running(r);
            }
            tracing::info!("no wallpaper left: surfaces and GPU device closed");
        } else if let Some(r) = running.as_mut() {
            {
                r.scenes = scenes;
                r.videos = videos;
                // Property values of the reused wallpapers.
                for (i, wp) in self.wallpapers.iter_mut().enumerate() {
                    let changed = wp.set_properties(&self.plan.properties);
                    if let Some(slot) = r.scenes.get_mut(i).and_then(Option::as_mut) {
                        for name in &changed {
                            slot.runtime.property_changed(&wp.project, name);
                        }
                    }
                }
                if old_plan.fps != self.plan.fps {
                    r.surfaces.set_max_fps(self.plan.fps);
                    for v in r.videos.iter().flatten() {
                        v.player.set_max_fps(self.plan.fps);
                    }
                }
                match surfaces {
                    Some(surfaces) => {
                        // Outputs come back as `OutputAdded` events and rebind there.
                        r.outputs.clear();
                        for k in &mut self.known {
                            k.live = false;
                        }
                        r.pointer = PointerTracker::new(self.plan.mouse);
                        self.waker.attach(surfaces.waker());
                        r.surfaces = surfaces;
                        r.platform = config;
                        tracing::info!("surfaces reopened: {:?}", r.surfaces.backend());
                    }
                    None if targets_changed => self.rebind_outputs(r),
                    None => {}
                }
                let a = (&old_plan.audio, &self.plan.audio);
                if targets_changed
                    || a.0.silent != a.1.silent
                    || a.0.automute != a.1.automute
                    || a.0.processing != a.1.processing
                {
                    self.rebuild_audio(r);
                } else if a.0.volume != a.1.volume {
                    r.audio.set_volume(self.plan.audio.volume);
                    for v in r.videos.iter().flatten() {
                        v.set_volume(&r.audio, self.plan.audio.volume);
                    }
                }
                sync_video_pause(r);
            }
        }
        for (what, changed) in [
            ("input", old_plan.mouse != self.plan.mouse),
            ("video decoding", old_plan.hwdec != self.plan.hwdec),
        ] {
            if changed {
                tracing::info!("{what} setting applies to the wallpapers loaded from now on");
            }
        }
        if targets_changed {
            tracing::info!(
                "new plan applied in {:.0} ms: {added} wallpaper(s) loaded, {released} released",
                t0.elapsed().as_secs_f64() * 1000.0
            );
        }
        self.publish(running.as_ref(), Change::State);
        Ok(())
    }

    /// After a target change without new surfaces: every output shows its new target.
    fn rebind_outputs(&mut self, r: &mut Running) {
        let mut i = 0;
        while i < r.outputs.len() {
            let name = r.outputs[i].name.clone();
            let Some(t) = target_for(&self.plan, &name) else {
                i += 1;
                continue;
            };
            if r.scenes[t].is_none() && r.videos[t].is_none() {
                let size = r.outputs[i].size;
                match build_renderer(
                    r,
                    &mut self.wallpapers[t],
                    &self.plan.targets[t],
                    &self.plan,
                    &self.waker,
                    size,
                ) {
                    Ok(Renderer::Scene(s)) => r.scenes[t] = Some(*s),
                    Ok(Renderer::Video(v)) => r.videos[t] = Some(*v),
                    Err(e) => {
                        tracing::error!("{name}: {e}");
                        self.states[t] = TargetState::Error(e);
                        i += 1;
                        continue;
                    }
                }
                self.states[t] = TargetState::Running;
            }
            let Some(source) = target_source(&r.videos, &r.scenes, t) else {
                i += 1;
                continue;
            };
            let tplan = &self.plan.targets[t];
            let settings = ComposeSettings {
                scaling: tplan.scaling,
                clamp: tplan.clamp,
                flip_vertical: false,
            };
            let generation = r.videos[t].as_ref().map_or(0, |v| v.player.generation());
            let o = &mut r.outputs[i];
            r.compositor
                .rebind(&r.ctx, &mut o.composer, source, settings);
            o.target = t;
            o.uv = UvRange::default();
            o.dirty = true;
            o.shown_serial = u64::MAX;
            o.video_generation = generation;
            if !o.paused {
                request(r.surfaces.as_mut(), o);
            }
            tracing::info!("output {name} shows '{}'", self.wallpapers[t].project.title);
            i += 1;
        }
    }

    /// New sound player for the current wallpapers and audio settings.
    fn rebuild_audio(&self, r: &mut Running) {
        let spectrum = self
            .wallpapers
            .iter()
            .any(|w| w.project.supports_audio_processing);
        let mut audio = Audio::new(&self.plan.audio, self.plan.fps, spectrum);
        for wp in &self.wallpapers {
            let objects = wp.raw_objects();
            audio.add_scene_sounds(&wp.vfs, objects.iter());
        }
        for v in r.videos.iter_mut().flatten() {
            v.attach_sound(&mut audio, &self.plan);
        }
        if self.user_paused {
            audio.set_paused(true);
        }
        let quiet =
            self.user_paused || (!r.outputs.is_empty() && r.outputs.iter().all(|o| o.paused));
        if !quiet {
            audio.resume();
        }
        r.audio = audio;
    }

    /// Rewrites the monitor's report from the current state (called on changes only).
    fn publish(&self, r: Option<&Running>, change: Change) {
        if let Some(r) = r {
            // Also right for scenes that draw no frame (static): counters keep the last value.
            let video: u64 = r
                .videos
                .iter()
                .flatten()
                .map(|v| v.player.gpu_bytes())
                .sum();
            self.monitor.set_gpu_memory(r.ctx.memory().bytes() + video);
        }
        let outputs = self
            .known
            .iter()
            .map(|k| {
                let shown = k.live && r.is_some();
                let out = r.and_then(|r| r.outputs.iter().find(|o| o.name == k.info.name));
                OutputReport {
                    name: k.info.name.clone(),
                    description: k.info.description.clone(),
                    position: k.info.position,
                    size: k.info.physical_size,
                    scale: k.info.scale,
                    refresh_mhz: k.info.refresh_mhz,
                    shown,
                    target: if shown {
                        target_for(&self.plan, &k.info.name)
                    } else {
                        None
                    },
                    paused: out.filter(|o| o.paused).map(|o| pause_cause(o.reasons)),
                }
            })
            .collect();
        let targets = self
            .plan
            .targets
            .iter()
            .zip(&self.wallpapers)
            .zip(&self.states)
            .map(|((t, wp), state)| TargetReport {
                key: t.key.clone(),
                outputs: t.outputs.clone(),
                wallpaper: t.wallpaper.clone(),
                title: wp.project.title.clone(),
                scaling: t.scaling,
                state: state.clone(),
                properties: wp.property_values(),
            })
            .collect();
        self.monitor.publish(
            EngineReport {
                active: r.is_some(),
                user_paused: self.user_paused,
                outputs,
                targets,
            },
            change,
        );
    }

    /// Records (or refreshes) a platform output in the known list.
    fn note_output(&mut self, r: &Running, id: OutputId) {
        let Some(info) = r.surfaces.output(id) else {
            return;
        };
        match self.known.iter_mut().find(|k| k.info.name == info.name) {
            Some(k) => {
                k.info = info.clone();
                k.live = true;
            }
            None => {
                self.known.push(Known {
                    info: info.clone(),
                    live: true,
                });
                self.known.sort_by(|a, b| a.info.name.cmp(&b.info.name));
            }
        }
    }

    fn handle_event(
        &mut self,
        r: &mut Running,
        ev: Event,
        first_surface: &mut Option<(OutputId, wgpu::Surface<'static>)>,
    ) -> Result<(), String> {
        match ev {
            Event::OutputAdded(id) => {
                self.note_output(r, id);
                let result = self.add_output(r, id, first_surface);
                self.publish(Some(r), Change::Outputs);
                result?;
            }
            Event::OutputRemoved(id) => {
                r.outputs.retain(|o| o.id != id);
                r.pointer.remove_output(id);
                self.known.retain(|k| !(k.live && k.info.id == id));
                sync_video_pause(r);
                self.publish(Some(r), Change::Outputs);
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
                self.note_output(r, output);
                self.publish(Some(r), Change::Outputs);
            }
            Event::Paused { output, reasons } => {
                if let Some(o) = r.outputs.iter_mut().find(|o| o.id == output) {
                    if !o.paused {
                        tracing::info!("output {output} paused: {reasons:?}");
                    }
                    o.paused = true;
                    o.reasons = reasons;
                }
                if r.outputs.iter().all(|o| o.paused) {
                    r.audio.pause();
                    r.audio.update(0.0, true);
                }
                sync_video_pause(r);
                self.publish(Some(r), Change::State);
            }
            Event::Resumed(output) => {
                let all_paused = r.outputs.iter().all(|o| o.paused);
                if let Some(o) = r.outputs.iter_mut().find(|o| o.id == output) {
                    tracing::info!("output {output} resumed");
                    o.paused = false;
                    o.reasons = PauseReasons::NONE;
                    o.dirty = true;
                    request(r.surfaces.as_mut(), o);
                }
                if all_paused && !self.user_paused {
                    r.audio.resume();
                }
                sync_video_pause(r);
                self.publish(Some(r), Change::State);
            }
            Event::Frame(id) => self.render(r, id),
            Event::Pointer(p) => {
                let h = r
                    .outputs
                    .iter()
                    .find(|o| o.id == p.output)
                    .map_or(0.0, |o| f64::from(o.size.1));
                r.pointer.handle(&p, h);
                for o in &mut r.outputs {
                    let uses_pointer = r
                        .scenes
                        .get(o.target)
                        .and_then(Option::as_ref)
                        .is_some_and(|s| s.runtime.uses_pointer);
                    if o.id == p.output && uses_pointer && !o.paused {
                        request(r.surfaces.as_mut(), o);
                    }
                }
            }
            Event::ScaleChanged { output, .. } | Event::Moved { output, .. } => {
                self.note_output(r, output);
                self.publish(Some(r), Change::Outputs);
            }
            Event::Wake | Event::CloseRequested => {}
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
        let Some(target) = target_for(&self.plan, &info.name) else {
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
        if r.videos[target].is_none() && r.scenes[target].is_none() {
            match build_renderer(
                r,
                &mut self.wallpapers[target],
                &self.plan.targets[target],
                &self.plan,
                &self.waker,
                size,
            ) {
                Ok(Renderer::Video(mut v)) => {
                    v.attach_sound(&mut r.audio, &self.plan);
                    r.videos[target] = Some(*v);
                }
                Ok(Renderer::Scene(s)) => r.scenes[target] = Some(*s),
                Err(e) => {
                    self.states[target] = TargetState::Error(e.clone());
                    return Err(e);
                }
            }
            self.states[target] = TargetState::Running;
        }
        let Some(source) = target_source(&r.videos, &r.scenes, target) else {
            return Ok(());
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
            name: info.name.clone(),
            target,
            surface,
            composer,
            size,
            uv: UvRange::default(),
            paused: !reasons.is_empty(),
            reasons,
            held: self.user_paused,
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

    /// GPU timer while someone measures (IPC statistics or `LUMENWALL_STATS`), none otherwise.
    fn update_timer(&self, r: &mut Running) {
        let wanted = r.stats.enabled || self.monitor.sampling();
        if wanted && r.timer.is_none() && !r.timer_tried {
            r.timer = GpuTimer::new(&r.ctx);
            r.timer_tried = true;
        } else if !wanted && r.timer_tried {
            r.timer = None;
            r.timer_tried = false;
        }
    }

    /// Frame counters for the monitor (only while sampling).
    fn sample_frame(&self, r: &Running, oi: usize, cpu: Duration, gpu: Option<Duration>) {
        if !self.monitor.sampling() {
            return;
        }
        self.monitor.record_frame(&r.outputs[oi].name, cpu, gpu);
        let video: u64 = r
            .videos
            .iter()
            .flatten()
            .map(|v| v.player.gpu_bytes())
            .sum();
        self.monitor.set_gpu_memory(r.ctx.memory().bytes() + video);
    }

    fn render(&mut self, r: &mut Running, id: OutputId) {
        let Some(oi) = r.outputs.iter().position(|o| o.id == id) else {
            return;
        };
        r.outputs[oi].frame_requested = false;
        if r.outputs[oi].paused || (r.outputs[oi].held && !r.outputs[oi].dirty) {
            return;
        }
        self.update_timer(r);
        let start = Instant::now();
        let target = r.outputs[oi].target;
        if r.videos[target].is_some() {
            if let Some((cpu, gpu)) = render_video(r, oi, start) {
                self.sample_frame(r, oi, cpu, gpu);
            }
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
        let mut measured = None;
        if redraw || o.dirty {
            match o.surface.acquire(&r.ctx) {
                Ok(Some(frame)) => {
                    let mut enc =
                        r.ctx
                            .device()
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("lumenwall.frame"),
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
                    measured = Some((cpu, gpu));
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("output {id}: {e}"),
            }
            if r.screenshot
                .as_ref()
                .is_some_and(|(_, delay)| r.rendered > u64::from(*delay))
                && let Some((path, _)) = r.screenshot.take()
            {
                // spec 07 §18: the wallpaper image (`_rt_FullFrameBuffer`). Lumenwall writes PNG
                // data whatever the extension (BMP/JPEG encoders are not linked).
                let rt = slot.runtime.scene.targets().get(slot.runtime.full);
                write_screenshot(&r.ctx, rt, &path);
            }
        }
        let animated = slot.runtime.animated || r.audio.spectrum_enabled();
        if animated || slot.runtime.scene.needs_redraw() {
            request(r.surfaces.as_mut(), o);
        }
        if let Some((cpu, gpu)) = measured {
            self.sample_frame(r, oi, cpu, gpu);
        }
    }
}

fn log_loaded(key: &str, wp: &Wallpaper) {
    tracing::info!(
        "{key}: '{}' ({:?}, {} propert{})",
        wp.project.title,
        wp.project.kind,
        wp.project.properties.len(),
        if wp.project.properties.len() == 1 {
            "y"
        } else {
            "ies"
        }
    );
}

/// Target shown on the output `name` under `plan`.
fn target_for(plan: &EnginePlan, name: &str) -> Option<usize> {
    if plan.targets.is_empty() {
        return None;
    }
    match plan.mode {
        OutputMode::Window | OutputMode::ExplicitWindow { .. } => Some(0),
        OutputMode::Desktop => plan
            .targets
            .iter()
            .position(|t| t.outputs.iter().any(|o| o == name))
            // The default wallpaper (no output names) covers the other outputs.
            .or_else(|| plan.targets.iter().position(|t| t.outputs.is_empty())),
    }
}

/// Whether the current surfaces cannot serve `plan` (other mode, layer, pause policy or
/// pointer setting; an output must disappear or appear).
fn needs_reopen(r: &Running, want: &Config, plan: &EnginePlan, known: &[Known]) -> bool {
    let cur = &r.platform;
    if cur.mode != want.mode
        || cur.layer != want.layer
        || cur.fullscreen != want.fullscreen
        || cur.pointer != want.pointer
    {
        return true;
    }
    if !plan.targets.is_empty()
        && known
            .iter()
            .any(|k| k.live && target_for(plan, &k.info.name).is_none())
    {
        return true;
    }
    match (&cur.outputs, &want.outputs) {
        (OutputSelection::All, _) => false,
        (OutputSelection::Named(_), OutputSelection::All) => true,
        (OutputSelection::Named(c), OutputSelection::Named(w)) => w.iter().any(|n| !c.contains(n)),
    }
}

/// Builds the scene or opens the video of a target (no sound: the caller attaches it).
fn build_renderer(
    r: &mut Running,
    wp: &mut Wallpaper,
    tplan: &TargetPlan,
    plan: &EnginePlan,
    waker: &EngineWaker,
    size: (u32, u32),
) -> Result<Renderer, String> {
    let t0 = Instant::now();
    if wp.kind() == lumenwall_scene::ProjectKind::Video {
        let slot = VideoSlot::open(&r.ctx, wp, plan, Some(notify_from(waker.clone())))?;
        tracing::info!(
            "video wallpaper '{}' {}x{} loaded in {:.0} ms; GPU memory tracked {:.1} MiB",
            wp.project.title,
            slot.player.size().0,
            slot.player.size().1,
            t0.elapsed().as_secs_f64() * 1000.0,
            r.ctx.memory().bytes() as f64 / 1048576.0,
        );
        return Ok(Renderer::Video(Box::new(slot)));
    }
    // One provider per scene: the shader ids it hands out belong to that scene's builder
    // (translations still come from the on-disk cache).
    let mut shaders = default_provider();
    let runtime = build_scene(
        &r.ctx,
        &mut wp.project,
        &wp.vfs,
        shaders.as_mut(),
        &SceneOptions {
            output_size: size,
            clamp: tplan.clamp,
            particles: plan.particles,
            parallax: plan.parallax,
            debug: plan.debug.clone(),
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
    Ok(Renderer::Scene(Box::new(SceneSlot {
        runtime,
        updated_at: u64::MAX,
        last_time: 0.0,
    })))
}

/// Image an output composes for target `t`.
fn target_source<'a>(
    videos: &'a [Option<VideoSlot>],
    scenes: &'a [Option<SceneSlot>],
    t: usize,
) -> Option<&'a RenderTarget> {
    match (&videos[t], &scenes[t]) {
        (Some(v), _) => Some(v.player.output()),
        (None, Some(slot)) => Some(slot.runtime.scene.targets().get(slot.runtime.full)),
        (None, None) => None,
    }
}

fn pause_cause(reasons: PauseReasons) -> PauseCause {
    if reasons.contains(PauseReasons::LOCKED) {
        PauseCause::Locked
    } else if reasons.contains(PauseReasons::FULLSCREEN) {
        PauseCause::Fullscreen
    } else {
        PauseCause::Hidden
    }
}

fn write_screenshot(ctx: &GpuContext, rt: &RenderTarget, path: &std::path::Path) {
    match lumenwall_render::headless::read_texture_rgba8(ctx, rt.texture()) {
        Ok(px) => {
            let (w, h) = rt.size();
            match lumenwall_render::headless::write_png(path, w, h, &px) {
                Ok(()) => tracing::info!("screenshot written to {}", path.display()),
                Err(e) => tracing::error!("screenshot: {e}"),
            }
        }
        Err(e) => tracing::error!("screenshot: {e}"),
    }
}

/// One frame of a video wallpaper on one output: the newest due frame is uploaded (or
/// bound, zero copy) and converted once per loop iteration, then composed like a scene.
/// Returns the CPU and GPU time when a frame was presented.
fn render_video(
    r: &mut Running,
    oi: usize,
    start: Instant,
) -> Option<(Duration, Option<Duration>)> {
    let id = r.outputs[oi].id;
    let target = r.outputs[oi].target;
    let v = r.videos[target].as_mut()?;
    if v.updated_at != r.iteration {
        v.updated_at = r.iteration;
        v.player.update(&r.ctx, Instant::now());
    }
    let o = &mut r.outputs[oi];
    let convert = v.player.needs_redraw();
    if !convert && !o.dirty && o.shown_serial == v.serial {
        return None;
    }
    let mut measured = None;
    match o.surface.acquire(&r.ctx) {
        Ok(Some(frame)) => {
            let mut enc = r
                .ctx
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("lumenwall.frame.video"),
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
            let cpu = start.elapsed();
            let gpu = r.timer.as_mut().and_then(|t| t.poll(&r.ctx));
            record_stats(&mut r.stats, cpu, gpu, &r.ctx, 0);
            measured = Some((cpu, gpu));
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("output {id}: {e}"),
    }
    if r.screenshot
        .as_ref()
        .is_some_and(|(_, delay)| r.rendered > u64::from(*delay))
        && let Some((path, _)) = r.screenshot.take()
    {
        write_screenshot(&r.ctx, v.player.output(), &path);
    }
    measured
}

/// A video is paused while every output showing it is paused (spec 07 §9.4), by the
/// platform or by the user.
fn sync_video_pause(r: &mut Running) {
    for (target, slot) in r.videos.iter_mut().enumerate() {
        let Some(v) = slot else {
            continue;
        };
        let mut outputs = r.outputs.iter().filter(|o| o.target == target).peekable();
        let paused = outputs.peek().is_none() || outputs.all(|o| o.paused || o.held);
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
                .any(|o| o.target == target && !o.paused && !o.held && !o.frame_requested);
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
                .filter(|o| o.target == target && !o.paused && !o.held)
            {
                request(r.surfaces.as_mut(), o);
            }
        }
    }
}

/// Asks the platform for a frame, unless one is pending or the output is held by a user
/// pause with nothing new to show.
fn request(s: &mut dyn WallpaperSurface, o: &mut Output) {
    if !o.frame_requested && (!o.held || o.dirty) {
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
    // SAFETY: see `create_wgpu_surface`; when the surfaces are reopened, every GPU surface of
    // the old backend is dropped before it.
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
    use crate::plan::TargetPlan;
    use lumenwall_render::{ClampMode, ScalingMode};

    #[test]
    fn waker_before_the_platform_is_remembered() {
        let w = EngineWaker::default();
        w.clone().wake();
        assert!(w.lock().pending);
        // The idle wait returns at once and consumes the wake-up.
        w.idle_wait();
        assert!(!w.lock().pending);
    }

    #[test]
    fn idle_wait_returns_on_wake_from_another_thread() {
        let w = EngineWaker::default();
        let other = w.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            other.wake();
        });
        w.idle_wait();
        t.join().unwrap();
    }

    #[test]
    fn daytime_is_a_fraction_of_a_day() {
        let d = daytime();
        assert!((0.0..1.0).contains(&d));
    }

    fn target(key: &str, outputs: &[&str]) -> TargetPlan {
        TargetPlan {
            key: key.into(),
            outputs: outputs.iter().map(|s| (*s).to_owned()).collect(),
            wallpaper: PathBuf::from("/w").join(key),
            scaling: ScalingMode::Fill,
            clamp: ClampMode::Clamp,
        }
    }

    #[test]
    fn outputs_map_to_their_target_or_the_default_one() {
        let mut plan = EnginePlan {
            mode: OutputMode::Desktop,
            targets: vec![target("DP-1", &["DP-1"]), target("default", &[])],
            ..EnginePlan::default()
        };
        assert_eq!(target_for(&plan, "DP-1"), Some(0));
        assert_eq!(target_for(&plan, "DP-2"), Some(1));
        plan.targets.pop();
        // Named outputs only: the others show nothing.
        assert_eq!(target_for(&plan, "DP-2"), None);
        plan.mode = OutputMode::Window;
        assert_eq!(target_for(&plan, "default"), Some(0));
        plan.targets.clear();
        assert_eq!(target_for(&plan, "default"), None);
    }

    #[test]
    fn platform_selection_follows_the_targets() {
        let mut plan = EnginePlan {
            mode: OutputMode::Desktop,
            targets: vec![target("default", &[])],
            ..EnginePlan::default()
        };
        assert_eq!(Engine::platform_config(&plan).outputs, OutputSelection::All);
        plan.targets = vec![target("DP-1", &["DP-1"]), target("DP-2", &["DP-2"])];
        assert_eq!(
            Engine::platform_config(&plan).outputs,
            OutputSelection::Named(vec!["DP-1".into(), "DP-2".into()])
        );
    }
}

//! The daemon side of the IPC protocol (`lumenwall-ipc`): answers clients from snapshots
//! (never waking the engine for a read), turns their choices into a new configuration that
//! is written atomically to the configuration file and applied live through
//! [`Control::Reload`] (the same path as a hand edit of the file), and samples statistics
//! only while a client subscribes to them.
//!
//! Choices made through IPC replace the command-line values they concern for the rest of the
//! session (the command line wins at start, the client afterwards), so that the file written
//! now gives the same result at the next session start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use lumenwall_ipc::{
    ErrorCode, Event, Handler, Output, OutputState, OutputStats, PauseReason, PropertyMap,
    PropertyValue, Publisher, RemoteError, Scaling as IpcScaling, Settings, State, Stats, Topic,
    WallpaperAudio, WallpaperStatus, wallpaper_key,
};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::cli::{CliArgs, CliLayout, CliMode};
use crate::config::{self, Config, LogConfig};
use crate::logging::LogFilterHandle;
use crate::plan::{self, ExecutionPlan, OutputMode, PlanServices, TargetPlan, WallpaperLevel};
use crate::services::{
    Control, ControlSender, EngineSnapshot, Monitor, MonitorChange, PlaylistCatalog, StopReason,
    TargetStatus, WallpaperResolver,
};
use crate::settings::{
    Scaling, WallpaperRef, WallpaperSource, volume_fraction, volume_from_fraction,
};
use crate::watch::FileChange;

/// How long a client request waits for the engine to apply a new plan.
const APPLY_TIMEOUT: Duration = Duration::from_millis(4500);
/// Statistics period while a client subscribes.
const STATS_PERIOD: Duration = Duration::from_secs(1);

/// Everything the daemon starts from.
pub struct DaemonInit {
    pub cli: Box<CliArgs>,
    pub config_path: Option<PathBuf>,
    pub config_bytes: Option<Vec<u8>>,
    pub config: Option<Config>,
    pub plan: Arc<ExecutionPlan>,
    pub resolver: Arc<dyn WallpaperResolver>,
    pub playlists: Arc<dyn PlaylistCatalog>,
    pub control: ControlSender,
    pub monitor: Option<Arc<dyn Monitor>>,
    pub log_filter: Option<LogFilterHandle>,
    pub log_config: LogConfig,
}

/// The configuration as the daemon sees it.
struct Core {
    cli: Box<CliArgs>,
    path: Option<PathBuf>,
    /// Bytes on disk (read or written by us): a change notification with these is ours.
    file_bytes: Option<Vec<u8>>,
    doc: DocumentMut,
    plan: Arc<ExecutionPlan>,
    /// Unsaved property values (slider being dragged).
    preview: BTreeMap<String, String>,
    log_config: LogConfig,
}

/// A configuration being changed by a request.
struct Draft {
    cli: Box<CliArgs>,
    doc: DocumentMut,
    preview: BTreeMap<String, String>,
}

pub struct Daemon {
    resolver: Arc<dyn WallpaperResolver>,
    playlists: Arc<dyn PlaylistCatalog>,
    control: ControlSender,
    monitor: Option<Arc<dyn Monitor>>,
    log_filter: Option<LogFilterHandle>,
    /// Serializes changes (requests and file reloads); held while waiting for the engine.
    ops: Mutex<()>,
    /// Short-lived lock, never held while waiting.
    core: Mutex<Core>,
    /// Client pause, kept here so that `get_state` answers at once.
    user_paused: Mutex<bool>,
    publisher: OnceLock<Publisher>,
    stats: Mutex<Sampler>,
    stats_wake: Condvar,
    me: Weak<Daemon>,
}

#[derive(Default)]
struct Sampler {
    /// Subscribers to the statistics.
    wanted: bool,
    running: bool,
    last: Option<Stats>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn invalid(message: impl Into<String>) -> RemoteError {
    RemoteError::new(ErrorCode::InvalidRequest, message)
}

fn internal(message: impl Into<String>) -> RemoteError {
    RemoteError::new(ErrorCode::Internal, message)
}

impl Daemon {
    pub fn new(init: DaemonInit) -> Arc<Self> {
        let doc = init
            .config_bytes
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok())
            .and_then(|t| t.parse::<DocumentMut>().ok())
            .unwrap_or_default();
        let daemon = Arc::new_cyclic(|me| Daemon {
            resolver: init.resolver,
            playlists: init.playlists,
            control: init.control,
            monitor: init.monitor,
            log_filter: init.log_filter,
            ops: Mutex::new(()),
            core: Mutex::new(Core {
                cli: init.cli,
                path: init.config_path,
                file_bytes: init.config_bytes,
                doc,
                plan: init.plan,
                preview: BTreeMap::new(),
                log_config: init.log_config,
            }),
            user_paused: Mutex::new(false),
            publisher: OnceLock::new(),
            stats: Mutex::new(Sampler::default()),
            stats_wake: Condvar::new(),
            me: me.clone(),
        });
        let _ = init.config;
        if let Some(m) = &daemon.monitor {
            let weak = Arc::downgrade(&daemon);
            m.set_listener(Some(Arc::new(move |change| {
                if let Some(d) = weak.upgrade() {
                    d.engine_changed(change);
                }
            })));
        }
        daemon
    }

    /// The IPC handler (keep the daemon alive as long as the server).
    pub fn handler(self: &Arc<Self>) -> Arc<dyn Handler> {
        Arc::new(DaemonHandler(Arc::clone(self)))
    }

    /// Events go to the clients of this server from now on.
    pub fn set_publisher(&self, publisher: Publisher) {
        let _ = self.publisher.set(publisher);
    }

    /// Drops the engine listener (end of run).
    pub fn detach(&self) {
        if let Some(m) = &self.monitor {
            m.set_listener(None);
            m.set_sampling(false);
        }
        lock(&self.stats).wanted = false;
        self.stats_wake.notify_all();
    }

    fn services(&self) -> PlanServices<'_> {
        PlanServices {
            resolver: &*self.resolver,
            playlists: &*self.playlists,
        }
    }

    fn snapshot(&self) -> EngineSnapshot {
        self.monitor
            .as_ref()
            .map(|m| m.snapshot())
            .unwrap_or_default()
    }

    fn publish(&self, event: Event) {
        if let Some(p) = self.publisher.get() {
            p.publish(&event);
        }
    }

    fn has_subscribers(&self, topic: Topic) -> bool {
        self.publisher
            .get()
            .is_some_and(|p| p.has_subscribers(topic))
    }

    /// Engine thread: something changed in the snapshot. Only cheap work, no core lock.
    fn engine_changed(&self, change: MonitorChange) {
        match change {
            MonitorChange::Outputs => {
                if self.has_subscribers(Topic::Outputs) {
                    self.publish(Event::Outputs {
                        outputs: self.outputs(),
                    });
                }
                self.publish_state();
            }
            MonitorChange::State => self.publish_state(),
            MonitorChange::Properties => {
                if self.has_subscribers(Topic::Properties) {
                    let snap = self.snapshot();
                    for o in &snap.outputs {
                        if let Some(t) = o.target.and_then(|t| snap.targets.get(t)) {
                            self.publish(Event::Properties {
                                output: o.info.id.clone(),
                                values: t.properties.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    fn publish_state(&self) {
        if self.has_subscribers(Topic::State) {
            self.publish(Event::State(self.state()));
        }
    }

    fn publish_settings(&self) {
        if self.has_subscribers(Topic::Settings) {
            self.publish(Event::Settings(self.settings()));
        }
    }

    // ----- reads -----

    fn outputs(&self) -> Vec<Output> {
        self.snapshot()
            .outputs
            .into_iter()
            .map(|o| o.info)
            .collect()
    }

    fn state(&self) -> State {
        let snap = self.snapshot();
        let user_paused = *lock(&self.user_paused);
        let audio = lock(&self.core).plan.audio.clone();
        let shown: Vec<_> = snap.outputs.iter().filter(|o| o.target.is_some()).collect();
        let platform_pause = (!shown.is_empty() && shown.iter().all(|o| o.paused.is_some()))
            .then(|| shown[0].paused)
            .flatten();
        let outputs = snap
            .outputs
            .iter()
            .map(|o| {
                let target = o.target.and_then(|t| snap.targets.get(t));
                let (status, message) = match target.map(|t| &t.status) {
                    None => (WallpaperStatus::Empty, None),
                    Some(TargetStatus::Loading) => (WallpaperStatus::Loading, None),
                    Some(TargetStatus::Error(e)) => (WallpaperStatus::Error, Some(e.clone())),
                    Some(TargetStatus::Running) if user_paused || o.paused.is_some() => {
                        (WallpaperStatus::Paused, None)
                    }
                    Some(TargetStatus::Running) => (WallpaperStatus::Running, None),
                };
                OutputState {
                    output: o.info.id.clone(),
                    wallpaper: target.map(|t| t.wallpaper.clone()),
                    scaling: target.map_or(IpcScaling::Fill, |t| t.scaling),
                    status,
                    message,
                }
            })
            .collect();
        State {
            paused: user_paused || platform_pause.is_some(),
            pause_reason: if user_paused {
                Some(PauseReason::User)
            } else {
                platform_pause
            },
            muted: audio.silent,
            volume: volume_fraction(audio.volume),
            outputs,
            wallpaper_audio: Some(
                audio
                    .wallpapers
                    .iter()
                    .map(|(key, level)| {
                        let level = WallpaperAudio {
                            volume: f32::from(level.percent) / 100.0,
                            muted: level.muted,
                        };
                        (key.clone(), level)
                    })
                    .collect(),
            ),
        }
    }

    fn settings(&self) -> Settings {
        let plan = lock(&self.core).plan.clone();
        Settings {
            fps_limit: plan.fps,
            pause_on_fullscreen: plan.fullscreen_pause.enabled,
            pause_on_battery: false,
            pause_on_lock: true,
            automute: plan.audio.automute,
            muted: plan.audio.silent,
            volume: volume_fraction(plan.audio.volume),
            autostart: autostart_enabled(),
        }
    }

    fn properties(&self, output: &str) -> Result<PropertyMap, RemoteError> {
        let snap = self.snapshot();
        match snap.target_of(output) {
            Some(t) => Ok(t.properties.clone()),
            None if snap.outputs.iter().any(|o| o.info.id == output) => Ok(PropertyMap::new()),
            None => Err(RemoteError::new(ErrorCode::UnknownOutput, output)),
        }
    }

    // ----- changes -----

    /// Applies `edit` to a copy of the configuration, builds the plan, has the engine apply
    /// it, then keeps and writes the new configuration. Nothing changes on error.
    fn transact(
        &self,
        edit: impl FnOnce(&mut Draft, &ExecutionPlan, &EngineSnapshot) -> Result<(), RemoteError>,
    ) -> Result<(), RemoteError> {
        let _ops = lock(&self.ops);
        let snapshot = self.snapshot();
        let (draft, text, new_plan, changes) = {
            let core = lock(&self.core);
            let mut draft = Draft {
                cli: core.cli.clone(),
                doc: core.doc.clone(),
                preview: core.preview.clone(),
            };
            edit(&mut draft, &core.plan, &snapshot)?;
            let text = draft.doc.to_string();
            let config =
                Config::parse(&text).map_err(|e| invalid(format!("configuration: {e}")))?;
            let new_plan = plan::build_with(&draft.cli, Some(&config), self.services(), true)
                .map_err(|e| RemoteError::new(ErrorCode::NotFound, e.to_string()))?;
            let changes = plan::diff(&core.plan, &new_plan);
            (draft, text, new_plan, changes)
        };
        if !changes.is_empty() {
            tracing::info!("client change: {changes:?}");
            self.reload_engine(&new_plan, changes, &draft.preview, true)?;
        }
        let (path, bytes) = {
            let mut core = lock(&self.core);
            core.cli = draft.cli;
            core.doc = draft.doc;
            core.preview = draft.preview;
            core.plan = Arc::new(new_plan);
            let bytes = text.into_bytes();
            if core.file_bytes.as_ref() == Some(&bytes) {
                (None, bytes)
            } else {
                core.file_bytes = Some(bytes.clone());
                (core.path.clone(), bytes)
            }
        };
        match path {
            Some(path) => {
                if let Err(e) = write_atomic(&path, &bytes) {
                    tracing::error!("cannot save the configuration {}: {e}", path.display());
                } else {
                    tracing::info!("configuration saved to {}", path.display());
                }
            }
            None if lock(&self.core).path.is_none() => {
                tracing::debug!("--no-config: client choices are not saved");
            }
            None => {}
        }
        self.publish_settings();
        self.publish_state();
        Ok(())
    }

    /// Sends a plan to the engine (with the unsaved previews) and, with `wait`, waits for it.
    fn reload_engine(
        &self,
        new_plan: &ExecutionPlan,
        changes: Vec<plan::PlanChange>,
        preview: &BTreeMap<String, String>,
        wait: bool,
    ) -> Result<(), RemoteError> {
        let mut sent = new_plan.clone();
        sent.properties
            .extend(preview.iter().map(|(k, v)| (k.clone(), v.clone())));
        let (tx, rx) = mpsc::sync_channel(1);
        let message = Control::Reload {
            plan: Arc::new(sent),
            changes,
            done: wait.then_some(tx),
        };
        if !self.control.send(message) {
            return Err(internal("the engine has stopped"));
        }
        if !wait {
            return Ok(());
        }
        match rx.recv_timeout(APPLY_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(RemoteError::new(ErrorCode::UnsupportedWallpaper, e)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!("the engine is still applying the change");
                Ok(())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Engines without plan support drop the acknowledgement: applied as far as
                // they can.
                Ok(())
            }
        }
    }

    fn apply(
        &self,
        output: Option<&str>,
        wallpaper: &Path,
        scaling: IpcScaling,
    ) -> Result<(), RemoteError> {
        let dir = wallpaper_dir(wallpaper)?;
        let text = dir
            .to_str()
            .ok_or_else(|| invalid("the wallpaper path is not valid UTF-8"))?
            .to_owned();
        let scaling = from_ipc_scaling(scaling);
        self.transact(|d, plan, snap| {
            let known = known_outputs(snap);
            if let Some(name) = output
                && !known.iter().any(|k| k == name)
                && !plan
                    .targets
                    .iter()
                    .any(|t| t.outputs.iter().any(|o| o == name))
            {
                return Err(RemoteError::new(ErrorCode::UnknownOutput, name));
            }
            let content = Content::Wallpaper(text);
            match output {
                Some(name) if !single_output_layout(plan, &known) => {
                    let mut entries = current_entries(plan, &known);
                    let clamp = entries
                        .iter()
                        .find(|e| e.outputs.iter().any(|o| o == name))
                        .and_then(|e| e.clamp.clone());
                    remove_output(&mut entries, name);
                    entries.push(Entry {
                        outputs: vec![name.to_owned()],
                        content,
                        scaling: Some(scaling),
                        clamp,
                    });
                    write_entries(&mut d.doc, entries);
                }
                _ => write_single(&mut d.doc, &content, scaling),
            }
            take_layout(&mut d.cli);
            Ok(())
        })
    }

    fn clear(&self, output: Option<&str>) -> Result<(), RemoteError> {
        self.transact(|d, plan, snap| {
            let known = known_outputs(snap);
            let entries = match output {
                Some(name) if !single_output_layout(plan, &known) => {
                    let mut entries = current_entries(plan, &known);
                    if !entries.iter().any(|e| e.outputs.iter().any(|o| o == name)) {
                        return Err(RemoteError::new(ErrorCode::UnknownOutput, name));
                    }
                    remove_output(&mut entries, name);
                    entries
                }
                _ => Vec::new(),
            };
            if entries.is_empty() {
                let t = d.doc.as_table_mut();
                for key in ["wallpaper", "playlist", "screen", "span"] {
                    t.remove(key);
                }
            } else {
                write_entries(&mut d.doc, entries);
            }
            take_layout(&mut d.cli);
            Ok(())
        })
    }

    fn set_properties(
        &self,
        output: &str,
        values: PropertyMap,
        persist: bool,
    ) -> Result<(), RemoteError> {
        let snap = self.snapshot();
        if snap.target_of(output).is_none() {
            return Err(RemoteError::new(ErrorCode::UnknownOutput, output));
        }
        let texts: Vec<(String, String)> = values
            .iter()
            .map(|(k, v)| (k.clone(), property_text(v)))
            .collect();
        if persist {
            return self.transact(|d, _, _| {
                let props = d
                    .doc
                    .as_table_mut()
                    .entry("properties")
                    .or_insert_with(|| Item::Table(Table::new()));
                let Some(table) = props.as_table_like_mut() else {
                    return Err(invalid("[properties] is not a table"));
                };
                for (k, v) in &values {
                    table.insert(k, property_toml(v));
                }
                d.cli
                    .settings
                    .properties
                    .retain(|(k, _)| !values.contains_key(k));
                d.preview.retain(|k, _| !values.contains_key(k));
                Ok(())
            });
        }
        let _ops = lock(&self.ops);
        let merged = {
            let mut core = lock(&self.core);
            core.preview.extend(texts);
            let mut all = core.plan.properties.clone();
            all.extend(core.preview.iter().map(|(k, v)| (k.clone(), v.clone())));
            all
        };
        if self.control.send(Control::Properties(merged)) {
            Ok(())
        } else {
            Err(internal("the engine has stopped"))
        }
    }

    fn reset_properties(&self) -> Result<(), RemoteError> {
        let _ops = lock(&self.ops);
        let saved = {
            let mut core = lock(&self.core);
            core.preview.clear();
            core.plan.properties.clone()
        };
        self.control.send(Control::Properties(saved));
        Ok(())
    }

    fn set_audio(&self, muted: Option<bool>, volume: Option<f32>) -> Result<(), RemoteError> {
        self.transact(|d, plan, _| {
            // Materialize the effective values, then drop the command-line overrides.
            let t = d.doc.as_table_mut();
            t.insert("silent", value(muted.unwrap_or(plan.audio.silent)));
            let v = volume.map_or(plan.audio.volume, volume_from_fraction);
            t.insert("volume", value(i64::from(v)));
            d.cli.settings.silent = false;
            d.cli.settings.volume = None;
            Ok(())
        })
    }

    /// Sound level of one wallpaper, saved as `[wallpaper-audio."<key>"]` (removed when back
    /// to the default) and applied live through the engine volume.
    fn set_wallpaper_audio(
        &self,
        wallpaper: &Path,
        volume: Option<f32>,
        muted: Option<bool>,
    ) -> Result<(), RemoteError> {
        let dir = wallpaper_folder(wallpaper)?;
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let key = wallpaper_key(&dir);
        self.transact(|d, plan, _| {
            let current = plan.audio.level(&dir);
            let level = WallpaperLevel {
                percent: volume.map_or(current.percent, |v| {
                    (v.clamp(0.0, 1.0) * 100.0).round() as u8
                }),
                muted: muted.unwrap_or(current.muted),
            };
            write_wallpaper_level(&mut d.doc, &key, level)
        })
    }

    fn set_settings(&self, new: Settings) -> Result<(), RemoteError> {
        let current = self.settings();
        let refuse = |message: &str| {
            // Clients that showed the change optimistically get the real settings back.
            self.publish_settings();
            Err(RemoteError::new(ErrorCode::Unsupported, message))
        };
        if new.pause_on_battery && !current.pause_on_battery {
            return refuse("pausing on battery is not supported yet");
        }
        if !new.pause_on_lock {
            return refuse("Lumenwall always pauses while the session is locked");
        }
        if new.fps_limit == 0 {
            return Err(invalid("fps_limit must be at least 1"));
        }
        self.transact(|d, plan, _| {
            let t = d.doc.as_table_mut();
            if new.fps_limit != current.fps_limit {
                t.insert("fps", value(i64::from(new.fps_limit)));
                d.cli.settings.fps = None;
            }
            if new.pause_on_fullscreen != current.pause_on_fullscreen {
                t.insert("fullscreen-pause", value(new.pause_on_fullscreen));
                d.cli.settings.no_fullscreen_pause = false;
            }
            if new.automute != current.automute {
                t.insert("automute", value(new.automute));
                d.cli.settings.no_automute = false;
            }
            if new.muted != current.muted || (new.volume - current.volume).abs() > 1e-3 {
                t.insert("silent", value(new.muted));
                let v = if (new.volume - current.volume).abs() > 1e-3 {
                    volume_from_fraction(new.volume)
                } else {
                    plan.audio.volume
                };
                t.insert("volume", value(i64::from(v)));
                d.cli.settings.silent = false;
                d.cli.settings.volume = None;
            }
            Ok(())
        })
    }

    fn set_paused(&self, paused: bool) -> Result<(), RemoteError> {
        *lock(&self.user_paused) = paused;
        if !self.control.send(Control::Pause(paused)) {
            return Err(internal("the engine has stopped"));
        }
        self.publish_state();
        Ok(())
    }

    /// The configuration file changed on disk (watcher thread).
    pub fn on_config_change(&self, path: &Path, change: FileChange) {
        let _ops = lock(&self.ops);
        let (config, bytes) = match change {
            FileChange::Contents(bytes) => {
                if lock(&self.core).file_bytes.as_ref() == Some(&bytes) {
                    return; // our own write
                }
                match config::parse_bytes(path, &bytes) {
                    Ok(config) => (Some(config), Some(bytes)),
                    Err(e) => {
                        tracing::error!("{e}; keeping the current settings");
                        return;
                    }
                }
            }
            FileChange::Removed => {
                tracing::info!(
                    "configuration file {} removed; using the defaults",
                    path.display()
                );
                (None, None)
            }
            FileChange::Unreadable(e) => {
                tracing::warn!("cannot read the configuration file {}: {e}", path.display());
                return;
            }
        };
        let mut core = lock(&self.core);
        let log = config.as_ref().map(|c| c.log.clone()).unwrap_or_default();
        if log.level != core.log_config.level
            && let Some(filter) = &self.log_filter
        {
            filter.set_level(log.level.as_deref());
        }
        if (&log.file, &log.directory, &log.max_files)
            != (
                &core.log_config.file,
                &core.log_config.directory,
                &core.log_config.max_files,
            )
        {
            tracing::warn!("log file settings change on the next start");
        }
        core.log_config = log;
        core.doc = bytes
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok())
            .and_then(|t| t.parse().ok())
            .unwrap_or_default();
        core.file_bytes = bytes;
        let allow_empty = core.plan.targets.is_empty() || core.cli.raw.len() <= 1;
        match plan::build_with(&core.cli, config.as_ref(), self.services(), allow_empty) {
            Ok(new_plan) => {
                let changes = plan::diff(&core.plan, &new_plan);
                if changes.is_empty() {
                    tracing::debug!("configuration reloaded, nothing to change");
                    return;
                }
                tracing::info!("configuration reloaded: {changes:?}");
                core.plan = Arc::new(new_plan);
                let (plan, preview) = (core.plan.clone(), core.preview.clone());
                drop(core);
                let _ = self.reload_engine(&plan, changes, &preview, false);
                self.publish_settings();
                self.publish_state();
            }
            Err(e) => tracing::error!("configuration not applied: {e}"),
        }
    }

    // ----- statistics -----

    fn subscribers_changed(&self, topic: Topic, count: usize) {
        if topic != Topic::Stats {
            return;
        }
        let start = {
            let mut s = lock(&self.stats);
            s.wanted = count > 0;
            if !s.wanted {
                s.last = None;
            }
            let start = s.wanted && !s.running;
            if start {
                s.running = true;
            }
            start
        };
        self.stats_wake.notify_all();
        if start {
            if let Some(m) = &self.monitor {
                m.set_sampling(true);
            }
            let me = self.me.clone();
            let spawned = std::thread::Builder::new()
                .name("lumenwall-ipc-stats".into())
                .spawn(move || stats_loop(me));
            if let Err(e) = spawned {
                tracing::warn!("statistics thread: {e}");
                lock(&self.stats).running = false;
            }
        }
    }

    /// One reading: frame counters since the previous one, process CPU and memory.
    fn sample(&self, base: &mut (Instant, Option<Duration>)) -> Stats {
        let counters = self
            .monitor
            .as_ref()
            .map(|m| m.take_counters())
            .unwrap_or_default();
        let now = Instant::now();
        let cpu = procstats::cpu_time();
        let wall = (now - base.0).as_secs_f64().max(1e-3);
        let cpu_percent = match (cpu, base.1) {
            (Some(c), Some(b)) => (c.saturating_sub(b).as_secs_f64() / wall * 100.0) as f32,
            _ => 0.0,
        };
        *base = (now, cpu);
        let elapsed = counters.elapsed.as_secs_f64().max(1e-3);
        let snap = self.snapshot();
        let user_paused = *lock(&self.user_paused);
        let outputs: Vec<OutputStats> = snap
            .outputs
            .iter()
            .filter(|o| o.shown)
            .map(|o| {
                let (frames, cpu) = counters
                    .outputs
                    .iter()
                    .find(|(n, ..)| *n == o.info.id)
                    .map_or((0, Duration::ZERO), |(_, f, c)| (*f, *c));
                OutputStats {
                    output: o.info.id.clone(),
                    fps: (f64::from(frames) / elapsed) as f32,
                    frame_time_ms: if frames > 0 {
                        (cpu.as_secs_f64() * 1000.0 / f64::from(frames)) as f32
                    } else {
                        0.0
                    },
                    rendering: o.target.is_some() && o.paused.is_none() && !user_paused,
                }
            })
            .collect();
        let frames: u32 = counters.outputs.iter().map(|(_, f, _)| f).sum();
        let cpu_total: Duration = counters.outputs.iter().map(|(_, _, c)| *c).sum();
        let fps = (f64::from(frames) / elapsed) as f32;
        let gpu_ms = (counters.gpu_samples > 0)
            .then(|| counters.gpu.as_secs_f64() * 1000.0 / f64::from(counters.gpu_samples));
        let stats = Stats {
            fps,
            frame_time_ms: if frames > 0 {
                (cpu_total.as_secs_f64() * 1000.0 / f64::from(frames)) as f32
            } else {
                0.0
            },
            cpu_percent,
            // Share of the time the GPU spends on Lumenwall's frames (timestamp queries).
            gpu_percent: gpu_ms.map(|g| (g * f64::from(fps) / 10.0) as f32),
            rss_bytes: procstats::resident_bytes().unwrap_or(0),
            vram_bytes: (counters.gpu_memory > 0).then_some(counters.gpu_memory),
            outputs,
        };
        tracing::debug!(
            "stats: {:.1} fps, CPU {:.2} ms/frame, GPU {}, process CPU {:.2} %, RSS {:.1} MiB, GPU memory {:.1} MiB",
            stats.fps,
            stats.frame_time_ms,
            gpu_ms.map_or("n/a".into(), |g| format!("{g:.2} ms/frame")),
            stats.cpu_percent,
            stats.rss_bytes as f64 / 1048576.0,
            counters.gpu_memory as f64 / 1048576.0,
        );
        stats
    }

    fn stats(&self) -> Result<Stats, RemoteError> {
        if let Some(last) = lock(&self.stats).last.clone() {
            return Ok(last);
        }
        // Nobody subscribed: measure one period now.
        let monitor_was = self.monitor.as_ref().map(|m| {
            let on = lock(&self.stats).running;
            m.set_sampling(true);
            let _ = m.take_counters();
            on
        });
        let mut base = (Instant::now(), procstats::cpu_time());
        std::thread::sleep(STATS_PERIOD);
        let stats = self.sample(&mut base);
        if let (Some(m), Some(false)) = (&self.monitor, monitor_was)
            && !lock(&self.stats).running
        {
            m.set_sampling(false);
        }
        Ok(stats)
    }
}

/// Publishes statistics once per period while someone subscribes, then stops sampling.
fn stats_loop(me: Weak<Daemon>) {
    let mut base = (Instant::now(), procstats::cpu_time());
    loop {
        let Some(d) = me.upgrade() else {
            return;
        };
        {
            let s = lock(&d.stats);
            let (s, _) = d
                .stats_wake
                .wait_timeout_while(s, STATS_PERIOD, |s| s.wanted)
                .unwrap_or_else(PoisonError::into_inner);
            if !s.wanted {
                drop(s);
                let mut s = lock(&d.stats);
                s.running = false;
                s.last = None;
                if let Some(m) = &d.monitor {
                    m.set_sampling(false);
                }
                return;
            }
        }
        let stats = d.sample(&mut base);
        lock(&d.stats).last = Some(stats.clone());
        d.publish(Event::Stats(stats));
    }
}

struct DaemonHandler(Arc<Daemon>);

impl Handler for DaemonHandler {
    fn outputs(&self) -> Result<Vec<Output>, RemoteError> {
        Ok(self.0.outputs())
    }
    fn state(&self) -> Result<State, RemoteError> {
        Ok(self.0.state())
    }
    fn apply(
        &self,
        output: Option<&str>,
        wallpaper: &Path,
        scaling: IpcScaling,
    ) -> Result<(), RemoteError> {
        self.0.apply(output, wallpaper, scaling)
    }
    fn clear(&self, output: Option<&str>) -> Result<(), RemoteError> {
        self.0.clear(output)
    }
    fn properties(&self, output: &str) -> Result<PropertyMap, RemoteError> {
        self.0.properties(output)
    }
    fn set_properties(
        &self,
        output: &str,
        values: PropertyMap,
        persist: bool,
    ) -> Result<(), RemoteError> {
        self.0.set_properties(output, values, persist)
    }
    fn reset_properties(&self, _output: &str) -> Result<(), RemoteError> {
        self.0.reset_properties()
    }
    fn set_paused(&self, paused: bool) -> Result<(), RemoteError> {
        self.0.set_paused(paused)
    }
    fn set_muted(&self, muted: bool) -> Result<(), RemoteError> {
        self.0.set_audio(Some(muted), None)
    }
    fn set_volume(&self, volume: f32) -> Result<(), RemoteError> {
        self.0.set_audio(None, Some(volume))
    }
    fn set_wallpaper_audio(
        &self,
        wallpaper: &Path,
        volume: Option<f32>,
        muted: Option<bool>,
    ) -> Result<(), RemoteError> {
        self.0.set_wallpaper_audio(wallpaper, volume, muted)
    }
    fn settings(&self) -> Result<Settings, RemoteError> {
        Ok(self.0.settings())
    }
    fn set_settings(&self, settings: Settings) -> Result<(), RemoteError> {
        self.0.set_settings(settings)
    }
    fn stats(&self) -> Result<Stats, RemoteError> {
        self.0.stats()
    }
    fn shutdown(&self) -> Result<(), RemoteError> {
        self.0.control.send(Control::Stop(StopReason::Client));
        Ok(())
    }
    fn subscribers_changed(&self, topic: Topic, count: usize) {
        self.0.subscribers_changed(topic, count);
    }
}

// ----- configuration edits -----

/// A wallpaper as the configuration names it.
#[derive(Clone, Debug, PartialEq)]
enum Content {
    Wallpaper(String),
    Playlist(String),
}

/// One `[[screen]]` (one output) or `[[span]]` (several).
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    outputs: Vec<String>,
    content: Content,
    scaling: Option<Scaling>,
    clamp: Option<String>,
}

fn known_outputs(snap: &EngineSnapshot) -> Vec<String> {
    snap.outputs.iter().map(|o| o.info.id.clone()).collect()
}

/// One wallpaper for everything: an explicit window, or a lone window output.
fn single_output_layout(plan: &ExecutionPlan, known: &[String]) -> bool {
    matches!(plan.mode, OutputMode::ExplicitWindow(_))
        || (plan.mode == OutputMode::Window && known.iter().all(|k| k == "default"))
}

fn content_of(t: &TargetPlan) -> Content {
    if let Some(p) = &t.playlist {
        return Content::Playlist(p.clone());
    }
    match &t.wallpaper.reference {
        WallpaperRef::Source(WallpaperSource::WorkshopId(id)) => Content::Wallpaper(id.clone()),
        WallpaperRef::Source(WallpaperSource::Path(p)) => Content::Wallpaper(
            std::path::absolute(p)
                .unwrap_or_else(|_| p.clone())
                .to_string_lossy()
                .into_owned(),
        ),
        WallpaperRef::PlaylistFirst(name) => Content::Playlist(name.clone()),
    }
}

/// The current layout as per-output entries: the plan's screens and spans, or, when one
/// wallpaper covers every output, one entry per known output.
fn current_entries(plan: &ExecutionPlan, known: &[String]) -> Vec<Entry> {
    let entry = |t: &TargetPlan, outputs: Vec<String>| Entry {
        outputs,
        content: content_of(t),
        scaling: Some(t.scaling),
        clamp: (t.clamp != Default::default()).then(|| t.clamp.to_string()),
    };
    match plan.mode {
        OutputMode::Desktop => plan
            .targets
            .iter()
            .map(|t| entry(t, t.outputs.clone()))
            .collect(),
        _ => match plan.targets.first() {
            Some(t) => known.iter().map(|k| entry(t, vec![k.clone()])).collect(),
            None => Vec::new(),
        },
    }
}

/// Takes `name` out of the entries (a span left with one output becomes a screen).
fn remove_output(entries: &mut Vec<Entry>, name: &str) {
    for e in entries.iter_mut() {
        e.outputs.retain(|o| o != name);
    }
    entries.retain(|e| !e.outputs.is_empty());
}

fn write_content(t: &mut Table, content: &Content) {
    match content {
        Content::Wallpaper(w) => {
            t.insert("wallpaper", value(w.as_str()));
            t.remove("playlist");
        }
        Content::Playlist(p) => {
            t.insert("playlist", value(p.as_str()));
            t.remove("wallpaper");
        }
    }
}

/// One wallpaper everywhere: `wallpaper` + `scaling`, no `[[screen]]`/`[[span]]`.
fn write_single(doc: &mut DocumentMut, content: &Content, scaling: Scaling) {
    let t = doc.as_table_mut();
    t.remove("screen");
    t.remove("span");
    write_content(t, content);
    t.insert("scaling", value(scaling.to_string()));
}

fn write_entries(doc: &mut DocumentMut, mut entries: Vec<Entry>) {
    entries.sort_by(|a, b| a.outputs.cmp(&b.outputs));
    let mut screens = ArrayOfTables::new();
    let mut spans = ArrayOfTables::new();
    for e in entries {
        let mut t = Table::new();
        if e.outputs.len() == 1 {
            t.insert("name", value(e.outputs[0].as_str()));
        } else {
            let mut list = toml_edit::Array::new();
            for o in &e.outputs {
                list.push(o.as_str());
            }
            t.insert("screens", value(list));
        }
        write_content(&mut t, &e.content);
        if let Some(s) = e.scaling {
            t.insert("scaling", value(s.to_string()));
        }
        if let Some(c) = &e.clamp {
            t.insert("clamp", value(c.as_str()));
        }
        if e.outputs.len() == 1 {
            screens.push(t);
        } else {
            spans.push(t);
        }
    }
    let root = doc.as_table_mut();
    root.remove("window");
    root.remove("screen");
    root.remove("span");
    if !screens.is_empty() {
        root.insert("screen", Item::ArrayOfTables(screens));
    }
    if !spans.is_empty() {
        root.insert("span", Item::ArrayOfTables(spans));
    }
}

/// `[wallpaper-audio."<key>"]`, removed when back to the default (and the table with its last
/// entry).
fn write_wallpaper_level(
    doc: &mut DocumentMut,
    key: &str,
    level: WallpaperLevel,
) -> Result<(), RemoteError> {
    let root = doc.as_table_mut();
    let levels = root.entry("wallpaper-audio").or_insert_with(|| {
        let mut t = Table::new();
        // `[wallpaper-audio."id"]` headers only, no empty `[wallpaper-audio]`.
        t.set_implicit(true);
        Item::Table(t)
    });
    let Some(levels) = levels.as_table_like_mut() else {
        return Err(invalid("[wallpaper-audio] is not a table"));
    };
    if level == WallpaperLevel::DEFAULT {
        levels.remove(key);
    } else {
        let mut entry = Table::new();
        entry.insert("volume", value(i64::from(level.percent)));
        entry.insert("muted", value(level.muted));
        levels.insert(key, Item::Table(entry));
    }
    if levels.is_empty() {
        root.remove("wallpaper-audio");
    }
    Ok(())
}

/// The configuration now decides the layout and the default wallpaper (an explicit `-w`
/// window stays).
fn take_layout(cli: &mut CliArgs) {
    let mode = match cli.layout.mode {
        CliMode::Window(g) => CliMode::Window(g),
        _ => CliMode::Unset,
    };
    cli.layout = CliLayout {
        mode,
        ..CliLayout::default()
    };
    cli.mentioned.clear();
}

fn from_ipc_scaling(s: IpcScaling) -> Scaling {
    match s {
        IpcScaling::Fill => Scaling::Fill,
        IpcScaling::Fit => Scaling::Fit,
        IpcScaling::Stretch => Scaling::Stretch,
    }
}

/// A wallpaper folder from a client path (the folder or its `project.json`).
fn wallpaper_dir(path: &Path) -> Result<PathBuf, RemoteError> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let dir = if path.file_name().is_some_and(|n| n == "project.json") && path.is_file() {
        path.parent().map(Path::to_owned).unwrap_or(path)
    } else {
        path
    };
    let project = dir.join("project.json");
    if !project.is_file() {
        return Err(RemoteError::new(
            ErrorCode::NotFound,
            format!(
                "{} is not a wallpaper folder (no project.json)",
                dir.display()
            ),
        ));
    }
    // Scene, video and web wallpapers: the engine checks what this build can show.
    Ok(dir)
}

/// A wallpaper folder (or its `project.json`) that exists, of any type: levels can be set for
/// wallpapers that are not shown.
fn wallpaper_folder(path: &Path) -> Result<PathBuf, RemoteError> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let dir = match path.parent() {
        Some(parent) if path.file_name().is_some_and(|n| n == "project.json") => parent.to_owned(),
        _ => path,
    };
    if dir.join("project.json").is_file() {
        Ok(dir)
    } else {
        Err(RemoteError::new(
            ErrorCode::NotFound,
            format!(
                "{} is not a wallpaper folder (no project.json)",
                dir.display()
            ),
        ))
    }
}

/// Integral numbers without decimals (combo values, `rain = 0`).
fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Colour component with a decimal point: integer colour text means 0–255 (spec 02 Q1).
fn color_text(c: [f32; 3]) -> String {
    let comp = |x: f32| {
        if x.fract() == 0.0 {
            format!("{x:.1}")
        } else {
            format!("{x}")
        }
    };
    format!("{} {} {}", comp(c[0]), comp(c[1]), comp(c[2]))
}

/// Override text, as `--set-property` would pass it.
fn property_text(v: &PropertyValue) -> String {
    match v {
        PropertyValue::Bool(b) => if *b { "1" } else { "0" }.to_owned(),
        PropertyValue::Number(n) => number_text(*n),
        PropertyValue::Color(c) => color_text(*c),
        PropertyValue::Text(s) => s.clone(),
    }
}

/// `[properties]` value (read back by `config::PropertyValue::to_text`).
fn property_toml(v: &PropertyValue) -> Item {
    match v {
        PropertyValue::Bool(b) => value(*b),
        PropertyValue::Number(n) if n.fract() == 0.0 && n.abs() < 1e15 => value(*n as i64),
        PropertyValue::Number(n) => value(*n),
        PropertyValue::Color(c) => value(color_text(*c)),
        PropertyValue::Text(s) => value(s.as_str()),
    }
}

/// Write to a temporary file in the same folder, flush, then rename over the target.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(dir) = dir {
        std::fs::create_dir_all(dir)?;
    }
    let name = path.file_name().map_or_else(
        || "config.toml".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Whether the session starts Lumenwall (the autostart entry the UI writes).
fn autostart_enabled() -> bool {
    #[cfg(unix)]
    {
        dirs::config_dir().is_some_and(|d| d.join("autostart/lumenwall.desktop").is_file())
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Process CPU time and resident memory, read on demand by the statistics.
mod procstats {
    use std::time::Duration;

    #[cfg(unix)]
    pub fn cpu_time() -> Option<Duration> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, writable timespec.
        let r = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
        (r == 0).then(|| Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
    }

    #[cfg(not(unix))]
    pub fn cpu_time() -> Option<Duration> {
        None
    }

    #[cfg(target_os = "linux")]
    pub fn resident_bytes() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Some(pages * u64::try_from(page).unwrap_or(4096))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn resident_bytes() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_values_become_override_text() {
        assert_eq!(property_text(&PropertyValue::Bool(true)), "1");
        assert_eq!(property_text(&PropertyValue::Number(0.0)), "0");
        assert_eq!(property_text(&PropertyValue::Number(0.25)), "0.25");
        assert_eq!(
            property_text(&PropertyValue::Color([1.0, 0.5, 0.0])),
            "1.0 0.5 0.0"
        );
        let doc_value = property_toml(&PropertyValue::Number(3.0));
        assert_eq!(doc_value.as_integer(), Some(3));
    }

    #[test]
    fn layout_edits_keep_the_rest_of_the_file() {
        let mut doc: DocumentMut = "# my settings\nfps = 24 # smooth enough\nwallpaper = \"1\"\n\n[properties]\nbloom = true\n"
            .parse()
            .unwrap();
        write_entries(
            &mut doc,
            vec![
                Entry {
                    outputs: vec!["DP-2".into()],
                    content: Content::Wallpaper("2".into()),
                    scaling: Some(Scaling::Fit),
                    clamp: None,
                },
                Entry {
                    outputs: vec!["DP-1".into()],
                    content: Content::Playlist("day".into()),
                    scaling: None,
                    clamp: Some("border".into()),
                },
            ],
        );
        let text = doc.to_string();
        assert!(text.contains("# my settings"), "{text}");
        assert!(text.contains("fps = 24 # smooth enough"), "{text}");
        let config = Config::parse(&text).unwrap();
        assert_eq!(config.screens.len(), 2);
        assert_eq!(config.screens[0].name, "DP-1");
        assert_eq!(config.screens[0].playlist.as_deref(), Some("day"));
        assert_eq!(config.screens[1].scaling, Some(Scaling::Fit));
        assert_eq!(config.properties["bloom"].to_text(), "1");

        write_single(&mut doc, &Content::Wallpaper("/w/3".into()), Scaling::Fill);
        let config = Config::parse(&doc.to_string()).unwrap();
        assert!(config.screens.is_empty());
        assert_eq!(config.wallpaper.as_deref(), Some(Path::new("/w/3")));
        assert_eq!(config.scaling, Some(Scaling::Fill));
        assert_eq!(config.fps, Some(24));
    }

    #[test]
    fn wallpaper_levels_are_written_per_key() {
        let mut doc: DocumentMut = "volume = 64 # half\n".parse().unwrap();
        let level = |percent, muted| WallpaperLevel { percent, muted };
        write_wallpaper_level(&mut doc, "3018340632", level(40, false)).unwrap();
        write_wallpaper_level(&mut doc, "/w/my wallpaper", level(100, true)).unwrap();
        let text = doc.to_string();
        assert!(text.contains("volume = 64 # half"), "{text}");
        assert!(text.contains("[wallpaper-audio.3018340632]"), "{text}");
        assert!(!text.contains("[wallpaper-audio]\n"), "{text}");
        let config = Config::parse(&text).unwrap();
        assert_eq!(config.wallpaper_audio["3018340632"].volume, Some(40));
        assert_eq!(config.wallpaper_audio["/w/my wallpaper"].muted, Some(true));
        // Back to the default: the entry, then the table, go away.
        write_wallpaper_level(&mut doc, "3018340632", WallpaperLevel::DEFAULT).unwrap();
        write_wallpaper_level(&mut doc, "/w/my wallpaper", WallpaperLevel::DEFAULT).unwrap();
        assert_eq!(doc.to_string(), "volume = 64 # half\n");
        let mut doc: DocumentMut = "wallpaper-audio = 3\n".parse().unwrap();
        assert!(write_wallpaper_level(&mut doc, "1", level(1, false)).is_err());
    }

    #[test]
    fn removing_a_span_member_keeps_the_others() {
        let mut entries = vec![
            Entry {
                outputs: vec!["A".into(), "B".into()],
                content: Content::Wallpaper("1".into()),
                scaling: None,
                clamp: None,
            },
            Entry {
                outputs: vec!["C".into()],
                content: Content::Wallpaper("2".into()),
                scaling: None,
                clamp: None,
            },
        ];
        remove_output(&mut entries, "A");
        assert_eq!(entries[0].outputs, ["B"]);
        remove_output(&mut entries, "C");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn atomic_write_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        write_atomic(&path, b"fps = 1\n").unwrap();
        write_atomic(&path, b"fps = 2\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"fps = 2\n");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn process_counters() {
        #[cfg(unix)]
        assert!(procstats::cpu_time().is_some());
        #[cfg(target_os = "linux")]
        assert!(procstats::resident_bytes().unwrap() > 0);
    }

    #[test]
    fn wallpaper_folders_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            wallpaper_dir(dir.path()).unwrap_err().code,
            ErrorCode::NotFound
        );
        // Web wallpapers go through like the others (the engine decides).
        std::fs::write(dir.path().join("project.json"), r#"{"type":"web"}"#).unwrap();
        assert_eq!(
            wallpaper_dir(&dir.path().join("project.json")).unwrap(),
            dir.path()
        );
        std::fs::write(dir.path().join("project.json"), r#"{"type":"scene"}"#).unwrap();
        assert_eq!(
            wallpaper_dir(&dir.path().join("project.json")).unwrap(),
            dir.path()
        );
    }
}

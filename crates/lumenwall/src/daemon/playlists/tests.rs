//! The daemon's playlists with a fake engine, a fake catalog and a fake clock: the tests call
//! `playlist_tick` with the time they want instead of waiting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lumenwall_ipc::{ErrorCode, Handler, PauseReason, PlaylistMode, PlaylistOrder, Scaling};

use super::super::{Daemon, DaemonInit, lock};
use crate::cli::{self, CliOutcome};
use crate::config::{self, LogConfig};
use crate::plan::{self, ExecutionPlan, PlanChange, PlanServices, TargetKey};
use crate::playlist::{Playlist, PlaylistSettings};
use crate::services::{
    Control, EngineSnapshot, FrameCounters, Monitor, MonitorListener, OutputSnapshot,
    PlaylistCatalog, ServiceError, TargetSnapshot, TargetStatus, Waker, WallpaperResolver,
};
use crate::settings::WallpaperSource;

const OUTPUTS: [&str; 2] = ["DP-1", "HDMI-A-1"];

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

struct Catalog(BTreeMap<String, Playlist>);

impl PlaylistCatalog for Catalog {
    fn first_wallpaper(&self, name: &str) -> Result<PathBuf, ServiceError> {
        Ok(PlaylistCatalog::playlist(self, name)?.items[0].clone())
    }
    fn playlist(&self, name: &str) -> Result<Playlist, ServiceError> {
        self.0.get(name).cloned().ok_or_else(|| {
            let names: Vec<_> = self.0.keys().cloned().collect();
            format!(
                "playlist '{name}' not found; available playlists: {}",
                names.join(", ")
            )
            .into()
        })
    }
    fn list(&self) -> Result<Vec<Playlist>, ServiceError> {
        Ok(self.0.values().cloned().collect())
    }
}

struct Resolver;

impl WallpaperResolver for Resolver {
    fn resolve(&self, source: &WallpaperSource) -> Result<PathBuf, ServiceError> {
        match source {
            WallpaperSource::Path(p) => Ok(p.clone()),
            WallpaperSource::WorkshopId(id) => Ok(PathBuf::from("/workshop").join(id)),
        }
    }
}

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
}

/// Reports the plan the fake engine applied last; every target runs, outputs are paused as
/// the test says.
#[derive(Default)]
struct FakeMonitor {
    plan: Mutex<Option<Arc<ExecutionPlan>>>,
    paused: Mutex<BTreeMap<String, PauseReason>>,
}

impl Monitor for FakeMonitor {
    fn snapshot(&self) -> EngineSnapshot {
        let Some(plan) = self.plan.lock().unwrap().clone() else {
            return EngineSnapshot::default();
        };
        let paused = self.paused.lock().unwrap().clone();
        let targets = plan
            .targets
            .iter()
            .map(|t| TargetSnapshot {
                key: t.key.to_string(),
                wallpaper: t.wallpaper.path.clone(),
                title: String::new(),
                scaling: Scaling::Fill,
                status: TargetStatus::Running,
                properties: Default::default(),
            })
            .collect();
        let outputs = OUTPUTS
            .iter()
            .map(|id| {
                let target = plan
                    .targets
                    .iter()
                    .position(|t| t.key == TargetKey::Window || t.outputs.iter().any(|o| o == id));
                OutputSnapshot {
                    info: lumenwall_ipc::Output {
                        id: (*id).to_owned(),
                        name: String::new(),
                        x: 0,
                        y: 0,
                        width: 1920,
                        height: 1080,
                        scale: 1.0,
                        refresh_mhz: 60_000,
                        primary: false,
                    },
                    shown: target.is_some(),
                    target,
                    paused: paused.get(*id).copied(),
                }
            })
            .collect();
        EngineSnapshot {
            active: true,
            outputs,
            targets,
        }
    }
    fn set_listener(&self, _listener: Option<MonitorListener>) {}
    fn set_sampling(&self, _on: bool) {}
    fn take_counters(&self) -> FrameCounters {
        FrameCounters::default()
    }
}

type Reloads = Arc<Mutex<Vec<(Arc<ExecutionPlan>, Vec<PlanChange>)>>>;

struct Rig {
    dir: tempfile::TempDir,
    daemon: Arc<Daemon>,
    monitor: Arc<FakeMonitor>,
    /// Plans the fake engine applied.
    reloads: Reloads,
    /// Taken right before the daemon (and its playlists) started.
    before: Instant,
    /// Taken right after.
    after: Instant,
}

/// Wallpaper folders `a` to `d` with a usable `project.json`, `bad` without `file`, and
/// `refused`, which the fake engine cannot show.
fn wallpapers(dir: &Path) {
    for name in ["a", "b", "c", "d", "refused"] {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(
            dir.join(name).join("project.json"),
            r#"{"type":"scene","file":"scene.json"}"#,
        )
        .unwrap();
    }
    std::fs::create_dir_all(dir.join("bad")).unwrap();
    std::fs::write(dir.join("bad").join("project.json"), r#"{"type":"scene"}"#).unwrap();
}

/// The fake Wallpaper Engine playlists, one minute each unless said otherwise.
fn playlists(dir: &Path) -> BTreeMap<String, Playlist> {
    let items = |names: &[&str]| names.iter().map(|n| dir.join(n)).collect::<Vec<_>>();
    let minute = PlaylistSettings {
        delay: secs(60),
        ..PlaylistSettings::default()
    };
    [
        ("day", items(&["a", "b", "c"]), minute.clone()),
        (
            "shuffle",
            items(&["a", "b", "c", "d"]),
            PlaylistSettings {
                random: true,
                ..minute.clone()
            },
        ),
        (
            "frozen",
            items(&["a", "b"]),
            PlaylistSettings {
                timer: false,
                ..minute.clone()
            },
        ),
        (
            "through-pause",
            items(&["a", "b"]),
            PlaylistSettings {
                update_on_pause: true,
                transition: true,
                ..minute.clone()
            },
        ),
        (
            "broken",
            items(&["a", "bad", "refused", "c"]),
            minute.clone(),
        ),
        (
            "fast",
            items(&["a", "b"]),
            PlaylistSettings {
                delay: Duration::from_millis(30),
                ..minute
            },
        ),
    ]
    .into_iter()
    .map(|(name, items, settings)| {
        let p = Playlist {
            name: name.to_owned(),
            items,
            settings,
        };
        (name.to_owned(), p)
    })
    .collect()
}

/// Applies every plan except those showing `refused`, and answers like the engine.
fn spawn_engine(rx: mpsc::Receiver<Control>, monitor: Arc<FakeMonitor>, reloads: Reloads) {
    std::thread::spawn(move || {
        while let Ok(message) = rx.recv() {
            let Control::Reload {
                plan,
                changes,
                done,
            } = message
            else {
                continue;
            };
            let refused = plan
                .targets
                .iter()
                .any(|t| t.wallpaper.path.ends_with("refused"));
            let result = if refused {
                Err("cannot load".to_owned())
            } else {
                *monitor.plan.lock().unwrap() = Some(plan.clone());
                reloads.lock().unwrap().push((plan, changes));
                Ok(())
            };
            if let Some(done) = done {
                let _ = done.send(result);
            }
        }
    });
}

impl Rig {
    /// `args` after `lumenwall`, `{dir}` standing for the wallpapers' folder. With `config`,
    /// the daemon reads and writes `<dir>/config.toml` (else `--no-config`).
    fn new(args: &[&str], config: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        wallpapers(dir.path());
        Self::start(dir, args, config)
    }

    /// Starts a daemon on `dir`; an existing `config.toml` there is used.
    fn start(dir: tempfile::TempDir, args: &[&str], config: Option<&str>) -> Self {
        let root = dir.path().to_string_lossy().into_owned();
        let config_path = dir.path().join("config.toml");
        let mut argv: Vec<String> = vec!["lumenwall".into()];
        if let Some(text) = config {
            std::fs::write(&config_path, text).unwrap();
        }
        if config_path.exists() {
            argv.extend(["--config".into(), config_path.to_string_lossy().into()]);
        } else {
            argv.push("--no-config".into());
        }
        argv.extend(args.iter().map(|a| a.replace("{dir}", &root)));
        let Ok(CliOutcome::Run(cli)) = cli::parse(argv) else {
            panic!("bad test arguments")
        };
        let loaded = config::load(&cli.config).unwrap();
        let catalog: Arc<dyn PlaylistCatalog> = Arc::new(Catalog(playlists(dir.path())));
        let resolver: Arc<dyn WallpaperResolver> = Arc::new(Resolver);
        let services = PlanServices {
            resolver: &*resolver,
            playlists: &*catalog,
        };
        let plan =
            Arc::new(plan::build_with(&cli, loaded.config.as_ref(), services, true).unwrap());
        let monitor = Arc::new(FakeMonitor::default());
        *monitor.plan.lock().unwrap() = Some(plan.clone());
        let (control, rx) = crate::services::control_channel(Arc::new(NoWake));
        let reloads = Reloads::default();
        spawn_engine(rx, monitor.clone(), reloads.clone());
        let before = Instant::now();
        let daemon = Daemon::new(DaemonInit {
            cli,
            config_path: loaded.path.clone(),
            config_bytes: loaded.bytes.clone(),
            config: loaded.config.clone(),
            plan,
            resolver,
            playlists: catalog,
            control,
            monitor: Some(monitor.clone() as Arc<dyn Monitor>),
            log_filter: None,
            log_config: LogConfig::default(),
        });
        let after = Instant::now();
        Rig {
            dir,
            daemon,
            monitor,
            reloads,
            before,
            after,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn handler(&self) -> Arc<dyn Handler> {
        self.daemon.handler()
    }

    /// What the plan shows on the target `key`.
    fn shows(&self, key: &str) -> PathBuf {
        let plan = lock(&self.daemon.core).plan.clone();
        plan.targets
            .iter()
            .find(|t| t.key.to_string() == key)
            .map(|t| t.wallpaper.path.clone())
            .unwrap_or_else(|| panic!("no target {key}"))
    }

    fn reload_count(&self) -> usize {
        self.reloads.lock().unwrap().len()
    }

    fn last_changes(&self) -> Vec<PlanChange> {
        self.reloads.lock().unwrap().last().unwrap().1.clone()
    }

    /// The playlist position `get_state` reports for `output`.
    fn position(&self, output: &str) -> Option<(String, Option<u32>, u32)> {
        let state = self.daemon.state();
        let o = state.outputs.iter().find(|o| o.output == output)?;
        o.playlist.clone().map(|p| (p.name, p.item, p.count))
    }

    fn pause(&self, output: &str, reason: Option<PauseReason>) {
        let mut paused = self.monitor.paused.lock().unwrap();
        match reason {
            Some(r) => paused.insert(output.to_owned(), r),
            None => paused.remove(output),
        };
    }

    fn tick(&self, at: Instant) {
        self.daemon.playlist_tick(at);
    }
}

#[test]
fn a_timer_playlist_changes_on_time_on_its_own_output() {
    let rig = Rig::new(
        &[
            "-r",
            "DP-1",
            "--playlist",
            "day",
            "-r",
            "HDMI-A-1",
            "--bg",
            "{dir}/d",
        ],
        None,
    );
    assert_eq!(rig.shows("DP-1"), rig.path("a"));
    assert_eq!(rig.position("DP-1"), Some(("day".into(), Some(0), 3)));
    assert_eq!(rig.position("HDMI-A-1"), None);
    // Not yet.
    rig.tick(rig.before + secs(59));
    assert_eq!(rig.reload_count(), 0);
    // Due: only DP-1 changes.
    rig.tick(rig.after + secs(60));
    assert_eq!(rig.shows("DP-1"), rig.path("b"));
    assert_eq!(rig.shows("HDMI-A-1"), rig.path("d"));
    assert_eq!(
        rig.last_changes(),
        [PlanChange::TargetContent(TargetKey::Screen("DP-1".into()))]
    );
    assert_eq!(rig.position("DP-1"), Some(("day".into(), Some(1), 3)));
    // The next delay counts from the change; then the playlist loops.
    rig.tick(rig.before + secs(119));
    assert_eq!(rig.reload_count(), 1);
    rig.tick(rig.after + secs(120));
    rig.tick(rig.after + secs(180));
    assert_eq!(rig.shows("DP-1"), rig.path("a"));
    assert_eq!(rig.reload_count(), 3);
    let deadline = lock(&rig.daemon.ops).deadline().unwrap();
    assert!(deadline >= rig.before + secs(240) && deadline <= rig.after + secs(240));
}

#[test]
fn each_output_runs_its_own_playlist() {
    let rig = Rig::new(
        &[
            "-r",
            "DP-1",
            "--playlist",
            "day",
            "-r",
            "HDMI-A-1",
            "--playlist",
            "shuffle",
        ],
        None,
    );
    let mut seen = vec![rig.shows("HDMI-A-1")];
    for minute in 1..=3 {
        rig.tick(rig.after + secs(60 * minute));
        seen.push(rig.shows("HDMI-A-1"));
    }
    // A random round shows every item once, starting with the one shown.
    assert_eq!(seen[0], rig.path("a"));
    seen.sort();
    assert_eq!(seen, ["a", "b", "c", "d"].map(|n| rig.path(n)));
    assert_eq!(rig.shows("DP-1"), rig.path("a"));
    assert_eq!(rig.position("HDMI-A-1").unwrap().2, 4);
    // Both changed every minute, each on its own.
    assert_eq!(rig.reload_count(), 6);
}

#[test]
fn pauses_stop_the_timer_unless_update_on_pause() {
    let rig = Rig::new(
        &[
            "-r",
            "DP-1",
            "--playlist",
            "day",
            "-r",
            "HDMI-A-1",
            "--playlist",
            "through-pause",
        ],
        None,
    );
    // Both outputs covered by a fullscreen window after 30 s, for 9.5 minutes.
    rig.pause("DP-1", Some(PauseReason::Fullscreen));
    rig.pause("HDMI-A-1", Some(PauseReason::Fullscreen));
    rig.tick(rig.before + secs(30));
    // `updateonpause`: HDMI-A-1 changes while paused; DP-1 waits.
    rig.tick(rig.after + secs(60));
    assert_eq!(rig.shows("DP-1"), rig.path("a"));
    assert_eq!(rig.shows("HDMI-A-1"), rig.path("b"));
    rig.pause("DP-1", None);
    rig.pause("HDMI-A-1", None);
    rig.tick(rig.before + secs(600));
    // DP-1 had 30 s left when the pause began.
    rig.tick(rig.before + secs(629));
    assert_eq!(rig.shows("DP-1"), rig.path("a"));
    rig.tick(rig.after + secs(630));
    assert_eq!(rig.shows("DP-1"), rig.path("b"));

    // A user pause with 40 s left, for 350 s.
    rig.daemon.set_paused(true).unwrap();
    rig.tick(rig.after + secs(650));
    rig.daemon.set_paused(false).unwrap();
    rig.tick(rig.after + secs(1000));
    rig.tick(rig.before + secs(1039));
    assert_eq!(rig.shows("DP-1"), rig.path("b"));
    rig.tick(rig.after + secs(1040));
    assert_eq!(rig.shows("DP-1"), rig.path("c"));
}

#[test]
fn a_target_pauses_when_all_its_outputs_do() {
    // One playlist on both outputs (the window target).
    let rig = Rig::new(&["--playlist", "day"], None);
    // A locked or hidden output: the other still shows the playlist, its time counts.
    rig.pause("DP-1", Some(PauseReason::Hidden));
    rig.tick(rig.before + secs(10));
    rig.tick(rig.after + secs(60));
    assert_eq!(rig.shows("default"), rig.path("b"));
    // Both: the timer stops, nothing to wake up for.
    rig.pause("HDMI-A-1", Some(PauseReason::Locked));
    rig.tick(rig.after + secs(70));
    assert_eq!(lock(&rig.daemon.ops).deadline(), None);
    rig.tick(rig.after + secs(500));
    assert_eq!(rig.shows("default"), rig.path("b"));
    rig.pause("DP-1", None);
    rig.tick(rig.after + secs(500));
    // 50 s were left when both paused.
    rig.tick(rig.before + secs(549));
    assert_eq!(rig.shows("default"), rig.path("b"));
    rig.tick(rig.after + secs(550));
    assert_eq!(rig.shows("default"), rig.path("c"));
}

#[test]
fn frozen_modes_only_move_on_request() {
    let rig = Rig::new(&["--playlist", "frozen"], None);
    rig.tick(rig.after + secs(3600));
    assert_eq!(rig.reload_count(), 0);
    assert!(lock(&rig.daemon.ops).deadline().is_none());
    // The window target covers every output: either one names it.
    let handler = rig.handler();
    handler.step_playlist(Some("HDMI-A-1"), true).unwrap();
    assert_eq!(rig.shows("default"), rig.path("b"));
    assert_eq!(rig.position("DP-1"), Some(("frozen".into(), Some(1), 2)));
    handler.step_playlist(None, false).unwrap();
    assert_eq!(rig.shows("default"), rig.path("a"));
}

#[test]
fn next_and_previous_restart_the_timer() {
    let rig = Rig::new(
        &[
            "-r",
            "DP-1",
            "--playlist",
            "day",
            "-r",
            "HDMI-A-1",
            "--bg",
            "{dir}/d",
        ],
        None,
    );
    let handler = rig.handler();
    handler.step_playlist(Some("DP-1"), true).unwrap();
    let stepped = Instant::now();
    assert_eq!(rig.shows("DP-1"), rig.path("b"));
    handler.step_playlist(Some("DP-1"), false).unwrap();
    handler.step_playlist(Some("DP-1"), false).unwrap();
    assert_eq!(rig.shows("DP-1"), rig.path("c"));
    // The minute started again at the last step.
    rig.tick(stepped + secs(59));
    assert_eq!(rig.shows("DP-1"), rig.path("c"));
    rig.tick(Instant::now() + secs(60));
    assert_eq!(rig.shows("DP-1"), rig.path("a"));

    let err = handler.step_playlist(Some("HDMI-A-1"), true).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidRequest);
    let err = handler.step_playlist(Some("DP-9"), true).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnknownOutput);
}

#[test]
fn unusable_items_are_skipped_for_the_session() {
    let rig = Rig::new(&["-r", "DP-1", "--playlist", "broken"], None);
    // `bad` has no `file` (precheck), `refused` cannot be loaded: nothing changes.
    rig.tick(rig.after + secs(60));
    assert_eq!(rig.shows("DP-1"), rig.path("a"));
    rig.tick(rig.after + secs(120));
    assert_eq!(rig.shows("DP-1"), rig.path("c"));
    rig.tick(rig.after + secs(180));
    assert_eq!(rig.shows("DP-1"), rig.path("a"));
    // Both are skipped from now on.
    rig.tick(rig.after + secs(240));
    assert_eq!(rig.shows("DP-1"), rig.path("c"));
    assert_eq!(rig.reload_count(), 3);
}

#[test]
fn other_changes_keep_the_current_item() {
    let rig = Rig::new(&["-r", "DP-1", "--playlist", "day"], None);
    rig.tick(rig.after + secs(60));
    assert_eq!(rig.shows("DP-1"), rig.path("b"));
    // A client change rebuilds the plan from the configuration, which names the first
    // item, but the playlist stays where it is.
    rig.daemon.set_audio(Some(true), None).unwrap();
    assert_eq!(rig.shows("DP-1"), rig.path("b"));
    assert_eq!(rig.last_changes(), [PlanChange::Audio]);
}

#[test]
fn a_playlist_chosen_by_a_client_survives_a_restart() {
    let rig = Rig::new(&[], Some("fps = 24\n"));
    let handler = rig.handler();
    let err = handler
        .apply_playlist(Some("DP-1"), "nope", Scaling::Fill)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(
        err.message.contains("available playlists: broken, day"),
        "{err}"
    );
    handler
        .apply_playlist(Some("DP-1"), "day", Scaling::Fit)
        .unwrap();
    assert_eq!(rig.shows("DP-1"), rig.path("a"));
    assert_eq!(rig.position("DP-1"), Some(("day".into(), Some(0), 3)));
    rig.tick(Instant::now() + secs(60));
    assert_eq!(rig.shows("DP-1"), rig.path("b"));
    let text = std::fs::read_to_string(rig.path("config.toml")).unwrap();
    assert!(text.contains("fps = 24"), "{text}");
    assert!(text.contains("playlist = \"day\""), "{text}");

    // Restart: a new daemon on the same configuration file.
    let Rig { dir, daemon, .. } = rig;
    drop(handler);
    daemon.detach();
    drop(daemon);
    let rig = Rig::start(dir, &[], None);
    assert_eq!(rig.shows("DP-1"), rig.path("a"));
    assert_eq!(rig.position("DP-1"), Some(("day".into(), Some(0), 3)));
    rig.tick(rig.after + secs(60));
    assert_eq!(rig.shows("DP-1"), rig.path("b"));

    // Stopping the playlist: a client applies the wallpaper to keep.
    rig.handler()
        .apply(Some("DP-1"), &rig.path("b"), Scaling::Fit)
        .unwrap();
    assert_eq!(rig.position("DP-1"), None);
    rig.tick(rig.after + secs(600));
    assert_eq!(rig.shows("DP-1"), rig.path("b"));
    let text = std::fs::read_to_string(rig.path("config.toml")).unwrap();
    assert!(!text.contains("playlist"), "{text}");
}

#[test]
fn a_playlist_on_every_output_is_one_window_target() {
    let rig = Rig::new(&[], Some(""));
    rig.handler()
        .apply_playlist(None, "day", Scaling::Fill)
        .unwrap();
    assert_eq!(rig.shows("default"), rig.path("a"));
    assert_eq!(rig.position("DP-1"), Some(("day".into(), Some(0), 3)));
    assert_eq!(rig.position("HDMI-A-1"), Some(("day".into(), Some(0), 3)));
    rig.tick(Instant::now() + secs(60));
    assert_eq!(rig.shows("default"), rig.path("b"));
    // Choosing another wallpaper for one output keeps the playlist where it is on the other.
    let handler = rig.handler();
    handler
        .apply(Some("HDMI-A-1"), &rig.path("d"), Scaling::Fill)
        .unwrap();
    assert_eq!(rig.shows("DP-1"), rig.path("b"));
    assert_eq!(rig.shows("HDMI-A-1"), rig.path("d"));
    assert_eq!(rig.position("DP-1"), Some(("day".into(), Some(1), 3)));
    let text = std::fs::read_to_string(rig.path("config.toml")).unwrap();
    assert!(text.contains("[[screen]]"), "{text}");
}

#[test]
fn clients_list_the_playlists() {
    let rig = Rig::new(&[], None);
    let list = rig.handler().playlists().unwrap();
    let names: Vec<_> = list.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "broken",
            "day",
            "fast",
            "frozen",
            "shuffle",
            "through-pause"
        ]
    );
    let get = |name: &str| list.iter().find(|p| p.name == name).unwrap();
    let day = &get("day").settings;
    assert_eq!(
        (day.mode, day.delay_minutes, day.order),
        (PlaylistMode::Timer, 1, PlaylistOrder::Sequential)
    );
    assert_eq!(get("frozen").settings.mode, PlaylistMode::Manual);
    assert_eq!(get("shuffle").settings.order, PlaylistOrder::Random);
    let through = &get("through-pause").settings;
    assert!(through.update_on_pause && through.transition);
    assert_eq!(get("day").items, ["a", "b", "c"].map(|n| rig.path(n)));
}

#[test]
fn the_playlist_thread_wakes_up_on_time_and_stops() {
    let rig = Rig::new(&["-r", "DP-1", "--playlist", "fast"], None);
    rig.daemon.start_playlists();
    let give_up = Instant::now() + secs(10);
    while rig.reload_count() < 2 {
        assert!(
            Instant::now() < give_up,
            "the playlist thread did not change items"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    rig.daemon.detach();
    // Stopped: no more changes.
    std::thread::sleep(Duration::from_millis(100));
    let count = rig.reload_count();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(rig.reload_count(), count);
}

//! Pause on battery with a fake power source, a fake engine and a real configuration file:
//! the setting is accepted, written, applied without an engine reload, and combined with the
//! client pause. Runs on every system (the real sources are `lumenwall-audio`'s).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lumenwall_ipc::{Handler, PauseReason, Settings, WallpaperStatus};

use super::{Daemon, DaemonInit};
use crate::cli::{self, CliOutcome};
use crate::config::{self, LogConfig};
use crate::plan::{self, PlanServices};
use crate::services::{
    Control, EngineSnapshot, FrameCounters, Monitor, MonitorListener, OutputSnapshot,
    PlaylistCatalog, PowerCallback, PowerSource, ServiceError, TargetSnapshot, TargetStatus, Waker,
    WallpaperResolver,
};
use crate::settings::WallpaperSource;
use crate::watch::FileChange;

struct NoPlaylists;

impl PlaylistCatalog for NoPlaylists {
    fn first_wallpaper(&self, name: &str) -> Result<PathBuf, ServiceError> {
        Err(format!("no playlist {name}").into())
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

/// One output showing the plan's only target, never paused by the platform.
struct OneOutput;

impl Monitor for OneOutput {
    fn snapshot(&self) -> EngineSnapshot {
        EngineSnapshot {
            active: true,
            outputs: vec![OutputSnapshot {
                info: lumenwall_ipc::Output {
                    id: "eDP-1".into(),
                    name: String::new(),
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    scale: 1.0,
                    refresh_mhz: 60_000,
                    primary: true,
                },
                shown: true,
                target: Some(0),
                paused: None,
            }],
            targets: vec![TargetSnapshot {
                key: "default".into(),
                wallpaper: PathBuf::from("/w"),
                title: String::new(),
                scaling: lumenwall_ipc::Scaling::Fill,
                status: TargetStatus::Running,
                properties: Default::default(),
            }],
        }
    }
    fn set_listener(&self, _listener: Option<MonitorListener>) {}
    fn set_sampling(&self, _on: bool) {}
    fn take_counters(&self) -> FrameCounters {
        FrameCounters::default()
    }
}

/// The fake power source: the test says when the computer is on battery.
#[derive(Default)]
struct FakePower {
    callback: Mutex<Option<PowerCallback>>,
    /// What it says as soon as it is watched (like the real sources).
    initial: Mutex<Option<bool>>,
    started: AtomicUsize,
    stopped: Arc<AtomicUsize>,
}

struct Watch(Arc<AtomicUsize>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl PowerSource for FakePower {
    fn watch(&self, on_change: PowerCallback) -> Box<dyn Send> {
        self.started.fetch_add(1, Ordering::SeqCst);
        if let Some(initial) = *self.initial.lock().unwrap() {
            on_change(initial);
        }
        *self.callback.lock().unwrap() = Some(on_change);
        Box::new(Watch(Arc::clone(&self.stopped)))
    }
}

impl FakePower {
    fn set(&self, on_battery: bool) {
        let cb = self.callback.lock().unwrap().clone().expect("not watched");
        cb(on_battery);
    }
    fn running(&self) -> bool {
        self.started.load(Ordering::SeqCst) > self.stopped.load(Ordering::SeqCst)
    }
}

/// What the fake engine received.
#[derive(Default)]
struct Received {
    pauses: Vec<bool>,
    reloads: usize,
}

struct Rig {
    dir: tempfile::TempDir,
    daemon: Arc<Daemon>,
    power: Arc<FakePower>,
    received: Arc<Mutex<Received>>,
}

impl Rig {
    /// A daemon on a configuration file holding `config`; the power source says `initial` as
    /// soon as it is watched.
    fn new(config: &str, initial: Option<bool>) -> Self {
        Self::build(config, initial, true)
    }

    /// `with_power`: whether the daemon is given the fake power source.
    fn build(config: &str, initial: Option<bool>, with_power: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, config).unwrap();
        let argv: Vec<String> = vec![
            "lumenwall".into(),
            "--config".into(),
            path.to_string_lossy().into(),
            "/w".into(),
        ];
        let Ok(CliOutcome::Run(cli)) = cli::parse(argv) else {
            panic!("bad test arguments")
        };
        let loaded = config::load(&cli.config).unwrap();
        let playlists: Arc<dyn PlaylistCatalog> = Arc::new(NoPlaylists);
        let resolver: Arc<dyn WallpaperResolver> = Arc::new(Resolver);
        let services = PlanServices {
            resolver: &*resolver,
            playlists: &*playlists,
        };
        let plan =
            Arc::new(plan::build_with(&cli, loaded.config.as_ref(), services, true).unwrap());
        let (control, rx) = crate::services::control_channel(Arc::new(NoWake));
        let received = Arc::new(Mutex::new(Received::default()));
        spawn_engine(rx, Arc::clone(&received));
        let power = Arc::new(FakePower::default());
        *power.initial.lock().unwrap() = initial;
        let daemon = Daemon::new(DaemonInit {
            cli,
            config_path: loaded.path.clone(),
            config_bytes: loaded.bytes.clone(),
            config: loaded.config.clone(),
            plan,
            resolver,
            playlists,
            control,
            monitor: Some(Arc::new(OneOutput) as Arc<dyn Monitor>),
            power: with_power.then(|| Arc::clone(&power) as Arc<dyn PowerSource>),
            log_filter: None,
            log_config: LogConfig::default(),
        });
        Self {
            dir,
            daemon,
            power,
            received,
        }
    }

    fn handler(&self) -> Arc<dyn Handler> {
        self.daemon.handler()
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("config.toml")).unwrap()
    }

    /// The pauses the engine got, once its thread caught up.
    fn pauses(&self, expected: usize) -> Vec<bool> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let got = self.received.lock().unwrap().pauses.clone();
            if got.len() >= expected || Instant::now() > deadline {
                return got;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn reloads(&self) -> usize {
        self.received.lock().unwrap().reloads
    }

    fn set_battery_setting(&self, on: bool) {
        let h = self.handler();
        let settings = Settings {
            pause_on_battery: on,
            ..h.settings().unwrap()
        };
        h.set_settings(settings).unwrap();
    }
}

fn spawn_engine(rx: mpsc::Receiver<Control>, received: Arc<Mutex<Received>>) {
    std::thread::spawn(move || {
        while let Ok(message) = rx.recv() {
            match message {
                Control::Pause(p) => received.lock().unwrap().pauses.push(p),
                Control::Reload { done, .. } => {
                    received.lock().unwrap().reloads += 1;
                    if let Some(done) = done {
                        let _ = done.send(Ok(()));
                    }
                }
                _ => {}
            }
        }
    });
}

#[test]
fn the_setting_is_accepted_written_and_applied() {
    let rig = Rig::new("fps = 24 # mine\n", None);
    let h = rig.handler();
    assert!(!h.settings().unwrap().pause_on_battery, "off by default");
    assert!(!rig.power.running(), "not followed while off");

    rig.set_battery_setting(true);
    assert!(h.settings().unwrap().pause_on_battery);
    let file = rig.file();
    assert!(file.contains("pause-on-battery = true"), "{file}");
    assert!(file.contains("fps = 24 # mine"), "{file}");
    assert!(rig.power.running());
    assert_eq!(
        rig.reloads(),
        0,
        "the engine is not reloaded for this setting"
    );
    assert!(rig.pauses(0).is_empty(), "on mains: nothing to pause");

    rig.power.set(true);
    assert_eq!(rig.pauses(1), [true]);
    let state = h.state().unwrap();
    assert!(state.paused);
    assert_eq!(state.pause_reason, Some(PauseReason::Battery));
    assert_eq!(state.outputs[0].status, WallpaperStatus::Paused);
    // The same news twice changes nothing.
    rig.power.set(true);
    rig.power.set(false);
    assert_eq!(rig.pauses(2), [true, false]);
    assert!(!h.state().unwrap().paused);

    // Switched off while on battery: resumed, and the power source is no longer followed.
    rig.power.set(true);
    assert_eq!(rig.pauses(3), [true, false, true]);
    rig.set_battery_setting(false);
    assert_eq!(rig.pauses(4), [true, false, true, false]);
    assert!(!rig.power.running());
    assert!(!h.state().unwrap().paused);
    assert!(rig.file().contains("pause-on-battery = false"));
    assert_eq!(rig.reloads(), 0);
}

#[test]
fn client_and_battery_pauses_combine() {
    let rig = Rig::new("pause-on-battery = true\n", Some(false));
    let h = rig.handler();
    assert!(rig.power.running(), "followed from the start");
    h.set_paused(true).unwrap();
    rig.power.set(true);
    // The client pause was sent; the battery adds nothing new for the engine.
    assert_eq!(rig.pauses(1), [true]);
    assert_eq!(h.state().unwrap().pause_reason, Some(PauseReason::User));
    // Resumed by the client while on battery: the engine stays paused, for battery.
    h.set_paused(false).unwrap();
    assert_eq!(rig.pauses(2), [true, true]);
    let state = h.state().unwrap();
    assert!(state.paused);
    assert_eq!(state.pause_reason, Some(PauseReason::Battery));
    rig.power.set(false);
    assert_eq!(rig.pauses(3), [true, true, false]);
    assert!(!h.state().unwrap().paused);
}

#[test]
fn on_battery_at_start_pauses_before_the_first_frame() {
    let rig = Rig::new("pause-on-battery = true\n", Some(true));
    assert_eq!(rig.pauses(1), [true]);
    assert_eq!(
        rig.handler().state().unwrap().pause_reason,
        Some(PauseReason::Battery)
    );
}

#[test]
fn a_hand_edit_of_the_file_switches_it() {
    let rig = Rig::new("fps = 30\n", Some(true));
    let path = rig.dir.path().join("config.toml");
    std::fs::write(&path, "fps = 30\npause-on-battery = true\n").unwrap();
    rig.daemon
        .on_config_change(&path, FileChange::Contents(std::fs::read(&path).unwrap()));
    assert!(rig.power.running());
    assert_eq!(rig.pauses(1), [true]);
    assert_eq!(rig.reloads(), 0);
    std::fs::write(&path, "fps = 30\n").unwrap();
    rig.daemon
        .on_config_change(&path, FileChange::Contents(std::fs::read(&path).unwrap()));
    assert!(!rig.power.running());
    assert_eq!(rig.pauses(2), [true, false]);
}

#[test]
fn without_a_power_source_the_setting_is_kept_but_never_pauses() {
    let rig = Rig::build("", None, false);
    rig.set_battery_setting(true);
    assert!(rig.handler().settings().unwrap().pause_on_battery);
    assert!(rig.file().contains("pause-on-battery = true"));
    assert_eq!(rig.power.started.load(Ordering::SeqCst), 0);
    assert!(!rig.handler().state().unwrap().paused);
    assert!(rig.pauses(0).is_empty());
}

/// What the app's Settings toggle does: `SetSettings` over a real IPC connection.
#[test]
fn the_app_toggle_works_over_ipc() {
    let rig = Rig::new("", Some(false));
    let endpoint = if cfg!(unix) {
        lumenwall_ipc::Endpoint::Path(rig.dir.path().join("battery.sock"))
    } else {
        lumenwall_ipc::Endpoint::Name(format!("lumenwall-battery-test-{}", std::process::id()))
    };
    let _server =
        lumenwall_ipc::Server::bind(&endpoint, rig.daemon.handler(), "lumenwall", "0.0.0").unwrap();
    let client = lumenwall_ipc::Client::connect(&endpoint, "lumenwall-ui").unwrap();
    let settings = client.settings().unwrap();
    assert!(!settings.pause_on_battery);
    client
        .set_settings(Settings {
            pause_on_battery: true,
            ..settings
        })
        .unwrap();
    assert!(client.settings().unwrap().pause_on_battery);
    assert!(rig.file().contains("pause-on-battery = true"));
    assert!(rig.power.running());
    rig.power.set(true);
    assert_eq!(rig.pauses(1), [true]);
    let state = client.state().unwrap();
    assert!(state.paused);
    assert_eq!(state.pause_reason, Some(PauseReason::Battery));
}

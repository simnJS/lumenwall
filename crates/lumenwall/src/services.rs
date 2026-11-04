//! Interfaces between the application layer and the subsystems implemented by the other
//! crates (assets, platform, rendering, audio...). Only the contracts live here; the
//! implementations are plugged in by `main`.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::plan::{ExecutionPlan, PlanChange};
use crate::playlist::Playlist;
use crate::settings::WallpaperSource;

/// Error reported by a subsystem; its message is shown to the user as is.
pub type ServiceError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Turns a Workshop ID or a path into the wallpaper folder (spec 07 §3.1). Provided by
/// `lumenwall-assets` (Steam library lookup, `$HOME` roots, Windows registry...).
pub trait WallpaperResolver: Send + Sync {
    /// Returns the wallpaper folder; an unknown ID or a missing folder is an error
    /// ("workshop folder not found for application 431960 and item <id>").
    fn resolve(&self, source: &WallpaperSource) -> Result<PathBuf, ServiceError>;
}

/// Wallpaper Engine playlists (spec 07 §17), e.g. [`crate::playlist::WeConfigCatalog`].
pub trait PlaylistCatalog: Send + Sync {
    /// Folder of the first usable item of the playlist; an unknown name is an error that
    /// lists the available names.
    fn first_wallpaper(&self, name: &str) -> Result<PathBuf, ServiceError>;

    /// The whole playlist (items and settings), to run it; same errors as
    /// [`first_wallpaper`](Self::first_wallpaper). Without it, a playlist only shows its first
    /// item.
    fn playlist(&self, name: &str) -> Result<Playlist, ServiceError> {
        Err(format!("playlist '{name}' cannot be played by this build").into())
    }

    /// Every playlist, sorted by name, read again from the source (a client lists them).
    fn list(&self) -> Result<Vec<Playlist>, ServiceError> {
        Ok(Vec::new())
    }
}

/// Receives `true` on battery, `false` on mains (see [`PowerSource::watch`]).
pub type PowerCallback = Arc<dyn Fn(bool) + Send + Sync>;

/// Whether the computer runs on battery, for "pause on battery" (`lumenwall-audio`'s
/// `PowerWatch`: Windows power notifications, UPower on Linux). Only watched while the setting
/// is on.
pub trait PowerSource: Send + Sync {
    /// Starts following the power source: `on_change` gets the current state as soon as it is
    /// known, then each change (possibly the same value twice), from any thread, maybe before
    /// this returns; it must not block. Dropping the returned value stops the notifications.
    fn watch(&self, on_change: PowerCallback) -> Box<dyn Send>;
}

/// Wakes the engine's blocking event loop from another thread (calloop ping, Win32 event...)
/// so that it drains the control channel. Must be cheap and never block.
pub trait Waker: Send + Sync {
    fn wake(&self);
}

/// Why the engine stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// SIGINT/SIGTERM (Unix signal number) or console control event (Windows event code).
    Signal(i32),
    /// The output driver asked to stop: window closed, compositor connection lost (§1.2).
    Driver,
    /// An IPC client asked the daemon to stop.
    Client,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Signal(n) => write!(f, "stop requested by signal {n}"),
            Self::Driver => f.write_str("stop requested by the output driver"),
            Self::Client => f.write_str("stop requested by a client"),
        }
    }
}

/// Message from the application layer to the running engine.
#[derive(Clone, Debug)]
pub enum Control {
    /// Leave the main loop cleanly.
    Stop(StopReason),
    /// The configuration changed (file edited, or a client chose something): switch to
    /// `plan` without restarting. `changes` says what differs from the previous plan. When
    /// `done` is set, the engine reports whether the plan applied (`Err`: nothing changed).
    Reload {
        plan: Arc<ExecutionPlan>,
        changes: Vec<PlanChange>,
        done: Option<mpsc::SyncSender<Result<(), String>>>,
    },
    /// Live property values (preview), the complete set of overrides: properties left out
    /// go back to their default.
    Properties(BTreeMap<String, String>),
    /// User pause of every output.
    Pause(bool),
}

/// What the engine shows, for the daemon. Read without waking the engine.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineSnapshot {
    /// Surfaces open (false while idle without wallpaper).
    pub active: bool,
    /// Known outputs, sorted by name.
    pub outputs: Vec<OutputSnapshot>,
    pub targets: Vec<TargetSnapshot>,
}

impl EngineSnapshot {
    pub fn target_of(&self, output: &str) -> Option<&TargetSnapshot> {
        self.outputs
            .iter()
            .find(|o| o.info.id == output)
            .and_then(|o| o.target)
            .and_then(|t| self.targets.get(t))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutputSnapshot {
    pub info: lumenwall_ipc::Output,
    /// Has a surface in the current layout.
    pub shown: bool,
    pub target: Option<usize>,
    /// Platform pause (fullscreen, lock, hidden).
    pub paused: Option<lumenwall_ipc::PauseReason>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TargetSnapshot {
    pub key: String,
    pub wallpaper: PathBuf,
    pub title: String,
    pub scaling: lumenwall_ipc::Scaling,
    /// `Err` holds the error of a renderer that could not be built.
    pub status: TargetStatus,
    pub properties: lumenwall_ipc::PropertyMap,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TargetStatus {
    Loading,
    Running,
    Error(String),
}

/// Frame counters since the previous [`Monitor::take_counters`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameCounters {
    pub elapsed: Duration,
    /// `(output, frames, CPU time)`.
    pub outputs: Vec<(String, u32, Duration)>,
    pub gpu: Duration,
    pub gpu_samples: u32,
    pub gpu_memory: u64,
}

/// Part of the snapshot that changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorChange {
    Outputs,
    State,
    Properties,
}

pub type MonitorListener = Arc<dyn Fn(MonitorChange) + Send + Sync>;

/// Engine status for the IPC daemon.
pub trait Monitor: Send + Sync {
    fn snapshot(&self) -> EngineSnapshot;
    /// Called on the engine thread after a change; must not block.
    fn set_listener(&self, listener: Option<MonitorListener>);
    /// Frame counters on/off (only while a client looks at statistics).
    fn set_sampling(&self, on: bool);
    fn take_counters(&self) -> FrameCounters;
}

/// Sending half of the control channel: queues a message, then wakes the engine.
#[derive(Clone)]
pub struct ControlSender {
    tx: mpsc::Sender<Control>,
    waker: Arc<dyn Waker>,
}

impl ControlSender {
    /// Returns `false` once the engine has dropped the receiver.
    pub fn send(&self, message: Control) -> bool {
        let sent = self.tx.send(message).is_ok();
        if sent {
            self.waker.wake();
        }
        sent
    }
}

impl std::fmt::Debug for ControlSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ControlSender")
    }
}

/// Creates the control channel; the receiver goes to [`Engine::run`].
pub fn control_channel(waker: Arc<dyn Waker>) -> (ControlSender, mpsc::Receiver<Control>) {
    let (tx, rx) = mpsc::channel();
    (ControlSender { tx, waker }, rx)
}

/// The wallpaper engine proper: output drivers, renderers, audio, video, web.
///
/// Called in this order (spec 07 §1.1): [`load`](Self::load), then either
/// [`write_properties`](Self::write_properties) for `--list-properties`, or
/// [`start`](Self::start), [`write_structure`](Self::write_structure) for `--dump-structure`,
/// and [`run`](Self::run).
pub trait Engine {
    /// Waker for the event loop that [`run`](Self::run) will block on.
    fn waker(&self) -> Arc<dyn Waker>;

    /// Status for the IPC daemon; `None`: the daemon only forwards controls.
    fn monitor(&self) -> Option<Arc<dyn Monitor>> {
        None
    }

    /// Reads every wallpaper of the plan (`project.json`), applies the property overrides,
    /// prepares playlists. Must not open windows, audio devices or the web engine when
    /// `plan.list_properties` is set.
    fn load(&mut self, plan: &ExecutionPlan) -> Result<(), ServiceError>;

    /// `--list-properties` output (spec 07 §3.6).
    fn write_properties(&mut self, out: &mut dyn io::Write) -> Result<(), ServiceError>;

    /// Creates the output driver, the fullscreen detector, audio and one renderer per
    /// output or span group (spec 07 §1.1 steps 11–13).
    fn start(&mut self) -> Result<(), ServiceError>;

    /// `--dump-structure` output (spec 07 §20).
    fn write_structure(&mut self, out: &mut dyn io::Write) -> Result<(), ServiceError>;

    /// Main loop. Blocks on OS events (no polling), drains `control` when woken, and
    /// returns when a [`Control::Stop`] arrives or the driver asks to stop.
    fn run(&mut self, control: mpsc::Receiver<Control>) -> Result<StopReason, ServiceError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingWaker(AtomicUsize);

    impl Waker for CountingWaker {
        fn wake(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn send_queues_then_wakes() {
        let waker = Arc::new(CountingWaker::default());
        let (tx, rx) = control_channel(waker.clone());
        assert!(tx.send(Control::Stop(StopReason::Signal(2))));
        assert_eq!(waker.0.load(Ordering::SeqCst), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(Control::Stop(StopReason::Signal(2)))
        ));
        drop(rx);
        assert!(!tx.send(Control::Stop(StopReason::Driver)));
        assert_eq!(waker.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stop_reason_messages() {
        assert_eq!(
            StopReason::Signal(15).to_string(),
            "stop requested by signal 15"
        );
        assert_eq!(
            StopReason::Driver.to_string(),
            "stop requested by the output driver"
        );
        assert_eq!(StopReason::Client.to_string(), "stop requested by a client");
    }
}

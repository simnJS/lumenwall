//! Interfaces between the application layer and the subsystems implemented by the other
//! crates (assets, platform, rendering, audio...). Only the contracts live here; the
//! implementations are plugged in by `main`.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use crate::plan::{ExecutionPlan, PlanChange};
use crate::settings::WallpaperSource;

/// Error reported by a subsystem; its message is shown to the user as is.
pub type ServiceError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Turns a Workshop ID or a path into the wallpaper folder (spec 07 §3.1). Provided by
/// `lumen-assets` (Steam library lookup, `$HOME` roots, Windows registry...).
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
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Signal(n) => write!(f, "stop requested by signal {n}"),
            Self::Driver => f.write_str("stop requested by the output driver"),
        }
    }
}

/// Message from the application layer to the running engine.
#[derive(Clone, Debug)]
pub enum Control {
    /// Leave the main loop cleanly.
    Stop(StopReason),
    /// The configuration file changed: switch to `plan`. `changes` says what differs from
    /// the previous plan so the engine only redoes that work.
    Reload {
        plan: Arc<ExecutionPlan>,
        changes: Vec<PlanChange>,
    },
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
    }
}

//! "Another application is playing" detection (automute, spec 07 §12, §22).
//!
//! The reference queries the stream list synchronously every frame; Lumenwall subscribes to
//! sound-server events on a background thread instead (PARITY-POLICY rule 1). The result is a
//! flag read with one atomic load by [`AutomuteDetector::update`], plus an optional callback
//! fired on each change so a player can react while nothing is rendered.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub mod pipewire;
#[cfg(all(target_os = "linux", feature = "pulse"))]
pub mod pulse;
#[cfg(all(windows, feature = "wasapi"))]
pub mod windows;

/// `true` if an `application.process.id` value designates this process.
#[cfg(all(target_os = "linux", any(feature = "pulse", feature = "pipewire")))]
pub(crate) fn is_own_pid(value: &str, own: u32) -> bool {
    value.trim().parse::<u32>() == Ok(own)
}

/// Automute settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutomuteSettings {
    /// `false` with `--noautomute`: neutral detector, never "playing", no server connection.
    pub enabled: bool,
}

impl Default for AutomuteSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Callback fired (on a backend thread) when the "another application plays" state changes.
pub type ChangeCallback = Arc<dyn Fn(bool) + Send + Sync>;

/// Event-driven source of the "another application plays" state.
pub trait PlaybackWatcher: Send {
    /// Latest state from server events; never blocks.
    fn other_app_playing(&self) -> bool;
    /// Whether the watcher currently has a server connection.
    fn is_connected(&self) -> bool;
}

/// Shared flag written by watcher backends.
#[derive(Clone)]
pub struct PlayingFlag {
    inner: Arc<FlagInner>,
}

struct FlagInner {
    playing: AtomicBool,
    connected: AtomicBool,
    on_change: Option<ChangeCallback>,
}

impl PlayingFlag {
    /// New flag ("nothing plays", disconnected).
    pub fn new(on_change: Option<ChangeCallback>) -> Self {
        Self {
            inner: Arc::new(FlagInner {
                playing: AtomicBool::new(false),
                connected: AtomicBool::new(false),
                on_change,
            }),
        }
    }

    /// Updates the state; fires the callback on change.
    pub fn set_playing(&self, playing: bool) {
        if self.inner.playing.swap(playing, Ordering::AcqRel) != playing
            && let Some(cb) = &self.inner.on_change
        {
            cb(playing);
        }
    }

    /// Updates the connection state. Losing the server means "nothing plays" (§12.2).
    pub fn set_connected(&self, connected: bool) {
        self.inner.connected.store(connected, Ordering::Release);
        if !connected {
            self.set_playing(false);
        }
    }

    /// Current state.
    pub fn playing(&self) -> bool {
        self.inner.playing.load(Ordering::Acquire)
    }

    /// Connection state.
    pub fn connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }
}

/// Automute decision (§12.1).
pub struct AutomuteDetector {
    enabled: bool,
    watcher: Option<Box<dyn PlaybackWatcher>>,
    playing: bool,
}

impl AutomuteDetector {
    /// Builds the detector and, when enabled, starts the platform watcher. Never blocks.
    pub fn new(settings: AutomuteSettings, on_change: Option<ChangeCallback>) -> Self {
        let watcher = if settings.enabled {
            default_watcher(on_change)
        } else {
            None
        };
        Self {
            enabled: settings.enabled,
            watcher,
            playing: false,
        }
    }

    /// Detector using a given watcher (tests, custom backends).
    pub fn with_watcher(
        settings: AutomuteSettings,
        watcher: Option<Box<dyn PlaybackWatcher>>,
    ) -> Self {
        Self {
            enabled: settings.enabled,
            watcher: if settings.enabled { watcher } else { None },
            playing: false,
        }
    }

    /// Recomputes the state; call once per non-paused frame with the fullscreen detector's
    /// answer. Costs one atomic load.
    pub fn update(&mut self, fullscreen: bool) -> bool {
        self.playing = if !self.enabled {
            // `--noautomute`: neutral, even in fullscreen.
            false
        } else if fullscreen {
            // parity: a fullscreen window counts as "another application plays" (§12.1 rule 2).
            true
        } else {
            self.watcher.as_ref().is_some_and(|w| w.other_app_playing())
        };
        self.playing
    }

    /// State computed by the last [`update`](Self::update).
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Whether a sound-server connection is established.
    pub fn is_connected(&self) -> bool {
        self.watcher.as_ref().is_some_and(|w| w.is_connected())
    }
}

/// Starts the best watcher compiled in and available, or `None`.
pub fn default_watcher(on_change: Option<ChangeCallback>) -> Option<Box<dyn PlaybackWatcher>> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        if crate::capture::pipewire::server_available() {
            return Some(Box::new(pipewire::PipeWireWatcher::new(on_change)));
        }
    }
    #[cfg(all(target_os = "linux", feature = "pulse"))]
    {
        return Some(Box::new(pulse::PulseWatcher::new(None, on_change)));
    }
    #[cfg(all(windows, feature = "wasapi"))]
    {
        return Some(Box::new(windows::SessionWatcher::new(on_change)));
    }
    #[allow(unreachable_code)]
    {
        let _ = on_change;
        tracing::info!("automute: no backend compiled in, other applications are never detected");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(bool);
    impl PlaybackWatcher for Fixed {
        fn other_app_playing(&self) -> bool {
            self.0
        }
        fn is_connected(&self) -> bool {
            false
        }
    }

    /// Spec §21.1 test 6: settings without a server.
    #[test]
    fn rules_without_server() {
        let mut d = AutomuteDetector::with_watcher(AutomuteSettings { enabled: true }, None);
        assert!(!d.update(false));
        assert!(d.update(true), "fullscreen forces 'playing'");
        let mut d = AutomuteDetector::with_watcher(
            AutomuteSettings { enabled: false },
            Some(Box::new(Fixed(true))),
        );
        assert!(
            !d.update(true),
            "automute disabled + fullscreen → nothing plays"
        );
        assert!(!d.update(false));
    }

    #[test]
    fn watcher_state_is_used() {
        let mut d = AutomuteDetector::with_watcher(
            AutomuteSettings::default(),
            Some(Box::new(Fixed(true))),
        );
        assert!(d.update(false));
        assert!(d.is_playing());
    }

    #[test]
    fn flag_fires_on_change_only() {
        use std::sync::atomic::AtomicUsize;
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let flag = PlayingFlag::new(Some(Arc::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        })));
        flag.set_playing(false);
        flag.set_playing(true);
        flag.set_playing(true);
        flag.set_connected(false);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert!(!flag.playing());
    }
}

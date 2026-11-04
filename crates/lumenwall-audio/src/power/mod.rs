//! On battery or on mains, for the application's "pause on battery" setting (Wallpaper
//! Engine's "on battery: pause / keep running").
//!
//! [`PowerWatch::start`] reports the current state, then every change, through a callback:
//!
//! - Windows: `PowerSettingRegisterNotification(GUID_ACDC_POWER_SOURCE)` in callback mode, no
//!   window and no thread of ours: the system calls back from one of its threads when the power
//!   source changes (and once right after the registration). The first value comes from
//!   `GetSystemPowerStatus`.
//! - Linux: UPower's `OnBattery` property on the system bus, read once, then followed through
//!   its `PropertiesChanged` signal (read again when UPower restarts). One thread, blocked on the
//!   bus socket.
//!
//! Nothing polls. Without a power service (no UPower, no system bus) the state stays "on mains"
//! and nothing is paused for battery (PARITY-POLICY rule 2).

use std::sync::Arc;

#[cfg(any(test, all(windows, feature = "power")))]
mod acdc;
#[cfg(all(target_os = "linux", feature = "power"))]
mod upower;
#[cfg(all(windows, feature = "power"))]
mod windows;

/// Receives `true` on battery (or another short-term source such as a UPS), `false` on mains.
/// Called from a system or watcher thread, possibly twice with the same value: it must be cheap
/// and must not block.
pub type PowerCallback = Arc<dyn Fn(bool) + Send + Sync>;

/// A running power source; dropping it stops the notifications (no callback afterwards).
pub struct PowerWatch {
    _source: Option<Box<dyn Send>>,
}

impl std::fmt::Debug for PowerWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PowerWatch")
            .field("running", &self._source.is_some())
            .finish()
    }
}

impl PowerWatch {
    /// Whether this build follows the power source on this system.
    pub const SUPPORTED: bool = cfg!(all(feature = "power", any(windows, target_os = "linux")));

    /// Starts following the power source. `on_change` gets the current state as soon as it is
    /// known, then each change. Never blocks; on a system without a power service it is never
    /// called (the caller's default, on mains, stays).
    pub fn start(on_change: PowerCallback) -> Self {
        #[cfg(all(windows, feature = "power"))]
        let source = windows::start(on_change).map(|r| Box::new(r) as Box<dyn Send>);
        #[cfg(all(target_os = "linux", feature = "power"))]
        let source = upower::start(on_change).map(|r| Box::new(r) as Box<dyn Send>);
        #[cfg(not(all(feature = "power", any(windows, target_os = "linux"))))]
        let source = {
            let _ = on_change;
            tracing::info!("power source: not available in this build");
            None
        };
        Self { _source: source }
    }
}

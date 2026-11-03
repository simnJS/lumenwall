//! Restricts the Vulkan loader to the driver of the GPU used last time (Linux).
//!
//! Without a restriction the loader opens every installed driver to enumerate the GPUs, and the
//! Mesa ones stay mapped (measured with 9 ICDs installed and an NVIDIA GPU: `libLLVM`, RADV and
//! lavapipe, ≈ 50 MiB RSS, 25–40 ms more per Vulkan instance, created at start and at every GPU
//! restore). The engine remembers the driver of the adapter it used
//! (`<cache>/vulkan-driver`: manifest glob, PCI vendor and device ids) and
//! [`restrict_vulkan_loader`], called first thing in `main`, sets `VK_LOADER_DRIVERS_SELECT` to
//! that glob for the whole process. When the restricted loader finds no adapter, or not the
//! remembered one (GPU removed, driver changed), the variable is removed and the instance
//! created again with every driver ([`lift`]); the new choice is remembered.
//!
//! Nothing is done when the user sets one of the loader's driver variables, nor on Windows,
//! where drivers are registered under other names (the variable would select nothing).

use std::path::PathBuf;
use std::sync::Mutex;

const SELECT: &str = "VK_LOADER_DRIVERS_SELECT";
/// Loader variables that choose drivers: set by the user, they win.
const USER_VARS: [&str; 5] = [
    SELECT,
    "VK_LOADER_DRIVERS_DISABLE",
    "VK_DRIVER_FILES",
    "VK_ICD_FILENAMES",
    "VK_ADD_DRIVER_FILES",
];

/// The adapter of the last run and its driver's manifest glob.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Remembered {
    glob: String,
    vendor: u32,
    device: u32,
}

impl Remembered {
    fn parse(text: &str) -> Option<Self> {
        let mut it = text.split_whitespace();
        let glob = it.next()?;
        // A glob of the manifest name only: no separator the loader would read as a list.
        if glob.is_empty() || glob.contains(',') || glob.len() > 64 {
            return None;
        }
        let hex = |s: Option<&str>| u32::from_str_radix(s?, 16).ok();
        Some(Self {
            glob: glob.to_owned(),
            vendor: hex(it.next())?,
            device: hex(it.next())?,
        })
    }

    fn format(&self) -> String {
        format!("{} {:04x} {:04x}\n", self.glob, self.vendor, self.device)
    }
}

/// What this process did with the loader.
#[derive(Debug, Default)]
enum State {
    /// Nothing decided yet: no memory, or not Linux.
    #[default]
    Free,
    /// The user chose drivers through the environment: leave everything alone.
    User,
    /// `VK_LOADER_DRIVERS_SELECT` set by [`restrict_vulkan_loader`].
    Restricted(Remembered),
    /// The restriction was lifted (or there was none): every driver is visible.
    Lifted,
}

static STATE: Mutex<State> = Mutex::new(State::Free);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn memory_file() -> Option<PathBuf> {
    crate::cache_dir().map(|d| d.join("vulkan-driver"))
}

/// Sets `VK_LOADER_DRIVERS_SELECT` to the driver of the GPU used last time, unless the user
/// chose drivers through the environment. Linux only (no effect elsewhere).
///
/// Call it first thing in `main`, before any thread exists: it changes the process
/// environment.
pub fn restrict_vulkan_loader() {
    if !cfg!(target_os = "linux") {
        return;
    }
    if USER_VARS.iter().any(|v| std::env::var_os(v).is_some()) {
        *state() = State::User;
        return;
    }
    let Some(remembered) = memory_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| Remembered::parse(&t))
    else {
        return;
    };
    // SAFETY: called at the start of `main`, before any other thread exists (documented
    // contract), so nothing reads the environment concurrently.
    unsafe { std::env::set_var(SELECT, &remembered.glob) };
    *state() = State::Restricted(remembered);
}

/// Whether `ctx` was opened under the restriction on another adapter than the remembered one
/// (the remembered GPU is gone: [`lift`] and open again).
pub(crate) fn wrong_adapter(ctx: &lumenwall_render::GpuContext) -> bool {
    let info = ctx.adapter().get_info();
    match &*state() {
        State::Restricted(r) => (info.vendor, info.device) != (r.vendor, r.device),
        _ => false,
    }
}

/// Removes the restriction for the rest of the process. Returns false when there was none
/// (nothing else to try).
pub(crate) fn lift() -> bool {
    let mut s = state();
    if !matches!(*s, State::Restricted(_)) {
        return false;
    }
    // SAFETY: the variable exists (set by `restrict_vulkan_loader` before any thread), so glibc
    // removes its entry by moving the following pointers down in place: the environment array
    // is neither reallocated nor freed, and Rust readers hold std's environment lock. The
    // loader reads it on this thread only, while creating the instance.
    unsafe { std::env::remove_var(SELECT) };
    *s = State::Lifted;
    true
}

/// Remembers the driver of `ctx`'s adapter for the next start (when the loader was not
/// restricted already, and the driver's manifest name is known).
pub(crate) fn remember(ctx: &lumenwall_render::GpuContext) {
    if !cfg!(target_os = "linux") || !matches!(*state(), State::Free | State::Lifted) {
        return;
    }
    let info = ctx.adapter().get_info();
    let Some(glob) = lumenwall_render::loader::adapter_manifest_glob(ctx.adapter()) else {
        return;
    };
    let new = Remembered {
        glob: glob.to_owned(),
        vendor: info.vendor,
        device: info.device,
    };
    let Some(path) = memory_file() else {
        return;
    };
    let old = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| Remembered::parse(&t));
    if old.as_ref() == Some(&new) {
        return;
    }
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, new.format())?;
        std::fs::rename(&tmp, &path)
    };
    match write() {
        Ok(()) => tracing::info!(
            "Vulkan driver remembered for the next start: {} ({:04x}:{:04x})",
            new.glob,
            new.vendor,
            new.device
        ),
        Err(e) => tracing::debug!("{}: {e}", path.display()),
    }
}

/// The drivers the loader sees, for the logs.
pub(crate) fn describe() -> String {
    match &*state() {
        State::Restricted(r) => r.glob.clone(),
        State::User => "chosen by the environment".into(),
        State::Free | State::Lifted => "all".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_round_trip() {
        let r = Remembered {
            glob: "*nvidia*".into(),
            vendor: 0x10de,
            device: 0x2702,
        };
        assert_eq!(r.format(), "*nvidia* 10de 2702\n");
        assert_eq!(Remembered::parse(&r.format()), Some(r));
        assert_eq!(Remembered::parse(""), None);
        assert_eq!(Remembered::parse("*nvidia*"), None);
        assert_eq!(Remembered::parse("*a*,*b* 10de 2702"), None);
        assert_eq!(Remembered::parse("*nvidia* zz 2702"), None);
    }
}

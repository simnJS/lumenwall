//! Desktop integration: wallpaper surfaces, monitors, fullscreen detection, input.
//!
//! [`open`] returns a [`WallpaperSurface`]: one native surface per output, placed behind the desktop
//! (wlr-layer-shell on Wayland, `_NET_WM_WINDOW_TYPE_DESKTOP` windows on X11, Progman/WorkerW children on
//! Windows) or a plain window in debug mode. The renderer creates its GPU surfaces from
//! [`WallpaperSurface::raw_display_handle`] / [`WallpaperSurface::raw_window_handle`]; this crate never
//! touches the GPU.
//!
//! The loop is fully blocking: [`WallpaperSurface::wait`] sleeps in the OS (calloop on Linux,
//! `MsgWaitForMultipleObjectsEx` on Windows) until an event arrives or a requested frame becomes due. Paused or
//! idle outputs arm no timer, so a covered desktop costs no wake-up at all (on XWayland, a desktop hidden by a
//! native Wayland window costs one probe per second, see `starvation`).
//!
//! ```no_run
//! use lumen_platform::{Config, Event, Wait};
//!
//! let mut surfaces = lumen_platform::open(&Config::default())?;
//! loop {
//!     surfaces.wait(Wait::Block)?;
//!     while let Some(event) = surfaces.next_event() {
//!         match event {
//!             Event::OutputAdded(id) => {
//!                 // create the wgpu surface from surfaces.raw_window_handle(id), configure it with
//!                 // surfaces.output(id).physical_size, then:
//!                 surfaces.request_frame(id);
//!             }
//!             Event::Frame(id) => {
//!                 // render, then: surfaces.pre_present(id); texture.present(); surfaces.request_frame(id);
//!             }
//!             Event::CloseRequested => return Ok(()),
//!             _ => {}
//!         }
//!     }
//! }
//! # Ok::<(), lumen_platform::PlatformError>(())
//! ```

// The shared plumbing (scheduling, selection, sorting) is only used by the desktop backends; building
// without any of them (e.g. `--no-default-features` on Linux) must not warn.
#![cfg_attr(
    not(any(
        windows,
        all(target_os = "linux", any(feature = "wayland", feature = "x11"))
    )),
    allow(dead_code)
)]

pub mod fullscreen;
pub mod geometry;
pub mod mouse;
pub mod uv;
pub mod win_logic;

mod error;
mod sched;
#[cfg(any(test, all(target_os = "linux", feature = "x11")))]
mod starvation;
mod types;

#[cfg(all(target_os = "linux", any(feature = "wayland", feature = "x11")))]
mod linux;
#[cfg(windows)]
mod win32;
#[cfg(feature = "windowed")]
mod windowed;

use std::fmt;
use std::sync::Arc;

pub use error::PlatformError;
pub use raw_window_handle;
pub use types::{
    BackendChoice, BackendKind, Config, Event, FullscreenPolicy, Layer, Mode, OutputId, OutputInfo,
    OutputSelection, PauseReasons, PauseScope, PointerButton, PointerEvent, PointerKind,
    PointerMode, Wait, WindowGeometry,
};

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

/// Wakes a blocked [`WallpaperSurface::wait`] from any thread; the wait then yields [`Event::Wake`].
///
/// Cheap to clone; waking never allocates.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    #[cfg_attr(
        not(any(windows, target_os = "linux", feature = "windowed")),
        allow(dead_code)
    )]
    pub(crate) fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Interrupts the wait.
    pub fn wake(&self) {
        (self.0)();
    }
}

impl fmt::Debug for Waker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Waker")
    }
}

/// Wallpaper surfaces of one backend, with their event stream.
///
/// # Handles
///
/// A window handle stays valid until the [`Event::OutputRemoved`] for its output has been returned by
/// [`next_event`](Self::next_event) **and** `wait` has been called again; the display handle stays valid for
/// the lifetime of the backend. Drop GPU surfaces before either goes away.
///
/// # Frames
///
/// Call [`request_frame`](Self::request_frame) when an output needs a new image; an [`Event::Frame`] follows
/// when the output is visible, the compositor is ready and the frame cap (`Config::max_fps`) allows it.
/// Call [`pre_present`](Self::pre_present) right before presenting: on Wayland it attaches the frame
/// callback and the scale/viewport state to the commit made by the present (prefer a non-blocking present
/// mode such as Mailbox there, pacing comes from these callbacks).
pub trait WallpaperSurface {
    /// Which backend this is.
    fn backend(&self) -> BackendKind;

    /// Outputs that currently have a surface, sorted by name (spec 07 §4.3).
    fn outputs(&self) -> &[OutputInfo];

    /// One output.
    fn output(&self, id: OutputId) -> Option<&OutputInfo> {
        self.outputs().iter().find(|o| o.id == id)
    }

    /// Display connection handle, shared by every output (pass it to `wgpu::InstanceDescriptor` too).
    fn raw_display_handle(&self) -> RawDisplayHandle;

    /// Native window/surface of an output.
    fn raw_window_handle(&self, id: OutputId) -> Option<RawWindowHandle>;

    /// Asks for an [`Event::Frame`] for this output.
    fn request_frame(&mut self, id: OutputId);

    /// Must be called right before presenting a frame of this output (cheap no-op on most backends).
    fn pre_present(&mut self, id: OutputId);

    /// Changes the frame cap; 0 disables it.
    fn set_max_fps(&mut self, fps: u32);

    /// Current pause reasons of an output.
    fn pause_reasons(&self, id: OutputId) -> PauseReasons;

    /// Handle to interrupt [`wait`](Self::wait) from another thread.
    fn waker(&self) -> Waker;

    /// Processes OS events, blocking according to `wait` while the event queue is empty.
    ///
    /// Returns as soon as at least one event is queued (or the deadline passed / nothing was available for
    /// [`Wait::Poll`]). Never busy-waits.
    fn wait(&mut self, wait: Wait) -> Result<(), PlatformError>;

    /// Pops the next queued event.
    fn next_event(&mut self) -> Option<Event>;
}

/// Opens the backend described by `config`.
pub fn open(config: &Config) -> Result<Box<dyn WallpaperSurface>, PlatformError> {
    match config.mode {
        Mode::Window(geometry) => open_windowed(config, geometry),
        Mode::Desktop => open_desktop(config),
    }
}

#[cfg(feature = "windowed")]
fn open_windowed(
    config: &Config,
    geometry: Option<WindowGeometry>,
) -> Result<Box<dyn WallpaperSurface>, PlatformError> {
    Ok(Box::new(windowed::WindowedBackend::new(config, geometry)?))
}

#[cfg(not(feature = "windowed"))]
fn open_windowed(
    _: &Config,
    _: Option<WindowGeometry>,
) -> Result<Box<dyn WallpaperSurface>, PlatformError> {
    Err(PlatformError::BackendUnavailable("windowed mode"))
}

/// Backend for a session, from `XDG_SESSION_TYPE`, then the presence of `WAYLAND_DISPLAY` / `DISPLAY`.
///
/// The reference aborts when `XDG_SESSION_TYPE` is unset or unknown; Lumen falls back on the display
/// variables instead (parity policy: fatal errors are fixed).
pub fn session_backend(
    xdg_session_type: Option<&str>,
    has_wayland_display: bool,
    has_x_display: bool,
) -> Option<BackendKind> {
    match xdg_session_type {
        Some("wayland") => Some(BackendKind::Wayland),
        Some("x11") => Some(BackendKind::X11),
        _ if has_wayland_display => Some(BackendKind::Wayland),
        _ if has_x_display => Some(BackendKind::X11),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn open_desktop(config: &Config) -> Result<Box<dyn WallpaperSurface>, PlatformError> {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let kind = match config.backend {
        BackendChoice::Wayland => BackendKind::Wayland,
        BackendChoice::X11 => BackendKind::X11,
        BackendChoice::Auto => {
            let session = std::env::var("XDG_SESSION_TYPE").ok();
            session_backend(session.as_deref(), set("WAYLAND_DISPLAY"), set("DISPLAY"))
                .ok_or(PlatformError::NoDisplay)?
        }
    };
    match kind {
        #[cfg(feature = "wayland")]
        BackendKind::Wayland => Ok(Box::new(linux::wayland::WaylandBackend::new(config)?)),
        #[cfg(feature = "x11")]
        BackendKind::X11 => Ok(Box::new(linux::x11::X11Backend::new(config)?)),
        #[cfg(not(feature = "wayland"))]
        BackendKind::Wayland => Err(PlatformError::BackendUnavailable("Wayland")),
        #[cfg(not(feature = "x11"))]
        BackendKind::X11 => Err(PlatformError::BackendUnavailable("X11")),
        BackendKind::Windows | BackendKind::Windowed => {
            Err(PlatformError::BackendUnavailable("desktop"))
        }
    }
}

#[cfg(windows)]
fn open_desktop(config: &Config) -> Result<Box<dyn WallpaperSurface>, PlatformError> {
    Ok(Box::new(win32::Win32Backend::new(config)?))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn open_desktop(_: &Config) -> Result<Box<dyn WallpaperSurface>, PlatformError> {
    Err(PlatformError::BackendUnavailable(
        "desktop wallpaper on this OS",
    ))
}

/// Sorts outputs by name, the order the reference renders and captures them in (spec 07 §4.3).
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub(crate) fn sort_outputs(outputs: &mut [OutputInfo]) {
    outputs.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_detection() {
        assert_eq!(
            session_backend(Some("wayland"), false, true),
            Some(BackendKind::Wayland)
        );
        assert_eq!(
            session_backend(Some("x11"), true, true),
            Some(BackendKind::X11)
        );
        assert_eq!(
            session_backend(Some("tty"), false, true),
            Some(BackendKind::X11)
        );
        assert_eq!(
            session_backend(None, true, true),
            Some(BackendKind::Wayland)
        );
        assert_eq!(session_backend(None, false, false), None);
    }

    #[test]
    fn waker_calls_back() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let w = Waker::new(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        });
        w.clone().wake();
        w.wake();
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn outputs_sorted_by_name() {
        let mk = |id, name: &str| OutputInfo {
            id: OutputId(id),
            name: name.into(),
            description: None,
            position: (0, 0),
            logical_size: (1, 1),
            physical_size: (1, 1),
            scale: 1.0,
            refresh_mhz: None,
        };
        let mut v = vec![mk(0, "HDMI-A-1"), mk(1, "DP-2"), mk(2, "DP-1")];
        sort_outputs(&mut v);
        assert_eq!(
            v.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
            ["DP-1", "DP-2", "HDMI-A-1"]
        );
    }
}

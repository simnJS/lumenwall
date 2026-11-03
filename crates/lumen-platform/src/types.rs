//! Backend-independent public types: outputs, events, configuration.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

/// Identifier of an output (one wallpaper surface), stable for the lifetime of the backend.
///
/// Ids are never reused: an output that is unplugged and plugged back gets a new id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutputId(pub(crate) u32);

impl OutputId {
    /// Raw numeric value (for logs and maps).
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for OutputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Description of one output that carries a wallpaper surface.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
    /// Identifier used in events and handle lookups.
    pub id: OutputId,
    /// Connector name used by `-r` / `--screen-span` (`DP-1`, `HDMI-A-1`; `DISPLAY1` on Windows; `default` in a window).
    pub name: String,
    /// Human readable description (make/model) when the system provides one.
    pub description: Option<String>,
    /// Top-left corner in the global desktop space. Logical coordinates on Wayland, physical pixels elsewhere
    /// (spec 07 §4.4: this is what span groups use).
    pub position: (i32, i32),
    /// Size in logical (scale-independent) units. Equal to `physical_size` when `scale` is 1.
    pub logical_size: (u32, u32),
    /// Size of the drawable in physical pixels: configure the GPU swapchain with this.
    pub physical_size: (u32, u32),
    /// Scale factor between logical and physical units (fractional on Wayland and Windows).
    pub scale: f64,
    /// Refresh rate in millihertz when known.
    pub refresh_mhz: Option<u32>,
}

impl OutputInfo {
    /// Physical height in pixels as a float (handy for the GL Y conversion of spec 07 §10).
    pub fn physical_height(&self) -> f64 {
        f64::from(self.physical_size.1)
    }
}

/// Why an output is paused. Several reasons can hold at once.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PauseReasons(u8);

impl PauseReasons {
    /// No reason: the output is running.
    pub const NONE: Self = Self(0);
    /// A fullscreen window covers the output (or any output, see [`PauseScope`]).
    pub const FULLSCREEN: Self = Self(1);
    /// The surface is not visible: fully covered, minimised, or the compositor stopped asking for frames.
    pub const OCCLUDED: Self = Self(1 << 1);
    /// The session is locked or a screen saver runs.
    pub const LOCKED: Self = Self(1 << 2);
    /// The monitor is powered off.
    pub const DISPLAY_OFF: Self = Self(1 << 3);

    /// True when no reason is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when every reason of `other` is set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `self` without the reasons of `other`.
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Sets or clears `reason`.
    pub fn set(&mut self, reason: Self, on: bool) {
        if on {
            self.0 |= reason.0;
        } else {
            self.0 &= !reason.0;
        }
    }

    /// Raw bits.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for PauseReasons {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for PauseReasons {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl fmt::Debug for PauseReasons {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("NONE");
        }
        let names = [
            (Self::FULLSCREEN, "FULLSCREEN"),
            (Self::OCCLUDED, "OCCLUDED"),
            (Self::LOCKED, "LOCKED"),
            (Self::DISPLAY_OFF, "DISPLAY_OFF"),
        ];
        let mut first = true;
        for (flag, name) in names {
            if self.contains(flag) {
                if !first {
                    f.write_str(" | ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

/// Mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerButton {
    /// Primary button.
    Left,
    /// Secondary button.
    Right,
    /// Middle button / wheel click.
    Middle,
    /// Any other button (backend specific code).
    Other(u32),
}

/// What happened to the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerKind {
    /// The pointer entered the surface (Wayland and windowed mode only).
    Enter,
    /// The pointer left the surface (Wayland and windowed mode only).
    Leave,
    /// New position.
    Motion,
    /// A button changed state.
    Button {
        /// Which button.
        button: PointerButton,
        /// `true` on press, `false` on release.
        pressed: bool,
    },
}

/// Pointer event for one output.
///
/// `x`/`y` are physical pixels relative to the top-left corner of the output surface (Y down). Backends that
/// track the cursor globally (X11, Windows) may report positions outside the surface; the scene mapping of
/// [`crate::mouse`] clamps them like the reference does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    /// Output whose surface the coordinates are relative to.
    pub output: OutputId,
    /// Horizontal position in physical pixels.
    pub x: f64,
    /// Vertical position in physical pixels, from the top.
    pub y: f64,
    /// Event kind.
    pub kind: PointerKind,
}

/// Event produced by a [`crate::WallpaperSurface`]. Events are `Copy` and never allocate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// A surface exists for this output and has its first size: create the GPU surface now.
    OutputAdded(OutputId),
    /// The output is gone. Drop the GPU surface for it **before** the next call to `wait`, after which the
    /// native window is destroyed.
    OutputRemoved(OutputId),
    /// The drawable size changed: reconfigure the swapchain.
    Resized {
        /// Output concerned.
        output: OutputId,
        /// New size in physical pixels.
        physical_size: (u32, u32),
        /// New size in logical units.
        logical_size: (u32, u32),
    },
    /// The scale factor changed (always followed or preceded by a `Resized` when the pixel size changes).
    ScaleChanged {
        /// Output concerned.
        output: OutputId,
        /// New scale factor.
        scale: f64,
    },
    /// The output moved in the global desktop space (matters for span groups).
    Moved {
        /// Output concerned.
        output: OutputId,
        /// New top-left corner.
        position: (i32, i32),
    },
    /// The output became paused, or the set of reasons changed while paused. Stop rendering it.
    Paused {
        /// Output concerned.
        output: OutputId,
        /// Current reasons (never empty).
        reasons: PauseReasons,
    },
    /// The output runs again. A pending frame request is delivered right after as a `Frame`.
    Resumed(OutputId),
    /// The output is ready for a new frame (answer to [`crate::WallpaperSurface::request_frame`]).
    Frame(OutputId),
    /// Pointer input.
    Pointer(PointerEvent),
    /// [`crate::Waker::wake`] was called from another thread.
    Wake,
    /// The window was closed (windowed mode) or the display connection is shutting down.
    CloseRequested,
}

/// How long [`crate::WallpaperSurface::wait`] may block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Never block: process what is already available.
    Poll,
    /// Block until at least one event is queued (no timeout, no wake-up otherwise).
    Block,
    /// Block until at least one event is queued or the deadline passes.
    Until(Instant),
}

impl Wait {
    /// Convenience: block at most `timeout`.
    pub fn timeout(timeout: Duration) -> Self {
        Wait::Until(Instant::now() + timeout)
    }

    pub(crate) fn deadline(self) -> Option<Instant> {
        match self {
            Wait::Poll => Some(Instant::now()),
            Wait::Block => None,
            Wait::Until(t) => Some(t),
        }
    }
}

/// Which backend is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// wlr-layer-shell surfaces.
    Wayland,
    /// Desktop-type windows on X11.
    X11,
    /// Windows desktop (Progman/WorkerW).
    Windows,
    /// A normal window (debug mode, `-w`).
    Windowed,
}

/// Backend requested by the caller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendChoice {
    /// Pick from the environment (`XDG_SESSION_TYPE`, then `WAYLAND_DISPLAY`/`DISPLAY`; Windows desktop on Windows).
    #[default]
    Auto,
    /// Force Wayland.
    Wayland,
    /// Force X11.
    X11,
}

/// Explicit window geometry from `-w XxYxWxH`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGeometry {
    /// Left position.
    pub x: i32,
    /// Top position.
    pub y: i32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// Output mode (spec 07 §4.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Wallpaper surfaces behind the desktop.
    Desktop,
    /// A window. `None`: normal resizable 640×480 window; `Some`: undecorated, fixed, always on top.
    Window(Option<WindowGeometry>),
}

/// Which outputs receive a surface.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OutputSelection {
    /// Every output, including hot-plugged ones.
    #[default]
    All,
    /// Only outputs with these names (`-r` and `--screen-span` members). Missing ones are ignored; an error is
    /// returned only when none exists (spec 07 §4.3).
    Named(Vec<String>),
}

impl OutputSelection {
    pub(crate) fn accepts(&self, name: &str) -> bool {
        match self {
            OutputSelection::All => true,
            OutputSelection::Named(names) => names.iter().any(|n| n == name),
        }
    }

    pub(crate) fn requested(&self) -> Vec<String> {
        match self {
            OutputSelection::All => Vec::new(),
            OutputSelection::Named(names) => names.clone(),
        }
    }
}

/// Wayland layer-shell layer (`--layer`, ignored elsewhere).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    /// Below everything.
    Background,
    /// Above the background layer, below windows (spec default).
    #[default]
    Bottom,
    /// Above windows.
    Top,
    /// Above everything.
    Overlay,
}

/// What pointer input to deliver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointerMode {
    /// No pointer at all (`--disable-mouse`): Wayland surfaces get an empty input region (clicks go through).
    Disabled,
    /// Position only. Scenes never react to clicks (spec 07 §10.3), so this is the cheap default.
    #[default]
    Motion,
    /// Position and left/right/middle buttons (web wallpapers). On Windows this registers Raw Input, which wakes
    /// the thread on every mouse report while at least one output runs.
    MotionAndButtons,
}

/// Which outputs a fullscreen window pauses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PauseScope {
    /// A fullscreen window anywhere pauses every output (reference behaviour, spec 07 §9.3).
    #[default]
    AllOutputs,
    /// Only the output(s) the fullscreen window is on.
    PerOutput,
}

/// Fullscreen pause settings (spec 07 §9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FullscreenPolicy {
    /// `false` with `--no-fullscreen-pause`.
    pub enabled: bool,
    /// `--fullscreen-pause-only-active` (Wayland).
    pub only_active: bool,
    /// `--fullscreen-pause-ignore-appid` substrings (Wayland, ASCII case-insensitive; empty ones ignored).
    pub ignore_app_ids: Vec<String>,
    /// Which outputs pause.
    pub scope: PauseScope,
}

impl Default for FullscreenPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            only_active: false,
            ignore_app_ids: Vec::new(),
            scope: PauseScope::AllOutputs,
        }
    }
}

/// Backend configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Desktop surfaces or a window.
    pub mode: Mode,
    /// Backend preference (desktop mode only).
    pub backend: BackendChoice,
    /// Outputs that get a surface (desktop mode only).
    pub outputs: OutputSelection,
    /// Layer-shell layer (Wayland).
    pub layer: Layer,
    /// Frame cap (`--fps`); 0 disables the cap. Frames are additionally aligned to compositor frame callbacks
    /// on Wayland.
    pub max_fps: u32,
    /// Pointer input.
    pub pointer: PointerMode,
    /// Fullscreen pause.
    pub fullscreen: FullscreenPolicy,
    /// Pause outputs that are fully covered by another window (maximised window on Windows, fully obscured
    /// window on X11 without compositor). Rendering what nobody sees is wasted work (docs/PERFORMANCE.md).
    pub pause_when_covered: bool,
    /// Name used as layer-shell namespace, X11 `WM_CLASS`, window title and Windows class suffix (QO-4).
    pub identity: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Desktop,
            backend: BackendChoice::Auto,
            outputs: OutputSelection::All,
            layer: Layer::Bottom,
            max_fps: 30,
            pointer: PointerMode::Motion,
            fullscreen: FullscreenPolicy::default(),
            pause_when_covered: true,
            identity: "lumen".to_owned(),
        }
    }
}

/// Reusable FIFO of events; allocated once, never grows in steady state.
#[derive(Debug)]
pub(crate) struct EventQueue(VecDeque<Event>);

impl EventQueue {
    pub(crate) fn new() -> Self {
        Self(VecDeque::with_capacity(128))
    }

    pub(crate) fn push(&mut self, ev: Event) {
        self.0.push_back(ev);
    }

    pub(crate) fn pop(&mut self) -> Option<Event> {
        self.0.pop_front()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_reasons_ops() {
        let mut r = PauseReasons::NONE;
        assert!(r.is_empty());
        r.set(PauseReasons::FULLSCREEN, true);
        r |= PauseReasons::LOCKED;
        assert!(r.contains(PauseReasons::FULLSCREEN | PauseReasons::LOCKED));
        assert_eq!(format!("{r:?}"), "FULLSCREEN | LOCKED");
        r.set(PauseReasons::FULLSCREEN, false);
        assert_eq!(r, PauseReasons::LOCKED);
        assert!(r.difference(PauseReasons::LOCKED).is_empty());
    }

    #[test]
    fn selection() {
        let s = OutputSelection::Named(vec!["DP-1".into(), "HDMI-A-1".into()]);
        assert!(s.accepts("DP-1"));
        assert!(!s.accepts("DP-2"));
        assert!(OutputSelection::All.accepts("anything"));
    }

    #[test]
    fn event_is_small_and_copy() {
        fn is_copy<T: Copy>() {}
        is_copy::<Event>();
        assert!(std::mem::size_of::<Event>() <= 48);
    }
}

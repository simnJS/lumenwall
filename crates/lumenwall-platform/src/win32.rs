//! Windows backend: one child window per monitor behind the desktop icons (spec 07 §22, research platform.md).
//!
//! * Attach: message `0x052C` to `Progman`, then [`choose_desktop_parent`] on a snapshot of the shell windows:
//!   the `WorkerW` behind the icons (Windows 10, 11 < 24H2), or, on 24H2+, a `WS_EX_LAYERED` child of
//!   `Progman` inserted right below `SHELLDLL_DefView`. Explorer restarts (`TaskbarCreated`) re-attach.
//! * DPI: PerMonitorV2; sizes are physical pixels, scale = effective DPI / 96.
//! * Pause, event driven: `SetWinEventHook` (foreground, minimise, show/hide, cloak, move/size end, plus
//!   location changes of the foreground thread only), `DWMWA_CLOAKED`, `SHQueryUserNotificationState`,
//!   `WTSRegisterSessionNotification` (lock) and `GUID_CONSOLE_DISPLAY_STATE` (screen off). The decision is
//!   [`decide_pause`] (pure, tested).
//! * Pointer: `GetCursorPos` once per rendered frame; clicks through Raw Input (`RIDEV_INPUTSINK`) only in
//!   `PointerMode::MotionAndButtons` and only while an output runs, forwarded when they land on the desktop.
//! * Loop: `MsgWaitForMultipleObjectsEx` on a wake event and a high-resolution waitable timer armed only for
//!   the next due frame. No timer is armed while every output is paused.
//!
//! The executable must carry a manifest declaring Windows 8+ compatibility, otherwise layered child windows
//! (24H2 path) are refused by the system.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::num::NonZeroIsize;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};
use windows::Win32::Foundation::{
    COLORREF, CloseHandle, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0,
    WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, DEVMODEW, ENUM_CURRENT_SETTINGS, EndPaint, EnumDisplayMonitors,
    EnumDisplaySettingsW, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONULL, MONITORINFO,
    MONITORINFOEXW, MonitorFromWindow, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
    UnregisterPowerSettingNotification,
};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateEventW,
    CreateWaitableTimerExW, GetCurrentProcessId, SetEvent, SetWaitableTimer, TIMER_ALL_ACCESS,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
    RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows::Win32::UI::Shell::SHQueryUserNotificationState;
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyWindow,
    DispatchMessageW, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND, EnumWindows,
    FindWindowExW, FindWindowW, GA_ROOT, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetCursorPos,
    GetForegroundWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, HTTRANSPARENT,
    IsIconic, IsWindowVisible, IsZoomed, LWA_ALPHA, MSG, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, OBJID_WINDOW, PBT_POWERSETTINGCHANGE, PM_REMOVE, PeekMessageW,
    QS_ALLINPUT, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP, RI_MOUSE_MIDDLE_BUTTON_DOWN,
    RI_MOUSE_MIDDLE_BUTTON_UP, RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_UP,
    RegisterClassExW, RegisterWindowMessageW, SMTO_NORMAL, SPI_SETWORKAREA, SWP_NOACTIVATE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SendMessageTimeoutW, SetLayeredWindowAttributes, SetWindowPos,
    UnregisterClassW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_DPICHANGED_AFTERPARENT, WM_DPICHANGED_BEFOREPARENT,
    WM_ENDSESSION, WM_ERASEBKGND, WM_INPUT, WM_NCHITTEST, WM_PAINT, WM_POWERBROADCAST, WM_QUIT,
    WM_SETTINGCHANGE, WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOPARENTNOTIFY, WS_EX_TOOLWINDOW,
    WS_POPUP, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::fullscreen::apply_scope;
use crate::geometry::Rect;
use crate::sched::{FrameSlot, min_deadline, pause_transition};
use crate::types::EventQueue;
use crate::win_logic::{
    DesktopParent, MonitorGeom, NotificationState, TopLevel, WindowState, child_rect,
    choose_desktop_parent, decide_pause, is_desktop_click, monitor_name, window_ignorable,
};
use crate::{
    BackendKind, Config, Event, OutputId, OutputInfo, PauseReasons, PlatformError, PointerButton,
    PointerEvent, PointerKind, PointerMode, Wait, Waker, WallpaperSurface,
};

const SURFACE_CLASS: PCWSTR = w!("LumenWallpaperSurface");
const SURFACE_CLASS_STR: &str = "LumenWallpaperSurface";
const HOST_CLASS: PCWSTR = w!("LumenWallpaperHost");
/// Undocumented Progman message that makes Explorer create the WorkerW behind the icons.
const SPAWN_WORKERW: u32 = 0x052C;
/// Second refresh after a display change: Explorer resizes its desktop windows asynchronously.
const DISPLAY_SETTLE: Duration = Duration::from_millis(500);
const REBUILD_RETRY: Duration = Duration::from_secs(1);

const F_PAUSE: u32 = 1;
const F_MONITORS: u32 = 1 << 1;
const F_FOREGROUND: u32 = 1 << 2;
const F_REBUILD: u32 = 1 << 3;
const F_CLOSE: u32 = 1 << 4;

fn hwnd(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

fn raw(h: HWND) -> isize {
    h.0 as isize
}

fn win_err(what: &str, e: windows::core::Error) -> PlatformError {
    PlatformError::Os(format!("{what}: {e}"))
}

fn rect_of(r: &RECT) -> Rect {
    Rect::from_edges(r.left, r.top, r.right, r.bottom)
}

/// Class name decoded into a stack buffer (class names of interest are ASCII); no allocation.
struct ClassName {
    buf: [u8; 128],
    len: usize,
}

impl ClassName {
    fn of(h: HWND) -> Self {
        let mut wide = [0u16; 128];
        // SAFETY: the buffer is valid for its length; an invalid window yields 0 characters.
        let n = usize::try_from(unsafe { GetClassNameW(h, &mut wide) })
            .unwrap_or(0)
            .min(wide.len());
        let mut out = Self {
            buf: [0; 128],
            len: n,
        };
        for (dst, &c) in out.buf.iter_mut().zip(&wide[..n]) {
            *dst = if c < 0x80 { c as u8 } else { b'?' };
        }
        out
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

#[derive(Clone, Copy)]
struct RawClick {
    x: i32,
    y: i32,
    button: PointerButton,
    pressed: bool,
}

/// State touched by window procedures and WinEvent callbacks. Only `Cell`s and short-lived `RefCell`
/// borrows, so re-entrant messages (sent while we call into user32) can never hit a held borrow.
struct Shared {
    flags: Cell<u32>,
    locked: Cell<bool>,
    display_off: Cell<bool>,
    paints: RefCell<Vec<isize>>,
    clicks: RefCell<Vec<RawClick>>,
    buttons_down: Cell<[bool; 3]>,
    destroying: Cell<isize>,
    taskbar_created: u32,
}

impl Shared {
    fn raise(&self, flag: u32) {
        self.flags.set(self.flags.get() | flag);
    }
}

thread_local! {
    static SHARED: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
}

fn with_shared(f: impl FnOnce(&Shared)) {
    SHARED.with(|s| {
        if let Ok(s) = s.try_borrow()
            && let Some(sh) = s.as_ref()
        {
            f(sh);
        }
    });
}

/// Owned kernel handle, shared with wakers living on other threads.
struct OwnedHandle(isize);

impl OwnedHandle {
    fn get(&self) -> HANDLE {
        HANDLE(self.0 as *mut c_void)
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was created by us and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.get());
        }
    }
}

// SAFETY: kernel event handles may be signalled from any thread.
unsafe impl Send for OwnedHandle {}
// SAFETY: as above; SetEvent is thread-safe.
unsafe impl Sync for OwnedHandle {}

struct MonitorDesc {
    handle: isize,
    name: String,
    rect: Rect,
    work: Rect,
    dpi: u32,
    refresh_mhz: Option<u32>,
}

struct WinOutput {
    id: OutputId,
    hwnd: isize,
    hmonitor: isize,
    rect: Rect,
    work: Rect,
    applied: Rect,
    slot: FrameSlot,
    paused: PauseReasons,
    last_pointer: Option<(i32, i32)>,
    info: OutputInfo,
}

/// The Win32 backend.
pub(crate) struct Win32Backend {
    shared: Rc<Shared>,
    hinstance: HINSTANCE,
    host: HWND,
    wake: Arc<OwnedHandle>,
    timer: OwnedHandle,
    timer_armed: Option<Instant>,
    hooks: Vec<HWINEVENTHOOK>,
    fg_hook: Option<HWINEVENTHOOK>,
    power: Option<HPOWERNOTIFY>,
    wts: bool,
    parent: Option<DesktopParent>,
    outputs: Vec<WinOutput>,
    infos: Vec<OutputInfo>,
    graveyard: Vec<isize>,
    queue: EventQueue,
    config: Config,
    max_fps: u32,
    next_id: u32,
    title: Vec<u16>,
    raw_input_on: bool,
    refresh_again: Option<Instant>,
    rebuild_retries: u32,
    hwnds: Vec<isize>,
    windows: Vec<WindowState>,
    monitors: Vec<MonitorGeom>,
    reasons: Vec<PauseReasons>,
    paints: Vec<isize>,
    clicks: Vec<RawClick>,
}

impl Win32Backend {
    pub(crate) fn new(config: &Config) -> Result<Self, PlatformError> {
        // SAFETY: plain Win32 calls with valid arguments; every handle created here is owned by the backend,
        // which is built as early as possible so that `Drop` releases everything on any later error.
        unsafe {
            // Fails harmlessly when the manifest or an earlier call already chose an awareness.
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let hinstance: HINSTANCE = GetModuleHandleW(PCWSTR::null())
                .map_err(|e| win_err("GetModuleHandleW", e))?
                .into();
            let wake = CreateEventW(None, false, false, PCWSTR::null())
                .map_err(|e| win_err("CreateEventW", e))?;
            let wake = Arc::new(OwnedHandle(wake.0 as isize));
            let timer = CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
            .or_else(|_| CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0))
            .map_err(|e| win_err("CreateWaitableTimerExW", e))?;
            let timer = OwnedHandle(timer.0 as isize);

            for (class, proc_) in [
                (SURFACE_CLASS, surface_proc as WndProc),
                (HOST_CLASS, host_proc as WndProc),
            ] {
                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(proc_),
                    hInstance: hinstance,
                    lpszClassName: class,
                    ..Default::default()
                };
                // 0 when already registered by a previous backend: fine.
                RegisterClassExW(&wc);
            }
            let mut title: Vec<u16> = config.identity.encode_utf16().collect();
            title.push(0);
            let host = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                HOST_CLASS,
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
            .map_err(|e| win_err("CreateWindowExW (host)", e))?;

            let shared = Rc::new(Shared {
                flags: Cell::new(0),
                locked: Cell::new(false),
                display_off: Cell::new(false),
                paints: RefCell::new(Vec::with_capacity(16)),
                clicks: RefCell::new(Vec::with_capacity(16)),
                buttons_down: Cell::new([false; 3]),
                destroying: Cell::new(0),
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            });
            SHARED.with(|s| *s.borrow_mut() = Some(shared.clone()));

            let mut backend = Self {
                shared,
                hinstance,
                host,
                wake,
                timer,
                timer_armed: None,
                hooks: Vec::new(),
                fg_hook: None,
                power: None,
                wts: false,
                parent: None,
                outputs: Vec::new(),
                infos: Vec::new(),
                graveyard: Vec::new(),
                queue: EventQueue::new(),
                config: config.clone(),
                max_fps: config.max_fps,
                next_id: 0,
                title,
                raw_input_on: false,
                refresh_again: None,
                rebuild_retries: 0,
                hwnds: Vec::with_capacity(512),
                windows: Vec::with_capacity(64),
                monitors: Vec::with_capacity(8),
                reasons: Vec::with_capacity(8),
                paints: Vec::with_capacity(16),
                clicks: Vec::with_capacity(16),
            };

            // Windows answers immediately with the current display state.
            backend.power = RegisterPowerSettingNotification(
                HANDLE(host.0),
                &GUID_CONSOLE_DISPLAY_STATE,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            )
            .ok();
            backend.wts = WTSRegisterSessionNotification(host, NOTIFY_FOR_THIS_SESSION).is_ok();
            for (min, max) in [
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
                (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
                (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
                (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
                (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
            ] {
                let hook = SetWinEventHook(
                    min,
                    max,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );
                if !hook.is_invalid() {
                    backend.hooks.push(hook);
                }
            }

            backend.parent = Some(find_desktop_parent().ok_or_else(|| {
                PlatformError::Desktop(
                    "Progman / SHELLDLL_DefView not found (is Explorer the shell?)".to_owned(),
                )
            })?);
            tracing::info!(parent = ?backend.parent, "attached to the desktop");
            backend.refresh_outputs(true)?;
            backend.update_foreground_hook();
            backend.evaluate_pause();
            Ok(backend)
        }
    }

    fn parent_hwnd(&self) -> Option<HWND> {
        self.parent.map(|p| match p {
            DesktopParent::WorkerW { worker } => hwnd(worker),
            DesktopParent::Progman { progman, .. } => hwnd(progman),
        })
    }

    /// Screen position of the parent's client origin (virtual-screen pixels).
    fn parent_origin(&self) -> (i32, i32) {
        let Some(parent) = self.parent_hwnd() else {
            return (0, 0);
        };
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: valid out pointer; a dead parent leaves (0,0).
        unsafe {
            let _ = ClientToScreen(parent, &mut pt);
        }
        (pt.x, pt.y)
    }

    fn create_surface_window(&self, r: Rect) -> Result<isize, PlatformError> {
        let parent = self
            .parent
            .ok_or_else(|| PlatformError::Desktop("not attached".to_owned()))?;
        let (parent_hwnd, ex_style, insert_after) = match parent {
            DesktopParent::WorkerW { worker } => {
                (hwnd(worker), WS_EX_NOACTIVATE | WS_EX_NOPARENTNOTIFY, None)
            }
            DesktopParent::Progman {
                progman, defview, ..
            } => (
                hwnd(progman),
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_NOPARENTNOTIFY,
                Some(hwnd(defview)),
            ),
        };
        // SAFETY: valid class, parent and instance; the window is owned by this thread.
        unsafe {
            let h = CreateWindowExW(
                ex_style,
                SURFACE_CLASS,
                PCWSTR(self.title.as_ptr()),
                WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                r.x,
                r.y,
                r.width.max(1),
                r.height.max(1),
                Some(parent_hwnd),
                None,
                Some(self.hinstance),
                None,
            )
            .map_err(|e| win_err("CreateWindowExW (wallpaper)", e))?;
            if ex_style.contains(WS_EX_LAYERED) {
                // A layered window is invisible until its attributes are set; fully opaque.
                SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA)
                    .map_err(|e| win_err("SetLayeredWindowAttributes", e))?;
            }
            let flags = SWP_NOACTIVATE
                | SWP_SHOWWINDOW
                | if insert_after.is_none() {
                    SWP_NOZORDER
                } else {
                    Default::default()
                };
            // 24H2: inserted right after (= below) the icon view, above the shell's own wallpaper worker.
            SetWindowPos(
                h,
                insert_after,
                r.x,
                r.y,
                r.width.max(1),
                r.height.max(1),
                flags,
            )
            .map_err(|e| win_err("SetWindowPos", e))?;
            Ok(raw(h))
        }
    }

    fn refresh_outputs(&mut self, initial: bool) -> Result<(), PlatformError> {
        let monitors = enumerate_monitors();
        let origin = self.parent_origin();
        let accepted = |m: &MonitorDesc| self.config.outputs.accepts(&m.name);

        let mut i = 0;
        while i < self.outputs.len() {
            if monitors
                .iter()
                .any(|m| m.name == self.outputs[i].info.name && accepted(m))
            {
                i += 1;
                continue;
            }
            let o = self.outputs.remove(i);
            tracing::info!(output = %o.info.name, "monitor removed");
            self.graveyard.push(o.hwnd);
            self.queue.push(Event::OutputRemoved(o.id));
        }

        for m in monitors.iter().filter(|m| accepted(m)) {
            let scale = f64::from(m.dpi.max(1)) / 96.0;
            let physical = (m.rect.width.max(1) as u32, m.rect.height.max(1) as u32);
            let logical = (
                (f64::from(physical.0) / scale).round() as u32,
                (f64::from(physical.1) / scale).round() as u32,
            );
            let target = child_rect(m.rect, origin);
            if let Some(o) = self.outputs.iter_mut().find(|o| o.info.name == m.name) {
                o.hmonitor = m.handle;
                o.work = m.work;
                o.info.refresh_mhz = m.refresh_mhz;
                if o.applied != target {
                    // SAFETY: our own child window.
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd(o.hwnd),
                            None,
                            target.x,
                            target.y,
                            target.width,
                            target.height,
                            SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                    }
                    o.applied = target;
                }
                if o.info.physical_size != physical {
                    o.info.physical_size = physical;
                    o.info.logical_size = logical;
                    self.queue.push(Event::Resized {
                        output: o.id,
                        physical_size: physical,
                        logical_size: logical,
                    });
                }
                if (o.info.scale - scale).abs() > f64::EPSILON {
                    o.info.scale = scale;
                    o.info.logical_size = logical;
                    self.queue.push(Event::ScaleChanged {
                        output: o.id,
                        scale,
                    });
                }
                if o.rect.x != m.rect.x || o.rect.y != m.rect.y {
                    o.info.position = (m.rect.x, m.rect.y);
                    self.queue.push(Event::Moved {
                        output: o.id,
                        position: (m.rect.x, m.rect.y),
                    });
                }
                o.rect = m.rect;
                continue;
            }
            let window = self.create_surface_window(target)?;
            let id = OutputId(self.next_id);
            self.next_id += 1;
            let mut slot = FrameSlot::new(self.max_fps);
            slot.requested = true;
            tracing::info!(output = %m.name, x = m.rect.x, y = m.rect.y, w = m.rect.width, h = m.rect.height, "monitor attached");
            self.outputs.push(WinOutput {
                id,
                hwnd: window,
                hmonitor: m.handle,
                rect: m.rect,
                work: m.work,
                applied: target,
                slot,
                paused: PauseReasons::NONE,
                last_pointer: None,
                info: OutputInfo {
                    id,
                    name: m.name.clone(),
                    description: None,
                    position: (m.rect.x, m.rect.y),
                    logical_size: logical,
                    physical_size: physical,
                    scale,
                    refresh_mhz: m.refresh_mhz,
                },
            });
            self.queue.push(Event::OutputAdded(id));
        }

        if initial && self.outputs.is_empty() {
            return Err(PlatformError::NoMatchingOutput {
                requested: self.config.outputs.requested(),
                detected: monitors.into_iter().map(|m| m.name).collect(),
            });
        }
        self.infos.clear();
        self.infos
            .extend(self.outputs.iter().map(|o| o.info.clone()));
        crate::sort_outputs(&mut self.infos);
        self.shared.raise(F_PAUSE);
        Ok(())
    }

    /// Explorer restarted (our windows died with their parent) or the attach must be redone.
    fn rebuild(&mut self) {
        for o in self.outputs.drain(..) {
            self.graveyard.push(o.hwnd);
            self.queue.push(Event::OutputRemoved(o.id));
        }
        self.infos.clear();
        self.parent = find_desktop_parent();
        match self.parent {
            Some(_) => {
                self.rebuild_retries = 0;
                tracing::info!(parent = ?self.parent, "re-attached to the desktop");
                if let Err(e) = self.refresh_outputs(false) {
                    tracing::error!("re-attach failed: {e}");
                }
            }
            None if self.rebuild_retries < 10 => {
                // Explorer is not ready yet; `TaskbarCreated` or this one-shot retry will try again.
                self.rebuild_retries += 1;
                self.refresh_again = Some(Instant::now() + REBUILD_RETRY);
            }
            None => tracing::error!("cannot re-attach to the desktop"),
        }
    }

    /// Location changes are only watched for the foreground window's thread (not system-wide).
    fn update_foreground_hook(&mut self) {
        // SAFETY: hook handles are ours; the callback is a plain function.
        unsafe {
            if let Some(h) = self.fg_hook.take() {
                let _ = UnhookWinEvent(h);
            }
            let fg = GetForegroundWindow();
            if fg.is_invalid() || window_ignorable(ClassName::of(fg).as_str(), 0) {
                return;
            }
            let mut pid = 0u32;
            let tid = GetWindowThreadProcessId(fg, Some(&mut pid));
            if tid == 0 || pid == GetCurrentProcessId() {
                return;
            }
            let hook = SetWinEventHook(
                EVENT_OBJECT_LOCATIONCHANGE,
                EVENT_OBJECT_LOCATIONCHANGE,
                None,
                Some(win_event_proc),
                pid,
                tid,
                WINEVENT_OUTOFCONTEXT,
            );
            if !hook.is_invalid() {
                self.fg_hook = Some(hook);
            }
        }
    }

    fn evaluate_pause(&mut self) {
        self.hwnds.clear();
        // SAFETY: the callback only pushes into the Vec passed through LPARAM, which outlives the call.
        unsafe {
            let _ = EnumWindows(
                Some(collect_hwnd),
                LPARAM(&mut self.hwnds as *mut Vec<isize> as isize),
            );
        }
        self.windows.clear();
        for &h in &self.hwnds {
            if let Some(w) = window_state(hwnd(h)) {
                self.windows.push(w);
            }
        }
        self.monitors.clear();
        self.monitors
            .extend(self.outputs.iter().map(|o| MonitorGeom {
                rect: o.rect,
                work: o.work,
            }));
        // SAFETY: simple queries without pointers.
        let (notification, fg_monitor) = unsafe {
            let n = SHQueryUserNotificationState().map_or(NotificationState::Normal, |s| {
                NotificationState::from_raw(s.0)
            });
            let fg = GetForegroundWindow();
            let mon = if fg.is_invalid() {
                None
            } else {
                Some(MonitorFromWindow(fg, MONITOR_DEFAULTTONULL).0 as isize)
            };
            (
                n,
                mon.and_then(|m| self.outputs.iter().position(|o| o.hmonitor == m)),
            )
        };
        self.reasons.clear();
        self.reasons.resize(self.outputs.len(), PauseReasons::NONE);
        decide_pause(
            &self.monitors,
            &self.windows,
            notification,
            fg_monitor,
            self.config.fullscreen.enabled,
            self.config.pause_when_covered,
            &mut self.reasons,
        );
        let (locked, off) = (self.shared.locked.get(), self.shared.display_off.get());
        for r in &mut self.reasons {
            if locked {
                *r |= PauseReasons::LOCKED;
            }
            if off {
                *r |= PauseReasons::DISPLAY_OFF;
            }
        }
        apply_scope(&mut self.reasons, self.config.fullscreen.scope);
        for (o, new) in self.outputs.iter_mut().zip(self.reasons.iter()) {
            if let Some(ev) = pause_transition(o.id, o.paused, *new) {
                self.queue.push(ev);
            }
            o.paused = *new;
        }
        self.update_raw_input();
    }

    /// Raw Input only while clicks are wanted and something runs: it wakes the thread on every mouse report.
    fn update_raw_input(&mut self) {
        let wanted = self.config.pointer == PointerMode::MotionAndButtons
            && self.outputs.iter().any(|o| o.paused.is_empty());
        if wanted == self.raw_input_on {
            return;
        }
        let device = RAWINPUTDEVICE {
            usUsagePage: 0x01, // generic desktop
            usUsage: 0x02,     // mouse
            dwFlags: if wanted {
                RIDEV_INPUTSINK
            } else {
                RIDEV_REMOVE
            },
            hwndTarget: if wanted { self.host } else { HWND::default() },
        };
        // SAFETY: one valid descriptor of the declared size.
        match unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) } {
            Ok(()) => self.raw_input_on = wanted,
            Err(e) => tracing::warn!("RegisterRawInputDevices: {e}"),
        }
        if !wanted {
            self.shared.buttons_down.set([false; 3]);
        }
    }

    fn pump(&mut self) {
        let mut msg = MSG::default();
        // SAFETY: standard message pump on the thread that owns our windows.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    self.shared.raise(F_CLOSE);
                    continue;
                }
                DispatchMessageW(&msg);
            }
        }
    }

    fn process_flags(&mut self, now: Instant) {
        let flags = self.shared.flags.replace(0);
        if flags & F_CLOSE != 0 {
            self.queue.push(Event::CloseRequested);
        }
        if flags & F_REBUILD != 0 {
            self.rebuild();
        }
        let settle = self.refresh_again.is_some_and(|t| now >= t);
        if settle {
            self.refresh_again = None;
            if self.parent.is_none() {
                self.rebuild();
            }
        }
        if (flags & F_MONITORS != 0 || settle) && self.parent.is_some() {
            if let Err(e) = self.refresh_outputs(false) {
                tracing::error!("monitor refresh failed: {e}");
            }
            if flags & F_MONITORS != 0 {
                self.refresh_again = Some(now + DISPLAY_SETTLE);
            }
        }
        if flags & F_FOREGROUND != 0 {
            self.update_foreground_hook();
        }
        // Work above may have raised F_PAUSE again; take only that bit, keep the others for the next round.
        let late = self.shared.flags.get();
        self.shared.flags.set(late & !F_PAUSE);
        if (flags | late) & F_PAUSE != 0 {
            self.evaluate_pause();
        }

        if let Ok(mut p) = self.shared.paints.try_borrow_mut() {
            std::mem::swap(&mut *p, &mut self.paints);
        }
        for h in self.paints.drain(..) {
            if let Some(o) = self.outputs.iter_mut().find(|o| o.hwnd == h) {
                o.slot.requested = true;
            }
        }
        if let Ok(mut c) = self.shared.clicks.try_borrow_mut() {
            std::mem::swap(&mut *c, &mut self.clicks);
        }
        for c in self.clicks.drain(..) {
            if let Some(o) = self
                .outputs
                .iter()
                .find(|o| o.rect.contains_point(c.x, c.y))
            {
                self.queue.push(Event::Pointer(PointerEvent {
                    output: o.id,
                    x: f64::from(c.x - o.rect.x),
                    y: f64::from(c.y - o.rect.y),
                    kind: PointerKind::Button {
                        button: c.button,
                        pressed: c.pressed,
                    },
                }));
            }
        }
    }

    fn tick(&mut self, now: Instant) {
        let pointer = self.config.pointer != PointerMode::Disabled;
        let mut cursor: Option<POINT> = None;
        for o in &mut self.outputs {
            if !o.slot.fire_if_due(now, !o.paused.is_empty(), true) {
                continue;
            }
            if pointer {
                let c = *cursor.get_or_insert_with(|| {
                    let mut p = POINT::default();
                    // SAFETY: valid out pointer.
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                    }
                    p
                });
                // parity: cursor relative to the output, tracked over every window like the X11 reference.
                let local = (c.x - o.rect.x, c.y - o.rect.y);
                if o.last_pointer != Some(local) {
                    o.last_pointer = Some(local);
                    self.queue.push(Event::Pointer(PointerEvent {
                        output: o.id,
                        x: f64::from(local.0),
                        y: f64::from(local.1),
                        kind: PointerKind::Motion,
                    }));
                }
            }
            self.queue.push(Event::Frame(o.id));
        }
    }

    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.outputs
            .iter()
            .filter_map(|o| o.slot.due(now, !o.paused.is_empty(), true))
            .min()
    }

    fn arm_timer(&mut self, deadline: Option<Instant>, now: Instant) {
        if deadline == self.timer_armed {
            return;
        }
        // SAFETY: our own timer handle; the due time pointer is valid for the call.
        unsafe {
            match deadline {
                Some(d) => {
                    // Negative = relative, in 100 ns units; at least 1 so it never means "absolute 0".
                    let due = -((d.saturating_duration_since(now).as_nanos() / 100).max(1) as i64);
                    if let Err(e) = SetWaitableTimer(self.timer.get(), &due, 0, None, None, false) {
                        tracing::warn!("SetWaitableTimer: {e}");
                    }
                }
                None => {
                    let _ = CancelWaitableTimer(self.timer.get());
                }
            }
        }
        self.timer_armed = deadline;
    }

    fn bury(&mut self) {
        for h in self.graveyard.drain(..) {
            self.shared.destroying.set(h);
            // SAFETY: our own window (may already be gone after an Explorer crash: the error is ignored).
            unsafe {
                let _ = DestroyWindow(hwnd(h));
            }
        }
        self.shared.destroying.set(0);
    }
}

impl Drop for Win32Backend {
    fn drop(&mut self) {
        self.graveyard.extend(self.outputs.iter().map(|o| o.hwnd));
        self.bury();
        if self.raw_input_on {
            self.config.pointer = PointerMode::Disabled;
            self.update_raw_input();
        }
        // SAFETY: releasing what `new` registered, on the same thread.
        unsafe {
            for h in self.hooks.drain(..).chain(self.fg_hook.take()) {
                let _ = UnhookWinEvent(h);
            }
            if let Some(p) = self.power.take() {
                let _ = UnregisterPowerSettingNotification(p);
            }
            if self.wts {
                let _ = WTSUnRegisterSessionNotification(self.host);
            }
            let _ = DestroyWindow(self.host);
            let _ = UnregisterClassW(SURFACE_CLASS, Some(self.hinstance));
            let _ = UnregisterClassW(HOST_CLASS, Some(self.hinstance));
        }
        SHARED.with(|s| *s.borrow_mut() = None);
    }
}

impl WallpaperSurface for Win32Backend {
    fn backend(&self) -> BackendKind {
        BackendKind::Windows
    }

    fn outputs(&self) -> &[OutputInfo] {
        &self.infos
    }

    fn raw_display_handle(&self) -> RawDisplayHandle {
        RawDisplayHandle::Windows(WindowsDisplayHandle::new())
    }

    fn raw_window_handle(&self, id: OutputId) -> Option<RawWindowHandle> {
        let o = self.outputs.iter().find(|o| o.id == id)?;
        let mut handle = Win32WindowHandle::new(NonZeroIsize::new(o.hwnd)?);
        handle.hinstance = NonZeroIsize::new(self.hinstance.0 as isize);
        Some(RawWindowHandle::Win32(handle))
    }

    fn request_frame(&mut self, id: OutputId) {
        if let Some(o) = self.outputs.iter_mut().find(|o| o.id == id) {
            o.slot.requested = true;
        }
    }

    fn pre_present(&mut self, _id: OutputId) {}

    fn set_max_fps(&mut self, fps: u32) {
        self.max_fps = fps;
        for o in &mut self.outputs {
            o.slot.clock.set_max_fps(fps);
        }
    }

    fn pause_reasons(&self, id: OutputId) -> PauseReasons {
        self.outputs
            .iter()
            .find(|o| o.id == id)
            .map_or(PauseReasons::NONE, |o| o.paused)
    }

    fn waker(&self) -> Waker {
        let wake = self.wake.clone();
        Waker::new(move || {
            // SAFETY: the event handle stays open while any waker holds the Arc.
            unsafe {
                let _ = SetEvent(wake.get());
            }
        })
    }

    fn wait(&mut self, wait: Wait) -> Result<(), PlatformError> {
        self.bury();
        let limit = wait.deadline();
        let mut waited = false;
        loop {
            self.pump();
            let now = Instant::now();
            self.process_flags(now);
            self.tick(now);
            if !self.queue.is_empty() {
                return Ok(());
            }
            if waited && limit.is_some_and(|l| now >= l) {
                return Ok(());
            }
            let deadline = min_deadline(self.next_deadline(now), self.refresh_again);
            self.arm_timer(deadline, now);
            let ms = limit.map_or(u32::MAX, |l| {
                let us = l.saturating_duration_since(now).as_micros();
                u32::try_from(us.div_ceil(1000))
                    .unwrap_or(u32::MAX - 1)
                    .min(u32::MAX - 1)
            });
            let handles = [self.wake.get(), self.timer.get()];
            // SAFETY: both handles are valid for the duration of the call.
            let r = unsafe {
                MsgWaitForMultipleObjectsEx(Some(&handles), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
            };
            if r == WAIT_OBJECT_0 {
                self.queue.push(Event::Wake);
            } else if r.0 == WAIT_OBJECT_0.0 + 1 {
                self.timer_armed = None;
            }
            waited = true;
        }
    }

    fn next_event(&mut self) -> Option<Event> {
        self.queue.pop()
    }
}

// ---------------------------------------------------------------------------------------------------------
// OS snapshots

fn window_state(h: HWND) -> Option<WindowState> {
    // SAFETY: read-only queries on a window handle; failures are tolerated.
    unsafe {
        if !IsWindowVisible(h).as_bool() {
            return None;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        );
        let mut r = RECT::default();
        if DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut r as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
        .is_err()
        {
            GetWindowRect(h, &mut r).ok()?;
        }
        let class = ClassName::of(h);
        let ex_style = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        Some(WindowState {
            rect: rect_of(&r),
            visible: true,
            minimized: IsIconic(h).as_bool(),
            maximized: IsZoomed(h).as_bool(),
            cloaked: cloaked != 0,
            ignorable: window_ignorable(class.as_str(), ex_style),
        })
    }
}

fn enumerate_monitors() -> Vec<MonitorDesc> {
    let mut handles: Vec<isize> = Vec::with_capacity(8);
    // SAFETY: the callback pushes into the Vec passed through LPARAM, alive for the call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut handles as *mut Vec<isize> as isize),
        );
    }
    handles
        .into_iter()
        .filter_map(|m| {
            let hmon = HMONITOR(m as *mut c_void);
            let mut mi = MONITORINFOEXW::default();
            mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            // SAFETY: MONITORINFOEXW starts with MONITORINFO and cbSize announces the extended size.
            unsafe {
                if !GetMonitorInfoW(hmon, (&mut mi as *mut MONITORINFOEXW).cast::<MONITORINFO>())
                    .as_bool()
                {
                    return None;
                }
            }
            let len = mi
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(mi.szDevice.len());
            let device = String::from_utf16_lossy(&mi.szDevice[..len]);
            let (mut dx, mut dy) = (96u32, 96u32);
            // SAFETY: valid monitor handle and out pointers.
            unsafe {
                if GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_err() {
                    dx = 96;
                }
            }
            let mut dm = DEVMODEW {
                dmSize: size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            // SAFETY: NUL-terminated device name from szDevice; dmSize set.
            let refresh = unsafe {
                EnumDisplaySettingsW(PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm)
                    .as_bool()
                    .then_some(dm.dmDisplayFrequency)
                    .filter(|f| *f > 1)
                    .map(|f| f * 1000)
            };
            Some(MonitorDesc {
                handle: m,
                name: monitor_name(&device).to_owned(),
                rect: rect_of(&mi.monitorInfo.rcMonitor),
                work: rect_of(&mi.monitorInfo.rcWork),
                dpi: dx,
                refresh_mhz: refresh,
            })
        })
        .collect()
}

/// Sends `0x052C` to Progman and picks the parent from a snapshot of the shell windows (see `win_logic`).
fn find_desktop_parent() -> Option<DesktopParent> {
    // SAFETY: window enumeration and a bounded SendMessageTimeout to the shell.
    unsafe {
        let progman = FindWindowW(w!("Progman"), PCWSTR::null()).ok();
        let mut choice = None;
        // (0, 0) is the classic form; (0xD, 1) is also observed on recent builds. Both are harmless.
        for (wparam, lparam) in [(0usize, 0isize), (0xD, 1)] {
            if let Some(p) = progman {
                let mut result = 0usize;
                SendMessageTimeoutW(
                    p,
                    SPAWN_WORKERW,
                    WPARAM(wparam),
                    LPARAM(lparam),
                    SMTO_NORMAL,
                    1000,
                    Some(&mut result),
                );
            }
            let children = progman.map(direct_children).unwrap_or_default();
            let child_refs: Vec<(isize, &str)> =
                children.iter().map(|(h, c)| (*h, c.as_str())).collect();
            let mut tops: Vec<isize> = Vec::with_capacity(512);
            let _ = EnumWindows(
                Some(collect_hwnd),
                LPARAM(&mut tops as *mut Vec<isize> as isize),
            );
            let infos: Vec<(isize, String, bool)> = tops
                .iter()
                .map(|&t| {
                    let has_defview =
                        FindWindowExW(Some(hwnd(t)), None, w!("SHELLDLL_DefView"), PCWSTR::null())
                            .is_ok();
                    (t, ClassName::of(hwnd(t)).as_str().to_owned(), has_defview)
                })
                .collect();
            let top_refs: Vec<TopLevel<'_>> = infos
                .iter()
                .map(|(h, c, d)| TopLevel {
                    hwnd: *h,
                    class: c.as_str(),
                    has_defview: *d,
                })
                .collect();
            choice = choose_desktop_parent(progman.map(raw), &child_refs, &top_refs);
            let complete = matches!(
                choice,
                Some(DesktopParent::WorkerW { .. })
                    | Some(DesktopParent::Progman {
                        worker: Some(_),
                        ..
                    })
            );
            if complete {
                break;
            }
        }
        choice
    }
}

fn direct_children(parent: HWND) -> Vec<(isize, String)> {
    let mut out = Vec::new();
    let mut after: Option<HWND> = None;
    // SAFETY: enumeration of the direct children of a window; bounded.
    unsafe {
        while out.len() < 256 {
            match FindWindowExW(Some(parent), after, PCWSTR::null(), PCWSTR::null()) {
                Ok(c) if !c.is_invalid() => {
                    out.push((raw(c), ClassName::of(c).as_str().to_owned()));
                    after = Some(c);
                }
                _ => break,
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Callbacks

type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

unsafe extern "system" fn collect_hwnd(h: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: LPARAM carries a `*mut Vec<isize>` that outlives the enumeration.
    let v = unsafe { &mut *(lparam.0 as *mut Vec<isize>) };
    v.push(raw(h));
    BOOL(1)
}

unsafe extern "system" fn collect_monitor(
    m: HMONITOR,
    _: HDC,
    _: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: as above.
    let v = unsafe { &mut *(lparam.0 as *mut Vec<isize>) };
    v.push(m.0 as isize);
    BOOL(1)
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    h: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole windows matter (not carets, cursors, menu items...).
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || h.is_invalid() {
        return;
    }
    if event != EVENT_SYSTEM_FOREGROUND {
        // SAFETY: plain query on the window the event is about.
        let top_level = unsafe { GetAncestor(h, GA_ROOT) } == h;
        if !top_level {
            return;
        }
    }
    with_shared(|s| {
        if event == EVENT_SYSTEM_FOREGROUND {
            s.raise(F_FOREGROUND);
        }
        s.raise(F_PAUSE);
    });
}

unsafe extern "system" fn surface_proc(
    h: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: validating the update region of our own window (prevents an endless WM_PAINT stream).
            unsafe {
                BeginPaint(h, &mut ps);
                let _ = EndPaint(h, &ps);
            }
            with_shared(|s| {
                if let Ok(mut p) = s.paints.try_borrow_mut() {
                    p.push(raw(h));
                }
            });
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_DPICHANGED_BEFOREPARENT | WM_DPICHANGED_AFTERPARENT => {
            with_shared(|s| s.raise(F_MONITORS));
            LRESULT(0)
        }
        WM_DESTROY => {
            // Not destroyed by us: the parent (Explorer) went away.
            with_shared(|s| {
                if s.destroying.get() != raw(h) {
                    s.raise(F_REBUILD);
                }
            });
            LRESULT(0)
        }
        // SAFETY: default processing.
        _ => unsafe { DefWindowProcW(h, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn host_proc(h: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mut handled = None;
    with_shared(|s| {
        handled = match msg {
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                s.raise(F_MONITORS | F_PAUSE);
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE if wparam.0 == SPI_SETWORKAREA.0 as usize => {
                s.raise(F_MONITORS | F_PAUSE);
                None
            }
            WM_POWERBROADCAST if wparam.0 == PBT_POWERSETTINGCHANGE as usize && lparam.0 != 0 => {
                // SAFETY: for PBT_POWERSETTINGCHANGE, LPARAM points to a POWERBROADCAST_SETTING.
                let setting = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
                if setting.PowerSetting == GUID_CONSOLE_DISPLAY_STATE && setting.DataLength >= 1 {
                    // 0 = off, 1 = on, 2 = dimmed.
                    s.display_off.set(setting.Data[0] == 0);
                    s.raise(F_PAUSE);
                }
                Some(LRESULT(1))
            }
            WM_WTSSESSION_CHANGE => {
                match wparam.0 as u32 {
                    WTS_SESSION_LOCK => s.locked.set(true),
                    WTS_SESSION_UNLOCK => s.locked.set(false),
                    _ => {}
                }
                s.raise(F_PAUSE);
                Some(LRESULT(0))
            }
            WM_INPUT => {
                // SAFETY: LPARAM is the HRAWINPUT of this message.
                unsafe { on_raw_input(s, lparam) };
                None // DefWindowProc must still run for cleanup.
            }
            WM_CLOSE => {
                s.raise(F_CLOSE);
                Some(LRESULT(0))
            }
            WM_ENDSESSION if wparam.0 != 0 => {
                s.raise(F_CLOSE);
                Some(LRESULT(0))
            }
            m if m == s.taskbar_created => {
                s.raise(F_REBUILD);
                Some(LRESULT(0))
            }
            _ => None,
        };
    });
    // SAFETY: default processing for everything we did not fully handle.
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(h, msg, wparam, lparam) })
}

const BUTTONS: [(u32, u32, PointerButton); 3] = [
    (
        RI_MOUSE_LEFT_BUTTON_DOWN,
        RI_MOUSE_LEFT_BUTTON_UP,
        PointerButton::Left,
    ),
    (
        RI_MOUSE_RIGHT_BUTTON_DOWN,
        RI_MOUSE_RIGHT_BUTTON_UP,
        PointerButton::Right,
    ),
    (
        RI_MOUSE_MIDDLE_BUTTON_DOWN,
        RI_MOUSE_MIDDLE_BUTTON_UP,
        PointerButton::Middle,
    ),
];

/// Reads one Raw Input report into a stack buffer; presses count only on the desktop, releases always
/// match an earlier press (no stuck buttons).
unsafe fn on_raw_input(s: &Shared, lparam: LPARAM) {
    let mut data = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    // SAFETY: the buffer is a RAWINPUT of `size` bytes; mouse reports fit in it.
    let n = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut c_void),
            RID_INPUT,
            Some((&mut data as *mut RAWINPUT).cast()),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if n == u32::MAX || n == 0 || data.header.dwType != RIM_TYPEMOUSE.0 {
        return;
    }
    // SAFETY: dwType says the union holds a mouse report.
    let flags = u32::from(unsafe { data.data.mouse.Anonymous.Anonymous.usButtonFlags });
    if flags & BUTTONS.iter().fold(0, |acc, (d, u, _)| acc | d | u) == 0 {
        return; // movement only
    }
    let mut pt = POINT::default();
    // SAFETY: valid out pointer and plain window queries.
    let on_desktop = unsafe {
        if GetCursorPos(&mut pt).is_err() {
            return;
        }
        let hit = windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(pt);
        !hit.is_invalid() && {
            let root = GetAncestor(hit, GA_ROOT);
            is_desktop_click(
                ClassName::of(hit).as_str(),
                ClassName::of(root).as_str(),
                SURFACE_CLASS_STR,
            )
        }
    };
    let mut down = s.buttons_down.get();
    let Ok(mut clicks) = s.clicks.try_borrow_mut() else {
        return;
    };
    for (i, (dn, up, button)) in BUTTONS.iter().enumerate() {
        if flags & dn != 0 && on_desktop {
            down[i] = true;
            clicks.push(RawClick {
                x: pt.x,
                y: pt.y,
                button: *button,
                pressed: true,
            });
        }
        if flags & up != 0 && down[i] {
            down[i] = false;
            clicks.push(RawClick {
                x: pt.x,
                y: pt.y,
                button: *button,
                pressed: false,
            });
        }
    }
    s.buttons_down.set(down);
}

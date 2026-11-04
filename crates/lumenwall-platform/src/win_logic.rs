//! Windows desktop rules as pure functions (compiled and tested on every OS).
//!
//! The Win32 backend snapshots the window tree and calls these; the decisions themselves never touch the OS.
//! Window handles are carried as `isize` so the logic does not depend on the `windows` crate.

use crate::geometry::Rect;
use crate::types::PauseReasons;

/// Class of the shell view that hosts the desktop icons.
pub const DEFVIEW_CLASS: &str = "SHELLDLL_DefView";
/// Class of the shell's desktop window.
pub const PROGMAN_CLASS: &str = "Progman";
/// Class of the shell worker windows; one of them sits behind the icons once `0x052C` was sent to Progman.
pub const WORKERW_CLASS: &str = "WorkerW";

/// One top-level window, as returned by `EnumWindows` (Z order, topmost first).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopLevel<'a> {
    /// Handle.
    pub hwnd: isize,
    /// Window class name.
    pub class: &'a str,
    /// Whether it has a `SHELLDLL_DefView` child.
    pub has_defview: bool,
}

/// Where the wallpaper windows must be parented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopParent {
    /// Windows 10 and Windows 11 before 24H2: the top-level `WorkerW` that follows the window hosting the
    /// icons in Z order (public "Progman 0x052C" technique). Children are plain `WS_CHILD` windows.
    WorkerW {
        /// Parent window.
        worker: isize,
    },
    /// Windows 11 24H2 and later: `SHELLDLL_DefView` and the `WorkerW` are children of `Progman`. Wallpaper
    /// windows become `WS_EX_LAYERED` children of Progman, inserted right below the icon view.
    Progman {
        /// Parent window.
        progman: isize,
        /// Icon view; the wallpaper goes immediately below it in Z order.
        defview: isize,
        /// The shell's own wallpaper worker, which must stay below ours.
        worker: Option<isize>,
    },
}

/// Chooses the parent from a snapshot taken after sending `0x052C` to Progman.
///
/// * `progman`: the `Progman` window, if any;
/// * `progman_children`: its direct children `(hwnd, class)` in Z order;
/// * `toplevels`: every top-level window in Z order (topmost first).
pub fn choose_desktop_parent(
    progman: Option<isize>,
    progman_children: &[(isize, &str)],
    toplevels: &[TopLevel<'_>],
) -> Option<DesktopParent> {
    let child = |class: &str| {
        progman_children
            .iter()
            .find(|(_, c)| *c == class)
            .map(|(h, _)| *h)
    };
    let defview_in_progman = progman.and(child(DEFVIEW_CLASS));
    let worker_in_progman = progman.and(child(WORKERW_CLASS));

    // 24H2+: icons and wallpaper worker both live inside Progman.
    if let (Some(progman), Some(defview), Some(worker)) =
        (progman, defview_in_progman, worker_in_progman)
    {
        return Some(DesktopParent::Progman {
            progman,
            defview,
            worker: Some(worker),
        });
    }

    // Legacy: the WorkerW following the top-level window that hosts the icons. When that host is Progman
    // itself, a WorkerW after it would sit *behind* Progman's own wallpaper: never use it.
    if let Some(host) = toplevels.iter().position(|t| t.has_defview)
        && Some(toplevels[host].hwnd) != progman
        && let Some(worker) = toplevels[host + 1..]
            .iter()
            .find(|t| t.class == WORKERW_CLASS)
    {
        return Some(DesktopParent::WorkerW {
            worker: worker.hwnd,
        });
    }

    // Icons still in Progman and no worker anywhere (0x052C ignored): draw as a Progman child below the icons.
    match (progman, defview_in_progman) {
        (Some(progman), Some(defview)) => Some(DesktopParent::Progman {
            progman,
            defview,
            worker: None,
        }),
        _ => None,
    }
}

/// Rectangle of a child window covering `monitor` (virtual-screen pixels) inside a parent whose client area
/// starts at `parent_origin` (virtual-screen pixels). Both WorkerW and Progman span the virtual screen, whose
/// origin is negative when a monitor sits left of or above the primary one.
pub fn child_rect(monitor: Rect, parent_origin: (i32, i32)) -> Rect {
    monitor.offset(-parent_origin.0, -parent_origin.1)
}

/// True for the classes that make up the desktop (a click there is a click on the wallpaper).
pub fn is_desktop_class(class: &str) -> bool {
    matches!(
        class,
        PROGMAN_CLASS | WORKERW_CLASS | DEFVIEW_CLASS | "SysListView32"
    )
}

/// Whether a click whose `WindowFromPoint` hit has class `hit_class` and root ancestor class `root_class`
/// lands on the wallpaper (desktop or one of our own windows of class `own_class`).
pub fn is_desktop_click(hit_class: &str, root_class: &str, own_class: &str) -> bool {
    hit_class == own_class
        || (is_desktop_class(hit_class)
            && (root_class == PROGMAN_CLASS || root_class == WORKERW_CLASS))
}

/// `WS_EX_TRANSPARENT`: click-through overlay windows never hide anything.
pub const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
/// `WS_EX_NOACTIVATE`: overlays (game bars, OSDs) that never take focus.
pub const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

/// Top-level windows that never count as covering the wallpaper: the shell itself and transparent overlays
/// (e.g. full-screen click-through overlays of GPU tools).
pub fn window_ignorable(class: &str, ex_style: u32) -> bool {
    matches!(
        class,
        PROGMAN_CLASS | WORKERW_CLASS | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    ) || ex_style & (WS_EX_TRANSPARENT | WS_EX_NOACTIVATE) != 0
}

/// State of a top-level window relevant to the pause decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowState {
    /// Visible bounds (`DWMWA_EXTENDED_FRAME_BOUNDS`, i.e. without the invisible resize borders).
    pub rect: Rect,
    /// `IsWindowVisible`.
    pub visible: bool,
    /// `IsIconic`.
    pub minimized: bool,
    /// `IsZoomed` (maximised windows hide the wallpaper but are not "fullscreen applications").
    pub maximized: bool,
    /// `DWMWA_CLOAKED` (other virtual desktop, suspended UWP app...).
    pub cloaked: bool,
    /// Result of [`window_ignorable`].
    pub ignorable: bool,
    /// The window is the foreground window or shares its root owner ([`is_active_window`]).
    pub active: bool,
}

/// Whether `hwnd` is "the active window": the foreground window itself, or a window with the same root owner
/// (`GetAncestor(GA_ROOTOWNER)`), so a game whose foreground window is an owned or child window (launcher,
/// splash, dialog, overlay) still counts. `fg` and `fg_root_owner` are 0 when there is no foreground window.
pub fn is_active_window(hwnd: isize, root_owner: isize, fg: isize, fg_root_owner: isize) -> bool {
    fg != 0 && (hwnd == fg || (root_owner != 0 && root_owner == fg_root_owner))
}

/// Result of `SHQueryUserNotificationState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationState {
    /// `QUNS_NOT_PRESENT`: screen saver, locked machine or fast user switching.
    NotPresent,
    /// `QUNS_BUSY`: a full-screen application runs or presentation settings apply.
    Busy,
    /// `QUNS_RUNNING_D3D_FULL_SCREEN`: exclusive-mode Direct3D application.
    D3dFullScreen,
    /// `QUNS_PRESENTATION_MODE`.
    PresentationMode,
    /// Anything else (accepts notifications, quiet time, app).
    Normal,
}

impl NotificationState {
    /// Maps the raw `QUERY_USER_NOTIFICATION_STATE` value.
    pub fn from_raw(v: i32) -> Self {
        match v {
            1 => Self::NotPresent,
            2 => Self::Busy,
            3 => Self::D3dFullScreen,
            4 => Self::PresentationMode,
            _ => Self::Normal,
        }
    }
}

/// Monitor geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorGeom {
    /// Full monitor rectangle.
    pub rect: Rect,
    /// Work area (monitor minus taskbars).
    pub work: Rect,
}

/// Pause reasons of every monitor, written into `out` (same length as `monitors`).
///
/// * a visible, non-minimised, non-cloaked, non-ignorable window covering a monitor → `FULLSCREEN`
///   (`fullscreen_enabled`), or `OCCLUDED` when it is merely maximised or covers only the work area
///   (`covered_pause`);
/// * `QUNS_RUNNING_D3D_FULL_SCREEN`, `QUNS_PRESENTATION_MODE`, `QUNS_BUSY` → `FULLSCREEN` on the monitor of
///   the foreground window (all monitors when unknown);
/// * `QUNS_NOT_PRESENT` → `LOCKED` everywhere.
///
/// With `only_active`, a window covering a monitor yields `FULLSCREEN` only while it is active
/// ([`WindowState::active`]); a background one only hides the wallpaper (`OCCLUDED` with `covered_pause`).
/// The notification states are tied to the foreground window already; with `only_active` they never pause
/// everything when the foreground monitor is unknown.
#[allow(clippy::too_many_arguments)]
pub fn decide_pause(
    monitors: &[MonitorGeom],
    windows: &[WindowState],
    notification: NotificationState,
    foreground_monitor: Option<usize>,
    fullscreen_enabled: bool,
    only_active: bool,
    covered_pause: bool,
    out: &mut [PauseReasons],
) {
    for r in out.iter_mut() {
        *r = PauseReasons::NONE;
    }
    for w in windows {
        if !w.visible || w.minimized || w.cloaked || w.ignorable || w.rect.is_empty() {
            continue;
        }
        for (m, r) in monitors.iter().zip(out.iter_mut()) {
            if w.maximized {
                if covered_pause && w.rect.contains_rect(&m.work) {
                    *r |= PauseReasons::OCCLUDED;
                }
            } else if w.rect.contains_rect(&m.rect) {
                if fullscreen_enabled && (!only_active || w.active) {
                    *r |= PauseReasons::FULLSCREEN;
                } else if covered_pause {
                    *r |= PauseReasons::OCCLUDED;
                }
            } else if covered_pause && w.rect.contains_rect(&m.work) {
                *r |= PauseReasons::OCCLUDED;
            }
        }
    }
    match notification {
        NotificationState::NotPresent => {
            for r in out.iter_mut() {
                *r |= PauseReasons::LOCKED;
            }
        }
        NotificationState::Busy
        | NotificationState::D3dFullScreen
        | NotificationState::PresentationMode
            if fullscreen_enabled =>
        {
            match foreground_monitor.and_then(|i| out.get_mut(i)) {
                Some(r) => *r |= PauseReasons::FULLSCREEN,
                None if only_active => {}
                None => {
                    for r in out.iter_mut() {
                        *r |= PauseReasons::FULLSCREEN;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Name of a monitor for `-r` from its GDI device name (`\\.\DISPLAY1` → `DISPLAY1`).
pub fn monitor_name(device: &str) -> &str {
    device.strip_prefix(r"\\.\").unwrap_or(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tl(hwnd: isize, class: &str, has_defview: bool) -> TopLevel<'_> {
        TopLevel {
            hwnd,
            class,
            has_defview,
        }
    }

    #[test]
    fn legacy_layout_uses_next_workerw() {
        // Z order after 0x052C on Windows 10: WorkerW(icons), WorkerW(empty), Progman.
        let tops = [
            tl(1, "Chrome_WidgetWin_1", false),
            tl(10, "WorkerW", true),
            tl(11, "WorkerW", false),
            tl(12, "Progman", false),
        ];
        let parent = choose_desktop_parent(Some(12), &[], &tops);
        assert_eq!(parent, Some(DesktopParent::WorkerW { worker: 11 }));
    }

    #[test]
    fn legacy_ignores_workerw_above_the_icons() {
        let tops = [
            tl(5, "WorkerW", false),
            tl(10, "WorkerW", true),
            tl(11, "WorkerW", false),
            tl(12, "Progman", false),
        ];
        assert_eq!(
            choose_desktop_parent(Some(12), &[], &tops),
            Some(DesktopParent::WorkerW { worker: 11 })
        );
    }

    #[test]
    fn layout_24h2_is_child_of_progman() {
        let tops = [tl(1, "Notepad", false), tl(12, "Progman", true)];
        let children = [(20, DEFVIEW_CLASS), (21, WORKERW_CLASS)];
        assert_eq!(
            choose_desktop_parent(Some(12), &children, &tops),
            Some(DesktopParent::Progman {
                progman: 12,
                defview: 20,
                worker: Some(21)
            })
        );
    }

    #[test]
    fn icons_in_progman_without_worker_falls_back_to_progman() {
        let tops = [tl(12, "Progman", true)];
        let children = [(20, DEFVIEW_CLASS)];
        assert_eq!(
            choose_desktop_parent(Some(12), &children, &tops),
            Some(DesktopParent::Progman {
                progman: 12,
                defview: 20,
                worker: None
            })
        );
    }

    #[test]
    fn workerw_behind_progman_is_never_used() {
        // 0x052C ignored: icons still in Progman, an unrelated WorkerW further down the Z order.
        let tops = [tl(12, "Progman", true), tl(13, "WorkerW", false)];
        let children = [(20, DEFVIEW_CLASS)];
        assert_eq!(
            choose_desktop_parent(Some(12), &children, &tops),
            Some(DesktopParent::Progman {
                progman: 12,
                defview: 20,
                worker: None
            })
        );
    }

    #[test]
    fn no_shell_no_parent() {
        assert_eq!(
            choose_desktop_parent(None, &[], &[tl(1, "Notepad", false)]),
            None
        );
        assert_eq!(
            choose_desktop_parent(Some(12), &[], &[tl(12, "Progman", false)]),
            None
        );
    }

    #[test]
    fn child_rect_with_negative_virtual_origin() {
        // Secondary monitor left of the primary: virtual screen starts at x = -1920.
        let secondary = Rect::new(-1920, 0, 1920, 1080);
        let primary = Rect::new(0, 0, 2560, 1440);
        assert_eq!(
            child_rect(secondary, (-1920, 0)),
            Rect::new(0, 0, 1920, 1080)
        );
        assert_eq!(
            child_rect(primary, (-1920, 0)),
            Rect::new(1920, 0, 2560, 1440)
        );
    }

    #[test]
    fn desktop_click_targets() {
        assert!(is_desktop_click(
            "SysListView32",
            "WorkerW",
            "LumenWallpaper"
        ));
        assert!(is_desktop_click(
            "SysListView32",
            "Progman",
            "LumenWallpaper"
        ));
        assert!(is_desktop_click(
            "LumenWallpaper",
            "Progman",
            "LumenWallpaper"
        ));
        assert!(!is_desktop_click(
            "Chrome_RenderWidgetHostHWND",
            "Chrome_WidgetWin_1",
            "LumenWallpaper"
        ));
        assert!(
            !is_desktop_click("SysListView32", "CabinetWClass", "LumenWallpaper"),
            "list view of an Explorer window"
        );
    }

    #[test]
    fn ignorable_windows() {
        assert!(window_ignorable("Shell_TrayWnd", 0));
        assert!(window_ignorable("SomeOverlay", WS_EX_TRANSPARENT | 0x80));
        assert!(!window_ignorable("UnityWndClass", 0));
    }

    const M0: MonitorGeom = MonitorGeom {
        rect: Rect::new(0, 0, 1920, 1080),
        work: Rect::new(0, 0, 1920, 1040),
    };
    const M1: MonitorGeom = MonitorGeom {
        rect: Rect::new(1920, 0, 2560, 1440),
        work: Rect::new(1920, 0, 2560, 1440),
    };

    fn win(rect: Rect) -> WindowState {
        WindowState {
            rect,
            visible: true,
            minimized: false,
            maximized: false,
            cloaked: false,
            ignorable: false,
            active: false,
        }
    }

    fn active(rect: Rect) -> WindowState {
        WindowState {
            active: true,
            ..win(rect)
        }
    }

    fn decide(
        windows: &[WindowState],
        n: NotificationState,
        fg: Option<usize>,
        covered: bool,
    ) -> [PauseReasons; 2] {
        let mut out = [PauseReasons::NONE; 2];
        decide_pause(&[M0, M1], windows, n, fg, true, false, covered, &mut out);
        out
    }

    /// Same with `only_active` on.
    fn decide_active(
        windows: &[WindowState],
        n: NotificationState,
        fg: Option<usize>,
        covered: bool,
    ) -> [PauseReasons; 2] {
        let mut out = [PauseReasons::NONE; 2];
        decide_pause(&[M0, M1], windows, n, fg, true, true, covered, &mut out);
        out
    }

    #[test]
    fn borderless_fullscreen_window() {
        let out = decide(&[win(M1.rect)], NotificationState::Normal, None, true);
        assert_eq!(out, [PauseReasons::NONE, PauseReasons::FULLSCREEN]);
    }

    #[test]
    fn maximized_window_is_occlusion_not_fullscreen() {
        let max = WindowState {
            maximized: true,
            ..win(M1.work)
        };
        assert_eq!(
            decide(&[max], NotificationState::Normal, None, true),
            [PauseReasons::NONE, PauseReasons::OCCLUDED]
        );
        assert_eq!(
            decide(&[max], NotificationState::Normal, None, false),
            [PauseReasons::NONE; 2]
        );
        // Work area of a monitor with a taskbar.
        assert_eq!(
            decide(&[win(M0.work)], NotificationState::Normal, None, true)[0],
            PauseReasons::OCCLUDED
        );
    }

    #[test]
    fn hidden_windows_do_not_count() {
        let cloaked = WindowState {
            cloaked: true,
            ..win(M0.rect)
        };
        let minimized = WindowState {
            minimized: true,
            ..win(M0.rect)
        };
        let invisible = WindowState {
            visible: false,
            ..win(M0.rect)
        };
        let overlay = WindowState {
            ignorable: true,
            ..win(Rect::new(0, 0, 4480, 1440))
        };
        assert_eq!(
            decide(
                &[cloaked, minimized, invisible, overlay],
                NotificationState::Normal,
                None,
                true
            ),
            [PauseReasons::NONE; 2]
        );
    }

    #[test]
    fn notification_states() {
        assert_eq!(
            decide(&[], NotificationState::NotPresent, None, true),
            [PauseReasons::LOCKED; 2]
        );
        assert_eq!(
            decide(&[], NotificationState::D3dFullScreen, Some(1), true),
            [PauseReasons::NONE, PauseReasons::FULLSCREEN]
        );
        assert_eq!(
            decide(&[], NotificationState::Busy, None, true),
            [PauseReasons::FULLSCREEN; 2]
        );
        assert_eq!(
            NotificationState::from_raw(3),
            NotificationState::D3dFullScreen
        );
        assert_eq!(NotificationState::from_raw(5), NotificationState::Normal);
    }

    #[test]
    fn fullscreen_disabled_still_reports_coverage() {
        let mut out = [PauseReasons::NONE; 2];
        decide_pause(
            &[M0, M1],
            &[win(M1.rect)],
            NotificationState::D3dFullScreen,
            Some(1),
            false,
            false,
            true,
            &mut out,
        );
        assert_eq!(out, [PauseReasons::NONE, PauseReasons::OCCLUDED]);
    }

    #[test]
    fn only_active_off_ignores_the_active_flag() {
        assert_eq!(
            decide(&[win(M1.rect)], NotificationState::Normal, None, true),
            [PauseReasons::NONE, PauseReasons::FULLSCREEN]
        );
        assert_eq!(
            decide(&[active(M1.rect)], NotificationState::Normal, Some(1), true),
            [PauseReasons::NONE, PauseReasons::FULLSCREEN]
        );
    }

    #[test]
    fn only_active_foreground_fullscreen_pauses() {
        assert_eq!(
            decide_active(&[active(M1.rect)], NotificationState::Normal, Some(1), true),
            [PauseReasons::NONE, PauseReasons::FULLSCREEN]
        );
    }

    #[test]
    fn only_active_background_fullscreen_does_not_pause() {
        // Fullscreen game on M1 behind a normal foreground window on M0.
        let out = decide_active(
            &[active(Rect::new(100, 100, 800, 600)), win(M1.rect)],
            NotificationState::Normal,
            Some(0),
            false,
        );
        assert_eq!(out, [PauseReasons::NONE; 2]);
        // It still hides the wallpaper: occlusion keeps its own switch.
        let out = decide_active(&[win(M1.rect)], NotificationState::Normal, Some(0), true);
        assert_eq!(out, [PauseReasons::NONE, PauseReasons::OCCLUDED]);
    }

    #[test]
    fn only_active_follows_the_foreground_change() {
        let n = NotificationState::Normal;
        let mut game = win(M1.rect);
        assert_eq!(
            decide_active(&[game], n, None, false),
            [PauseReasons::NONE; 2]
        );
        game.active = true;
        assert_eq!(
            decide_active(&[game], n, Some(1), false),
            [PauseReasons::NONE, PauseReasons::FULLSCREEN]
        );
        game.active = false;
        assert_eq!(
            decide_active(&[game], n, Some(0), false),
            [PauseReasons::NONE; 2]
        );
    }

    #[test]
    fn active_window_uses_the_root_owner() {
        // Foreground is an owned dialog (20) of the game (10).
        assert!(is_active_window(20, 10, 20, 10));
        assert!(is_active_window(10, 10, 20, 10), "the game window itself");
        assert!(!is_active_window(11, 11, 20, 10), "unrelated window");
        assert!(!is_active_window(10, 10, 0, 0), "no foreground window");
        assert!(
            !is_active_window(10, 0, 20, 0),
            "unknown owners never match"
        );
    }

    #[test]
    fn only_active_notification_states() {
        assert_eq!(
            decide_active(&[], NotificationState::D3dFullScreen, Some(1), true),
            [PauseReasons::NONE, PauseReasons::FULLSCREEN]
        );
        assert_eq!(
            decide_active(&[], NotificationState::PresentationMode, Some(0), true),
            [PauseReasons::FULLSCREEN, PauseReasons::NONE]
        );
        // Unknown foreground monitor: nothing is active, nothing pauses.
        assert_eq!(
            decide_active(&[], NotificationState::Busy, None, true),
            [PauseReasons::NONE; 2]
        );
        assert_eq!(
            decide_active(&[], NotificationState::NotPresent, None, true),
            [PauseReasons::LOCKED; 2]
        );
    }

    #[test]
    fn monitor_names() {
        assert_eq!(monitor_name(r"\\.\DISPLAY2"), "DISPLAY2");
        assert_eq!(monitor_name("DISPLAY2"), "DISPLAY2");
    }
}

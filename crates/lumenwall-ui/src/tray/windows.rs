//! Windows notification-area icon through `tray-icon`, driven by a plain Win32
//! message loop that sleeps until a message arrives or the taskbar theme changes
//! (`MsgWaitForMultipleObjects` on a registry change notification).

use std::sync::mpsc;

use lumenwall_ipc::Endpoint;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_QUERY_VALUE, REG_NOTIFY_CHANGE_LAST_SET,
    RRF_RT_REG_DWORD, RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentThreadId, INFINITE};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, PostThreadMessageW,
    QS_ALLINPUT, TranslateMessage, WM_APP, WM_QUIT,
};

use super::{Action, Msg, TrayError, TrayState, labels, tooltip};
use crate::i18n::Lang;
use crate::icons::TrayInk;

fn other(e: impl std::fmt::Display) -> TrayError {
    TrayError::Other(e.to_string())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// White glyph on a dark taskbar (the default), black on a light one
/// (Settings › Personalisation › Colours › "Choose your default Windows mode").
fn taskbar_ink() -> TrayInk {
    let (key, value) = (wide(PERSONALIZE), wide("SystemUsesLightTheme"));
    let mut data: u32 = 0;
    let mut len = std::mem::size_of::<u32>() as u32;
    // SAFETY: null-terminated strings; `data` is a DWORD whose size is in `len`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast(),
            &mut len,
        )
    };
    if status == 0 && data != 0 {
        TrayInk::Black
    } else {
        TrayInk::White
    }
}

/// Notification-area icon size at the system DPI: 16 px at 100 %, 20 at 125 %,
/// 24 at 150 %, 32 at 200 %.
fn icon_size() -> u32 {
    // SAFETY: plain query.
    let dpi = unsafe { GetDpiForSystem() }.max(96);
    (16 * dpi).div_ceil(96)
}

fn glyph(ink: TrayInk, paused: bool, size: u32) -> Result<Icon, TrayError> {
    let png = crate::icons::tray_png(ink, paused, size);
    let (w, h, rgba) = crate::icons::decode_png(png).ok_or_else(|| other("tray glyph"))?;
    Icon::from_rgba(rgba, w, h).map_err(other)
}

/// Signals an event when a value under the theme key changes. Each notification
/// fires once, so it is re-armed after every wake-up.
struct ThemeWatch {
    key: HKEY,
    event: HANDLE,
}

impl ThemeWatch {
    fn new() -> Option<Self> {
        let path = wide(PERSONALIZE);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: valid string; `key` receives an open key closed in `Drop`.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                KEY_NOTIFY | KEY_QUERY_VALUE,
                &mut key,
            )
        };
        if status != 0 {
            return None;
        }
        // SAFETY: unnamed auto-reset event, default security.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        let watch = Self { key, event };
        (!event.is_null() && watch.arm()).then_some(watch)
    }

    fn arm(&self) -> bool {
        // SAFETY: open key and event owned by `self`; asynchronous notification.
        let status = unsafe {
            RegNotifyChangeKeyValue(self.key, 0, REG_NOTIFY_CHANGE_LAST_SET, self.event, 1)
        };
        status == 0
    }
}

impl Drop for ThemeWatch {
    fn drop(&mut self) {
        // SAFETY: closing the handles opened in `new`.
        unsafe {
            RegCloseKey(self.key);
            if !self.event.is_null() {
                CloseHandle(self.event);
            }
        }
    }
}

pub fn run(lang: Lang, endpoint: Endpoint) -> Result<(), TrayError> {
    // The tray process has no window of its own: declare DPI awareness so the
    // shell gets a glyph drawn for the real icon size instead of stretching one.
    // SAFETY: plain call; it fails harmlessly if awareness was already set.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // SAFETY: plain query of the calling thread id.
    let thread = unsafe { GetCurrentThreadId() };
    // Wakes the message loop when the daemon state or a menu click arrives.
    let wake = move || {
        // SAFETY: posting a private message to our own thread's queue.
        unsafe { PostThreadMessageW(thread, WM_APP, 0, 0) };
    };
    let (tx, rx) = mpsc::channel();
    let mut state = TrayState::default();

    let menu = Menu::new();
    let items: Vec<(Action, MenuItem)> = labels(
        lang,
        &TrayState {
            online: true,
            ..Default::default()
        },
    )
    .into_iter()
    .chain(std::iter::once((
        Action::StartEngine,
        crate::i18n::K::StartEngine.get(lang).to_owned(),
        true,
    )))
    .map(|(a, label, enabled)| (a, MenuItem::new(label, enabled, None)))
    .collect();
    for (a, item) in &items {
        if *a == Action::Quit {
            menu.append(&PredefinedMenuItem::separator())
                .map_err(other)?;
        }
        menu.append(item).map_err(other)?;
    }
    let mut shown = (taskbar_ink(), state.shows_paused(), icon_size());
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tooltip(lang, &state))
        .with_icon(glyph(shown.0, shown.1, shown.2)?)
        .build()
        .map_err(other)?;
    let theme = ThemeWatch::new();
    // Redraws the glyph when the taskbar theme, the state or the DPI changed.
    let mut refresh = |state: &TrayState| {
        let want = (taskbar_ink(), state.shows_paused(), icon_size());
        if want != shown
            && let Ok(icon) = glyph(want.0, want.1, want.2)
        {
            let _ = tray.set_icon(Some(icon));
            shown = want;
        }
    };

    let menu_tx = tx.clone();
    let ids: Vec<(Action, tray_icon::menu::MenuId)> =
        items.iter().map(|(a, i)| (*a, i.id().clone())).collect();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if let Some((a, _)) = ids.iter().find(|(_, id)| *id == e.id) {
            let _ = menu_tx.send(Msg::Action(*a));
            wake();
        }
    }));
    let click_tx = tx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } = e
        {
            let _ = click_tx.send(Msg::Action(Action::Open));
            wake();
        }
    }));

    let mut client = super::connect(&endpoint, &tx, wake);
    if client.is_none() {
        let _ = tx.send(Msg::Daemon(None));
    }
    let handles: Vec<HANDLE> = theme.iter().map(|t| t.event).collect();
    // SAFETY: MSG is plain data; PeekMessageW fills it.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: waits on our own event handle(s) or any input for this thread.
        let woke = unsafe {
            MsgWaitForMultipleObjects(
                handles.len() as u32,
                handles.as_ptr(),
                0,
                INFINITE,
                QS_ALLINPUT,
            )
        };
        if !handles.is_empty() && woke == WAIT_OBJECT_0 {
            // The taskbar theme changed (light / dark).
            if let Some(t) = &theme {
                t.arm();
            }
            refresh(&state);
            continue;
        }
        // SAFETY: standard message pump on this thread.
        while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if msg.message == WM_QUIT {
                return Ok(());
            }
            // SAFETY: dispatching the message we just received.
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Action(a) => {
                    if !super::perform(a, &mut client, &state, &endpoint, &tx, wake) {
                        return Ok(());
                    }
                }
                Msg::Daemon(s) => {
                    if s.is_none() {
                        client = None;
                    }
                    state = TrayState::from_daemon(s.as_ref());
                    let current = labels(lang, &state);
                    for (a, item) in &items {
                        match current.iter().find(|(x, _, _)| x == a) {
                            Some((_, label, enabled)) => {
                                item.set_text(label);
                                item.set_enabled(*enabled);
                            }
                            None => item.set_enabled(false),
                        }
                    }
                    let _ = tray.set_tooltip(Some(tooltip(lang, &state)));
                    refresh(&state);
                }
            }
        }
    }
}

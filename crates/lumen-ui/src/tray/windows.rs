//! Windows notification-area icon through `tray-icon`, driven by a plain Win32
//! message loop (`GetMessageW` sleeps until something happens).

use std::sync::mpsc;

use lumen_ipc::Endpoint;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, TranslateMessage, WM_APP,
};

use super::{Action, Msg, TrayError, TrayState, labels, tooltip};
use crate::i18n::Lang;

fn other(e: impl std::fmt::Display) -> TrayError {
    TrayError::Other(e.to_string())
}

pub fn run(lang: Lang, endpoint: Endpoint) -> Result<(), TrayError> {
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
    let icon =
        Icon::from_rgba(crate::icons::logo_rgba(32, [0xff, 0xb3, 0x47]), 32, 32).map_err(other)?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tooltip(lang, &state))
        .with_icon(icon)
        .build()
        .map_err(other)?;

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
    // SAFETY: MSG is plain data; GetMessageW fills it.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: standard message loop on this thread.
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            break;
        }
        // SAFETY: dispatching the message we just received.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
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
                }
            }
        }
    }
    Ok(())
}

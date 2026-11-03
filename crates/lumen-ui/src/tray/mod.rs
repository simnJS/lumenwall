//! `lumen-ui --tray`: a separate, windowless process (no GPU context, a few MB)
//! with pause/resume, mute, open the app and quit. It blocks on D-Bus / the Win32
//! message loop and on the IPC connection: zero work while idle.

use std::sync::mpsc::Sender;

use lumen_ipc::{Client, ClientEvent, Endpoint, State, Topic};

use crate::i18n::{K, Lang};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

/// What the tray shows, rebuilt on every daemon state change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrayState {
    pub online: bool,
    pub paused: bool,
    pub muted: bool,
}

impl TrayState {
    pub fn from_daemon(state: Option<&State>) -> Self {
        match state {
            Some(s) => Self {
                online: true,
                paused: s.paused,
                muted: s.muted,
            },
            None => Self::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    TogglePause,
    ToggleMute,
    StartEngine,
    Quit,
}

pub enum Msg {
    Action(Action),
    Daemon(Option<State>),
}

/// Labels of the menu entries, in the menu order.
pub fn labels(lang: Lang, s: &TrayState) -> Vec<(Action, String, bool)> {
    let pause = if s.paused { K::Resume } else { K::Pause };
    let mute = if s.muted { K::Unmute } else { K::Mute };
    let mut v = vec![(Action::Open, K::TrayOpen.get(lang).to_owned(), true)];
    if s.online {
        v.push((Action::TogglePause, pause.get(lang).to_owned(), true));
        v.push((Action::ToggleMute, mute.get(lang).to_owned(), true));
    } else {
        v.push((
            Action::StartEngine,
            K::StartEngine.get(lang).to_owned(),
            true,
        ));
    }
    v.push((Action::Quit, K::TrayQuit.get(lang).to_owned(), true));
    v
}

pub fn tooltip(lang: Lang, s: &TrayState) -> String {
    let status = match (s.online, s.paused) {
        (false, _) => K::EngineStopped,
        (true, true) => K::Paused,
        (true, false) => K::Playing,
    };
    format!("Lumen · {}", status.get(lang))
}

/// Connects to the daemon and forwards its state to `tx`.
pub fn connect(
    endpoint: &Endpoint,
    tx: &Sender<Msg>,
    wake: impl Fn() + Send + Sync + Clone + 'static,
) -> Option<Client> {
    let events = tx.clone();
    let w = wake.clone();
    let client = Client::connect_with_events(endpoint, "lumen-tray", move |e| {
        let msg = match e {
            ClientEvent::Event(lumen_ipc::Event::State(s)) => Msg::Daemon(Some(s)),
            ClientEvent::Disconnected => Msg::Daemon(None),
            ClientEvent::Event(_) => return,
        };
        if events.send(msg).is_ok() {
            w();
        }
    })
    .ok()?;
    let _ = client.subscribe(&[Topic::State]);
    if let Ok(s) = client.state()
        && tx.send(Msg::Daemon(Some(s))).is_ok()
    {
        wake();
    }
    Some(client)
}

/// Carries out a menu action. Returns false when the tray should exit.
pub fn perform(
    action: Action,
    client: &mut Option<Client>,
    state: &TrayState,
    endpoint: &Endpoint,
    tx: &Sender<Msg>,
    wake: impl Fn() + Send + Sync + Clone + 'static,
) -> bool {
    match action {
        Action::Open => {
            if let Err(e) = crate::platform::spawn_self(&[]) {
                tracing::error!("cannot open the window: {e}");
            }
        }
        Action::TogglePause => {
            if let Some(c) = client {
                let _ = c.set_paused(!state.paused);
            }
        }
        Action::ToggleMute => {
            if let Some(c) = client {
                let _ = c.set_muted(!state.muted);
            }
        }
        Action::StartEngine => {
            if let Some(exe) = crate::platform::find_daemon() {
                let _ = crate::platform::spawn_daemon(&exe);
                for i in 0..7 {
                    std::thread::sleep(std::time::Duration::from_millis(100 << i));
                    if let Some(c) = connect(endpoint, tx, wake.clone()) {
                        *client = Some(c);
                        break;
                    }
                }
            }
        }
        Action::Quit => {
            if let Some(c) = client {
                let _ = c.shutdown();
            }
            return false;
        }
    }
    true
}

#[derive(Debug)]
pub enum TrayError {
    /// No StatusNotifierWatcher on the session bus (e.g. GNOME without the
    /// AppIndicator extension).
    #[cfg_attr(windows, allow(dead_code))]
    NoHost,
    Other(String),
}

impl std::fmt::Display for TrayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrayError::NoHost => write!(
                f,
                "no system tray available (on GNOME, install the AppIndicator extension)"
            ),
            TrayError::Other(e) => write!(f, "{e}"),
        }
    }
}

pub fn run(lang: Lang, endpoint: Endpoint) -> Result<(), TrayError> {
    #[cfg(target_os = "linux")]
    return linux::run(lang, endpoint);
    #[cfg(windows)]
    return windows::run(lang, endpoint);
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (lang, endpoint);
        Err(TrayError::NoHost)
    }
}

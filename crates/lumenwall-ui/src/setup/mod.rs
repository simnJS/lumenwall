//! Installation on this computer: the first-run dialog and the Installation card of
//! the Settings.
//!
//! What "installed" means, per platform:
//!
//! | Item | Linux | Windows |
//! |---|---|---|
//! | Programs (only when run from an extracted release archive) | `~/.local/bin` | `%LOCALAPPDATA%\Programs\Lumenwall` |
//! | Menu | `~/.local/share/applications/lumenwall-ui.desktop`, `hicolor` icons | Start menu shortcut |
//! | Start with the session | `~/.config/autostart/lumenwall.desktop` | `HKCU\…\Run` value `Lumenwall` |
//! | GNOME Shell extension (GNOME only) | `~/.local/share/gnome-shell/extensions/…`, enabled | — |
//!
//! Detection runs on a worker thread at start. Nothing is written until the user
//! clicks: the dialog opens by itself once (and again only for a newer GNOME extension),
//! and the Settings card repairs or removes. The files written on Linux are compiled
//! into the binary (`assets.rs`), so a bare `lumenwall-ui` can set itself up.

#[cfg(not(windows))]
mod assets;
#[cfg(not(windows))]
mod desktop;
mod files;
#[cfg(not(windows))]
pub mod linux;
#[cfg(any(windows, test))]
pub mod windows;

#[cfg(not(windows))]
use linux as platform;
#[cfg(windows)]
use windows as platform;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::{button, center, column, container, opaque, row, space, text};
use iced::{Alignment, Border, Color, Element, Length, Padding, Task};
use serde::{Deserialize, Serialize};

use crate::app::{App, Message};
use crate::i18n::Lang;
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens, radius, size, space as sp};
use crate::view::widgets::{self, label, strong};

/// One thing the setup puts in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemId {
    Programs,
    Menu,
    Autostart,
    Extension,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// In place and current.
    Ok,
    Missing,
    /// Present but stale: another version of the programs, an entry naming a program
    /// that is gone, a lost icon, an older extension.
    Outdated,
    /// The GNOME extension is installed but turned off for this user.
    #[cfg_attr(windows, allow(dead_code))]
    Disabled,
    /// Provided by a system package.
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    pub state: State,
    /// Where it is or goes.
    pub path: PathBuf,
    /// Extension update: installed and embedded `version-name`.
    pub versions: Option<(String, String)>,
}

impl Item {
    pub fn new(id: ItemId, state: State, path: PathBuf) -> Self {
        Self {
            id,
            state,
            path,
            versions: None,
        }
    }

    fn needs_work(&self) -> bool {
        matches!(
            self.state,
            State::Missing | State::Outdated | State::Disabled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub items: Vec<Item>,
    /// Windows: installed with the installer, whose uninstaller removes it.
    pub uninstaller: Option<PathBuf>,
    /// Installed by a package manager (`/usr/bin`…): its files are not ours.
    pub managed: bool,
    /// A development build: the dialog never opens by itself.
    pub development: bool,
    pub gnome: bool,
}

impl Report {
    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn needs_setup(&self) -> bool {
        self.items.iter().any(Item::needs_work)
    }
}

/// What an action did, item by item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub results: Vec<(ItemId, Result<(), String>)>,
    /// The GNOME extension runs only after the user logs out and back in.
    pub relogin: bool,
    /// The programs were copied there.
    pub copied_to: Option<PathBuf>,
    /// Windows: the folder Lumenwall runs from, to delete once it is closed.
    pub leftover: Option<PathBuf>,
}

impl Outcome {
    fn failed(&self) -> bool {
        self.results.iter().any(|(_, r)| r.is_err())
    }
}

/// The user's answers, in `ui.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The first-run dialog was answered ("Set up" or "Not now").
    pub prompted: bool,
    /// The extension version whose update was declined (asked again for the next one).
    pub declined_extension: Option<String>,
}

/// The embedded GNOME extension's version (Linux).
fn embedded_extension() -> Option<String> {
    #[cfg(not(windows))]
    return assets::extension_version();
    #[cfg(windows)]
    None
}

/// Whether the dialog opens by itself. Never for a copy the Windows installer put in
/// place: it already asked about the shortcut and the start with Windows.
pub fn should_prompt(report: &Report, prefs: &Prefs, embedded_extension: Option<&str>) -> bool {
    if report.development || report.uninstaller.is_some() || !report.needs_setup() {
        return false;
    }
    if !prefs.prompted {
        return true;
    }
    report
        .item(ItemId::Extension)
        .is_some_and(|i| i.state == State::Outdated)
        && prefs.declined_extension.as_deref() != embedded_extension
}

/// This computer, as seen from the running program. `None` without a home folder.
fn system_report() -> Option<Report> {
    let setup = platform::Setup::system()?;
    let mut report = setup.detect();
    if let Ok(exe) = std::env::current_exe() {
        use lumenwall_update::Availability;
        match lumenwall_update::availability(&exe) {
            Availability::Managed(_) => report.managed = true,
            Availability::Development => report.development = true,
            _ => {}
        }
    }
    Some(report)
}

fn no_home() -> Outcome {
    Outcome {
        results: vec![(ItemId::Menu, Err("no home folder".into()))],
        ..Default::default()
    }
}

/// Asks the engine on `endpoint` to quit and waits until it is gone (15 s at most).
/// `Ok(false)`: none was running. Used before an uninstall and by `--stop-engine`
/// (the Windows installer, before it replaces the programs).
pub fn stop_engine(endpoint: &lumenwall_ipc::Endpoint) -> Result<bool, String> {
    use lumenwall_ipc::{Client, Error};
    const NAME: &str = "lumenwall-ui";
    let client = match Client::connect(endpoint, NAME) {
        Ok(c) => c,
        Err(Error::NotRunning) => return Ok(false),
        Err(e) => return Err(format!("cannot reach the engine: {e}")),
    };
    match client.shutdown() {
        Ok(()) | Err(Error::Disconnected | Error::Timeout) => {}
        Err(e) => return Err(format!("the engine refused to stop: {e}")),
    }
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut pause = Duration::from_millis(50);
    loop {
        if matches!(Client::connect(endpoint, NAME), Err(Error::NotRunning)) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Err("the engine did not stop within 15 s".into());
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_secs(1));
    }
}

/// Runs `f` on a short-lived thread (the UI thread never waits on the disk).
async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("lumenwall-ui-setup".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    rx.await.ok()
}

const UI_EXE: &str = if cfg!(windows) {
    "lumenwall-ui.exe"
} else {
    "lumenwall-ui"
};

#[derive(Debug, Clone)]
enum Dialog {
    Choose,
    Working,
    Done(Arc<Outcome>),
}

/// The app's view of the installation.
pub struct SetupState {
    report: Option<Arc<Report>>,
    dialog: Option<Dialog>,
    picks: BTreeSet<ItemId>,
    busy: bool,
    confirm_uninstall: bool,
    /// The last uninstall, shown in the Settings card.
    removed: Option<Arc<Outcome>>,
    /// `--setup`: open the dialog even if everything is in place.
    force: bool,
    /// The dialog may open by itself (not in screenshots, demos, development builds).
    auto: bool,
}

impl SetupState {
    pub fn new(force: bool, auto: bool) -> Self {
        Self {
            report: None,
            dialog: None,
            picks: BTreeSet::new(),
            busy: false,
            confirm_uninstall: false,
            removed: None,
            force,
            auto,
        }
    }

    /// Everything that needs work is ticked; if nothing does, everything ("Repair").
    fn default_picks(report: &Report) -> BTreeSet<ItemId> {
        let needed: BTreeSet<ItemId> = report
            .items
            .iter()
            .filter(|i| i.needs_work())
            .map(|i| i.id)
            .collect();
        if !needed.is_empty() {
            return needed;
        }
        report
            .items
            .iter()
            .filter(|i| i.state != State::System)
            .map(|i| i.id)
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Detected(Option<Arc<Report>>),
    Toggle(ItemId),
    Open,
    Apply,
    Applied(Arc<Outcome>),
    NotNow,
    Close,
    AskUninstall,
    CancelUninstall,
    Uninstall,
    Uninstalled(Arc<Outcome>),
    RunUninstaller(PathBuf),
    /// Restart the engine and the app from the copied programs.
    Relaunch(PathBuf),
    Relaunched(PathBuf, Result<(), String>),
    OpenFolder(PathBuf),
}

fn msg(m: Msg) -> Message {
    Message::Setup(m)
}

impl App {
    /// At start: what is installed (worker thread); Windows also refreshes the version
    /// shown in Settings › Apps after an in-app update.
    pub(crate) fn setup_start(&mut self) -> Task<Message> {
        #[cfg(windows)]
        std::thread::Builder::new()
            .name("lumenwall-ui-setup".into())
            .spawn(windows::refresh_uninstall_entry)
            .ok();
        self.setup_detect()
    }

    fn setup_detect(&mut self) -> Task<Message> {
        Task::perform(off_thread(system_report), |r| {
            msg(Msg::Detected(r.flatten().map(Arc::new)))
        })
    }

    fn setup_save_answer(&mut self) {
        let s = &self.setup;
        let declined = s.report.as_deref().and_then(|r| {
            let ext = r.item(ItemId::Extension)?;
            (ext.state == State::Outdated && !s.picks.contains(&ItemId::Extension))
                .then(embedded_extension)
                .flatten()
        });
        self.cfg.setup.prompted = true;
        if declined.is_some() {
            self.cfg.setup.declined_extension = declined;
        }
        self.save_cfg();
    }

    pub(crate) fn setup_update(&mut self, m: Msg) -> Task<Message> {
        match m {
            Msg::Detected(report) => {
                let Some(report) = report else {
                    tracing::warn!("installation: no home folder");
                    return Task::none();
                };
                tracing::debug!(?report, "installation");
                let first = self.setup.report.is_none();
                let open = first
                    && (self.setup.force
                        || (self.setup.auto
                            && should_prompt(
                                &report,
                                &self.cfg.setup,
                                embedded_extension().as_deref(),
                            )));
                if open {
                    self.setup.picks = SetupState::default_picks(&report);
                    self.setup.dialog = Some(Dialog::Choose);
                }
                self.setup.report = Some(report);
            }
            Msg::Toggle(id) => {
                if !self.setup.picks.remove(&id) {
                    self.setup.picks.insert(id);
                }
            }
            Msg::Open => {
                if let Some(r) = &self.setup.report {
                    self.setup.picks = SetupState::default_picks(r);
                    self.setup.dialog = Some(Dialog::Choose);
                    self.setup.confirm_uninstall = false;
                }
            }
            Msg::Apply if !self.setup.busy => {
                self.setup_save_answer();
                self.setup.busy = true;
                self.setup.dialog = Some(Dialog::Working);
                let picks: Vec<ItemId> = self.setup.picks.iter().copied().collect();
                return Task::perform(
                    off_thread(move || match platform::Setup::system() {
                        Some(s) => s.apply(&picks),
                        None => no_home(),
                    }),
                    |o| msg(Msg::Applied(Arc::new(o.unwrap_or_else(no_home)))),
                );
            }
            Msg::Apply => {}
            Msg::Applied(outcome) => {
                self.setup.busy = false;
                for (id, r) in &outcome.results {
                    match r {
                        Ok(()) => tracing::info!(?id, "set up"),
                        Err(e) => tracing::warn!(?id, "setup: {e}"),
                    }
                }
                self.setup.dialog = Some(Dialog::Done(outcome));
                self.autostart = crate::config::autostart::is_enabled();
                return self.setup_detect();
            }
            Msg::NotNow => {
                self.setup_save_answer_declined();
                self.setup.dialog = None;
            }
            Msg::Close => self.setup.dialog = None,
            Msg::AskUninstall => self.setup.confirm_uninstall = true,
            Msg::CancelUninstall => self.setup.confirm_uninstall = false,
            Msg::Uninstall if !self.setup.busy => {
                self.setup.busy = true;
                self.setup.confirm_uninstall = false;
                let endpoint = self.endpoint.clone();
                return Task::perform(
                    off_thread(move || {
                        // The engine would keep running from deleted programs.
                        if let Err(e) = stop_engine(&endpoint) {
                            tracing::warn!("uninstall: {e}");
                        }
                        match platform::Setup::system() {
                            Some(s) => s.uninstall(),
                            None => no_home(),
                        }
                    }),
                    |o| msg(Msg::Uninstalled(Arc::new(o.unwrap_or_else(no_home)))),
                );
            }
            Msg::Uninstall => {}
            Msg::Uninstalled(outcome) => {
                self.setup.busy = false;
                for (id, r) in &outcome.results {
                    if let Err(e) = r {
                        tracing::warn!(?id, "uninstall: {e}");
                    }
                }
                let text = if outcome.failed() {
                    S::RemovedPartly
                } else {
                    S::Removed
                };
                let error = outcome.failed();
                self.toast(text.get(self.lang), error);
                self.setup.removed = Some(outcome);
                self.autostart = crate::config::autostart::is_enabled();
                return self.setup_detect();
            }
            Msg::RunUninstaller(path) => match crate::platform::spawn_daemon(&path) {
                Ok(()) => return self.setup_quit(),
                Err(e) => {
                    let m = format!("{}: {e}", path.display());
                    self.toast(m, true);
                }
            },
            Msg::Relaunch(dir) if !self.setup.busy => {
                self.setup.busy = true;
                let endpoint = self.endpoint.clone();
                let engine = dir.join(crate::platform::DAEMON_EXE);
                return Task::perform(
                    off_thread(move || {
                        lumenwall_update::restart_daemon(
                            &endpoint,
                            &engine,
                            crate::platform::spawn_daemon,
                        )
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                    }),
                    move |r| {
                        msg(Msg::Relaunched(
                            dir.clone(),
                            r.unwrap_or_else(|| Err("no worker thread".into())),
                        ))
                    },
                );
            }
            Msg::Relaunch(_) => {}
            Msg::Relaunched(dir, engine) => {
                self.setup.busy = false;
                if let Err(e) = &engine {
                    // The new app shows the engine as stopped, with its Start button.
                    tracing::warn!("engine from the installed copy: {e}");
                }
                match crate::platform::spawn_daemon(&dir.join(UI_EXE)) {
                    Ok(()) => return self.setup_quit(),
                    Err(e) => {
                        let m = format!("{}: {e}", dir.join(UI_EXE).display());
                        self.toast(m, true);
                    }
                }
            }
            Msg::OpenFolder(p) => {
                if let Err(e) = crate::platform::open(&p.to_string_lossy()) {
                    self.toast(e.to_string(), true);
                }
            }
        }
        Task::none()
    }

    pub(crate) fn setup_dialog_open(&self) -> bool {
        self.setup.dialog.is_some()
    }

    /// Escape answers "Not now" (or closes the result). No key sets anything up: the
    /// dialog opens by itself, and a keystroke meant for another window must not
    /// change the system.
    pub(crate) fn setup_key(&self, key: &iced::keyboard::Key) -> Option<Msg> {
        use iced::keyboard::{Key, key::Named};
        match (self.setup.dialog.as_ref()?, key) {
            (Dialog::Choose, Key::Named(Named::Escape)) => Some(Msg::NotNow),
            (Dialog::Done(_), Key::Named(Named::Escape | Named::Enter)) => Some(Msg::Close),
            _ => None,
        }
    }

    fn setup_save_answer_declined(&mut self) {
        // "Not now" declines everything offered, the extension update included.
        self.setup.picks.clear();
        self.setup_save_answer();
    }

    /// Closes the app as when its window is closed.
    fn setup_quit(&mut self) -> Task<Message> {
        self.cfg.window = Some([self.window.width, self.window.height]);
        self.cfg.last_session = Some(lumenwall_update::now());
        self.daemon.want_stats(false);
        self.save_cfg();
        iced::exit()
    }

    // ------------------------------------------------------------------ views

    fn item_title(&self, id: ItemId) -> &'static str {
        let s = match id {
            ItemId::Programs => S::Programs,
            ItemId::Menu if cfg!(windows) => S::MenuWin,
            ItemId::Menu => S::Menu,
            ItemId::Autostart if cfg!(windows) => S::AutostartWin,
            ItemId::Autostart => S::Autostart,
            ItemId::Extension => S::Extension,
        };
        s.get(self.lang)
    }

    fn item_caption(&self, item: &Item) -> String {
        let lang = self.lang;
        let dir = widgets::tilde(&item.path);
        match item.id {
            ItemId::Programs => S::ProgramsHelp.get(lang).replace("{dir}", &dir),
            ItemId::Menu if item.state == State::System => S::MenuSystemHelp.get(lang).to_owned(),
            ItemId::Menu if cfg!(windows) => S::MenuWinHelp.get(lang).to_owned(),
            ItemId::Menu => S::MenuHelp.get(lang).to_owned(),
            ItemId::Autostart => S::AutostartHelp.get(lang).to_owned(),
            ItemId::Extension => S::ExtensionHelp.get(lang).to_owned(),
        }
    }

    fn state_text(&self, item: &Item) -> String {
        let lang = self.lang;
        match (&item.state, item.id) {
            (State::Ok, _) => S::StateOk.get(lang).to_owned(),
            (State::Missing, _) => S::StateMissing.get(lang).to_owned(),
            (State::Outdated, ItemId::Programs) => S::StateOtherVersion.get(lang).to_owned(),
            (State::Outdated, ItemId::Extension) => match &item.versions {
                Some((old, new)) => S::StateUpdate
                    .get(lang)
                    .replace("{old}", old)
                    .replace("{new}", new),
                None => S::StateRepair.get(lang).to_owned(),
            },
            (State::Outdated, _) => S::StateRepair.get(lang).to_owned(),
            (State::Disabled, _) => S::StateOff.get(lang).to_owned(),
            (State::System, _) => S::StateSystem.get(lang).to_owned(),
        }
    }

    /// The first-run dialog (also opened from the Settings), over the whole window.
    pub(crate) fn setup_dialog(&self, t: &'static Tokens) -> Option<Element<'_, Message>> {
        let dialog = self.setup.dialog.as_ref()?;
        let report = self.setup.report.as_deref()?;
        let body = match dialog {
            Dialog::Choose => self.dialog_choose(t, report),
            Dialog::Working => self.dialog_working(t),
            Dialog::Done(o) => self.dialog_done(t, o),
        };
        let card = container(body)
            .padding(sp::XL)
            .max_width(620)
            .style(theme::dialog);
        Some(opaque(
            center(card).padding(sp::XL).style(theme::modal_backdrop),
        ))
    }

    fn dialog_header<'a>(
        &self,
        t: &'static Tokens,
        title: &'a str,
        body: &'a str,
    ) -> Element<'a, Message> {
        row![
            container(icons::symbol(t, theme::icon::BRAND))
                .center(Length::Fixed(48.0))
                .style(move |_| container::Style {
                    background: Some(t.accent_soft.into()),
                    border: Border {
                        radius: 24.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            column![
                strong(title, size::TITLE, t.text),
                label(body, size::BODY, t.muted)
            ]
            .spacing(sp::XS)
            .width(Length::Fill),
        ]
        .spacing(sp::L)
        .align_y(Alignment::Center)
        .into()
    }

    fn dialog_choose<'a>(&'a self, t: &'static Tokens, report: &'a Report) -> Element<'a, Message> {
        let lang = self.lang;
        let intro = if report.items.iter().any(|i| i.id == ItemId::Programs) {
            S::IntroArchive
        } else {
            S::Intro
        };
        let mut list = column![].spacing(sp::XS);
        for item in &report.items {
            list = list.push(self.check_row(t, item));
        }
        let mut col = column![
            self.dialog_header(t, S::Title.get(lang), intro.get(lang)),
            list
        ]
        .spacing(sp::L);
        if self.setup.picks.contains(&ItemId::Extension) {
            col = col.push(relogin_note(t, lang));
        }
        let can = !self.setup.picks.is_empty() && !self.setup.busy;
        col = col.push(
            row![
                space().width(Length::Fill),
                widgets::ghost_button(t, None, S::NotNow.get(lang), Some(msg(Msg::NotNow))),
                widgets::primary_button(
                    t,
                    Some(Icon::Check),
                    S::SetUp.get(lang),
                    can.then(|| msg(Msg::Apply))
                ),
            ]
            .spacing(sp::S)
            .align_y(Alignment::Center),
        );
        col.into()
    }

    /// A tickable item; items already in place show a check mark instead.
    fn check_row<'a>(&'a self, t: &'static Tokens, item: &'a Item) -> Element<'a, Message> {
        let tickable = item.needs_work() || matches!(item.state, State::Ok);
        let tickable = tickable && item.state != State::System;
        let checked = self.setup.picks.contains(&item.id);
        let mark: Element<'_, Message> = if tickable {
            let fill = checked;
            container(if fill {
                icons::icon(Icon::Check, theme::icon::S, t.on_accent)
            } else {
                space().into()
            })
            .center(Length::Fixed(20.0))
            .style(move |_| container::Style {
                background: Some(if fill { t.accent } else { t.raised }.into()),
                border: Border {
                    color: if fill { t.accent } else { t.faint },
                    width: 1.5,
                    radius: radius::S.into(),
                },
                ..Default::default()
            })
            .into()
        } else {
            container(icons::icon(Icon::Package, theme::icon::M, t.muted))
                .center(Length::Fixed(20.0))
                .into()
        };
        let (pill_fg, pill_bg) = match item.state {
            State::Ok => (
                t.success,
                Color {
                    a: 0.14,
                    ..t.success
                },
            ),
            State::System => (t.muted, t.overlay),
            _ => (t.accent_text, t.accent_soft),
        };
        let pill = container(
            text(self.state_text(item))
                .size(size::MICRO)
                .font(theme::medium())
                .color(pill_fg),
        )
        .padding(Padding::from([2.0, 8.0]))
        .style(move |_| container::Style {
            background: Some(pill_bg.into()),
            border: Border {
                radius: radius::PILL.into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let content = row![
            mark,
            column![
                label(self.item_title(item.id), size::BODY, t.text),
                label(self.item_caption(item), size::CAPTION, t.muted),
            ]
            .spacing(2)
            .width(Length::Fill),
            pill,
        ]
        .spacing(sp::M)
        .align_y(Alignment::Center);
        button(content)
            .width(Length::Fill)
            .padding(Padding::from([sp::S, sp::M]))
            .style(theme::ghost)
            .on_press_maybe(tickable.then(|| msg(Msg::Toggle(item.id))))
            .into()
    }

    fn dialog_working(&self, t: &'static Tokens) -> Element<'_, Message> {
        let lang = self.lang;
        self.dialog_header(t, S::Working.get(lang), S::WorkingBody.get(lang))
    }

    fn dialog_done<'a>(&'a self, t: &'static Tokens, o: &'a Outcome) -> Element<'a, Message> {
        let lang = self.lang;
        let (title, body) = if o.failed() {
            (S::DonePartly, S::DonePartlyBody)
        } else {
            (S::Done, S::DoneBody)
        };
        let mut list = column![].spacing(sp::S);
        for (id, r) in &o.results {
            let (icon, color, detail) = match r {
                Ok(()) => (Icon::Check, t.success, None),
                Err(e) => (Icon::Alert, t.danger, Some(e.as_str())),
            };
            let mut texts = column![label(self.item_title(*id), size::BODY, t.text)].spacing(2);
            if let Some(d) = detail {
                texts = texts.push(label(d, size::CAPTION, t.muted));
            }
            list = list.push(
                row![
                    icons::icon(icon, theme::icon::M, color),
                    texts.width(Length::Fill)
                ]
                .spacing(sp::M)
                .align_y(Alignment::Center),
            );
        }
        let mut col = column![
            self.dialog_header(t, title.get(lang), body.get(lang)),
            container(list).padding(Padding::default().left(48.0 + sp::L)),
        ]
        .spacing(sp::L);
        if o.relogin {
            col = col.push(relogin_note(t, lang));
        }
        let mut actions = row![space().width(Length::Fill)]
            .spacing(sp::S)
            .align_y(Alignment::Center);
        if let Some(dir) = &o.copied_to {
            col = col.push(label(
                S::CopiedHelp
                    .get(lang)
                    .replace("{dir}", &widgets::tilde(dir)),
                size::CAPTION,
                t.muted,
            ));
            actions = actions.push(widgets::secondary_button(
                t,
                Some(Icon::Refresh),
                S::Relaunch.get(lang),
                (!self.setup.busy).then(|| msg(Msg::Relaunch(dir.clone()))),
            ));
        }
        actions = actions.push(widgets::primary_button(
            t,
            None,
            S::Close.get(lang),
            Some(msg(Msg::Close)),
        ));
        col.push(actions).into()
    }

    /// The Installation card of the Settings.
    pub(crate) fn install_section(&self, t: &'static Tokens) -> Element<'_, Message> {
        let lang = self.lang;
        let title = S::SecInstall.get(lang);
        let Some(report) = self.setup.report.as_deref() else {
            return widgets::section(
                t,
                title,
                label(S::Detecting.get(lang), size::CAPTION, t.muted),
            );
        };
        let mut col = column![].spacing(sp::M);
        if report.managed {
            col = col.push(widgets::notice(
                t,
                theme::Notice::Info,
                S::ManagedTitle.get(lang),
                S::ManagedBody.get(lang),
                None,
            ));
        }
        for item in &report.items {
            let (icon, color) = match item.state {
                State::Ok => (Icon::Check, t.success),
                State::System => (Icon::Package, t.muted),
                _ => (Icon::Alert, t.warning),
            };
            col = col.push(
                row![
                    icons::icon(icon, theme::icon::M, color),
                    column![
                        label(self.item_title(item.id), size::BODY, t.text),
                        label(widgets::tilde(&item.path), size::CAPTION, t.faint),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    label(self.state_text(item), size::CAPTION, t.muted),
                ]
                .spacing(sp::M)
                .align_y(Alignment::Center),
            );
        }
        if let Some(o) = &self.setup.removed
            && let Some(dir) = &o.leftover
        {
            col = col.push(widgets::notice(
                t,
                theme::Notice::Warning,
                S::LeftoverTitle.get(lang),
                S::LeftoverBody.get(lang),
                Some(widgets::secondary_button(
                    t,
                    Some(Icon::Folder),
                    S::OpenFolder.get(lang),
                    Some(msg(Msg::OpenFolder(dir.clone()))),
                )),
            ));
        }
        if self.setup.confirm_uninstall {
            let body = if report.managed {
                S::ConfirmManagedBody
            } else if cfg!(windows) {
                S::ConfirmBodyWin
            } else {
                S::ConfirmBody
            };
            col = col.push(widgets::notice(
                t,
                theme::Notice::Warning,
                S::ConfirmTitle.get(lang),
                body.get(lang),
                Some(
                    row![
                        widgets::ghost_button(
                            t,
                            None,
                            S::Cancel.get(lang),
                            Some(msg(Msg::CancelUninstall))
                        ),
                        widgets::secondary_button(
                            t,
                            Some(Icon::Close),
                            S::Uninstall.get(lang),
                            (!self.setup.busy).then(|| msg(Msg::Uninstall))
                        ),
                    ]
                    .spacing(sp::S)
                    .align_y(Alignment::Center)
                    .into(),
                ),
            ));
        }
        let needs = report.needs_setup();
        let mut actions = row![space().width(Length::Fill)]
            .spacing(sp::S)
            .align_y(Alignment::Center);
        let uninstall = match &report.uninstaller {
            Some(p) => Some(msg(Msg::RunUninstaller(p.clone()))),
            None => Some(msg(Msg::AskUninstall)),
        };
        actions = actions.push(widgets::ghost_button(
            t,
            Some(Icon::Close),
            S::Uninstall.get(lang),
            uninstall.filter(|_| !self.setup.busy && !self.setup.confirm_uninstall),
        ));
        actions = actions.push(widgets::secondary_button(
            t,
            Some(if needs { Icon::Download } else { Icon::Refresh }),
            if needs { S::SetUpMore } else { S::Repair }.get(lang),
            (!self.setup.busy).then(|| msg(Msg::Open)),
        ));
        col = col.push(actions);
        widgets::section(t, title, col)
    }
}

/// "Log out and back in": information, not a problem (no alert triangle).
fn relogin_note<'a>(t: &'static Tokens, lang: Lang) -> Element<'a, Message> {
    container(
        row![
            icons::icon(Icon::Clock, theme::icon::L, t.accent_text),
            column![
                strong(S::ReloginTitle.get(lang), size::BODY, t.text),
                label(S::ReloginBody.get(lang), size::CAPTION, t.muted),
            ]
            .spacing(2)
            .width(Length::Fill),
        ]
        .spacing(sp::M)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([10.0, 14.0]))
    .width(Length::Fill)
    .style(theme::hint_bar)
    .into()
}

macro_rules! texts {
    ($($key:ident => $en:literal, $fr:literal;)*) => {
        /// Strings of this module (EN / FR).
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum S { $($key),* }

        impl S {
            #[cfg(test)]
            const ALL: &'static [S] = &[$(S::$key),*];

            fn get(self, lang: Lang) -> &'static str {
                match lang {
                    Lang::En => match self { $(S::$key => $en),* },
                    Lang::Fr => match self { $(S::$key => $fr),* },
                }
            }
        }
    };
}

texts! {
    Title => "Set up Lumenwall on this computer", "Installer Lumenwall sur cet ordinateur";
    Intro => "Choose what to set up. Nothing changes until you click Set up; you can change it later in the Settings.",
        "Choisissez ce qu'il faut mettre en place. Rien ne change avant que vous cliquiez sur Installer ; vous pourrez y revenir dans les Réglages.";
    IntroArchive => "Lumenwall runs from the folder you downloaded. Choose what to set up; nothing changes until you click Set up.",
        "Lumenwall fonctionne depuis le dossier téléchargé. Choisissez ce qu'il faut mettre en place ; rien ne change avant que vous cliquiez sur Installer.";
    Programs => "Install the programs", "Installer les programmes";
    ProgramsHelp => "Copies Lumenwall to {dir}, where it updates itself. You can then delete the downloaded folder.",
        "Copie Lumenwall dans {dir}, où il se met à jour. Vous pourrez ensuite supprimer le dossier téléchargé.";
    Menu => "Applications menu entry and icon", "Entrée dans le menu des applications et icône";
    MenuHelp => "Open Lumenwall from your desktop's applications menu.", "Ouvrez Lumenwall depuis le menu des applications de votre bureau.";
    MenuSystemHelp => "The system package added it.", "Le paquet système l'a ajoutée.";
    MenuWin => "Start menu shortcut", "Raccourci dans le menu Démarrer";
    MenuWinHelp => "Open Lumenwall from the Start menu.", "Ouvrez Lumenwall depuis le menu Démarrer.";
    Autostart => "Start with the session", "Démarrer avec la session";
    AutostartWin => "Start with Windows", "Démarrer avec Windows";
    AutostartHelp => "Starts the engine and the tray icon when you log in.", "Démarre le moteur et l'icône de notification à l'ouverture de session.";
    Extension => "GNOME Shell extension", "Extension GNOME Shell";
    ExtensionHelp => "Pauses the wallpaper behind maximised windows and passes desktop clicks to it.",
        "Met le fond d'écran en pause derrière les fenêtres maximisées et lui transmet les clics sur le bureau.";
    StateOk => "In place", "En place";
    StateMissing => "Not set up", "Non installé";
    StateOtherVersion => "Other version there", "Autre version présente";
    StateRepair => "Needs repair", "À réparer";
    StateUpdate => "Update {old} → {new}", "Mise à jour {old} → {new}";
    StateOff => "Turned off", "Désactivée";
    StateSystem => "From the system package", "Fourni par le paquet système";
    ReloginTitle => "Log out and back in to finish", "Déconnectez-vous puis reconnectez-vous pour terminer";
    ReloginBody => "GNOME loads the extension at login: then the wallpaper pauses behind maximised windows and reacts to desktop clicks.",
        "GNOME charge l'extension à l'ouverture de session : le fond d'écran se met alors en pause derrière les fenêtres maximisées et réagit aux clics sur le bureau.";
    NotNow => "Not now", "Plus tard";
    SetUp => "Set up", "Installer";
    Working => "Setting up Lumenwall…", "Installation de Lumenwall…";
    WorkingBody => "Copying files. This takes a few seconds.", "Copie des fichiers. Cela prend quelques secondes.";
    Done => "Lumenwall is set up", "Lumenwall est installé";
    DoneBody => "Everything you chose is in place.", "Tout ce que vous avez choisi est en place.";
    DonePartly => "Some steps failed", "Certaines étapes ont échoué";
    DonePartlyBody => "The others are in place. Fix the cause shown below, then use Repair in the Settings.",
        "Les autres sont en place. Corrigez la cause indiquée ci-dessous, puis utilisez Réparer dans les Réglages.";
    CopiedHelp => "From now on, open Lumenwall from the menu: it runs from {dir}. You can delete the downloaded folder once you have restarted.",
        "Désormais, ouvrez Lumenwall depuis le menu : il fonctionne depuis {dir}. Vous pourrez supprimer le dossier téléchargé après le redémarrage.";
    Relaunch => "Restart from the installed copy", "Redémarrer depuis la copie installée";
    Close => "Close", "Fermer";
    SecInstall => "Installation", "Installation";
    Detecting => "Checking the installation…", "Vérification de l'installation…";
    SetUpMore => "Set up…", "Installer…";
    Repair => "Repair…", "Réparer…";
    Uninstall => "Uninstall", "Désinstaller";
    Cancel => "Cancel", "Annuler";
    ConfirmTitle => "Remove Lumenwall from this account?", "Retirer Lumenwall de ce compte ?";
    ConfirmBody => "This stops the engine and removes the menu entry, the icons, the start-up entry, the GNOME extension and the installed programs. Your settings and wallpapers are kept.",
        "Cela arrête le moteur et retire l'entrée de menu, les icônes, le démarrage automatique, l'extension GNOME et les programmes installés. Vos réglages et fonds d'écran sont conservés.";
    ConfirmBodyWin => "This stops the engine and removes the Start menu shortcut, the start-up entry and the programs the setup copied. Your settings and wallpapers are kept.",
        "Cela arrête le moteur et retire le raccourci du menu Démarrer, le démarrage automatique et les programmes copiés par l'installation. Vos réglages et fonds d'écran sont conservés.";
    ConfirmManagedBody =>"This stops the engine and removes the start-up entry and turns the GNOME extension off for you. Remove the package itself with your package manager.",
        "Cela arrête le moteur, retire le démarrage automatique et désactive l'extension GNOME pour vous. Retirez le paquet lui-même avec votre gestionnaire de paquets.";
    Removed => "Lumenwall was removed from this account. Your settings are kept.", "Lumenwall a été retiré de ce compte. Vos réglages sont conservés.";
    RemovedPartly => "Lumenwall was partly removed: see the Installation settings.", "Lumenwall a été retiré en partie : voyez les réglages d'installation.";
    ManagedTitle => "Installed by a system package", "Installé par un paquet système";
    ManagedBody => "The programs, menu entry, icons and GNOME extension come from the package and are updated with it.",
        "Les programmes, l'entrée de menu, les icônes et l'extension GNOME viennent du paquet et sont mis à jour avec lui.";
    LeftoverTitle => "Delete the program folder after closing Lumenwall", "Supprimez le dossier du programme après avoir fermé Lumenwall";
    LeftoverBody => "Windows does not let a running program delete itself.", "Windows ne laisse pas un programme en cours d'exécution se supprimer.";
    OpenFolder => "Open folder", "Ouvrir le dossier";
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;

    /// A unique folder under the system temporary folder, removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "lumenwall-ui-setup-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn report(states: &[(ItemId, State)]) -> Report {
        Report {
            items: states
                .iter()
                .map(|(id, s)| Item::new(*id, s.clone(), PathBuf::new()))
                .collect(),
            uninstaller: None,
            managed: false,
            development: false,
            gnome: true,
        }
    }

    #[test]
    fn the_dialog_opens_once() {
        let fresh = report(&[
            (ItemId::Menu, State::Missing),
            (ItemId::Autostart, State::Missing),
        ]);
        let mut prefs = Prefs::default();
        assert!(should_prompt(&fresh, &prefs, Some("2")));
        let done = report(&[(ItemId::Menu, State::Ok), (ItemId::Autostart, State::Ok)]);
        assert!(!should_prompt(&done, &prefs, Some("2")));
        let dev = Report {
            development: true,
            ..fresh.clone()
        };
        assert!(!should_prompt(&dev, &prefs, Some("2")));
        let installer = Report {
            uninstaller: Some(PathBuf::from("uninstall.exe")),
            ..fresh.clone()
        };
        assert!(
            !should_prompt(&installer, &prefs, Some("2")),
            "the installer asked"
        );
        prefs.prompted = true;
        assert!(!should_prompt(&fresh, &prefs, Some("2")), "answered once");
        // A newer extension asks again, once per version.
        let old_ext = report(&[
            (ItemId::Menu, State::Ok),
            (ItemId::Extension, State::Outdated),
        ]);
        assert!(should_prompt(&old_ext, &prefs, Some("3")));
        prefs.declined_extension = Some("3".into());
        assert!(!should_prompt(&old_ext, &prefs, Some("3")));
        assert!(should_prompt(&old_ext, &prefs, Some("4")));
        // A system package with the extension turned off: asked once.
        let deb = report(&[
            (ItemId::Menu, State::System),
            (ItemId::Extension, State::Disabled),
        ]);
        assert!(should_prompt(&deb, &Prefs::default(), Some("2")));
    }

    #[test]
    fn picks_default_to_what_needs_work() {
        let r = report(&[
            (ItemId::Programs, State::Ok),
            (ItemId::Menu, State::Outdated),
            (ItemId::Autostart, State::Missing),
            (ItemId::Extension, State::System),
        ]);
        assert_eq!(
            SetupState::default_picks(&r),
            BTreeSet::from([ItemId::Menu, ItemId::Autostart])
        );
        let all_ok = report(&[
            (ItemId::Menu, State::Ok),
            (ItemId::Extension, State::System),
        ]);
        assert_eq!(
            SetupState::default_picks(&all_ok),
            BTreeSet::from([ItemId::Menu])
        );
    }

    #[test]
    fn prefs_from_old_files() {
        let p: Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(p, Prefs::default());
        let p: Prefs = serde_json::from_str(r#"{"prompted": true}"#).unwrap();
        assert!(p.prompted && p.declined_extension.is_none());
    }

    #[test]
    fn strings_are_translated_with_the_same_placeholders() {
        for s in S::ALL {
            let (en, fr) = (s.get(Lang::En), s.get(Lang::Fr));
            assert!(!en.is_empty() && !fr.is_empty(), "{s:?}");
            let holes = |x: &str| {
                let mut v: Vec<String> = x
                    .split('{')
                    .skip(1)
                    .filter_map(|p| p.split_once('}'))
                    .map(|(h, _)| h.to_owned())
                    .collect();
                v.sort();
                v
            };
            assert_eq!(holes(en), holes(fr), "{s:?}");
            assert!(
                !en.contains('!') && !fr.contains('!'),
                "no exclamation marks: {s:?}"
            );
        }
    }
}

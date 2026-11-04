//! Self-update in the app: the "Lumenwall X.Y.Z is available" banner above the pages,
//! the Updates card of the Settings, and the glue to `lumenwall-update` (checks,
//! download and installation on worker threads, then engine and app restart).
//!
//! Requests: a check runs at start and when the window gets focus, only if one is due
//! (at most every 12 hours, `lumenwall_update::State::due`), or when the user asks.
//! No timer, no retry loop. Copies installed by a package manager, read-only folders,
//! development builds and builds without the release key make no request at all.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, column, container, progress_bar, row, scrollable, space, text};
use iced::{Alignment, Border, Color, Element, Length, Padding, Task, Theme};
use lumenwall_update::{
    Availability, Channel, Check, DaemonRestart, Error, InstalledNote, Layout, Release, Source,
    State, Version,
};
use serde::{Deserialize, Serialize};

use crate::app::{App, Message};
use crate::i18n::Lang;
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens, layout, radius, size, space as sp};
use crate::view::widgets::{self, label, setting_row, strong};

type NoteLine = lumenwall_update::notes::Line;

/// Lines of release notes shown in the banner (the rest is on GitHub).
const NOTE_LINES: usize = 16;

/// The user's choices, in `ui.json` (`"updates": {"auto_check": false}` turns the
/// automatic checks off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub auto_check: bool,
    pub prereleases: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            auto_check: true,
            prereleases: false,
        }
    }
}

impl Prefs {
    fn channel(self) -> Channel {
        if self.prereleases {
            Channel::Prerelease
        } else {
            Channel::Stable
        }
    }
}

pub struct Updater {
    current: Version,
    avail: Availability,
    source: Option<Source>,
    /// Debug builds with `LUMENWALL_UPDATE_*`: installs into a scratch folder and
    /// restarts nothing.
    dev: bool,
    state_path: Option<PathBuf>,
    state: State,
    phase: Phase,
    /// The banner above the pages ("Later" hides it until the next start).
    banner: bool,
    notes_open: bool,
    /// Release notes of the offered release, as text lines.
    notes: Vec<NoteLine>,
    /// Why the last check failed (shown in the Settings).
    last_error: Option<Error>,
    cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
enum Phase {
    Idle,
    Checking,
    Available(Arc<Release>),
    Downloading {
        release: Arc<Release>,
        done: u64,
        total: u64,
    },
    Installing(Arc<Release>),
    Restarting(Arc<Release>),
    /// A user action failed. With a release: download, install or restart.
    Failed {
        error: Error,
        release: Option<Arc<Release>>,
    },
}

#[derive(Debug, Clone)]
pub enum Msg {
    Checked {
        result: Result<Check, Error>,
        state: Option<Box<State>>,
        manual: bool,
    },
    CheckNow,
    AutoCheck(bool),
    Prereleases(bool),
    Install,
    Cancel,
    Later,
    ToggleNotes,
    Dismiss,
    Open(String),
    Worker(Work),
    Restarted(Result<DaemonRestart, Error>),
}

/// Events from the download-and-install thread.
#[derive(Debug, Clone)]
pub enum Work {
    Progress(u64, u64),
    Installing,
    Done(Result<lumenwall_update::Installed, Error>),
}

impl Updater {
    pub fn new() -> Self {
        let current =
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0));
        let (avail, source, dev) = match lumenwall_update::dev_override() {
            Some((source, layout)) => (Availability::Ready(layout), Some(source), true),
            None => {
                let avail = std::env::current_exe()
                    .map(|exe| lumenwall_update::availability(&exe))
                    .unwrap_or(Availability::Unsupported);
                let source = matches!(avail, Availability::Ready(_))
                    .then(Source::official)
                    .flatten();
                (avail, source, false)
            }
        };
        let ready = source.is_some();
        let state_path = State::default_path().map(|p| {
            if dev {
                p.with_file_name("update-dev.json")
            } else {
                p
            }
        });
        let state = match (&state_path, ready) {
            (Some(p), true) => State::load(p),
            _ => State::default(),
        };
        tracing::debug!(?avail, "updates");
        Self {
            current,
            avail,
            source,
            dev,
            state_path,
            state,
            phase: Phase::Idle,
            banner: false,
            notes_open: false,
            notes: Vec::new(),
            last_error: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn layout(&self) -> Option<&Layout> {
        match &self.avail {
            Availability::Ready(l) if self.source.is_some() => Some(l),
            _ => None,
        }
    }

    fn busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::Checking
                | Phase::Downloading { .. }
                | Phase::Installing(_)
                | Phase::Restarting(_)
        )
    }

    fn save(&self) {
        if let Some(p) = &self.state_path
            && let Err(e) = self.state.save(p)
        {
            tracing::warn!("cannot save {}: {e}", p.display());
        }
    }

    /// Back to what is known once an action ended or failed quietly.
    fn settle(&mut self, prefs: Prefs) {
        self.phase = match self.state.available.clone() {
            Some(r)
                if r.version > self.current && (prefs.prereleases || r.version.pre.is_empty()) =>
            {
                self.notes = lumenwall_update::notes::lines(&r.notes, NOTE_LINES);
                Phase::Available(Arc::new(r))
            }
            _ => Phase::Idle,
        };
    }

    fn release(&self) -> Option<Arc<Release>> {
        match &self.phase {
            Phase::Available(r)
            | Phase::Downloading { release: r, .. }
            | Phase::Installing(r)
            | Phase::Restarting(r)
            | Phase::Failed {
                release: Some(r), ..
            } => Some(Arc::clone(r)),
            _ => None,
        }
    }
}

/// Runs `f` on a short-lived thread (the UI thread never waits on the network or disk).
async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("lumenwall-ui-update".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    rx.await.ok()
}

fn no_thread() -> Error {
    Error::Io("could not start a worker thread".into())
}

impl App {
    /// At start: says "updated" once after an update, shows an update an earlier check
    /// found, removes what an earlier update left behind, and checks if one is due.
    pub(crate) fn updater_start(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if let Some(InstalledNote { to, .. }) = self.updater.state.installed.take() {
            if to == self.updater.current.to_string() {
                let msg = S::Updated.get(self.lang).replace("{v}", &to);
                self.toast(msg, false);
            }
            self.updater.save();
        }
        self.updater.settle(self.cfg.updates);
        self.updater.banner = matches!(self.updater.phase, Phase::Available(_));
        if let Some(l) = self.updater.layout()
            && !self.updater.dev
        {
            let dir = l.dir.clone();
            tasks.push(Task::perform(
                off_thread(move || lumenwall_update::cleanup(&dir)),
                |_| Message::Noop,
            ));
        }
        tasks.push(self.updater_maybe_check());
        Task::batch(tasks)
    }

    /// The window got focus: a check, if one is due.
    pub(crate) fn updater_focus(&mut self) -> Task<Message> {
        self.updater_maybe_check()
    }

    fn updater_maybe_check(&mut self) -> Task<Message> {
        if !self.cfg.updates.auto_check
            || self.updater.busy()
            || !self.updater.state.due(lumenwall_update::now())
        {
            return Task::none();
        }
        self.updater_check(false)
    }

    fn updater_check(&mut self, manual: bool) -> Task<Message> {
        let (Some(source), Some(target)) = (self.updater.source.clone(), lumenwall_update::TARGET)
        else {
            return Task::none();
        };
        let channel = self.cfg.updates.channel();
        let current = self.updater.current.clone();
        let mut state = self.updater.state.clone();
        if !matches!(self.updater.phase, Phase::Available(_)) {
            self.updater.phase = Phase::Checking;
        }
        Task::perform(
            off_thread(move || {
                let now = lumenwall_update::now();
                let result =
                    lumenwall_update::check(&source, &current, target, channel, &mut state, now);
                (result, state)
            }),
            move |out| {
                let (result, state) = match out {
                    Some((r, s)) => (r, Some(Box::new(s))),
                    None => (Err(no_thread()), None),
                };
                Message::Updater(Msg::Checked {
                    result,
                    state,
                    manual,
                })
            },
        )
    }

    pub(crate) fn updater_update(&mut self, msg: Msg) -> Task<Message> {
        let prefs = self.cfg.updates;
        match msg {
            Msg::Checked {
                result,
                state,
                manual,
            } => {
                if let Some(s) = state {
                    self.updater.state = *s;
                    self.updater.save();
                }
                match result {
                    Ok(Check::Available(r)) => {
                        tracing::info!(version = %r.version, "update available");
                        self.updater.last_error = None;
                        let known = matches!(&self.updater.phase,
                            Phase::Available(a) if a.version == r.version);
                        self.updater.notes = lumenwall_update::notes::lines(&r.notes, NOTE_LINES);
                        self.updater.phase = Phase::Available(Arc::from(r));
                        self.updater.banner |= !known || manual;
                    }
                    Ok(Check::UpToDate) => {
                        self.updater.last_error = None;
                        self.updater.phase = Phase::Idle;
                        self.updater.banner = false;
                        if manual {
                            let msg = S::UpToDateToast.get(self.lang);
                            self.toast(msg, false);
                        }
                    }
                    Err(e) => {
                        tracing::info!("update check: {e}");
                        self.updater.last_error = Some(e.clone());
                        if manual {
                            self.updater.phase = Phase::Failed {
                                error: e,
                                release: None,
                            };
                        } else {
                            self.updater.settle(prefs);
                        }
                    }
                }
            }
            Msg::CheckNow if !self.updater.busy() => return self.updater_check(true),
            Msg::CheckNow => {}
            Msg::AutoCheck(on) => {
                self.cfg.updates.auto_check = on;
                self.save_cfg();
                return self.updater_maybe_check();
            }
            Msg::Prereleases(on) => {
                self.cfg.updates.prereleases = on;
                self.save_cfg();
                // Another channel: what was found no longer applies.
                self.updater.state.available = None;
                if !self.updater.busy() {
                    self.updater.phase = Phase::Idle;
                    self.updater.banner = false;
                    return self.updater_check(false);
                }
            }
            Msg::Install => return self.updater_install(),
            Msg::Cancel => self.updater.cancel.store(true, Ordering::Relaxed),
            Msg::Later => self.updater.banner = false,
            Msg::ToggleNotes => self.updater.notes_open = !self.updater.notes_open,
            Msg::Dismiss => {
                self.updater.settle(prefs);
                self.updater.banner = false;
            }
            Msg::Open(url) => {
                if let Err(e) = crate::platform::open(&url) {
                    self.toast(e.to_string(), true);
                }
            }
            Msg::Worker(work) => return self.updater_work(work),
            Msg::Restarted(result) => return self.updater_relaunch(result),
        }
        Task::none()
    }

    fn updater_install(&mut self) -> Task<Message> {
        let u = &self.updater;
        let (Some(release), Some(source), Some(layout)) =
            (u.release(), u.source.clone(), u.layout().cloned())
        else {
            return Task::none();
        };
        if u.busy() {
            return Task::none();
        }
        let Some(dir) = dirs::cache_dir().map(|d| d.join("lumenwall").join("updates")) else {
            self.updater.phase = Phase::Failed {
                error: Error::Io("no cache folder".into()),
                release: Some(release),
            };
            return Task::none();
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.updater.cancel = Arc::clone(&cancel);
        let current = self.updater.current.clone();
        let (tx, rx) = iced::futures::channel::mpsc::unbounded();
        let r = Arc::clone(&release);
        let spawned = std::thread::Builder::new()
            .name("lumenwall-ui-update".into())
            .spawn(move || {
                let progress = tx.clone();
                let result =
                    lumenwall_update::download(&source, &current, &r, &dir, &cancel, |d, t| {
                        let _ = progress.unbounded_send(Work::Progress(d, t));
                    })
                    .and_then(|verified| {
                        let _ = tx.unbounded_send(Work::Installing);
                        lumenwall_update::install(&verified, &layout)
                    });
                let _ = tx.unbounded_send(Work::Done(result));
            });
        if spawned.is_err() {
            self.updater.phase = Phase::Failed {
                error: no_thread(),
                release: Some(release),
            };
            return Task::none();
        }
        self.updater.banner = true;
        self.updater.phase = Phase::Downloading {
            done: 0,
            total: release.archive.size,
            release,
        };
        Task::run(rx, |w| Message::Updater(Msg::Worker(w)))
    }

    fn updater_work(&mut self, work: Work) -> Task<Message> {
        let release = self.updater.release();
        match work {
            Work::Progress(d, t) => {
                if let Phase::Downloading { done, total, .. } = &mut self.updater.phase {
                    *done = d;
                    *total = t;
                }
            }
            Work::Installing => {
                if let Some(r) = release {
                    self.updater.phase = Phase::Installing(r);
                }
            }
            Work::Done(Ok(installed)) => return self.updater_installed(installed),
            Work::Done(Err(Error::Cancelled)) => self.updater.settle(self.cfg.updates),
            Work::Done(Err(e)) => {
                tracing::warn!("update not installed: {e}");
                self.updater.banner = true;
                self.updater.phase = Phase::Failed { error: e, release };
            }
        }
        Task::none()
    }

    fn updater_installed(&mut self, installed: lumenwall_update::Installed) -> Task<Message> {
        let release = self.updater.release();
        self.updater.state.available = None;
        if self.updater.dev {
            self.updater.save();
            self.updater.phase = Phase::Idle;
            self.updater.banner = false;
            let msg = S::InstalledDev
                .get(self.lang)
                .replace("{dir}", &installed.layout.dir.display().to_string());
            self.toast(msg, false);
            return Task::none();
        }
        self.updater.state.installed = Some(InstalledNote {
            from: self.updater.current.to_string(),
            to: installed.version.to_string(),
            at: lumenwall_update::now(),
        });
        self.updater.save();
        if let Some(r) = release {
            self.updater.phase = Phase::Restarting(r);
        }
        let endpoint = self.endpoint.clone();
        let daemon = installed.layout.program("lumenwall");
        Task::perform(
            off_thread(move || {
                lumenwall_update::restart_daemon(&endpoint, &daemon, crate::platform::spawn_daemon)
            }),
            |r| Message::Updater(Msg::Restarted(r.unwrap_or_else(|| Err(no_thread())))),
        )
    }

    /// Starts the new app and closes this one.
    fn updater_relaunch(&mut self, engine: Result<DaemonRestart, Error>) -> Task<Message> {
        match &engine {
            Ok(r) => tracing::info!(?r, "engine after update"),
            // The new app shows the engine as stopped, with its Start button.
            Err(e) => tracing::warn!("engine after update: {e}"),
        }
        let Some(layout) = self.updater.layout().cloned() else {
            return Task::none();
        };
        match crate::platform::spawn_daemon(&layout.program("lumenwall-ui")) {
            Ok(()) => {
                // As when the window is closed.
                self.cfg.window = Some([self.window.width, self.window.height]);
                self.cfg.last_session = Some(lumenwall_update::now());
                self.daemon.want_stats(false);
                self.save_cfg();
                iced::exit()
            }
            Err(e) => {
                self.updater.banner = true;
                self.updater.phase = Phase::Failed {
                    error: Error::Restart(e.to_string()),
                    release: self.updater.release(),
                };
                Task::none()
            }
        }
    }

    // ------------------------------------------------------------------ views

    /// The banner above the pages while an update is offered, in progress or failed.
    pub(crate) fn updater_banner(&self, t: &'static Tokens) -> Option<Element<'_, Message>> {
        let u = &self.updater;
        if !u.banner {
            return None;
        }
        let body: Element<'_, Message> = match &u.phase {
            Phase::Available(r) => self.banner_offer(t, r),
            Phase::Downloading {
                release,
                done,
                total,
            } => self.banner_progress(t, S::Downloading, release, Some((*done, *total))),
            Phase::Installing(r) => self.banner_progress(t, S::Installing, r, None),
            Phase::Restarting(r) => self.banner_progress(t, S::Restarting, r, None),
            Phase::Failed {
                error,
                release: Some(r),
            } => self.banner_error(t, error, r),
            _ => return None,
        };
        Some(
            container(body)
                .padding(Padding {
                    top: layout::PAGE_PAD,
                    right: layout::PAGE_PAD,
                    bottom: 0.0,
                    left: layout::PAGE_PAD,
                })
                .into(),
        )
    }

    fn banner_offer(&self, t: &'static Tokens, r: &Release) -> Element<'_, Message> {
        let lang = self.lang;
        let mut title = row![strong(
            S::Available
                .get(lang)
                .replace("{v}", &r.version.to_string()),
            size::BODY,
            t.text
        )]
        .spacing(sp::S)
        .align_y(Alignment::Center);
        if r.prerelease {
            title = title.push(pill(t, S::PreBadge.get(lang)));
        }
        let head = row![
            badge(Icon::Sparkle, t.accent_text, t.accent_soft),
            column![
                title,
                label(self.version_caption(r), size::CAPTION, t.muted)
            ]
            .spacing(2)
            .width(Length::Fill),
            widgets::ghost_button(
                t,
                None,
                if self.updater.notes_open {
                    S::HideNotes.get(lang)
                } else {
                    S::WhatsNew.get(lang)
                },
                (!self.updater.notes.is_empty()).then_some(Message::Updater(Msg::ToggleNotes)),
            ),
            widgets::ghost_button(
                t,
                None,
                S::Later.get(lang),
                Some(Message::Updater(Msg::Later))
            ),
            // Secondary: the page below may have its own primary action (Apply).
            secondary_accent(
                t,
                Icon::Download,
                S::UpdateRestart.get(lang),
                Message::Updater(Msg::Install),
            ),
        ]
        .spacing(sp::M)
        .align_y(Alignment::Center);
        let mut col = column![head].spacing(sp::M);
        if self.updater.notes_open && !self.updater.notes.is_empty() {
            col = col.push(widgets::divider(t)).push(
                container(
                    scrollable(
                        container(notes_view(t, &self.updater.notes))
                            .padding(Padding::default().right(sp::M)),
                    )
                    .style(theme::scroll_style),
                )
                .max_height(180)
                .padding(Padding::default().left(36.0 + sp::M)),
            );
            col = col.push(
                row![
                    space().width(Length::Fill),
                    widgets::ghost_button(
                        t,
                        Some(Icon::External),
                        S::ReleaseNotes.get(lang),
                        Some(Message::Updater(Msg::Open(r.page.clone()))),
                    )
                ]
                .align_y(Alignment::Center),
            );
        }
        container(col)
            .padding(Padding::from([sp::M, sp::L]))
            .width(Length::Fill)
            .style(theme::hint_bar)
            .into()
    }

    fn banner_progress(
        &self,
        t: &'static Tokens,
        what: S,
        r: &Release,
        bytes: Option<(u64, u64)>,
    ) -> Element<'_, Message> {
        let lang = self.lang;
        let title = what.get(lang).replace("{v}", &r.version.to_string());
        let (fraction, detail) = match bytes {
            Some((done, total)) => (
                if total > 0 {
                    (done as f32 / total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                },
                S::Progress
                    .get(lang)
                    .replace("{done}", &widgets::format_bytes(done, lang))
                    .replace("{total}", &widgets::format_bytes(total, lang)),
            ),
            None => (1.0, S::VerifiedNote.get(lang).to_owned()),
        };
        let mut head = row![
            badge(Icon::Download, t.accent_text, t.accent_soft),
            column![
                strong(title, size::BODY, t.text),
                progress_bar(0.0..=1.0, fraction)
                    .girth(6)
                    .style(bar_style(t)),
                label(detail, size::CAPTION, t.muted),
            ]
            .spacing(sp::XS)
            .width(Length::Fill),
        ]
        .spacing(sp::M)
        .align_y(Alignment::Center);
        if bytes.is_some() {
            head = head.push(widgets::ghost_button(
                t,
                Some(Icon::Close),
                S::Cancel.get(lang),
                Some(Message::Updater(Msg::Cancel)),
            ));
        }
        container(head)
            .padding(Padding::from([sp::M, sp::L]))
            .width(Length::Fill)
            .style(theme::hint_bar)
            .into()
    }

    fn banner_error(&self, t: &'static Tokens, e: &Error, r: &Release) -> Element<'_, Message> {
        let lang = self.lang;
        let (title, body) = error_text(e, lang, &r.version);
        let retry = !matches!(e, Error::Restart(_));
        let mut line = row![
            badge(
                Icon::Alert,
                t.danger,
                Color {
                    a: 0.14,
                    ..t.danger
                }
            ),
            column![
                strong(title, size::BODY, t.text),
                label(body, size::CAPTION, t.muted)
            ]
            .spacing(2)
            .width(Length::Fill),
        ]
        .spacing(sp::M)
        .align_y(Alignment::Center);
        if retry {
            line = line.push(widgets::secondary_button(
                t,
                Some(Icon::Refresh),
                S::Retry.get(lang),
                Some(Message::Updater(Msg::Install)),
            ));
        }
        line = line.push(widgets::ghost_button(
            t,
            None,
            S::Close.get(lang),
            Some(Message::Updater(Msg::Dismiss)),
        ));
        container(line)
            .padding(Padding::from([sp::M, sp::L]))
            .width(Length::Fill)
            .style(theme::notice(theme::Notice::Danger))
            .into()
    }

    fn version_caption(&self, r: &Release) -> String {
        let lang = self.lang;
        let mut s = S::InstalledVersion
            .get(lang)
            .replace("{v}", &self.updater.current.to_string());
        if let Some(d) = r.published.as_deref().and_then(|d| date(d, lang)) {
            s.push_str(&S::Released.get(lang).replace("{date}", &d));
        }
        s
    }

    /// The Updates card of the Settings.
    pub(crate) fn updates_section(&self, t: &'static Tokens) -> Element<'_, Message> {
        let lang = self.lang;
        let title = S::SecUpdates.get(lang);
        if self.updater.layout().is_none() {
            return widgets::section(t, title, self.unavailable(t));
        }
        let prefs = self.cfg.updates;
        let toggle = |on: bool, f: fn(bool) -> Msg| {
            iced::widget::toggler(on)
                .size(22)
                .style(theme::toggler_style)
                .on_toggle(move |b| Message::Updater(f(b)))
        };
        let col = column![
            self.update_status(t),
            widgets::divider(t),
            setting_row(
                t,
                S::AutoCheck.get(lang),
                Some(S::AutoCheckHelp.get(lang)),
                toggle(prefs.auto_check, Msg::AutoCheck),
            ),
            setting_row(
                t,
                S::Prereleases.get(lang),
                Some(S::PrereleasesHelp.get(lang)),
                toggle(prefs.prereleases, Msg::Prereleases),
            ),
        ]
        .spacing(sp::M);
        widgets::section(t, title, col)
    }

    fn update_status(&self, t: &'static Tokens) -> Element<'_, Message> {
        let lang = self.lang;
        let u = &self.updater;
        let check_now = |enabled: bool| {
            widgets::secondary_button(
                t,
                Some(Icon::Refresh),
                S::CheckNow.get(lang),
                enabled.then_some(Message::Updater(Msg::CheckNow)),
            )
        };
        let (icon, color, title, caption, action): (
            Icon,
            Color,
            String,
            String,
            Element<'_, Message>,
        ) = match &u.phase {
            Phase::Checking => (
                Icon::Refresh,
                t.muted,
                S::Checking.get(lang).to_owned(),
                self.checked_caption(),
                check_now(false),
            ),
            Phase::Available(r) => (
                Icon::Sparkle,
                t.accent_text,
                S::Available
                    .get(lang)
                    .replace("{v}", &r.version.to_string()),
                self.version_caption(r),
                primary_small(
                    t,
                    Icon::Download,
                    S::UpdateRestart.get(lang),
                    Some(Message::Updater(Msg::Install)),
                ),
            ),
            Phase::Downloading { release, .. }
            | Phase::Installing(release)
            | Phase::Restarting(release) => (
                Icon::Download,
                t.accent_text,
                S::InProgress
                    .get(lang)
                    .replace("{v}", &release.version.to_string()),
                S::SeeBanner.get(lang).to_owned(),
                space().into(),
            ),
            Phase::Failed { error, release } => {
                let v = release
                    .as_ref()
                    .map_or_else(|| u.current.clone(), |r| r.version.clone());
                let (title, body) = error_text(error, lang, &v);
                (Icon::Alert, t.danger, title, body, check_now(true))
            }
            Phase::Idle => match &u.last_error {
                Some(e) => {
                    let (title, body) = error_text(e, lang, &u.current);
                    (Icon::Alert, t.warning, title, body, check_now(true))
                }
                None => (
                    Icon::Check,
                    t.success,
                    S::UpToDate.get(lang).replace("{v}", &u.current.to_string()),
                    self.checked_caption(),
                    check_now(true),
                ),
            },
        };
        row![
            icons::icon(icon, theme::icon::L, color),
            column![
                strong(title, size::BODY, t.text),
                label(caption, size::CAPTION, t.muted)
            ]
            .spacing(2)
            .width(Length::Fill),
            action,
        ]
        .spacing(sp::M)
        .align_y(Alignment::Center)
        .into()
    }

    fn checked_caption(&self) -> String {
        let lang = self.lang;
        match self.updater.state.last_success {
            Some(at) => S::CheckedAgo.get(lang).replace(
                "{ago}",
                &ago(lumenwall_update::now().saturating_sub(at), lang),
            ),
            None => S::NotChecked.get(lang).to_owned(),
        }
    }

    /// Why this copy does not update itself, and where new versions are.
    fn unavailable(&self, t: &'static Tokens) -> Element<'_, Message> {
        let lang = self.lang;
        let icon = match &self.updater.avail {
            Availability::Managed(_) => Icon::Package,
            _ => Icon::Alert,
        };
        let (title, body, releases) = match &self.updater.avail {
            Availability::Managed(m) => match m.name() {
                Some(name) => (
                    S::ManagedTitle.get(lang).replace("{name}", name),
                    S::ManagedBody.get(lang).replace("{name}", name),
                    false,
                ),
                // A distribution's package, or Lumenwall's own .deb (no apt
                // repository yet): the releases page has the newer packages.
                None => (
                    S::DistroTitle.get(lang).to_owned(),
                    S::DistroBody.get(lang).to_owned(),
                    true,
                ),
            },
            Availability::ReadOnly(dir) => (
                S::ReadOnlyTitle.get(lang).to_owned(),
                S::ReadOnlyBody
                    .get(lang)
                    .replace("{dir}", &widgets::tilde(dir)),
                true,
            ),
            Availability::Development => (
                S::DevTitle.get(lang).to_owned(),
                S::DevBody.get(lang).to_owned(),
                false,
            ),
            Availability::Disabled => (
                S::OffTitle.get(lang).to_owned(),
                S::OffBody.get(lang).to_owned(),
                false,
            ),
            Availability::Unsupported => (
                S::UnsupportedTitle.get(lang).to_owned(),
                S::UnsupportedBody.get(lang).to_owned(),
                true,
            ),
            // No release key in this build (or the dev server failed to start).
            Availability::NoKey | Availability::Ready(_) => (
                S::NoKeyTitle.get(lang).to_owned(),
                S::NoKeyBody.get(lang).to_owned(),
                true,
            ),
        };
        let mut line = row![
            icons::icon(icon, theme::icon::L, t.accent_text),
            column![
                strong(title, size::BODY, t.text),
                label(body, size::CAPTION, t.muted)
            ]
            .spacing(2)
            .width(Length::Fill),
        ]
        .spacing(sp::M)
        .align_y(Alignment::Center);
        if releases {
            line = line.push(widgets::secondary_button(
                t,
                Some(Icon::External),
                S::OpenReleases.get(lang),
                Some(Message::Updater(Msg::Open(
                    lumenwall_update::RELEASES_PAGE.to_owned(),
                ))),
            ));
        }
        container(line)
            .padding(Padding::from([10.0, 14.0]))
            .width(Length::Fill)
            .style(theme::notice(theme::Notice::Info))
            .into()
    }
}

/// Title and explanation of an error, in the user's language. Every message says
/// whether anything changed and what to do.
fn error_text(e: &Error, lang: Lang, version: &Version) -> (String, String) {
    let (title, body) = match e {
        Error::Offline => (S::ErrOffline, S::ErrOfflineBody),
        Error::RateLimited { retry_at } => {
            let minutes = retry_at
                .saturating_sub(lumenwall_update::now())
                .div_ceil(60)
                .max(1);
            return (
                S::ErrRate.get(lang).to_owned(),
                S::ErrRateBody
                    .get(lang)
                    .replace("{n}", &minutes.to_string()),
            );
        }
        Error::Status(_) | Error::BadResponse(_) | Error::Refused(_) => {
            (S::ErrServer, S::ErrServerBody)
        }
        Error::Signature(_) => (S::ErrSignature, S::ErrSignatureBody),
        Error::Checksum(_) | Error::TooLarge => (S::ErrChecksum, S::ErrChecksumBody),
        Error::BadArchive(_) => (S::ErrArchive, S::ErrArchiveBody),
        Error::Io(detail) => {
            return (
                S::ErrInstall.get(lang).to_owned(),
                S::ErrInstallBody.get(lang).replace("{detail}", detail),
            );
        }
        Error::Restart(_) => (S::ErrRestart, S::ErrRestartBody),
        Error::Cancelled => (S::ErrServer, S::ErrServerBody),
    };
    (
        title.get(lang).to_owned(),
        body.get(lang).replace("{v}", &version.to_string()),
    )
}

/// "5 min ago", "il y a 3 h"…
fn ago(secs: u64, lang: Lang) -> String {
    let (n, unit) = match secs {
        0..60 => return S::JustNow.get(lang).to_owned(),
        60..3600 => (secs / 60, S::Minutes),
        3600..86400 => (secs / 3600, S::Hours),
        _ => (
            secs / 86400,
            if secs < 2 * 86400 { S::Day } else { S::Days },
        ),
    };
    S::Ago
        .get(lang)
        .replace("{n}", &format!("{n} {}", unit.get(lang)))
}

/// `2026-11-03` → "3 Nov 2026" / "3 nov. 2026".
fn date(iso: &str, lang: Lang) -> Option<String> {
    let mut parts = iso.splitn(3, '-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    let m: usize = m.parse().ok()?;
    let d: u32 = d.parse().ok()?;
    const EN: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const FR: [&str; 12] = [
        "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
        "déc.",
    ];
    let month = match lang {
        Lang::En => EN.get(m.checked_sub(1)?)?,
        Lang::Fr => FR.get(m.checked_sub(1)?)?,
    };
    Some(format!("{d} {month} {y}"))
}

/// Release notes: headings in semibold, list items with a bullet and their indent.
fn notes_view<'a>(t: &Tokens, lines: &'a [NoteLine]) -> Element<'a, Message> {
    column(lines.iter().map(|l| -> Element<'a, Message> {
        match l {
            NoteLine::Heading(h) => container(strong(h.as_str(), size::CAPTION, t.text))
                .padding(Padding::default().top(sp::XS))
                .into(),
            NoteLine::Item { depth, text } => row![
                space().width(*depth as f32 * sp::L),
                label("•", size::CAPTION, t.accent_text),
                label(text.as_str(), size::CAPTION, t.text).width(Length::Fill),
            ]
            .spacing(sp::S)
            .into(),
            NoteLine::Text(x) => label(x.as_str(), size::CAPTION, t.muted).into(),
            NoteLine::Blank => space().height(sp::XS).into(),
        }
    }))
    .spacing(3)
    .into()
}

/// Icon in a tinted disc.
fn badge<'a>(icon: Icon, fg: Color, bg: Color) -> Element<'a, Message> {
    container(icons::icon(icon, theme::icon::L, fg))
        .center(Length::Fixed(36.0))
        .style(move |_| container::Style {
            background: Some(bg.into()),
            border: Border {
                radius: 18.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

fn pill<'a>(t: &'static Tokens, s: &'a str) -> Element<'a, Message> {
    container(
        text(s)
            .size(size::MICRO)
            .font(theme::medium())
            .color(t.on_warning),
    )
    .padding(Padding::from([1.0, 8.0]))
    .style(theme::badge(t.warning, t.on_warning))
    .into()
}

/// Primary button at the height of the secondary ones.
fn primary_small<'a>(
    t: &Tokens,
    icon: Icon,
    s: &'a str,
    msg: Option<Message>,
) -> Element<'a, Message> {
    let fg = if msg.is_some() { t.on_accent } else { t.faint };
    button(
        row![
            icons::icon(icon, theme::icon::M, fg),
            text(s).size(size::BODY).font(theme::semibold()).color(fg)
        ]
        .spacing(sp::S)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([7.0, 14.0]))
    .style(theme::primary)
    .on_press_maybe(msg)
    .into()
}

/// Secondary button whose icon carries the accent.
fn secondary_accent<'a>(t: &Tokens, icon: Icon, s: &'a str, msg: Message) -> Element<'a, Message> {
    button(
        row![
            icons::icon(icon, theme::icon::M, t.accent_text),
            text(s).size(size::BODY).font(theme::medium())
        ]
        .spacing(sp::S)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([7.0, 14.0]))
    .style(theme::secondary)
    .on_press(msg)
    .into()
}

fn bar_style(t: &'static Tokens) -> impl Fn(&Theme) -> progress_bar::Style {
    move |_| progress_bar::Style {
        background: t.overlay.into(),
        bar: t.accent.into(),
        border: Border {
            radius: radius::PILL.into(),
            ..Default::default()
        },
    }
}

macro_rules! texts {
    ($($key:ident => $en:literal, $fr:literal;)*) => {
        /// Strings of this module (EN / FR), kept here so the update feature stays in
        /// one file.
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
    SecUpdates => "Updates", "Mises à jour";
    AutoCheck => "Check for updates automatically", "Rechercher les mises à jour automatiquement";
    AutoCheckHelp => "At most every 12 hours, from GitHub. Lumenwall sends nothing about you.",
        "Au plus toutes les 12 heures, auprès de GitHub. Lumenwall n'envoie rien sur vous.";
    Prereleases => "Include pre-releases", "Inclure les préversions";
    PrereleasesHelp => "Try new versions before their release. They can have bugs.",
        "Essayez les nouvelles versions avant leur sortie. Elles peuvent avoir des bugs.";
    UpToDate => "Lumenwall {v} is up to date", "Lumenwall {v} est à jour";
    UpToDateToast => "Lumenwall is up to date", "Lumenwall est à jour";
    NotChecked => "Not checked yet", "Pas encore vérifié";
    CheckedAgo => "Last checked {ago}", "Dernière vérification {ago}";
    Checking => "Checking for updates…", "Recherche de mises à jour…";
    CheckNow => "Check now", "Vérifier maintenant";
    Available => "Lumenwall {v} is available", "Lumenwall {v} est disponible";
    InstalledVersion => "Installed: {v}", "Version installée : {v}";
    Released => " · released {date}", " · publiée le {date}";
    PreBadge => "Pre-release", "Préversion";
    UpdateRestart => "Update and restart", "Mettre à jour et redémarrer";
    Later => "Later", "Plus tard";
    WhatsNew => "What's new", "Nouveautés";
    HideNotes => "Hide notes", "Masquer les notes";
    ReleaseNotes => "Release notes on GitHub", "Notes de version sur GitHub";
    Downloading => "Downloading Lumenwall {v}…", "Téléchargement de Lumenwall {v}…";
    Progress => "{done} of {total} · signature and checksum are checked before anything is installed",
        "{done} sur {total} · signature et somme de contrôle vérifiées avant toute installation";
    VerifiedNote => "Signature and checksum verified", "Signature et somme de contrôle vérifiées";
    Installing => "Installing Lumenwall {v}…", "Installation de Lumenwall {v}…";
    Restarting => "Restarting Lumenwall {v}…", "Redémarrage de Lumenwall {v}…";
    InProgress => "Updating to {v}…", "Mise à jour vers la {v}…";
    SeeBanner => "Progress is shown at the top of the window.", "La progression s'affiche en haut de la fenêtre.";
    Cancel => "Cancel", "Annuler";
    Retry => "Retry", "Réessayer";
    Close => "Close", "Fermer";
    Updated => "Lumenwall is now version {v}", "Lumenwall est passé à la version {v}";
    InstalledDev => "Development update installed into {dir} (nothing restarted)",
        "Mise à jour de développement installée dans {dir} (rien n'a redémarré)";
    OpenReleases => "Releases page", "Page des versions";
    JustNow => "just now", "à l'instant";
    Ago => "{n} ago", "il y a {n}";
    Minutes => "min", "min";
    Hours => "h", "h";
    Day => "day", "jour";
    Days => "days", "jours";

    ErrOffline => "GitHub can't be reached", "GitHub est injoignable";
    ErrOfflineBody => "Check your connection, then retry. Nothing was changed.",
        "Vérifiez votre connexion, puis réessayez. Rien n'a été modifié.";
    ErrRate => "GitHub asks to wait", "GitHub demande de patienter";
    ErrRateBody => "Too many requests from this network. Try again in {n} min.",
        "Trop de requêtes depuis ce réseau. Réessayez dans {n} min.";
    ErrServer => "GitHub answered with an error", "GitHub a répondu par une erreur";
    ErrServerBody => "Try again later. Nothing was changed.", "Réessayez plus tard. Rien n'a été modifié.";
    ErrSignature => "Update refused: invalid signature", "Mise à jour refusée : signature invalide";
    ErrSignatureBody => "The download is not signed with Lumenwall's release key. Nothing was installed. If this happens again, please report it.",
        "Le téléchargement n'est pas signé avec la clé de publication de Lumenwall. Rien n'a été installé. Si cela se reproduit, merci de le signaler.";
    ErrChecksum => "Update refused: damaged download", "Mise à jour refusée : téléchargement endommagé";
    ErrChecksumBody => "It does not match the published checksum. Nothing was installed; try again.",
        "Il ne correspond pas à la somme de contrôle publiée. Rien n'a été installé ; réessayez.";
    ErrArchive => "The update package is not valid", "Le paquet de mise à jour n'est pas valide";
    ErrArchiveBody => "Nothing was installed. Try again later.", "Rien n'a été installé. Réessayez plus tard.";
    ErrInstall => "The update could not be installed", "La mise à jour n'a pas pu être installée";
    ErrInstallBody => "{detail}. Your current version was kept.", "{detail}. Votre version actuelle est conservée.";
    ErrRestart => "Updated, but Lumenwall could not restart", "Mise à jour faite, mais Lumenwall n'a pas pu redémarrer";
    ErrRestartBody => "Quit and open Lumenwall again to use version {v}.",
        "Quittez puis rouvrez Lumenwall pour utiliser la version {v}.";

    ManagedTitle => "Updates come from {name}", "Les mises à jour viennent de {name}";
    ManagedBody => "This copy of Lumenwall was installed with {name}, which keeps it up to date.",
        "Cette copie de Lumenwall a été installée avec {name}, qui la tient à jour.";
    DistroTitle => "Installed from a system package", "Installé depuis un paquet système";
    DistroBody => "This copy comes from a system package, whose files Lumenwall does not replace. Update it with your package manager, or install the newer .deb from the releases page.",
        "Cette copie vient d'un paquet système, dont Lumenwall ne remplace pas les fichiers. Mettez-la à jour avec votre gestionnaire de paquets, ou installez le .deb plus récent depuis la page des versions.";
    ReadOnlyTitle => "Lumenwall can't update itself here", "Lumenwall ne peut pas se mettre à jour ici";
    ReadOnlyBody => "Its folder ({dir}) is read-only for your account. Download new versions from GitHub.",
        "Son dossier ({dir}) est en lecture seule pour votre compte. Téléchargez les nouvelles versions sur GitHub.";
    DevTitle => "Development build", "Version de développement";
    DevBody => "Updates are off for builds made from source.", "Les mises à jour sont désactivées pour les versions compilées depuis les sources.";
    OffTitle => "Updates are turned off", "Les mises à jour sont désactivées";
    OffBody => "The LUMENWALL_NO_UPDATE environment variable turns them off.",
        "La variable d'environnement LUMENWALL_NO_UPDATE les désactive.";
    UnsupportedTitle => "No updates for this system", "Pas de mises à jour pour ce système";
    UnsupportedBody => "Lumenwall publishes builds for Linux and Windows on x86-64.",
        "Lumenwall publie des versions pour Linux et Windows sur x86-64.";
    NoKeyTitle => "Updates are not available in this build", "Les mises à jour ne sont pas disponibles dans cette version";
    NoKeyBody => "It was built without Lumenwall's release key, so downloads could not be verified. Download new versions from GitHub.",
        "Elle a été compilée sans la clé de publication de Lumenwall : les téléchargements ne pourraient pas être vérifiés. Téléchargez les nouvelles versions sur GitHub.";
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }

    #[test]
    fn dates_and_durations() {
        assert_eq!(date("2026-11-03", Lang::En).as_deref(), Some("3 Nov 2026"));
        assert_eq!(
            date("2026-02-14", Lang::Fr).as_deref(),
            Some("14 févr. 2026")
        );
        assert_eq!(date("2026-13-01", Lang::En), None);
        assert_eq!(ago(10, Lang::En), "just now");
        assert_eq!(ago(300, Lang::En), "5 min ago");
        assert_eq!(ago(3 * 3600, Lang::Fr), "il y a 3 h");
        assert_eq!(ago(86400 + 5, Lang::En), "1 day ago");
        assert_eq!(ago(3 * 86400, Lang::Fr), "il y a 3 jours");
    }

    #[test]
    fn every_error_has_a_clear_message() {
        let v = Version::new(0, 2, 0);
        for e in [
            Error::Offline,
            Error::RateLimited { retry_at: 0 },
            Error::Status(500),
            Error::Signature("x".into()),
            Error::Checksum("x".into()),
            Error::TooLarge,
            Error::BadArchive("x".into()),
            Error::Io("disk full".into()),
            Error::Restart("x".into()),
        ] {
            for lang in [Lang::En, Lang::Fr] {
                let (title, body) = error_text(&e, lang, &v);
                assert!(!title.is_empty() && !body.is_empty());
                assert!(!body.contains('{'), "{e:?}: {body}");
            }
        }
        let (_, body) = error_text(&Error::Restart("x".into()), Lang::En, &v);
        assert!(body.contains("0.2.0"));
    }

    #[test]
    fn prefs_default_and_old_files() {
        assert_eq!(
            Prefs::default(),
            Prefs {
                auto_check: true,
                prereleases: false
            }
        );
        let p: Prefs = serde_json::from_str(r#"{"prereleases":true}"#).unwrap();
        assert!(p.auto_check && p.prereleases);
        assert_eq!(p.channel(), Channel::Prerelease);
    }

    #[test]
    fn manager_names_are_shown_as_is() {
        use lumenwall_update::Manager;
        assert_eq!(Manager::Flatpak.name(), Some("Flatpak"));
        assert_eq!(Manager::Distribution.name(), None);
    }
}

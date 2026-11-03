//! Workshop page state and update logic.
//!
//! Network work happens on a worker thread that exists only while the Workshop page
//! is displayed (its subscription is dropped when the user leaves): no request can
//! start from another page. Previews of results are downloaded only for cards on
//! screen; queued downloads are cancelled when the page is left.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant, SystemTime};

use iced::futures::channel::mpsc::unbounded;
use iced::futures::{SinkExt, StreamExt};
use iced::widget::{Id, operation, scrollable};
use iced::{Size, Task};

use super::api::{self, NetError, Page as ApiPage};
use super::watch::WatchEvent;
use super::{Item, Query, Sort};
use crate::app::{App, Message, grid_geom};
use crate::catalog::{self, Kind, Origin};
use crate::i18n::K;

pub const WS_GRID_ID: &str = "workshop-grid";
/// Installed items listed on the keyless page.
pub const INSTALLED_SHOWN: usize = 60;

#[derive(Default)]
pub struct PreviewTargets {
    pub remote: Vec<(PathBuf, String)>,
    pub local: Vec<PathBuf>,
    pub animate: Vec<PathBuf>,
}
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(450);
/// Quiet time before a new Workshop folder is considered complete.
const SETTLE: Duration = Duration::from_secs(2);

pub enum WCmd {
    Query {
        id: u64,
        query: Query,
        cursor: String,
        key: String,
    },
    Details {
        ids: Vec<String>,
        key: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Ready(Sender<WCmd>),
    Page {
        id: u64,
        page: Arc<ApiPage>,
        append: bool,
    },
    Failed {
        id: u64,
        err: NetError,
    },
    Installed(Arc<Vec<Item>>),
    InstalledFailed(NetError),
    Names(Arc<HashMap<String, String>>),
}

impl std::fmt::Debug for WCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key.
        f.write_str("WCmd")
    }
}

/// Text that must never be printed (the API key being typed).
#[derive(Clone)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone)]
pub enum WsMsg {
    Worker(WorkerEvent),
    Watch(WatchEvent),
    Sort(Sort),
    Kind(Option<Kind>),
    Tag(Option<String>),
    Resolution(Option<String>),
    Search(String),
    SearchSubmit,
    Scrolled(scrollable::Viewport),
    LoadMore,
    Retry,
    Select(String),
    Close,
    Shot(Option<usize>),
    Hover(String, bool),
    ToggleDescription,
    KeyInput(Secret),
    SaveKey,
    RemoveKey,
    ToggleKeyVisible,
    Questionable(bool),
    Mature(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Fetch {
    Idle,
    Loading,
    Done(Instant),
    Failed(NetError),
}

pub struct WorkshopState {
    pub query: Query,
    pub search_input: String,
    search_due: Option<Instant>,
    pub items: Vec<Item>,
    pub cursor: Option<String>,
    pub total: u64,
    pub loading: bool,
    pub error: Option<NetError>,
    request: u64,
    pub loaded: Option<(Query, Instant)>,
    pub names: HashMap<String, String>,
    pub installed: HashMap<String, Item>,
    pub installed_fetch: Fetch,
    pub selected: Option<String>,
    pub shot: Option<usize>,
    pub hovered: Option<String>,
    pub scroll_y: f32,
    pub viewport: Option<Size>,
    cmd: Option<Sender<WCmd>>,
    pub key: Option<String>,
    pub key_input: String,
    pub key_visible: bool,
    /// Library folders that appeared since the previous session or live.
    pub new_items: HashSet<PathBuf>,
    pending: HashMap<PathBuf, Instant>,
    pub desc_open: bool,
    pub prev_session: Option<u64>,
    /// Select the first result whose title contains this (screenshots).
    pub pending_select: Option<String>,
}

impl WorkshopState {
    pub fn new(cfg: &crate::config::UiConfig) -> Self {
        Self {
            query: Query {
                questionable: cfg.workshop_questionable,
                mature: cfg.workshop_mature,
                ..Query::default()
            },
            search_input: String::new(),
            search_due: None,
            items: Vec::new(),
            cursor: None,
            total: 0,
            loading: false,
            error: None,
            request: 0,
            loaded: None,
            names: HashMap::new(),
            installed: HashMap::new(),
            installed_fetch: Fetch::Idle,
            selected: None,
            shot: None,
            hovered: None,
            scroll_y: 0.0,
            viewport: None,
            cmd: None,
            key: super::key::load(),
            key_input: String::new(),
            key_visible: false,
            new_items: HashSet::new(),
            pending: HashMap::new(),
            desc_open: false,
            prev_session: cfg.last_session,
            pending_select: None,
        }
    }

    pub fn wants_timer(&self) -> bool {
        self.search_due.is_some() || !self.pending.is_empty()
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.items
            .iter()
            .find(|i| i.id == id)
            .or_else(|| self.installed.get(id))
    }
}

/// The network worker; its subscription exists only while the page is shown.
pub fn worker() -> iced::Subscription<WorkerEvent> {
    iced::Subscription::run(worker_stream)
}

fn worker_stream() -> impl iced::futures::Stream<Item = WorkerEvent> {
    iced::stream::channel(16, async move |mut output| {
        let (cmd_tx, cmd_rx) = mpsc::channel::<WCmd>();
        let (tx, mut rx) = unbounded();
        let spawned = std::thread::Builder::new()
            .name("lumenwall-ui-workshop".into())
            .spawn(move || {
                for cmd in cmd_rx {
                    let mut events = Vec::new();
                    match cmd {
                        WCmd::Query {
                            id,
                            query,
                            cursor,
                            key,
                        } => match api::query_files(&query, &cursor, &key) {
                            Ok(page) => {
                                let mut creators: Vec<String> =
                                    page.items.iter().map(|i| i.creator.clone()).collect();
                                creators.sort_unstable();
                                creators.dedup();
                                events.push(WorkerEvent::Page {
                                    id,
                                    page: Arc::new(page),
                                    append: cursor != "*",
                                });
                                if !creators.is_empty()
                                    && let Ok(n) = api::player_names(&creators, &key)
                                {
                                    events.push(WorkerEvent::Names(Arc::new(n)));
                                }
                            }
                            Err(err) => events.push(WorkerEvent::Failed { id, err }),
                        },
                        WCmd::Details { ids, key } => match api::file_details(&ids) {
                            Ok(items) => {
                                let mut creators: Vec<String> =
                                    items.iter().map(|i| i.creator.clone()).collect();
                                creators.sort_unstable();
                                creators.dedup();
                                events.push(WorkerEvent::Installed(Arc::new(items)));
                                if let Some(key) = key
                                    && !creators.is_empty()
                                    && let Ok(n) = api::player_names(&creators, &key)
                                {
                                    events.push(WorkerEvent::Names(Arc::new(n)));
                                }
                            }
                            Err(e) => events.push(WorkerEvent::InstalledFailed(e)),
                        },
                    }
                    for e in events {
                        if tx.unbounded_send(e).is_err() {
                            return;
                        }
                    }
                }
            });
        if spawned.is_err() {
            return;
        }
        let _ = output.send(WorkerEvent::Ready(cmd_tx)).await;
        while let Some(e) = rx.next().await {
            if output.send(e).await.is_err() {
                break;
            }
        }
    })
}

impl App {
    /// Workshop id → installed library folder.
    pub fn installed_ids(&self) -> HashMap<String, PathBuf> {
        let mut map = HashMap::new();
        if let Some(lib) = &self.library {
            for w in &lib.wallpapers {
                if w.origin != Origin::Workshop {
                    continue;
                }
                let id = w
                    .project
                    .workshop_id
                    .clone()
                    .or_else(|| w.dir.file_name().map(|n| n.to_string_lossy().into_owned()));
                if let Some(id) = id {
                    map.insert(id, w.dir.clone());
                }
            }
        }
        map
    }

    /// Library folder of an installed Workshop item (demo items carry ids too).
    pub fn installed_dir(&self, id: &str) -> Option<PathBuf> {
        self.library.as_ref()?.wallpapers.iter().find_map(|w| {
            let wid = w
                .project
                .workshop_id
                .as_deref()
                .or_else(|| w.dir.file_name().and_then(|n| n.to_str()));
            (wid == Some(id)).then(|| w.dir.clone())
        })
    }

    fn ws_send_query(&mut self, append: bool) {
        let Some(key) = self.ws.key.clone() else {
            return;
        };
        let Some(tx) = &self.ws.cmd else { return };
        let cursor = if append {
            match &self.ws.cursor {
                Some(c) => c.clone(),
                None => return,
            }
        } else {
            "*".to_owned()
        };
        self.ws.request += 1;
        self.ws.loading = true;
        self.ws.error = None;
        let _ = tx.send(WCmd::Query {
            id: self.ws.request,
            query: self.ws.query.clone(),
            cursor,
            key,
        });
    }

    /// Fetches what the page needs and is missing or stale (disk cache first).
    pub(crate) fn ws_refresh(&mut self) {
        let stale = match &self.ws.loaded {
            Some((q, at)) => *q != self.ws.query || at.elapsed() > api::TTL_QUERY,
            None => true,
        };
        if stale && !self.ws.loading && self.ws.key.is_some() {
            self.ws_send_query(false);
        }
        let installed_stale = match &self.ws.installed_fetch {
            Fetch::Idle | Fetch::Failed(_) => true,
            Fetch::Done(at) => at.elapsed() > api::TTL_DETAILS,
            Fetch::Loading => false,
        };
        if installed_stale && let Some(tx) = &self.ws.cmd {
            let mut ids: Vec<String> = self
                .installed_ids()
                .into_keys()
                .filter(|i| i.bytes().all(|b| b.is_ascii_digit()))
                .collect();
            ids.sort();
            if !ids.is_empty() {
                self.ws.installed_fetch = Fetch::Loading;
                let _ = tx.send(WCmd::Details {
                    ids,
                    key: self.ws.key.clone(),
                });
            }
        }
    }

    /// Leaving the page: nothing more is fetched.
    pub fn ws_leave(&mut self) {
        self.ws.cmd = None;
        self.ws.loading = false;
        self.ws.hovered = None;
        self.previews.cancel_remote();
    }

    fn ws_scroll_to_top(&mut self) -> Task<Message> {
        self.ws.scroll_y = 0.0;
        operation::snap_to(Id::new(WS_GRID_ID), scrollable::RelativeOffset::START)
    }

    fn ws_query_changed(&mut self) -> Task<Message> {
        self.ws.items.clear();
        self.ws.cursor = None;
        self.ws.loaded = None;
        self.ws_send_query(false);
        self.ws_scroll_to_top()
    }

    pub fn ws_update(&mut self, msg: WsMsg) -> Task<Message> {
        match msg {
            WsMsg::Worker(ev) => match ev {
                WorkerEvent::Ready(tx) => {
                    self.ws.cmd = Some(tx);
                    self.ws_refresh();
                }
                WorkerEvent::Page { id, page, append } => {
                    if id != self.ws.request {
                        return Task::none();
                    }
                    let page = Arc::unwrap_or_clone(page);
                    self.ws.loading = false;
                    self.ws.cursor = page.next_cursor;
                    self.ws.total = page.total;
                    if append {
                        let known: HashSet<String> =
                            self.ws.items.iter().map(|i| i.id.clone()).collect();
                        self.ws
                            .items
                            .extend(page.items.into_iter().filter(|i| !known.contains(&i.id)));
                    } else {
                        self.ws.items = page.items;
                    }
                    self.ws.loaded = Some((self.ws.query.clone(), Instant::now()));
                    if let Some(want) = self.ws.pending_select.take() {
                        let want = want.to_lowercase();
                        if let Some(id) = self
                            .ws
                            .items
                            .iter()
                            .find(|i| i.title.to_lowercase().contains(&want))
                            .map(|i| i.id.clone())
                        {
                            return self.ws_update(WsMsg::Select(id));
                        }
                    }
                }
                WorkerEvent::Failed { id, err } => {
                    if id == self.ws.request {
                        self.ws.loading = false;
                        self.ws.error = Some(err);
                    }
                }
                WorkerEvent::Installed(items) => {
                    self.ws.installed = items.iter().map(|i| (i.id.clone(), i.clone())).collect();
                    self.ws.installed_fetch = Fetch::Done(Instant::now());
                }
                WorkerEvent::InstalledFailed(e) => self.ws.installed_fetch = Fetch::Failed(e),
                WorkerEvent::Names(n) => self
                    .ws
                    .names
                    .extend(n.iter().map(|(k, v)| (k.clone(), v.clone()))),
            },
            WsMsg::Watch(ev) => self.ws_watch(ev),
            WsMsg::Sort(s) => {
                self.ws.query.sort = s;
                self.ws.query.search.clear();
                self.ws.search_input.clear();
                return self.ws_query_changed();
            }
            WsMsg::Kind(k) => {
                self.ws.query.kind = k;
                return self.ws_query_changed();
            }
            WsMsg::Tag(t) => {
                self.ws.query.tag = t;
                return self.ws_query_changed();
            }
            WsMsg::Resolution(r) => {
                self.ws.query.resolution = r;
                return self.ws_query_changed();
            }
            WsMsg::Search(s) => {
                self.ws.search_input = s;
                self.ws.search_due = Some(Instant::now() + SEARCH_DEBOUNCE);
            }
            WsMsg::SearchSubmit => {
                self.ws.search_due = None;
                if self.ws.query.search != self.ws.search_input.trim() {
                    self.ws.query.search = self.ws.search_input.trim().to_owned();
                    return self.ws_query_changed();
                }
            }
            WsMsg::Scrolled(v) => {
                self.ws.scroll_y = v.absolute_offset().y;
                self.ws.viewport = Some(v.bounds().size());
                let near_end = v.absolute_offset().y + v.bounds().height
                    > v.content_bounds().height - 2.0 * v.bounds().height.min(400.0);
                if near_end
                    && !self.ws.loading
                    && self.ws.cursor.is_some()
                    && self.ws.error.is_none()
                {
                    self.ws_send_query(true);
                }
            }
            WsMsg::LoadMore => self.ws_send_query(true),
            WsMsg::Retry => {
                self.ws.error = None;
                if self.ws.items.is_empty() {
                    self.ws_send_query(false);
                } else {
                    self.ws_send_query(true);
                }
                if matches!(self.ws.installed_fetch, Fetch::Failed(_)) {
                    self.ws.installed_fetch = Fetch::Idle;
                    self.ws_refresh();
                }
            }
            WsMsg::Select(id) => {
                if self.ws.selected.as_deref() != Some(id.as_str()) {
                    self.ws.shot = None;
                    self.ws.desc_open = false;
                }
                let was_open = self.ws.selected.is_some();
                self.ws.selected = Some(id);
                if !was_open {
                    self.panel = crate::theme::motion::panel_go(&self.panel, true, Instant::now());
                    self.ws.viewport = None;
                }
            }
            WsMsg::Close => {
                self.ws.selected = None;
                self.ws.viewport = None;
                self.panel = crate::theme::motion::panel_go(&self.panel, false, Instant::now());
            }
            WsMsg::Shot(i) => self.ws.shot = i,
            WsMsg::Hover(id, inside) => {
                if inside {
                    self.ws.hovered = Some(id);
                } else if self.ws.hovered.as_deref() == Some(id.as_str()) {
                    self.ws.hovered = None;
                }
            }
            WsMsg::ToggleDescription => self.ws.desc_open = !self.ws.desc_open,
            WsMsg::KeyInput(s) => self.ws.key_input = s.0,
            WsMsg::SaveKey => {
                let k = self.ws.key_input.trim().to_owned();
                match super::key::save(&k) {
                    Ok(()) => {
                        self.ws.key = Some(k);
                        self.ws.key_input.clear();
                        self.ws.loaded = None;
                        let msg = self.t(K::KeySaved);
                        self.toast(msg, false);
                    }
                    Err(_) => {
                        let msg = self.t(K::KeyInvalid);
                        self.toast(msg, true);
                    }
                }
            }
            WsMsg::RemoveKey => {
                if super::key::remove().is_ok() {
                    self.ws.key = None;
                    self.ws.items.clear();
                    self.ws.loaded = None;
                }
            }
            WsMsg::ToggleKeyVisible => self.ws.key_visible = !self.ws.key_visible,
            WsMsg::Questionable(on) => {
                self.cfg.workshop_questionable = on;
                self.ws.query.questionable = on;
                self.save_cfg();
                self.ws.loaded = None;
                self.ws.items.clear();
            }
            WsMsg::Mature(on) => {
                self.cfg.workshop_mature = on;
                self.ws.query.mature = on;
                self.save_cfg();
                self.ws.loaded = None;
                self.ws.items.clear();
            }
        }
        Task::none()
    }

    fn ws_watch(&mut self, ev: WatchEvent) {
        match ev {
            WatchEvent::Changed(dir) => {
                tracing::debug!(dir = %dir.display(), "Workshop folder event");
                let is_item = dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
                if is_item {
                    self.ws.pending.insert(dir, Instant::now());
                }
            }
            WatchEvent::Removed(dir) => {
                if let Some(lib) = &mut self.library {
                    let before = lib.wallpapers.len();
                    lib.wallpapers.retain(|w| w.dir != dir);
                    if lib.wallpapers.len() != before {
                        self.bump_library();
                    }
                }
                self.ws.pending.remove(&dir);
                self.ws.new_items.remove(&dir);
            }
        }
    }

    /// Called on the housekeeping timer: debounced search and new item checks.
    pub fn ws_housekeeping(&mut self, now: Instant) -> Task<Message> {
        let mut task = Task::none();
        if let Some(due) = self.ws.search_due
            && now >= due
        {
            self.ws.search_due = None;
            let s = self.ws.search_input.trim().to_owned();
            if s != self.ws.query.search {
                self.ws.query.search = s;
                task = self.ws_query_changed();
            }
        }
        let ready: Vec<PathBuf> = self
            .ws
            .pending
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= SETTLE)
            .map(|(d, _)| d.clone())
            .collect();
        for dir in ready {
            // Late file events on a folder already added (or a Steam update)
            // reload it without announcing it again.
            let known = self
                .library
                .as_ref()
                .is_some_and(|l| l.wallpapers.iter().any(|w| w.dir == dir));
            if let Some(added) = self.ws_try_add(&dir) {
                self.ws.pending.remove(&dir);
                if !known {
                    let msg = format!("{} · {added}", self.t(K::NewWallpaper));
                    self.toast(msg, false);
                }
            } else if self
                .ws
                .pending
                .get(&dir)
                .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(600))
            {
                self.ws.pending.remove(&dir);
            }
        }
        task
    }

    /// Adds a complete new Workshop folder to the library; its title if added.
    fn ws_try_add(&mut self, dir: &Path) -> Option<String> {
        if !dir.join("project.json").is_file() {
            return None;
        }
        let w = catalog::load(dir, Origin::Workshop).ok()?;
        let title = w.title().to_owned();
        let dir = w.dir.clone();
        tracing::info!(dir = %dir.display(), "new Workshop item in the library");
        let lib = self.library.as_mut()?;
        lib.wallpapers.retain(|x| x.dir != dir);
        lib.wallpapers.push(w);
        self.ws.new_items.insert(dir);
        self.bump_library();
        Some(title)
    }

    /// Marks scanned Workshop folders newer than the previous session as new.
    pub fn ws_mark_new_since_last_session(&mut self) {
        let Some(prev) = self.ws.prev_session else {
            return;
        };
        let Some(lib) = &self.library else { return };
        let since = SystemTime::UNIX_EPOCH + Duration::from_secs(prev);
        for w in &lib.wallpapers {
            if w.origin == Origin::Workshop && w.added > since {
                self.ws.new_items.insert(w.dir.clone());
            }
        }
    }

    /// Preview file of a Workshop item: the installed copy's own preview when it
    /// is installed (no download), else the cached remote image.
    pub fn ws_preview_file(&self, it: &Item) -> Option<(PathBuf, Option<String>)> {
        if let Some(p) = self
            .installed_dir(&it.id)
            .and_then(|d| self.wallpaper(&d).and_then(|w| w.preview.clone()))
        {
            return Some((p, None));
        }
        let url = it.preview_url.clone()?;
        Some((api::image_path(&url)?, Some(url)))
    }

    /// Previews the page shows now: remote `(file, url)` to download, local files,
    /// and the ones to animate (hovered card, open panel).
    pub fn ws_preview_targets(&self) -> PreviewTargets {
        use crate::theme::layout;
        let mut out = PreviewTargets::default();
        let add = |out: &mut PreviewTargets, it: &Item| match self.ws_preview_file(it) {
            Some((p, Some(url))) => out.remote.push((p, url)),
            Some((p, None)) => out.local.push(p),
            None => {}
        };
        if self.ws.key.is_some() {
            let panel = if self.ws.selected.is_some() {
                layout::PANEL_W
            } else {
                0.0
            };
            let width = self.ws.viewport.map_or(
                (self.window.width - layout::SIDEBAR_W - panel - 2.0 * layout::PAGE_PAD).max(200.0),
                |v| v.width,
            );
            // Before the first scroll event, the grid sits under ~330 px of header.
            let height = self
                .ws
                .viewport
                .map_or((self.window.height - 330.0).max(200.0), |v| v.height);
            let g = grid_geom(width - 12.0);
            let first = (self.ws.scroll_y / g.row_h).floor().max(0.0) as usize * g.cols;
            let last = (((self.ws.scroll_y + height) / g.row_h).ceil() as usize + 1) * g.cols;
            for it in self
                .ws
                .items
                .iter()
                .skip(first)
                .take(last.saturating_sub(first))
            {
                add(&mut out, it);
            }
        } else {
            for (id, dir) in self.ws_installed_list() {
                if let Some(p) = self.wallpaper(&dir).and_then(|w| w.preview.clone()) {
                    out.local.push(p);
                }
                let _ = id;
            }
        }
        if let Some(lib) = &self.library {
            out.local.extend(
                lib.wallpapers
                    .iter()
                    .filter(|w| self.ws.new_items.contains(&w.dir))
                    .filter_map(|w| w.preview.clone()),
            );
        }
        for id in [self.ws.hovered.as_deref(), self.ws.selected.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some((p, _)) = self.ws.item(id).and_then(|it| self.ws_preview_file(it)) {
                out.animate.push(p);
            }
        }
        // The open panel: its preview and screenshots.
        if let Some(it) = self.ws.selected.as_deref().and_then(|id| self.ws.item(id)) {
            add(&mut out, it);
            for url in &it.screenshots {
                if let Some(p) = api::image_path(url) {
                    out.remote.push((p, url.clone()));
                }
            }
        }
        out
    }

    /// Installed Workshop items shown without a key (most recent first, capped:
    /// the library lists them all).
    pub fn ws_installed_list(&self) -> Vec<(String, PathBuf)> {
        let mut list: Vec<(String, PathBuf)> = self.installed_ids().into_iter().collect();
        let added = |d: &Path| self.wallpaper(d).map(|w| w.added);
        list.sort_by(|a, b| added(&b.1).cmp(&added(&a.1)).then(a.1.cmp(&b.1)));
        list.truncate(INSTALLED_SHOWN);
        list
    }
}

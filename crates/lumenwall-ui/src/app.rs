//! Application state, messages and update logic (Elm architecture). Views are in
//! `view/`. Every subscription here is conditional: nothing ticks while nothing moves.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::{Id, operation, scrollable};
use iced::{
    Animation, Event, Point, Size, Subscription, Task, Theme, event, keyboard, mouse, time, window,
};
use lumenwall_ipc::{Endpoint, PropertyMap, PropertyValue, Scaling, Settings};

use crate::catalog::{self, Discovery, Kind, Library, ResClass, ScanEvent, Wallpaper};
use crate::config::{LangPref, PreviewMode, SortKey, ThemePref, UiConfig};
use crate::daemon::{DaemonEvent, DaemonLink, Link};
use crate::i18n::{K, Lang};
use crate::previews::{Previews, ThumbEvent};
use crate::theme::{layout, motion};

/// Delay after the last change before previewed properties are saved.
const SAVE_DELAY: Duration = Duration::from_millis(700);
const TOAST_TIME: Duration = Duration::from_secs(5);
/// Movement before a press on a card becomes a drag.
const DRAG_THRESHOLD: f32 = 6.0;

pub const SEARCH_ID: &str = "search";
pub const GRID_ID: &str = "grid";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Library,
    Workshop,
    Screens,
    Settings,
}

/// Options from the command line.
#[derive(Default)]
pub struct Launch {
    pub page: Option<Page>,
    pub select: Option<String>,
    /// Pre-filled search (screenshots, tests).
    pub search: Option<String>,
    pub screenshot: Option<PathBuf>,
    pub lang: Option<LangPref>,
    /// Benchmark: animate visible previews even without window focus.
    pub bench_animate: bool,
    /// `--setup`: open the installation dialog even if everything is in place.
    pub setup: bool,
    /// Minimum wait before the screenshot (to let a scripted change land).
    pub screenshot_delay: Option<Duration>,
    /// `Some(None)`: behave as if no Steam Web API key was saved; `Some(Some(k))`:
    /// use `k` for this run only (fake Steam server). `None`: the saved key.
    pub workshop_key: Option<Option<String>>,
    /// Open the first Workshop result whose title contains this.
    pub ws_select: Option<String>,
    pub discovery: Option<Arc<dyn Discovery>>,
    pub endpoint: Option<Endpoint>,
    pub keep_alive: Option<Box<dyn std::any::Any + Send>>,
}

/// Inputs of the filtered list; recomputed only when one changes.
type FilterKey = (
    String,
    Option<Kind>,
    Option<String>,
    Option<ResClass>,
    SortKey,
    u64,
);

pub struct Toast {
    pub text: String,
    pub error: bool,
    /// When it starts leaving, then when it is removed.
    pub until: Instant,
    /// Fades and slides in, then out (`motion::SLOW`).
    pub shown: Animation<bool>,
    pub leaving: bool,
}

#[derive(Debug, Clone)]
pub struct Drag {
    pub path: PathBuf,
    /// First pointer position seen after the press.
    pub origin: Option<Point>,
    pub pos: Point,
    pub active: bool,
}

pub struct App {
    pub cfg: UiConfig,
    pub lang: Lang,
    pub page: Page,
    pub library: Option<Library>,
    library_gen: u64,
    discovery: Arc<dyn Discovery>,
    pub search: String,
    pub filtered: Vec<usize>,
    filter_key: Option<FilterKey>,
    pub selected: Option<PathBuf>,
    pub hovered: Option<PathBuf>,
    pub hover_in: Animation<bool>,
    pub hover_prev: Option<PathBuf>,
    pub hover_out: Animation<bool>,
    pub panel: Animation<bool>,
    pub previews: Previews,
    anim_tick: Option<Duration>,
    pub daemon: DaemonLink,
    pub(crate) endpoint: Endpoint,
    pub targets: BTreeSet<String>,
    pub edits: HashMap<PathBuf, PropertyMap>,
    save_at: Option<(Instant, PathBuf)>,
    fetched: HashMap<String, PathBuf>,
    pub toasts: Vec<Toast>,
    pub folder_input: String,
    pub autostart: bool,
    pub color_open: Option<String>,
    pub desc_open: bool,
    pub scroll_y: f32,
    pub viewport: Option<Size>,
    pub window: Size,
    pub focused: bool,
    pub system_dark: bool,
    pub drag: Option<Drag>,
    pub drop_target: Option<String>,
    pub volume_drag: Option<f32>,
    /// A wallpaper's sound level being dragged in the details panel.
    pub wallpaper_volume_drag: Option<(PathBuf, f32)>,
    /// Set by the root view when a frame is actually drawn (see `view::drawn`).
    pub drawn: std::cell::Cell<bool>,
    /// Animation ticks in a row that found no new frame drawn (window hidden).
    idle_ticks: u32,
    /// The app's own memory, sampled at most every 2 s (shown in the status bar).
    rss: std::cell::Cell<(Option<u64>, Option<Instant>)>,
    screenshot: Option<(PathBuf, Instant, bool)>,
    bench_animate: bool,
    screenshot_delay: Duration,
    select_title: Option<String>,
    _keep_alive: Option<Box<dyn std::any::Any + Send>>,
    pub ws: crate::workshop::state::WorkshopState,
    pub chrome: crate::chrome::Chrome,
    pub updater: crate::updater::Updater,
    /// Installation on this computer (`setup/`).
    pub setup: crate::setup::SetupState,
}

#[derive(Debug, Clone)]
pub enum Message {
    Ws(crate::workshop::state::WsMsg),
    /// Update checks, download, installation (`updater.rs`).
    Updater(crate::updater::Msg),
    /// First-run dialog and the Installation settings (`setup/`).
    Setup(crate::setup::Msg),
    Scan(Arc<ScanEvent>),
    Thumb(ThumbEvent),
    Daemon(DaemonEvent),
    /// Preview animation tick.
    Tick,
    /// Micro-animation frame (hover, panel).
    Frame,
    /// Toast expiry, debounced saves, screenshot mode.
    Housekeeping(Instant),
    Key(keyboard::Key, keyboard::Modifiers),
    Window(window::Id, window::Event),
    /// Native handle of the window, once it exists (Windows: icon from the resources).
    NativeWindow(u64),
    /// The title bar the app draws on Linux.
    Chrome(crate::chrome::Msg),
    SystemTheme(iced::theme::Mode),
    /// The system's "reduce animations" setting, read at start and on focus.
    ReduceMotion(bool),
    CursorMoved(Point),
    MouseReleased,

    Nav(Page),
    Search(String),
    SearchSubmit,
    FilterKind(Option<Kind>),
    FilterTag(Option<String>),
    FilterRes(Option<ResClass>),
    Sort(SortKey),
    ClearFilters,
    Scrolled(scrollable::Viewport),
    CardPress(PathBuf),
    CardDouble(PathBuf),
    Hover(PathBuf, bool),
    ClosePanel,
    Apply,
    /// Apply a library wallpaper by folder (from the Workshop page).
    ApplyDir(PathBuf),
    ShowInLibrary(PathBuf),
    TargetToggle(String),
    TargetOnly(String),
    AllScreens,
    SetScaling(Scaling),
    DropHover(Option<String>),
    Prop(String, PropertyValue),
    PickProp(String, bool),
    PropPicked(PathBuf, String, Option<PathBuf>),
    ResetProps,
    ColorEditor(Option<String>),
    ToggleDescription,

    Pause,
    Mute,
    Volume(f32),
    VolumeRelease,
    /// Sound level of one wallpaper (details panel), 0..1.
    WallpaperVolume(PathBuf, f32),
    WallpaperVolumeRelease,
    WallpaperMute(PathBuf),
    StartEngine,
    EngineSettings(Settings),
    Autostart(bool),
    FolderInput(String),
    AddTypedFolder,
    PickFolder,
    FolderPicked(Option<PathBuf>),
    RemoveFolder(usize),
    Rescan,
    Lang(LangPref),
    ThemePref(ThemePref),
    Preview(PreviewMode),
    Open(String),
    ClearScreen(String),
    Shot(window::Screenshot),
    Noop,
}

pub struct GridGeom {
    pub cols: usize,
    pub card_w: f32,
    pub card_h: f32,
    pub row_h: f32,
}

pub fn grid_geom(width: f32) -> GridGeom {
    let g = layout::CARD_GAP;
    let cols = (((width + g) / (layout::CARD_MIN_W + g)).floor() as usize).max(1);
    let card_w = ((width - g * (cols as f32 - 1.0)) / cols as f32)
        .floor()
        .max(120.0);
    let card_h = (card_w * 9.0 / 16.0).round();
    GridGeom {
        cols,
        card_w,
        card_h,
        row_h: card_h + g,
    }
}

impl App {
    pub fn new(mut launch: Launch) -> (Self, Task<Message>) {
        let cfg = UiConfig::load();
        let lang = Lang::resolve(launch.lang.unwrap_or(cfg.language));
        let window = cfg
            .window
            .map_or(Size::new(1360.0, 860.0), |[w, h]| Size::new(w, h));
        // The installation dialog opens by itself only for a real session.
        let setup_auto =
            launch.screenshot.is_none() && launch.discovery.is_none() && launch.endpoint.is_none();
        let mut app = Self {
            lang,
            page: launch.page.unwrap_or(Page::Library),
            library: None,
            library_gen: 0,
            discovery: launch
                .discovery
                .take()
                .unwrap_or_else(|| Arc::new(catalog::NativeDiscovery)),
            search: launch.search.take().unwrap_or_default(),
            filtered: Vec::new(),
            filter_key: None,
            selected: None,
            hovered: None,
            hover_in: Animation::new(false),
            hover_prev: None,
            hover_out: Animation::new(false),
            panel: motion::panel_at(false),
            previews: Previews::default(),
            anim_tick: None,
            daemon: DaemonLink::new(),
            endpoint: launch
                .endpoint
                .take()
                .unwrap_or_else(Endpoint::for_current_user),
            targets: cfg.targets.iter().cloned().collect(),
            edits: HashMap::new(),
            save_at: None,
            fetched: HashMap::new(),
            toasts: Vec::new(),
            folder_input: String::new(),
            autostart: crate::config::autostart::is_enabled(),
            color_open: None,
            desc_open: false,
            scroll_y: 0.0,
            viewport: None,
            window,
            focused: true,
            system_dark: true,
            drag: None,
            drop_target: None,
            volume_drag: None,
            wallpaper_volume_drag: None,
            rss: std::cell::Cell::new((None, None)),
            drawn: std::cell::Cell::new(true),
            idle_ticks: 0,
            screenshot: launch.screenshot.take().map(|p| (p, Instant::now(), false)),
            select_title: launch.select.take(),
            bench_animate: launch.bench_animate,
            screenshot_delay: launch
                .screenshot_delay
                .unwrap_or(Duration::from_millis(1800)),
            _keep_alive: launch.keep_alive.take(),
            ws: crate::workshop::state::WorkshopState::new(&cfg),
            chrome: crate::chrome::Chrome::default(),
            updater: crate::updater::Updater::new(),
            setup: crate::setup::SetupState::new(launch.setup, setup_auto),
            cfg,
        };
        // Engine stats are wanted while the window exists; the daemon samples
        // them only while at least one client asks.
        app.daemon.want_stats(true);
        if let Some(k) = launch.workshop_key.take() {
            app.ws.key = k;
        }
        app.ws.pending_select = launch.ws_select.take();
        let scan = app.rescan();
        let theme = iced::system::theme().map(Message::SystemTheme);
        let motion = Task::perform(
            crate::platform::reduce_motion_async(),
            Message::ReduceMotion,
        );
        let updates = app.updater_start();
        let setup = app.setup_start();
        (app, Task::batch([scan, theme, motion, updates, setup]))
    }

    pub fn own_rss(&self) -> Option<u64> {
        let (value, at) = self.rss.get();
        if at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return value;
        }
        let v = crate::platform::own_rss();
        self.rss.set((v, Some(Instant::now())));
        v
    }

    pub fn t(&self, k: K) -> &'static str {
        k.get(self.lang)
    }

    pub fn title(&self) -> String {
        "Lumenwall".to_owned()
    }

    pub fn theme(&self) -> Theme {
        let dark = match self.cfg.theme {
            ThemePref::System => self.system_dark,
            ThemePref::Dark => true,
            ThemePref::Light => false,
        };
        crate::theme::theme(dark)
    }

    /// What the window is cleared to: nothing where the window is see-through, so
    /// the corners the app's frame rounds are clear (`chrome.rs`).
    pub fn style(&self, theme: &Theme) -> iced::theme::Style {
        use iced::theme::Base;
        let mut style = theme.base();
        if crate::chrome::transparent() {
            style.background_color = iced::Color::TRANSPARENT;
        }
        style
    }

    fn rescan(&mut self) -> Task<Message> {
        let (tx, rx) = iced::futures::channel::mpsc::unbounded();
        catalog::scan_in_background(
            Arc::clone(&self.discovery),
            self.cfg.folders.clone(),
            move |ev| tx.unbounded_send(ev).is_ok(),
        );
        Task::run(rx, |ev| Message::Scan(Arc::new(ev)))
    }

    pub fn has_we_assets(&self) -> bool {
        self.library
            .as_ref()
            .is_some_and(|l| l.steam.we_assets().is_some())
    }

    pub fn wallpaper(&self, dir: &Path) -> Option<&Wallpaper> {
        self.library
            .as_ref()?
            .wallpapers
            .iter()
            .find(|w| w.dir == dir)
    }

    pub fn selected_wallpaper(&self) -> Option<&Wallpaper> {
        self.selected.as_deref().and_then(|d| self.wallpaper(d))
    }

    /// Screens currently showing the wallpaper in `dir`.
    pub fn outputs_showing(&self, dir: &Path) -> Vec<String> {
        self.daemon
            .state
            .as_ref()
            .map(|s| {
                s.outputs
                    .iter()
                    .filter(|o| o.wallpaper.as_deref() == Some(dir))
                    .map(|o| o.output.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 1-based numbers (as on the screen map) of the screens showing `dir`.
    pub fn screen_numbers(&self, dir: &Path) -> Vec<usize> {
        let showing = self.outputs_showing(dir);
        self.daemon
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| showing.contains(&o.id))
            .map(|(i, _)| i + 1)
            .collect()
    }

    pub fn wallpaper_on(&self, output: &str) -> Option<&Wallpaper> {
        let dir = self
            .daemon
            .state
            .as_ref()?
            .outputs
            .iter()
            .find(|o| o.output == output)?
            .wallpaper
            .clone()?;
        self.wallpaper(&dir)
    }

    pub fn filters_active(&self) -> bool {
        !self.search.is_empty()
            || self.cfg.filter_kind.is_some()
            || self.cfg.filter_tag.is_some()
            || self.cfg.filter_res.is_some()
    }

    fn refresh_filter(&mut self) {
        let key = (
            self.search.clone(),
            self.cfg.filter_kind,
            self.cfg.filter_tag.clone(),
            self.cfg.filter_res,
            self.cfg.sort,
            self.library_gen,
        );
        if self.filter_key.as_ref() == Some(&key) {
            return;
        }
        let Some(lib) = &self.library else { return };
        let query = self.search.to_lowercase();
        let terms: Vec<&str> = query.split_whitespace().collect();
        let mut out: Vec<usize> = lib
            .wallpapers
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                terms.iter().all(|t| w.search_text.contains(t))
                    && self.cfg.filter_kind.is_none_or(|k| w.project.kind == k)
                    && self
                        .cfg
                        .filter_tag
                        .as_ref()
                        .is_none_or(|t| w.project.tags.iter().any(|x| x == t))
                    && self
                        .cfg
                        .filter_res
                        .is_none_or(|r| ResClass::of(w.resolution) == r)
            })
            .map(|(i, _)| i)
            .collect();
        let ws = &lib.wallpapers;
        match self.cfg.sort {
            SortKey::Name => out.sort_by_cached_key(|&i| ws[i].title().to_lowercase()),
            SortKey::Recent => out.sort_by(|&a, &b| ws[b].added.cmp(&ws[a].added)),
            SortKey::Kind => {
                out.sort_by_cached_key(|&i| (ws[i].project.kind, ws[i].title().to_lowercase()))
            }
        }
        self.filtered = out;
        self.filter_key = Some(key);
    }

    /// Width of the library grid (exact once the user scrolled, estimated before).
    pub fn grid_width(&self) -> f32 {
        if let Some(v) = self.viewport {
            return v.width;
        }
        let panel = if self.selected.is_some() {
            layout::PANEL_W
        } else {
            0.0
        };
        (self.window.width - layout::SIDEBAR_W - panel - 2.0 * layout::PAGE_PAD).max(200.0)
    }

    fn grid_height(&self) -> f32 {
        self.viewport.map_or(self.window.height, |v| v.height)
    }

    /// Indices into `filtered` of the cards currently on screen (plus one row ahead).
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        let g = grid_geom(self.grid_width());
        let first = (self.scroll_y / g.row_h).floor().max(0.0) as usize;
        let last = ((self.scroll_y + self.grid_height()) / g.row_h).ceil() as usize + 1;
        let n = self.filtered.len();
        (first * g.cols).min(n)..(last * g.cols).min(n)
    }

    /// Requests on-screen previews and advances the animated ones.
    fn sync_previews(&mut self) {
        let mut seen: Vec<PathBuf> = Vec::new();
        if let Some(lib) = &self.library {
            if self.page == Page::Library {
                for i in self.visible_range() {
                    if let Some(p) = lib.wallpapers[self.filtered[i]].preview.clone() {
                        seen.push(p);
                    }
                }
            }
            let mut extra: Vec<PathBuf> = Vec::new();
            if let Some(p) = self.selected_wallpaper().and_then(|w| w.preview.clone()) {
                extra.push(p);
            }
            for o in &self.daemon.outputs {
                if let Some(p) = self.wallpaper_on(&o.id).and_then(|w| w.preview.clone()) {
                    extra.push(p);
                }
            }
            seen.extend(extra);
        }
        self.previews.touch(seen.iter().map(PathBuf::as_path));
        let targets = if self.page == Page::Workshop {
            self.ws_preview_targets()
        } else {
            crate::workshop::state::PreviewTargets::default()
        };
        self.previews.touch_remote(
            targets
                .remote
                .iter()
                .map(|(p, u)| (p.as_path(), u.as_str())),
        );
        self.previews
            .touch(targets.local.iter().map(PathBuf::as_path));
        let remote_anim = targets.animate;
        if self.cfg.preview_mode == PreviewMode::Visible {
            seen.extend(targets.remote.into_iter().map(|(p, _)| p));
            seen.extend(targets.local);
        }
        let animating: Vec<PathBuf> = match self.cfg.preview_mode {
            PreviewMode::Never => Vec::new(),
            PreviewMode::Hover => {
                let mut v: Vec<PathBuf> = remote_anim;
                for dir in [self.hovered.as_ref(), self.selected.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    if let Some(p) = self.wallpaper(dir).and_then(|w| w.preview.clone()) {
                        v.push(p);
                    }
                }
                v
            }
            // Only while the window has focus: nobody watches a background grid.
            PreviewMode::Visible if self.focused || self.bench_animate => seen,
            PreviewMode::Visible => Vec::new(),
        };
        let (tick, swapped) = self.previews.advance(
            animating.iter().map(PathBuf::as_path),
            Instant::now(),
            self.drawn.get(),
        );
        if swapped {
            self.drawn.set(false);
        }
        self.anim_tick = tick.filter(|_| self.idle_ticks < 8);
        self.previews.evict();
    }

    pub(crate) fn bump_library(&mut self) {
        self.library_gen += 1;
    }

    pub(crate) fn toast(&mut self, text: impl Into<String>, error: bool) {
        let now = Instant::now();
        self.toasts.push(Toast {
            text: text.into(),
            error,
            until: now + TOAST_TIME,
            shown: motion::toast(now),
            leaving: false,
        });
    }

    /// Toasts past their time start leaving; those that have left are removed.
    fn expire_toasts(&mut self, now: Instant) {
        self.toasts.retain(|t| !(t.leaving && now >= t.until));
        for t in &mut self.toasts {
            if !t.leaving && now >= t.until {
                t.leaving = true;
                t.until = now + motion::duration(motion::SLOW);
                t.shown = t
                    .shown
                    .clone()
                    .duration(motion::duration(motion::SLOW))
                    .go(false, now);
            }
        }
    }

    pub(crate) fn save_cfg(&mut self) {
        self.cfg.targets = self.targets.iter().cloned().collect();
        self.cfg.save();
    }

    fn select(&mut self, dir: Option<PathBuf>) {
        let was_open = self.selected.is_some();
        if dir != self.selected {
            self.color_open = None;
            self.desc_open = false;
        }
        self.selected = dir;
        if self.selected.is_some() != was_open {
            self.panel = motion::panel_go(&self.panel, self.selected.is_some(), Instant::now());
            // The grid width changes with the panel; recompute from the window.
            self.viewport = None;
        }
    }

    fn ensure_visible(&self, index: usize) -> Task<Message> {
        let g = grid_geom(self.grid_width());
        let y = (index / g.cols) as f32 * g.row_h;
        let h = self.grid_height();
        let target = if y < self.scroll_y {
            y
        } else if y + g.card_h > self.scroll_y + h {
            y + g.card_h - h + layout::CARD_GAP
        } else {
            return Task::none();
        };
        operation::scroll_to(
            Id::new(GRID_ID),
            scrollable::AbsoluteOffset {
                x: 0.0,
                y: target.max(0.0),
            },
        )
    }

    pub fn apply_targets(&self) -> Vec<Option<String>> {
        let all = self.targets.is_empty() || self.targets.len() == self.daemon.outputs.len();
        if all || self.daemon.outputs.is_empty() {
            vec![None]
        } else {
            self.targets.iter().cloned().map(Some).collect()
        }
    }

    pub fn can_apply(&self, w: &Wallpaper) -> bool {
        self.daemon.is_online() && w.blocker(self.has_we_assets()).is_none()
    }

    fn apply(&mut self, dir: &Path, outputs: Vec<Option<String>>) {
        let Some(w) = self.wallpaper(dir) else { return };
        if !self.can_apply(w) {
            let msg = if self.daemon.is_online() {
                self.t(K::Unsupported)
            } else {
                self.t(K::NeedEngine)
            };
            self.toast(msg, true);
            return;
        }
        let title = w.title().to_owned();
        for out in &outputs {
            self.daemon
                .apply(out.clone(), dir.to_owned(), self.cfg.scaling);
        }
        if let Some(values) = self.edits.get(dir).cloned() {
            let ids: Vec<String> = match outputs.as_slice() {
                [None] => self.daemon.outputs.iter().map(|o| o.id.clone()).collect(),
                _ => outputs.into_iter().flatten().collect(),
            };
            for id in ids {
                self.daemon.save_properties(id, values.clone());
            }
        }
        self.toast(format!("{} · {title}", self.t(K::Applied)), false);
    }

    fn change_properties(&mut self, dir: &Path, changed: PropertyMap) {
        self.edits
            .entry(dir.to_owned())
            .or_default()
            .extend(changed.clone());
        let showing = self.outputs_showing(dir);
        for out in &showing {
            self.daemon.preview_properties(out.clone(), changed.clone());
        }
        if !showing.is_empty() {
            self.save_at = Some((Instant::now() + SAVE_DELAY, dir.to_owned()));
        }
    }

    fn move_selection(&mut self, delta: isize) -> Task<Message> {
        if self.filtered.is_empty() {
            return Task::none();
        }
        let lib = self.library.as_ref().expect("filtered implies library");
        let current = self.selected.as_ref().and_then(|d| {
            self.filtered
                .iter()
                .position(|&i| &lib.wallpapers[i].dir == d)
        });
        let next = match current {
            Some(i) => (i as isize + delta).clamp(0, self.filtered.len() as isize - 1) as usize,
            None => 0,
        };
        let dir = lib.wallpapers[self.filtered[next]].dir.clone();
        self.select(Some(dir));
        self.ensure_visible(next)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if matches!(message, Message::Tick) {
            // Nothing drawn since the last frame swap: the window is hidden or
            // occluded. Stop ticking after a few tries; any other event resumes.
            self.idle_ticks = if self.drawn.get() {
                0
            } else {
                self.idle_ticks + 1
            };
        } else {
            self.idle_ticks = 0;
        }
        let task = self.update_inner(message);
        self.refresh_filter();
        self.sync_previews();
        task
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Scan(ev) => match Arc::unwrap_or_clone(ev) {
                ScanEvent::Done(lib) => {
                    tracing::info!(
                        wallpapers = lib.wallpapers.len(),
                        steam = lib.steam.roots.len(),
                        "library scanned"
                    );
                    if let Some(want) = self.select_title.take() {
                        let want = want.to_lowercase();
                        let dir = lib
                            .wallpapers
                            .iter()
                            .find(|w| w.title().to_lowercase().contains(&want))
                            .map(|w| w.dir.clone());
                        self.library = Some(lib);
                        self.select(dir);
                    } else {
                        if self
                            .selected
                            .as_ref()
                            .is_some_and(|d| !lib.wallpapers.iter().any(|w| &w.dir == d))
                        {
                            self.select(None);
                        }
                        self.library = Some(lib);
                    }
                    self.library_gen += 1;
                    self.ws_mark_new_since_last_session();
                    if self.page == Page::Workshop {
                        self.ws_refresh();
                    }
                }
                ScanEvent::Meta(found) => {
                    if let Some(lib) = &mut self.library {
                        let map: HashMap<_, _> = found.into_iter().collect();
                        for w in &mut lib.wallpapers {
                            if let Some(m) = map.get(&w.dir) {
                                w.resolution = w.resolution.or(m.resolution);
                                w.sound = Some(m.sound);
                            }
                        }
                        self.library_gen += 1;
                    }
                }
            },
            Message::Thumb(ev) => self.previews.handle_event(ev),
            Message::Daemon(ev) => {
                self.daemon.handle(ev);
                for e in std::mem::take(&mut self.daemon.errors) {
                    let msg = format!("{}: {e}", self.t(K::Error));
                    self.toast(msg, true);
                }
                let ids: Vec<String> = self.daemon.outputs.iter().map(|o| o.id.clone()).collect();
                if !ids.is_empty() {
                    self.targets.retain(|t| ids.contains(t));
                }
                // Fetch the properties of wallpapers that changed on a screen.
                if let Some(state) = self.daemon.state.clone() {
                    for o in &state.outputs {
                        if let Some(w) = &o.wallpaper
                            && self.fetched.get(&o.output) != Some(w)
                        {
                            self.fetched.insert(o.output.clone(), w.clone());
                            self.daemon.fetch_properties(o.output.clone());
                        }
                    }
                }
            }
            Message::Tick => {}
            // Only there to keep frames coming while a micro-animation runs.
            Message::Frame => {}
            Message::Ws(m) => return self.ws_update(m),
            Message::Updater(m) => return self.updater_update(m),
            Message::Setup(m) => return self.setup_update(m),
            Message::Housekeeping(now) => {
                self.expire_toasts(now);
                let ws_task = self.ws_housekeeping(now);
                if let Some((at, dir)) = self.save_at.clone()
                    && now >= at
                {
                    self.save_at = None;
                    if let Some(values) = self.edits.get(&dir).cloned() {
                        for out in self.outputs_showing(&dir) {
                            self.daemon.save_properties(out, values.clone());
                        }
                    }
                }
                return Task::batch([ws_task, self.check_screenshot()]);
            }
            Message::Key(key, mods) => return self.on_key(key, mods),
            Message::Window(id, ev) => {
                let chrome = self.chrome_window_event(id, &ev);
                let task = match ev {
                    window::Event::Resized(size) => {
                        self.window = size;
                        self.viewport = None;
                        Task::none()
                    }
                    window::Event::Opened { .. } if cfg!(windows) => {
                        window::raw_id::<Message>(id).map(Message::NativeWindow)
                    }
                    window::Event::Focused => {
                        self.focused = true;
                        // Event-driven reconnect: the user came back to the window.
                        self.daemon.reconnect();
                        // The system setting may have changed while the user was away.
                        let motion = Task::perform(
                            crate::platform::reduce_motion_async(),
                            Message::ReduceMotion,
                        );
                        // An update check, if one is due (at most every 12 hours).
                        Task::batch([motion, self.updater_focus()])
                    }
                    window::Event::Unfocused => {
                        self.focused = false;
                        Task::none()
                    }
                    window::Event::CloseRequested => {
                        self.cfg.window = Some([self.window.width, self.window.height]);
                        self.cfg.last_session = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_secs());
                        self.daemon.want_stats(false);
                        self.save_cfg();
                        window::close(id)
                    }
                    _ => Task::none(),
                };
                return Task::batch([chrome, task]);
            }
            Message::Chrome(m) => return self.chrome_update(m),
            Message::NativeWindow(raw) => crate::platform::set_window_icon(raw),
            Message::SystemTheme(mode) => {
                self.system_dark = !matches!(mode, iced::theme::Mode::Light)
            }
            Message::ReduceMotion(reduced) => {
                if reduced != motion::reduced() {
                    tracing::info!(reduced, "system animation setting");
                }
                motion::set_reduced(reduced);
                if reduced {
                    // Anything still moving ends now.
                    let open = self.panel.value();
                    self.panel = motion::panel_at(open);
                    self.hover_in = Animation::new(self.hover_in.value());
                    self.hover_out = Animation::new(self.hover_out.value());
                    for t in &mut self.toasts {
                        t.shown = Animation::new(t.shown.value());
                    }
                }
            }
            Message::CursorMoved(pos) => {
                if let Some(d) = &mut self.drag {
                    d.pos = pos;
                    match d.origin {
                        None => d.origin = Some(pos),
                        Some(o) if !d.active && o.distance(pos) > DRAG_THRESHOLD => d.active = true,
                        _ => {}
                    }
                }
            }
            Message::MouseReleased => {
                if let Some(d) = self.drag.take()
                    && d.active
                    && let Some(out) = self.drop_target.take()
                {
                    self.apply(&d.path, vec![Some(out)]);
                }
                self.drop_target = None;
            }

            Message::Nav(page) => {
                if self.page == Page::Workshop && page != Page::Workshop {
                    self.ws_leave();
                }
                if page == Page::Workshop && self.page != Page::Workshop {
                    self.panel = motion::panel_at(self.ws.selected.is_some());
                } else if page == Page::Library && self.page != Page::Library {
                    self.panel = motion::panel_at(self.selected.is_some());
                }
                self.page = page;
                self.viewport = None;
            }
            Message::Search(s) => {
                self.search = s;
                self.scroll_y = 0.0;
                return operation::snap_to(Id::new(GRID_ID), scrollable::RelativeOffset::START);
            }
            Message::SearchSubmit => {
                self.refresh_filter();
                if self.selected.is_none()
                    && let (Some(&first), Some(lib)) = (self.filtered.first(), &self.library)
                {
                    let dir = lib.wallpapers[first].dir.clone();
                    self.select(Some(dir));
                } else if let Some(dir) = self.selected.clone() {
                    let outs = self.apply_targets();
                    self.apply(&dir, outs);
                }
            }
            Message::FilterKind(k) => {
                self.cfg.filter_kind = k;
                self.save_cfg();
            }
            Message::FilterTag(t) => {
                self.cfg.filter_tag = t;
                self.save_cfg();
            }
            Message::FilterRes(r) => {
                self.cfg.filter_res = r;
                self.save_cfg();
            }
            Message::Sort(s) => {
                self.cfg.sort = s;
                self.save_cfg();
            }
            Message::ClearFilters => {
                self.search.clear();
                self.cfg.filter_kind = None;
                self.cfg.filter_tag = None;
                self.cfg.filter_res = None;
                self.save_cfg();
            }
            Message::Scrolled(v) => {
                self.scroll_y = v.absolute_offset().y;
                self.viewport = Some(v.bounds().size());
            }
            Message::CardPress(dir) => {
                self.select(Some(dir.clone()));
                self.drag = Some(Drag {
                    path: dir,
                    origin: None,
                    pos: Point::ORIGIN,
                    active: false,
                });
            }
            Message::CardDouble(dir) => {
                self.drag = None;
                let outs = self.apply_targets();
                self.apply(&dir, outs);
            }
            Message::Hover(dir, inside) => {
                let now = Instant::now();
                if inside {
                    if self.hovered.as_ref() != Some(&dir) {
                        self.hover_prev = self.hovered.take();
                        self.hover_out = motion::hover(true, false, now);
                        self.hovered = Some(dir);
                        self.hover_in = motion::hover(false, true, now);
                    }
                } else if self.hovered.as_ref() == Some(&dir) {
                    self.hover_prev = self.hovered.take();
                    self.hover_out = motion::hover(true, false, now);
                }
            }
            Message::ClosePanel => self.select(None),
            Message::ApplyDir(dir) => {
                let outs = self.apply_targets();
                self.apply(&dir, outs);
            }
            Message::ShowInLibrary(dir) => {
                if self.page == Page::Workshop {
                    self.ws_leave();
                }
                self.page = Page::Library;
                self.search.clear();
                self.panel = motion::panel_at(self.selected.is_some());
                self.select(Some(dir.clone()));
                if let Some(i) = self.library.as_ref().and_then(|l| {
                    self.filtered
                        .iter()
                        .position(|&i| l.wallpapers[i].dir == dir)
                }) {
                    return self.ensure_visible(i);
                }
            }
            Message::Apply => {
                if let Some(dir) = self.selected.clone() {
                    let outs = self.apply_targets();
                    self.apply(&dir, outs);
                }
            }
            Message::TargetToggle(id) => {
                if !self.targets.remove(&id) {
                    self.targets.insert(id);
                }
                self.save_cfg();
            }
            Message::TargetOnly(id) => {
                if self.page == Page::Screens {
                    // From the screens page: pick a wallpaper for this screen.
                    self.page = Page::Library;
                    let n = self
                        .daemon
                        .outputs
                        .iter()
                        .position(|o| o.id == id)
                        .map_or(0, |i| i + 1);
                    let msg = self
                        .t(K::PickForScreen)
                        .replace("{screen}", &format!("{n} ({id})"));
                    self.toast(msg, false);
                }
                self.targets = std::iter::once(id).collect();
                self.save_cfg();
            }
            Message::AllScreens => {
                self.targets.clear();
                self.save_cfg();
            }
            Message::SetScaling(s) => {
                self.cfg.scaling = s;
                self.save_cfg();
            }
            Message::DropHover(t) => {
                if self.drag.as_ref().is_some_and(|d| d.active) || t.is_none() {
                    self.drop_target = t;
                }
            }
            Message::Prop(key, value) => {
                if let Some(dir) = self.selected.clone() {
                    self.change_properties(&dir, PropertyMap::from([(key, value)]));
                }
            }
            Message::PickProp(key, directory) => {
                if let Some(dir) = self.selected.clone() {
                    return Task::perform(crate::picker::pick_async(directory), move |p| {
                        Message::PropPicked(dir.clone(), key.clone(), p)
                    });
                }
            }
            Message::PropPicked(dir, key, Some(path)) => {
                let value = PropertyValue::Text(path.to_string_lossy().into_owned());
                self.change_properties(&dir, PropertyMap::from([(key, value)]));
            }
            Message::PropPicked(_, _, None) => {}
            Message::ResetProps => {
                if let Some(dir) = self.selected.clone() {
                    self.edits.remove(&dir);
                    for out in self.outputs_showing(&dir) {
                        self.daemon.reset_properties(out);
                    }
                }
            }
            Message::ColorEditor(key) => self.color_open = key,
            Message::ToggleDescription => self.desc_open = !self.desc_open,

            Message::Pause => {
                if let Some(s) = &self.daemon.state {
                    self.daemon.set_paused(!s.paused);
                }
            }
            Message::Mute => {
                if let Some(s) = &self.daemon.state {
                    self.daemon.set_muted(!s.muted);
                }
            }
            Message::Volume(v) => {
                self.volume_drag = Some(v);
                self.daemon.set_volume(v);
            }
            Message::VolumeRelease => self.volume_drag = None,
            Message::WallpaperVolume(dir, v) => {
                self.daemon.set_wallpaper_audio(dir.clone(), Some(v), None);
                self.wallpaper_volume_drag = Some((dir, v));
            }
            Message::WallpaperVolumeRelease => self.wallpaper_volume_drag = None,
            Message::WallpaperMute(dir) => {
                let muted = self.daemon.wallpaper_audio(&dir).is_some_and(|l| l.muted);
                self.daemon.set_wallpaper_audio(dir, None, Some(!muted));
            }
            Message::StartEngine => self.daemon.launch(),
            Message::EngineSettings(s) => self.daemon.set_settings(s),
            Message::Autostart(on) => match crate::config::autostart::set(on) {
                Ok(()) => self.autostart = on,
                Err(e) => {
                    let msg = format!("{}: {e}", self.t(K::Error));
                    self.toast(msg, true);
                }
            },
            Message::FolderInput(s) => self.folder_input = s,
            Message::AddTypedFolder => {
                let p = PathBuf::from(self.folder_input.trim());
                if p.is_dir() {
                    self.folder_input.clear();
                    return self.add_folder(p);
                }
                let msg = self.t(K::FolderMissing);
                self.toast(msg, true);
            }
            Message::PickFolder => {
                return Task::perform(crate::picker::pick_async(true), Message::FolderPicked);
            }
            Message::FolderPicked(Some(p)) => return self.add_folder(p),
            Message::FolderPicked(None) => {}
            Message::RemoveFolder(i) => {
                if i < self.cfg.folders.len() {
                    self.cfg.folders.remove(i);
                    self.save_cfg();
                    self.library = None;
                    return self.rescan();
                }
            }
            Message::Rescan => {
                self.library = None;
                return self.rescan();
            }
            Message::Lang(l) => {
                self.cfg.language = l;
                self.lang = Lang::resolve(l);
                self.save_cfg();
            }
            Message::ThemePref(t) => {
                self.cfg.theme = t;
                self.save_cfg();
            }
            Message::Preview(m) => {
                self.cfg.preview_mode = m;
                self.save_cfg();
            }
            Message::Open(target) => {
                if let Err(e) = crate::platform::open(&target) {
                    self.toast(e.to_string(), true);
                }
            }
            Message::ClearScreen(id) => self.daemon.clear(Some(id)),
            Message::Shot(shot) => {
                if let Some((path, _, _)) = &self.screenshot {
                    let img = image::RgbaImage::from_raw(
                        shot.size.width,
                        shot.size.height,
                        shot.rgba.to_vec(),
                    );
                    match img.map(|i| i.save(path)) {
                        Some(Ok(())) => tracing::info!("screenshot saved to {}", path.display()),
                        Some(Err(e)) => tracing::error!("screenshot: {e}"),
                        None => tracing::error!("screenshot: bad size"),
                    }
                }
                return iced::exit();
            }
            Message::Noop => {}
        }
        Task::none()
    }

    fn add_folder(&mut self, dir: PathBuf) -> Task<Message> {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        if !self.cfg.folders.contains(&dir) {
            self.cfg.folders.push(dir);
            self.save_cfg();
        }
        self.library = None;
        self.rescan()
    }

    fn on_key(&mut self, key: keyboard::Key, mods: keyboard::Modifiers) -> Task<Message> {
        use keyboard::key::Named;
        // The installation dialog takes the keyboard while it is open.
        if let Some(m) = self.setup_key(&key) {
            return self.setup_update(m);
        }
        if self.setup_dialog_open() {
            return Task::none();
        }
        match key.as_ref() {
            keyboard::Key::Character("f") if mods.command() && self.page != Page::Workshop => {
                self.page = Page::Library;
                return operation::focus(Id::new(SEARCH_ID));
            }
            keyboard::Key::Named(Named::Escape) if self.page == Page::Workshop => {
                if !self.ws.search_input.is_empty() {
                    return self.ws_update(crate::workshop::state::WsMsg::Search(String::new()));
                }
                return self.ws_update(crate::workshop::state::WsMsg::Close);
            }
            keyboard::Key::Character("f") if mods.command() && self.page == Page::Workshop => {
                return operation::focus(Id::new("ws-search"));
            }
            keyboard::Key::Named(Named::Escape) => {
                if self.drag.take().is_some() {
                    self.drop_target = None;
                } else if !self.search.is_empty() {
                    self.search.clear();
                } else if self.color_open.is_some() {
                    self.color_open = None;
                } else {
                    self.select(None);
                }
            }
            _ if self.page != Page::Library => {}
            keyboard::Key::Named(Named::Enter) => {
                if let Some(dir) = self.selected.clone() {
                    let outs = self.apply_targets();
                    self.apply(&dir, outs);
                }
            }
            keyboard::Key::Named(Named::ArrowRight) => return self.move_selection(1),
            keyboard::Key::Named(Named::ArrowLeft) => return self.move_selection(-1),
            keyboard::Key::Named(Named::ArrowDown) => {
                let cols = grid_geom(self.grid_width()).cols as isize;
                return self.move_selection(cols);
            }
            keyboard::Key::Named(Named::ArrowUp) => {
                let cols = grid_geom(self.grid_width()).cols as isize;
                return self.move_selection(-cols);
            }
            _ => {}
        }
        Task::none()
    }

    fn check_screenshot(&mut self) -> Task<Message> {
        let Some((path, started, requested)) = self.screenshot.clone() else {
            return Task::none();
        };
        if requested {
            return Task::none();
        }
        let settled = self.library.is_some()
            && self.previews.pending() == 0
            && !matches!(self.daemon.link, Link::Connecting | Link::Starting)
            && (!self.daemon.is_online()
                || self.daemon.stats.is_some()
                || started.elapsed() > Duration::from_secs(4));
        if (settled && started.elapsed() > self.screenshot_delay)
            || started.elapsed() > self.screenshot_delay.max(Duration::from_secs(15))
        {
            self.screenshot = Some((path, started, true));
            return window::latest()
                .and_then(window::screenshot)
                .map(Message::Shot);
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            crate::daemon::subscription(self.endpoint.clone()).map(Message::Daemon),
            crate::previews::subscription(crate::config::cache_dir().map(|d| d.join("thumbs")))
                .map(Message::Thumb),
            keyboard::listen().filter_map(|e| match e {
                keyboard::Event::KeyPressed { key, modifiers, .. } => {
                    Some(Message::Key(key, modifiers))
                }
                _ => None,
            }),
            window::events().map(|(id, e)| Message::Window(id, e)),
            iced::system::theme_changes().map(Message::SystemTheme),
        ];
        if let Some(tick) = self.anim_tick {
            subs.push(time::every(tick).map(|_| Message::Tick));
        }
        // The Workshop network worker exists only while its page is shown.
        if self.page == Page::Workshop {
            subs.push(
                crate::workshop::state::worker()
                    .map(|e| Message::Ws(crate::workshop::state::WsMsg::Worker(e))),
            );
        }
        // New subscriptions arrive through file events, on every page.
        if let Some(lib) = &self.library
            && !lib.steam.workshop_dirs.is_empty()
        {
            subs.push(
                crate::workshop::watch::subscription(lib.steam.workshop_dirs.clone())
                    .map(|e| Message::Ws(crate::workshop::state::WsMsg::Watch(e))),
            );
        }
        let now = Instant::now();
        if self.hover_in.is_animating(now)
            || self.hover_out.is_animating(now)
            || self.panel.is_animating(now)
            || self.toasts.iter().any(|t| t.shown.is_animating(now))
        {
            subs.push(window::frames().map(|_| Message::Frame));
        }
        if !self.toasts.is_empty()
            || self.save_at.is_some()
            || self.screenshot.is_some()
            || self.ws.wants_timer()
        {
            subs.push(time::every(Duration::from_millis(250)).map(Message::Housekeeping));
        }
        // Pointer tracking only while a card is pressed (possible drag).
        if self.drag.is_some() {
            subs.push(event::listen_with(|e, _status, _w| match e {
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    Some(Message::CursorMoved(position))
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    Some(Message::MouseReleased)
                }
                _ => None,
            }));
        }
        Subscription::batch(subs)
    }
}

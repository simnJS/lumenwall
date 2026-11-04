//! Wallpaper Engine playlists (spec 07 §17): reading them from Wallpaper Engine's
//! `config.json`, normalising item paths, and the timer that advances them.
//!
//! The timer exposes its next deadline instead of being polled every frame, so the event
//! loop can sleep until it (docs/PERFORMANCE.md).

use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::services::{PlaylistCatalog, ServiceError};

/// Playlist `settings` (spec 07 §17.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistSettings {
    /// Time between changes (`delay`, minutes, at least 1; default 60).
    pub delay: Duration,
    /// `mode == "timer"`; any other mode freezes the playlist.
    pub timer: bool,
    /// `order == "random"`.
    pub random: bool,
    /// `updateonpause`: when false, fullscreen pauses do not count towards the delay.
    pub update_on_pause: bool,
    /// `videosequence`: read but unused.
    pub video_sequence: bool,
    /// `transition`: Wallpaper Engine blends one item into the next. Read; Lumenwall cuts
    /// from one item to the next for now (a transition needs rendering in the engine).
    pub transition: bool,
}

impl Default for PlaylistSettings {
    fn default() -> Self {
        Self {
            delay: Duration::from_secs(60 * 60),
            timer: true,
            random: false,
            update_on_pause: false,
            video_sequence: false,
            transition: false,
        }
    }
}

impl PlaylistSettings {
    fn from_json(settings: Option<&Value>) -> Self {
        let mut out = Self::default();
        let Some(settings) = settings else { return out };
        if let Some(minutes) = settings.get("delay").and_then(Value::as_f64) {
            // parity: whole minutes, at least one.
            out.delay = Duration::from_secs(minutes.max(1.0) as u64 * 60);
        }
        if let Some(mode) = settings.get("mode").and_then(Value::as_str) {
            out.timer = mode == "timer";
        }
        if let Some(order) = settings.get("order").and_then(Value::as_str) {
            out.random = order == "random";
        }
        if let Some(b) = settings.get("updateonpause").and_then(Value::as_bool) {
            out.update_on_pause = b;
        }
        if let Some(b) = settings.get("videosequence").and_then(Value::as_bool) {
            out.video_sequence = b;
        }
        if let Some(b) = settings.get("transition").and_then(Value::as_bool) {
            out.transition = b;
        }
        out
    }
}

/// A playlist with at least one usable item (wallpaper folders).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Playlist {
    pub name: String,
    pub items: Vec<PathBuf>,
    pub settings: PlaylistSettings,
}

/// How item paths written by Wallpaper Engine (Windows paths) are mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStyle {
    /// Spec 07 §17.2: drop `\\?\` and the drive letter, `\` → `/`, root at `/`.
    Posix,
    /// QO-24: keep native Windows paths (only `\\?\` is dropped).
    Windows,
}

impl PathStyle {
    pub const fn native() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

/// What a path points to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    Other,
}

/// File-system queries, replaceable in tests.
pub trait PathProbe: Send + Sync {
    /// `None` when the path does not exist.
    fn kind(&self, path: &Path) -> Option<PathKind>;
}

/// [`PathProbe`] backed by the real file system.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealFs;

impl PathProbe for RealFs {
    fn kind(&self, path: &Path) -> Option<PathKind> {
        let meta = std::fs::metadata(path).ok()?;
        Some(if meta.is_file() {
            PathKind::File
        } else if meta.is_dir() {
            PathKind::Directory
        } else {
            PathKind::Other
        })
    }
}

/// Spec 07 §17.2 steps 1–5 (no file-system access).
pub fn normalize_item(raw: &str, style: PathStyle) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let s = raw.strip_prefix(r"\\?\").unwrap_or(raw);
    match style {
        PathStyle::Windows => Some(lexical_normalize(Path::new(s))),
        PathStyle::Posix => {
            let s = s.replace('\\', "/");
            let s = match s.char_indices().nth(1) {
                Some((i, ':')) => &s[i + 1..],
                _ => &s[..],
            };
            Some(PathBuf::from(posix_normalize(s)))
        }
    }
}

/// Resolves `.` and `..` in a POSIX path given as text, rooted at `/`; `..` never climbs above
/// the root. Works on the string rather than on `Path` so that the result uses `/` whatever the
/// host OS (on a Windows host `Path` would turn `/a/b` into `\a\b`).
fn posix_normalize(s: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    let mut out = String::new();
    for part in &parts {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// Resolves `.` and `..` without touching the file system; `..` never climbs above the root.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Spec 07 §17.2 step 6 and existence check: a file stands for its folder.
fn resolve_item(raw: &Value, style: PathStyle, probe: &dyn PathProbe) -> Option<PathBuf> {
    let path = normalize_item(raw.as_str()?, style)?;
    match probe.kind(&path)? {
        PathKind::File => path.parent().map(Path::to_path_buf),
        PathKind::Directory | PathKind::Other => Some(path),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlaylistError {
    #[error("cannot read the Wallpaper Engine configuration: {0}")]
    Read(String),
    #[error("invalid Wallpaper Engine configuration (JSON): {0}")]
    InvalidJson(String),
    #[error("the Wallpaper Engine configuration has no 'steamuser' section")]
    MissingSteamUser,
    #[error("playlist '{name}' not found; available playlists: {}", available.join(", "))]
    Unknown {
        name: String,
        available: Vec<String>,
    },
}

/// Collects the playlists of a Wallpaper Engine `config.json` (spec 07 §17.1). Unusable
/// items and playlists are skipped with a warning.
pub fn parse_playlists(
    json: &str,
    style: PathStyle,
    probe: &dyn PathProbe,
) -> Result<BTreeMap<String, Playlist>, PlaylistError> {
    let root: Value =
        serde_json::from_str(json).map_err(|e| PlaylistError::InvalidJson(e.to_string()))?;
    let user = root
        .get("steamuser")
        .filter(|v| v.is_object())
        .ok_or(PlaylistError::MissingSteamUser)?;

    let mut found: Vec<(Option<String>, &Value)> = Vec::new();
    if let Some(list) = user.pointer("/general/playlists").and_then(Value::as_array) {
        found.extend(
            list.iter()
                .map(|p| (p.get("name").and_then(Value::as_str).map(str::to_owned), p)),
        );
    }
    if let Some(selected) = user
        .pointer("/wallpaperconfig/selectedwallpapers")
        .and_then(Value::as_object)
    {
        for (key, entry) in selected {
            if let Some(p) = entry.get("playlist").filter(|p| p.is_object()) {
                let name = p
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(key.as_str());
                found.push((Some(name.to_owned()), p));
            }
        }
    }

    let mut playlists = BTreeMap::new();
    for (name, value) in found {
        let Some(name) = name.filter(|n| !n.is_empty()) else {
            tracing::warn!("ignoring a Wallpaper Engine playlist without a name");
            continue;
        };
        let raw_items = value
            .get("items")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut items = Vec::with_capacity(raw_items.len());
        for raw in raw_items {
            match resolve_item(raw, style, probe) {
                Some(path) => items.push(path),
                None => tracing::warn!(playlist = %name, item = %raw, "ignoring playlist item"),
            }
        }
        if items.is_empty() {
            tracing::error!(playlist = %name, "ignoring playlist without usable items");
            continue;
        }
        let settings = PlaylistSettings::from_json(value.get("settings"));
        // A later playlist with the same name replaces the earlier one.
        playlists.insert(
            name.clone(),
            Playlist {
                name,
                items,
                settings,
            },
        );
    }
    Ok(playlists)
}

type ConfigSource = dyn Fn() -> Result<String, ServiceError> + Send + Sync;
type Loaded = Result<Arc<BTreeMap<String, Playlist>>, PlaylistError>;

/// [`PlaylistCatalog`] reading Wallpaper Engine's `config.json` on first use, then again
/// only when a client lists the playlists ([`PlaylistCatalog::list`]), so that playlists
/// made in Wallpaper Engine meanwhile show up.
pub struct WeConfigCatalog {
    source: Box<ConfigSource>,
    probe: Box<dyn PathProbe>,
    style: PathStyle,
    loaded: Mutex<Option<Loaded>>,
}

impl WeConfigCatalog {
    /// `source` returns the contents of `<Wallpaper Engine install>/config.json`; locating
    /// the installation (spec 07 §3.2) is the caller's job.
    pub fn new(
        source: impl Fn() -> Result<String, ServiceError> + Send + Sync + 'static,
        probe: impl PathProbe + 'static,
        style: PathStyle,
    ) -> Self {
        Self {
            source: Box::new(source),
            probe: Box::new(probe),
            style,
            loaded: Mutex::new(None),
        }
    }

    fn load(&self) -> Loaded {
        let json = (self.source)().map_err(|e| PlaylistError::Read(e.to_string()))?;
        match parse_playlists(&json, self.style, self.probe.as_ref()) {
            // Invalid JSON is logged and yields no playlist (spec 07 §17.1).
            Err(PlaylistError::InvalidJson(message)) => {
                tracing::error!("invalid Wallpaper Engine configuration: {message}");
                Ok(Arc::default())
            }
            other => other.map(Arc::new),
        }
    }

    fn loaded(&self) -> std::sync::MutexGuard<'_, Option<Loaded>> {
        self.loaded.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The playlists read so far (reads them the first time).
    fn playlists(&self) -> Loaded {
        self.loaded().get_or_insert_with(|| self.load()).clone()
    }

    /// Reads the configuration again.
    pub fn refresh(&self) -> Loaded {
        let fresh = self.load();
        *self.loaded() = Some(fresh.clone());
        fresh
    }

    /// The named playlist.
    pub fn playlist(&self, name: &str) -> Result<Playlist, PlaylistError> {
        let playlists = self.playlists()?;
        playlists
            .get(name)
            .cloned()
            .ok_or_else(|| PlaylistError::Unknown {
                name: name.to_owned(),
                available: playlists.keys().cloned().collect(),
            })
    }
}

impl PlaylistCatalog for WeConfigCatalog {
    fn first_wallpaper(&self, name: &str) -> Result<PathBuf, ServiceError> {
        Ok(self.playlist(name)?.items[0].clone())
    }

    fn playlist(&self, name: &str) -> Result<Playlist, ServiceError> {
        Ok(WeConfigCatalog::playlist(self, name)?)
    }

    fn list(&self) -> Result<Vec<Playlist>, ServiceError> {
        Ok(self.refresh()?.values().cloned().collect())
    }
}

/// Random numbers for shuffling; replaceable in tests.
pub trait Shuffle {
    fn next_u64(&mut self) -> u64;
}

/// SplitMix64, randomly seeded by default (spec 07 §17.4: random initial seed).
#[derive(Clone, Debug)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn from_entropy() -> Self {
        Self(RandomState::new().hash_one(Instant::now()))
    }
}

impl Shuffle for SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn shuffle(order: &mut [usize], rng: &mut impl Shuffle) {
    for i in (1..order.len()).rev() {
        let j = (rng.next_u64() % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
}

/// Runs one playlist for one target (spec 07 §17.4): its timer, the order of its items,
/// the items that failed, and what the target shows.
///
/// Time is passed in by the caller (`now`), so the runner is driven by a fake clock in tests;
/// the caller sleeps until [`deadline`](Self::deadline) instead of polling.
#[derive(Debug)]
pub struct PlaylistRunner<R> {
    playlist: Playlist,
    order: Vec<usize>,
    position: usize,
    /// The target shows a wallpaper that is not an item (until the first change).
    outside: bool,
    shown: PathBuf,
    failed: Vec<bool>,
    deadline: Instant,
    /// Since when the target is paused.
    paused_since: Option<Instant>,
    rng: R,
}

impl<R: Shuffle> PlaylistRunner<R> {
    /// Starts at the item currently shown (`current`), else at the first one. A `current`
    /// that is not an item stays shown until the first change.
    pub fn new(playlist: Playlist, current: Option<&Path>, now: Instant, mut rng: R) -> Self {
        let count = playlist.items.len();
        let mut order: Vec<usize> = (0..count).collect();
        let mut found = current.and_then(|c| order.iter().position(|&i| playlist.items[i] == c));
        if playlist.settings.random {
            shuffle(&mut order, &mut rng);
            // lumenwall: the first round starts with the wallpaper shown, so that it shows
            // every item (the reference starts in the middle of its shuffled order and skips
            // the items before it until the next round).
            if let Some(item) = found {
                let at = order.iter().position(|&i| i == item).unwrap_or(0);
                order.swap(0, at);
                found = Some(0);
            }
        }
        let position = found.unwrap_or(0);
        let shown = current.map_or_else(|| playlist.items[order[0]].clone(), Path::to_path_buf);
        let deadline = now + playlist.settings.delay;
        Self {
            failed: vec![false; count],
            outside: current.is_some() && found.is_none(),
            shown,
            playlist,
            order,
            position,
            deadline,
            paused_since: None,
            rng,
        }
    }

    pub fn playlist(&self) -> &Playlist {
        &self.playlist
    }

    /// Only timer playlists with at least two items change by themselves.
    pub fn is_active(&self) -> bool {
        self.playlist.settings.timer && self.playlist.items.len() >= 2
    }

    /// When [`advance`](Self::advance) has work to do; `None` if never, or not before the
    /// pause ends (paused time does not count unless `updateonpause`).
    pub fn deadline(&self) -> Option<Instant> {
        let waiting = self.paused_since.is_some() && !self.playlist.settings.update_on_pause;
        (self.is_active() && !waiting).then_some(self.deadline)
    }

    /// What the target shows.
    pub fn current(&self) -> &Path {
        &self.shown
    }

    /// Index in the playlist's items of what the target shows, `None` when it is not an item.
    pub fn current_item(&self) -> Option<usize> {
        if self.outside {
            return None;
        }
        self.playlist.items.iter().position(|p| *p == self.shown)
    }

    /// The target's outputs paused (fullscreen window, locked session, hidden, user pause)
    /// or resumed at `now`. Calls for an unchanged state are ignored.
    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        match (paused, self.paused_since) {
            (true, None) => self.paused_since = Some(now),
            (false, Some(since)) => {
                self.paused_since = None;
                self.pause_ended(now.saturating_duration_since(since));
            }
            _ => {}
        }
    }

    /// Call when a pause ends: unless `updateonpause`, the pause does not count.
    pub fn pause_ended(&mut self, paused_for: Duration) {
        if !self.playlist.settings.update_on_pause {
            self.deadline += paused_for;
        }
    }

    /// If the deadline has passed, moves to the next usable item. `precheck` tells whether
    /// `project.json` is readable with `type` and `file`; `load` switches the target to the
    /// item. Returns the item now shown, or `None` if nothing changed. Failed items are
    /// skipped for the rest of the session.
    pub fn advance(
        &mut self,
        now: Instant,
        precheck: impl FnMut(&Path) -> bool,
        load: impl FnMut(&Path) -> bool,
    ) -> Option<PathBuf> {
        match self.deadline() {
            Some(deadline) if now >= deadline => self.step(now, true, precheck, load),
            _ => None,
        }
    }

    /// Next (`forward`) or previous usable item now, whatever the mode (Lumenwall's next and
    /// previous commands); the timer starts again. Same contract as [`advance`](Self::advance).
    pub fn step(
        &mut self,
        now: Instant,
        forward: bool,
        mut precheck: impl FnMut(&Path) -> bool,
        mut load: impl FnMut(&Path) -> bool,
    ) -> Option<PathBuf> {
        let count = self.order.len();
        if count < 2 && !self.outside {
            return None;
        }
        // A new delay in every case (§17.4 step 6); a pause going on counts from now.
        self.deadline = now + self.playlist.settings.delay;
        if self.paused_since.is_some() {
            self.paused_since = Some(now);
        }
        let move_on = |runner: &mut Self| {
            if std::mem::take(&mut runner.outside) {
                // lumenwall: from a wallpaper outside the playlist, the first change shows
                // the start of the order (the reference starts at the second item).
                runner.position = if forward { 0 } else { count - 1 };
            } else if forward {
                runner.position = (runner.position + 1) % count;
                if runner.position == 0 && runner.playlist.settings.random {
                    let last = runner.order[count - 1];
                    shuffle(&mut runner.order, &mut runner.rng);
                    // lumenwall: a new round never starts with the item that ended the last
                    // one (the reference can show it twice in a row).
                    if runner.order[0] == last {
                        runner.order.swap(0, count - 1);
                    }
                }
            } else {
                runner.position = (runner.position + count - 1) % count;
            }
        };
        move_on(self);
        let mut tries = 0;
        let item = loop {
            if tries >= count {
                tracing::warn!(playlist = %self.playlist.name, "every playlist item failed; keeping the current wallpaper");
                return None;
            }
            let item = self.order[self.position];
            if !self.failed[item] {
                if precheck(&self.playlist.items[item]) {
                    break item;
                }
                tracing::warn!(item = %self.playlist.items[item].display(), "playlist item has no valid project.json");
                self.failed[item] = true;
            }
            move_on(self);
            tries += 1;
        };
        let path = &self.playlist.items[item];
        if load(path) {
            self.shown = path.clone();
            Some(path.clone())
        } else {
            tracing::warn!(item = %path.display(), "cannot load playlist item");
            self.failed[item] = true;
            None
        }
    }
}

/// Spec 07 §17.4 step 3: `project.json` readable, with `type` and `file`.
pub fn project_is_usable(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("project.json")) else {
        return false;
    };
    serde_json::from_str::<Value>(&text).is_ok_and(|v| {
        v.get("type").and_then(Value::as_str).is_some()
            && v.get("file").and_then(Value::as_str).is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeFs(HashMap<PathBuf, PathKind>);

    impl FakeFs {
        fn with(entries: &[(&str, PathKind)]) -> Self {
            Self(
                entries
                    .iter()
                    .map(|(p, k)| (PathBuf::from(p), *k))
                    .collect(),
            )
        }
    }

    impl PathProbe for FakeFs {
        fn kind(&self, path: &Path) -> Option<PathKind> {
            self.0.get(path).copied()
        }
    }

    #[test]
    fn normalizes_windows_paths_for_posix() {
        let n =
            |s: &str| normalize_item(s, PathStyle::Posix).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(
            n(r"\\?\C:\Steam\431960\123"),
            Some("/Steam/431960/123".into())
        );
        assert_eq!(
            n(r"D:\a\.\b\..\c\scene.json"),
            Some("/a/c/scene.json".into())
        );
        assert_eq!(n("relative/dir"), Some("/relative/dir".into()));
        assert_eq!(n("/x/../../y"), Some("/y".into()));
        assert_eq!(n(""), None);
    }

    #[test]
    fn posix_normalization_never_uses_the_host_separator() {
        // On a Windows host `Path` would render these with `\`.
        let n = |s: &str| normalize_item(s, PathStyle::Posix).unwrap();
        assert_eq!(n(r"C:\a\b").to_str(), Some("/a/b"));
        assert_eq!(n("C:").to_str(), Some("/"));
        assert_eq!(n("/").to_str(), Some("/"));
        assert_eq!(n("a//b/").to_str(), Some("/a/b"));
    }

    #[test]
    fn parses_both_playlist_groups() {
        let fs = FakeFs::with(&[
            ("/w/a", PathKind::Directory),
            ("/w/b/project.json", PathKind::File),
            ("/w/c", PathKind::Directory),
        ]);
        let json = r#"{
            "steamuser": {
                "general": { "playlists": [
                    { "name": "day", "items": ["C:\\w\\a", "C:\\w\\b\\project.json", "", 5, "C:\\missing"],
                      "settings": { "delay": 0, "order": "random", "mode": "timer", "updateonpause": true } },
                    { "name": "empty", "items": ["C:\\missing"] },
                    { "items": ["C:\\w\\a"] },
                    { "name": "night", "items": ["C:\\w\\a"] }
                ]},
                "wallpaperconfig": { "selectedwallpapers": {
                    "Monitor0": { "playlist": { "items": ["C:\\w\\c"], "settings": { "mode": "never", "delay": 5 } } },
                    "Monitor1": { "playlist": { "name": "night", "items": ["C:\\w\\c"] } },
                    "Monitor2": { "file": "x" }
                }}
            }
        }"#;
        let playlists = parse_playlists(json, PathStyle::Posix, &fs).unwrap();
        assert_eq!(
            playlists.keys().collect::<Vec<_>>(),
            ["Monitor0", "day", "night"]
        );
        let day = &playlists["day"];
        assert_eq!(day.items, [PathBuf::from("/w/a"), PathBuf::from("/w/b")]);
        assert_eq!(day.settings.delay, Duration::from_secs(60));
        assert!(day.settings.random && day.settings.timer && day.settings.update_on_pause);
        let monitor = &playlists["Monitor0"];
        assert!(!monitor.settings.timer);
        assert_eq!(monitor.settings.delay, Duration::from_secs(300));
        // The second group replaces the first one with the same name.
        assert_eq!(playlists["night"].items, [PathBuf::from("/w/c")]);
    }

    #[test]
    fn config_errors() {
        let fs = FakeFs::default();
        assert!(matches!(
            parse_playlists("{", PathStyle::Posix, &fs),
            Err(PlaylistError::InvalidJson(_))
        ));
        assert_eq!(
            parse_playlists("{}", PathStyle::Posix, &fs),
            Err(PlaylistError::MissingSteamUser)
        );
        assert!(
            parse_playlists(r#"{"steamuser":{}}"#, PathStyle::Posix, &fs)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn catalog_reads_once_and_lists_names() {
        let reads = Arc::new(Mutex::new(0));
        let counter = reads.clone();
        let catalog = WeConfigCatalog::new(
            move || {
                *counter.lock().unwrap() += 1;
                Ok(r#"{"steamuser":{"general":{"playlists":[
                    {"name":"b","items":["/w/x"]},{"name":"a","items":["/w/y"]}]}}}"#
                    .to_owned())
            },
            FakeFs::with(&[("/w/x", PathKind::Directory), ("/w/y", PathKind::Directory)]),
            PathStyle::Posix,
        );
        assert_eq!(catalog.first_wallpaper("b").unwrap(), PathBuf::from("/w/x"));
        let err = catalog.first_wallpaper("zzz").unwrap_err().to_string();
        assert_eq!(err, "playlist 'zzz' not found; available playlists: a, b");
        assert_eq!(*reads.lock().unwrap(), 1);
    }

    #[test]
    fn catalog_errors() {
        let invalid = WeConfigCatalog::new(
            || Ok("not json".into()),
            FakeFs::default(),
            PathStyle::Posix,
        );
        let err = invalid.first_wallpaper("a").unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
        let unreadable = WeConfigCatalog::new(
            || Err("no install".into()),
            FakeFs::default(),
            PathStyle::Posix,
        );
        let err = unreadable.first_wallpaper("a").unwrap_err().to_string();
        assert!(err.contains("no install"), "{err}");
    }

    fn playlist(count: usize, settings: PlaylistSettings) -> Playlist {
        Playlist {
            name: "p".into(),
            items: (0..count)
                .map(|i| PathBuf::from(format!("/w/{i}")))
                .collect(),
            settings,
        }
    }

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    #[test]
    fn runner_advances_on_deadline() {
        let t0 = Instant::now();
        let settings = PlaylistSettings {
            delay: minutes(1),
            ..PlaylistSettings::default()
        };
        let mut runner = PlaylistRunner::new(
            playlist(3, settings),
            Some(Path::new("/w/1")),
            t0,
            SplitMix64(1),
        );
        assert_eq!(runner.current(), Path::new("/w/1"));
        assert_eq!(runner.deadline(), Some(t0 + minutes(1)));
        assert_eq!(
            runner.advance(
                t0 + minutes(1) - Duration::from_millis(1),
                |_| true,
                |_| true
            ),
            None
        );
        let t1 = t0 + minutes(1);
        assert_eq!(
            runner.advance(t1, |_| true, |_| true),
            Some(PathBuf::from("/w/2"))
        );
        assert_eq!(runner.deadline(), Some(t1 + minutes(1)));
        let t2 = t1 + minutes(1);
        assert_eq!(
            runner.advance(t2, |_| true, |_| true),
            Some(PathBuf::from("/w/0"))
        );
    }

    #[test]
    fn runner_skips_failed_items() {
        let t0 = Instant::now();
        let settings = PlaylistSettings {
            delay: minutes(1),
            ..PlaylistSettings::default()
        };
        let mut runner = PlaylistRunner::new(playlist(4, settings), None, t0, SplitMix64(1));
        let mut t = t0 + minutes(1);
        // /w/1 fails its precheck: /w/2 is loaded instead.
        let next = runner.advance(t, |p| p != Path::new("/w/1"), |_| true);
        assert_eq!(next, Some(PathBuf::from("/w/2")));
        // /w/3 fails to load: nothing changes, the item is marked failed.
        t += minutes(1);
        assert_eq!(
            runner.advance(t, |_| true, |p| p != Path::new("/w/3")),
            None
        );
        // Next round skips /w/3 and /w/1 for good.
        t += minutes(1);
        assert_eq!(
            runner.advance(t, |_| true, |_| true),
            Some(PathBuf::from("/w/0"))
        );
        t += minutes(1);
        assert_eq!(
            runner.advance(t, |_| true, |_| true),
            Some(PathBuf::from("/w/2"))
        );
        // Everything failing keeps the current wallpaper and re-arms the timer.
        t += minutes(1);
        assert_eq!(runner.advance(t, |_| false, |_| true), None);
        assert_eq!(runner.deadline(), Some(t + minutes(1)));
        t += minutes(1);
        assert_eq!(runner.advance(t, |_| true, |_| true), None);
    }

    #[test]
    fn runner_inactive_cases() {
        let t0 = Instant::now();
        let single = PlaylistRunner::new(
            playlist(1, PlaylistSettings::default()),
            None,
            t0,
            SplitMix64(1),
        );
        assert_eq!(single.deadline(), None);
        let frozen = PlaylistSettings {
            timer: false,
            ..PlaylistSettings::default()
        };
        let mut runner = PlaylistRunner::new(playlist(3, frozen), None, t0, SplitMix64(1));
        assert_eq!(runner.deadline(), None);
        assert_eq!(runner.advance(t0 + minutes(1000), |_| true, |_| true), None);
    }

    #[test]
    fn pause_shifts_the_deadline_unless_update_on_pause() {
        let t0 = Instant::now();
        let mut runner = PlaylistRunner::new(
            playlist(2, PlaylistSettings::default()),
            None,
            t0,
            SplitMix64(1),
        );
        runner.pause_ended(minutes(5));
        assert_eq!(runner.deadline(), Some(t0 + minutes(65)));
        let settings = PlaylistSettings {
            update_on_pause: true,
            ..PlaylistSettings::default()
        };
        let mut runner = PlaylistRunner::new(playlist(2, settings), None, t0, SplitMix64(1));
        runner.pause_ended(minutes(5));
        assert_eq!(runner.deadline(), Some(t0 + minutes(60)));
    }

    #[test]
    fn random_order_visits_every_item_each_round() {
        let t0 = Instant::now();
        let settings = PlaylistSettings {
            delay: minutes(1),
            random: true,
            ..PlaylistSettings::default()
        };
        let mut runner = PlaylistRunner::new(playlist(5, settings), None, t0, SplitMix64(42));
        let mut t = t0;
        for _round in 0..3 {
            let mut seen = vec![runner.current().to_path_buf()];
            for _ in 0..4 {
                t += minutes(1);
                seen.push(runner.advance(t, |_| true, |_| true).unwrap());
            }
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), 5);
            t += minutes(1);
            runner.advance(t, |_| true, |_| true).unwrap(); // wraps and reshuffles
        }
    }

    /// A WE `config.json` playlist, as the runner gets it from the catalog.
    fn from_config(settings: &str, count: usize) -> Playlist {
        let items: Vec<String> = (0..count).map(|i| format!("\"C:\\\\w\\\\{i}\"")).collect();
        let json = format!(
            r#"{{"steamuser":{{"general":{{"playlists":[
                {{"name":"p","items":[{}],"settings":{settings}}}]}}}}}}"#,
            items.join(",")
        );
        let entries: Vec<(String, PathKind)> = (0..count)
            .map(|i| (format!("/w/{i}"), PathKind::Directory))
            .collect();
        let fs = FakeFs(
            entries
                .into_iter()
                .map(|(p, k)| (PathBuf::from(p), k))
                .collect(),
        );
        parse_playlists(&json, PathStyle::Posix, &fs)
            .unwrap()
            .remove("p")
            .unwrap()
    }

    fn shown(runner: &PlaylistRunner<SplitMix64>) -> String {
        runner.current().to_string_lossy().into_owned()
    }

    #[test]
    fn timer_mode_from_config_follows_the_delay() {
        let p = from_config(
            r#"{"clock":"clock24","delay":15,"mode":"timer","order":"sorted",
                "transition":true,"updateonpause":false,"videosequence":false}"#,
            3,
        );
        assert!(p.settings.timer && !p.settings.random && p.settings.transition);
        assert_eq!(p.settings.delay, minutes(15));
        let t0 = Instant::now();
        let mut runner = PlaylistRunner::new(p, None, t0, SplitMix64(7));
        assert_eq!(shown(&runner), "/w/0");
        assert_eq!(runner.current_item(), Some(0));
        let mut t = t0;
        let mut seen = Vec::new();
        for _ in 0..4 {
            assert_eq!(runner.deadline(), Some(t + minutes(15)));
            assert_eq!(runner.advance(t + minutes(14), |_| true, |_| true), None);
            t += minutes(15);
            seen.push(runner.advance(t, |_| true, |_| true).unwrap());
        }
        // `sorted` (and anything but `random`) plays in the playlist's order, then loops.
        let seen: Vec<_> = seen
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(seen, ["/w/1", "/w/2", "/w/0", "/w/1"]);
        assert_eq!(runner.current_item(), Some(1));
    }

    #[test]
    fn random_mode_from_config_shuffles_each_round() {
        let p = from_config(r#"{"delay":1,"mode":"timer","order":"random"}"#, 6);
        assert!(p.settings.random);
        let t0 = Instant::now();
        let mut runner = PlaylistRunner::new(p, None, t0, SplitMix64(3));
        let mut t = t0;
        let mut rounds = Vec::new();
        for _ in 0..3 {
            let mut round = vec![shown(&runner)];
            for _ in 0..5 {
                t += minutes(1);
                round.push(
                    runner
                        .advance(t, |_| true, |_| true)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            let mut sorted = round.clone();
            sorted.sort();
            assert_eq!(sorted, ["/w/0", "/w/1", "/w/2", "/w/3", "/w/4", "/w/5"]);
            rounds.push(round);
            t += minutes(1);
            runner.advance(t, |_| true, |_| true).unwrap();
        }
        // A new shuffle each round (fixed seed: deterministic).
        assert!(rounds.windows(2).any(|w| w[0] != w[1]), "{rounds:?}");
    }

    #[test]
    fn random_rounds_start_with_the_wallpaper_shown_and_never_repeat_at_the_seam() {
        for seed in 0..50 {
            let p = from_config(r#"{"delay":1,"order":"random"}"#, 4);
            let t0 = Instant::now();
            let mut runner = PlaylistRunner::new(p, Some(Path::new("/w/2")), t0, SplitMix64(seed));
            let mut t = t0;
            let mut seen = vec![shown(&runner)];
            for _ in 0..11 {
                t += minutes(1);
                seen.push(
                    runner
                        .advance(t, |_| true, |_| true)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            for round in seen.chunks(4) {
                let mut sorted = round.to_vec();
                sorted.sort();
                assert_eq!(
                    sorted,
                    ["/w/0", "/w/1", "/w/2", "/w/3"],
                    "seed {seed}: {seen:?}"
                );
            }
            assert_eq!(seen[0], "/w/2");
            assert!(
                seen.windows(2).all(|w| w[0] != w[1]),
                "seed {seed}: {seen:?}"
            );
        }
    }

    #[test]
    fn other_modes_from_config_never_change_by_themselves() {
        // Spec 07 §17.3: only `timer` changes; next and previous still work.
        let p = from_config(r#"{"delay":1,"mode":"never-heard-of"}"#, 3);
        assert!(!p.settings.timer);
        let t0 = Instant::now();
        let mut runner = PlaylistRunner::new(p, None, t0, SplitMix64(1));
        assert_eq!(runner.deadline(), None);
        assert_eq!(runner.advance(t0 + minutes(600), |_| true, |_| true), None);
        let back = runner.step(t0 + minutes(601), false, |_| true, |_| true);
        assert_eq!(back, Some(PathBuf::from("/w/2")));
        assert_eq!(runner.deadline(), None);
    }

    #[test]
    fn next_and_previous_restart_the_timer() {
        let p = from_config(r#"{"delay":10}"#, 3);
        let t0 = Instant::now();
        let mut runner = PlaylistRunner::new(p, None, t0, SplitMix64(1));
        let t = t0 + minutes(4);
        assert_eq!(
            runner.step(t, true, |_| true, |_| true),
            Some(PathBuf::from("/w/1"))
        );
        assert_eq!(runner.deadline(), Some(t + minutes(10)));
        assert_eq!(
            runner.step(t, false, |_| true, |_| true),
            Some(PathBuf::from("/w/0"))
        );
        assert_eq!(
            runner.step(t, false, |_| true, |_| true),
            Some(PathBuf::from("/w/2"))
        );
        // Backwards skips failed items too.
        assert_eq!(
            runner.step(t, false, |p| p != Path::new("/w/1"), |_| true),
            Some(PathBuf::from("/w/0"))
        );
        // The timer then goes on from the new item.
        assert_eq!(
            runner.advance(t + minutes(10), |_| true, |_| true),
            Some(PathBuf::from("/w/2"))
        );
        let single = from_config(r#"{"delay":10}"#, 1);
        let mut runner = PlaylistRunner::new(single, None, t0, SplitMix64(1));
        assert_eq!(runner.step(t, true, |_| true, |_| true), None);
    }

    #[test]
    fn paused_time_does_not_count_unless_update_on_pause() {
        let t0 = Instant::now();
        let p = from_config(r#"{"delay":10,"updateonpause":false}"#, 2);
        let mut runner = PlaylistRunner::new(p, None, t0, SplitMix64(1));
        runner.set_paused(true, t0 + minutes(4));
        // Nothing to wake up for while paused.
        assert_eq!(runner.deadline(), None);
        assert_eq!(runner.advance(t0 + minutes(30), |_| true, |_| true), None);
        runner.set_paused(true, t0 + minutes(5)); // same state: ignored
        runner.set_paused(false, t0 + minutes(34));
        // 4 minutes played, 6 to go.
        assert_eq!(runner.deadline(), Some(t0 + minutes(40)));
        assert_eq!(runner.advance(t0 + minutes(39), |_| true, |_| true), None);
        assert_eq!(
            runner.advance(t0 + minutes(40), |_| true, |_| true),
            Some(PathBuf::from("/w/1"))
        );

        // A manual change during a pause: the new delay counts from the change.
        runner.set_paused(true, t0 + minutes(41));
        runner.step(t0 + minutes(45), true, |_| true, |_| true);
        runner.set_paused(false, t0 + minutes(50));
        assert_eq!(runner.deadline(), Some(t0 + minutes(60)));

        let p = from_config(r#"{"delay":10,"updateonpause":true}"#, 2);
        let mut runner = PlaylistRunner::new(p, None, t0, SplitMix64(1));
        runner.set_paused(true, t0 + minutes(4));
        assert_eq!(runner.deadline(), Some(t0 + minutes(10)));
        // It changes while paused.
        assert_eq!(
            runner.advance(t0 + minutes(10), |_| true, |_| true),
            Some(PathBuf::from("/w/1"))
        );
        runner.set_paused(false, t0 + minutes(30));
        assert_eq!(runner.deadline(), Some(t0 + minutes(20)));
    }

    #[test]
    fn a_wallpaper_outside_the_playlist_stays_until_the_first_change() {
        let t0 = Instant::now();
        let p = from_config(r#"{"delay":1}"#, 3);
        let mut runner = PlaylistRunner::new(p, Some(Path::new("/elsewhere")), t0, SplitMix64(1));
        assert_eq!(shown(&runner), "/elsewhere");
        assert_eq!(runner.current_item(), None);
        assert_eq!(
            runner.advance(t0 + minutes(1), |_| true, |_| true),
            Some(PathBuf::from("/w/0"))
        );
        assert_eq!(runner.current_item(), Some(0));
        // Starting backwards goes to the end.
        let p = from_config(r#"{"delay":1}"#, 3);
        let mut runner = PlaylistRunner::new(p, Some(Path::new("/elsewhere")), t0, SplitMix64(1));
        assert_eq!(
            runner.step(t0, false, |_| true, |_| true),
            Some(PathBuf::from("/w/2"))
        );
        // A load failure keeps what is shown.
        let p = from_config(r#"{"delay":1}"#, 2);
        let mut runner = PlaylistRunner::new(p, None, t0, SplitMix64(1));
        assert_eq!(runner.advance(t0 + minutes(1), |_| true, |_| false), None);
        assert_eq!(shown(&runner), "/w/0");
    }

    #[test]
    fn catalog_refresh_reads_new_playlists() {
        let text = Arc::new(Mutex::new(
            r#"{"steamuser":{"general":{"playlists":[{"name":"a","items":["/w/x"]}]}}}"#.to_owned(),
        ));
        let source = text.clone();
        let catalog = WeConfigCatalog::new(
            move || Ok(source.lock().unwrap().clone()),
            FakeFs::with(&[("/w/x", PathKind::Directory), ("/w/y", PathKind::Directory)]),
            PathStyle::Posix,
        );
        let names = |list: Vec<Playlist>| list.into_iter().map(|p| p.name).collect::<Vec<_>>();
        assert_eq!(names(catalog.list().unwrap()), ["a"]);
        *text.lock().unwrap() = r#"{"steamuser":{"general":{"playlists":[
            {"name":"a","items":["/w/x"]},{"name":"b","items":["/w/y"],
             "settings":{"transition":true}}]}}}"#
            .to_owned();
        // Plans keep what was read until a client lists the playlists again.
        assert!(PlaylistCatalog::playlist(&catalog, "b").is_err());
        assert_eq!(names(catalog.list().unwrap()), ["a", "b"]);
        let b = PlaylistCatalog::playlist(&catalog, "b").unwrap();
        assert!(b.settings.transition);
        assert_eq!(catalog.first_wallpaper("b").unwrap(), PathBuf::from("/w/y"));
    }

    #[test]
    fn usable_projects_need_type_and_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!project_is_usable(dir.path()));
        std::fs::write(dir.path().join("project.json"), r#"{"type":"scene"}"#).unwrap();
        assert!(!project_is_usable(dir.path()));
        std::fs::write(
            dir.path().join("project.json"),
            r#"{"type":"scene","file":"scene.json"}"#,
        )
        .unwrap();
        assert!(project_is_usable(dir.path()));
        std::fs::write(dir.path().join("project.json"), "{").unwrap();
        assert!(!project_is_usable(dir.path()));
    }
}

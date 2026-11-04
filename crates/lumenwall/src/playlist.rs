//! Wallpaper Engine playlists (spec 07 §17): reading them from Wallpaper Engine's
//! `config.json`, normalising item paths, and the timer that advances them.
//!
//! The timer exposes its next deadline instead of being polled every frame, so the event
//! loop can sleep until it (docs/PERFORMANCE.md).

use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};
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
}

impl Default for PlaylistSettings {
    fn default() -> Self {
        Self {
            delay: Duration::from_secs(60 * 60),
            timer: true,
            random: false,
            update_on_pause: false,
            video_sequence: false,
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

/// [`PlaylistCatalog`] reading Wallpaper Engine's `config.json` on first use, once.
pub struct WeConfigCatalog {
    source: Box<ConfigSource>,
    probe: Box<dyn PathProbe>,
    style: PathStyle,
    loaded: OnceLock<Result<Arc<BTreeMap<String, Playlist>>, PlaylistError>>,
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
            loaded: OnceLock::new(),
        }
    }

    fn playlists(&self) -> Result<&Arc<BTreeMap<String, Playlist>>, PlaylistError> {
        self.loaded
            .get_or_init(|| {
                let json = (self.source)().map_err(|e| PlaylistError::Read(e.to_string()))?;
                match parse_playlists(&json, self.style, self.probe.as_ref()) {
                    // Invalid JSON is logged and yields no playlist (spec 07 §17.1).
                    Err(PlaylistError::InvalidJson(message)) => {
                        tracing::error!("invalid Wallpaper Engine configuration: {message}");
                        Ok(Arc::default())
                    }
                    other => other.map(Arc::new),
                }
            })
            .as_ref()
            .map_err(Clone::clone)
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

/// Advances one playlist on its timer (spec 07 §17.4).
#[derive(Debug)]
pub struct PlaylistRunner<R> {
    playlist: Playlist,
    order: Vec<usize>,
    position: usize,
    failed: Vec<bool>,
    deadline: Instant,
    rng: R,
}

impl<R: Shuffle> PlaylistRunner<R> {
    /// Starts at the item currently shown (`current`), else at the first one.
    pub fn new(playlist: Playlist, current: Option<&Path>, now: Instant, mut rng: R) -> Self {
        let count = playlist.items.len();
        let mut order: Vec<usize> = (0..count).collect();
        if playlist.settings.random {
            shuffle(&mut order, &mut rng);
        }
        let position = current
            .and_then(|c| order.iter().position(|&i| playlist.items[i] == c))
            .unwrap_or(0);
        let deadline = now + playlist.settings.delay;
        Self {
            failed: vec![false; count],
            playlist,
            order,
            position,
            deadline,
            rng,
        }
    }

    /// Only timer playlists with at least two items ever change.
    pub fn is_active(&self) -> bool {
        self.playlist.settings.timer && self.playlist.items.len() >= 2
    }

    /// When [`advance`](Self::advance) has work to do; `None` if never.
    pub fn deadline(&self) -> Option<Instant> {
        self.is_active().then_some(self.deadline)
    }

    /// Item at the current position.
    pub fn current(&self) -> &Path {
        &self.playlist.items[self.order[self.position]]
    }

    /// Call when a fullscreen pause ends: unless `updateonpause`, the pause does not count.
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
        mut precheck: impl FnMut(&Path) -> bool,
        mut load: impl FnMut(&Path) -> bool,
    ) -> Option<PathBuf> {
        if !self.is_active() || now < self.deadline {
            return None;
        }
        self.deadline = now + self.playlist.settings.delay;
        let count = self.order.len();
        self.position = (self.position + 1) % count;
        if self.position == 0 && self.playlist.settings.random {
            shuffle(&mut self.order, &mut self.rng);
        }
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
            self.position = (self.position + 1) % count;
            tries += 1;
        };
        let path = &self.playlist.items[item];
        if load(path) {
            Some(path.clone())
        } else {
            tracing::warn!(item = %path.display(), "cannot load playlist item");
            self.failed[item] = true;
            None
        }
    }
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
}

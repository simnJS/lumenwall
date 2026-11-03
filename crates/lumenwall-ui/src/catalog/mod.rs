//! The wallpaper library: discovery of folders, `project.json` reading, scanning on a
//! background thread.

pub mod meta;
pub mod project;
pub mod steam;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

pub use project::{Kind, Project, PropertyDef, PropertyKind};
pub use steam::{Discovery, NativeDiscovery, SteamInstall};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Workshop,
    /// Shipped with Wallpaper Engine or made in its editor.
    WallpaperEngine,
    /// A folder the user added.
    Local,
}

#[derive(Debug, Clone)]
pub struct Wallpaper {
    pub dir: PathBuf,
    pub project: Project,
    pub preview: Option<PathBuf>,
    pub origin: Origin,
    /// When the folder appeared (Workshop download or copy), for "recently added".
    pub added: SystemTime,
    pub resolution: Option<(u32, u32)>,
    /// Whether it can make sound ([`meta::Meta::sound`]); `None` until the headers are read.
    pub sound: Option<bool>,
    /// Lower-case title, tags and id, for search.
    pub search_text: String,
}

/// Why a wallpaper cannot be played as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocker {
    /// Scenes need the assets of an installed Wallpaper Engine.
    NeedsWallpaperEngine,
    /// Applications are never run; unknown types cannot be played.
    Unsupported,
}

impl Wallpaper {
    pub fn blocker(&self, has_we_assets: bool) -> Option<Blocker> {
        match self.project.kind {
            Kind::Scene if !has_we_assets => Some(Blocker::NeedsWallpaperEngine),
            Kind::Application | Kind::Unknown => Some(Blocker::Unsupported),
            _ => None,
        }
    }

    /// Whether to offer a sound level for it: what the headers say, else what its type allows.
    pub fn may_have_sound(&self) -> bool {
        self.sound.unwrap_or(matches!(
            self.project.kind,
            Kind::Scene | Kind::Video | Kind::Web
        ))
    }

    pub fn title(&self) -> &str {
        if self.project.title.is_empty() {
            self.dir.file_name().and_then(|n| n.to_str()).unwrap_or("?")
        } else {
            &self.project.title
        }
    }
}

/// Resolution classes offered by the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResClass {
    UpTo1080,
    Qhd,
    Uhd,
    /// Web wallpapers adapt; others could not be read.
    Unknown,
}

impl ResClass {
    pub const ALL: [ResClass; 4] = [
        ResClass::UpTo1080,
        ResClass::Qhd,
        ResClass::Uhd,
        ResClass::Unknown,
    ];

    pub fn of(res: Option<(u32, u32)>) -> Self {
        match res {
            None => ResClass::Unknown,
            Some((w, h)) => match w.min(h) {
                0..=1200 => ResClass::UpTo1080,
                1201..=1700 => ResClass::Qhd,
                _ => ResClass::Uhd,
            },
        }
    }
}

/// Everything the library knows after a scan.
#[derive(Debug, Default, Clone)]
pub struct Library {
    pub steam: SteamInstall,
    pub wallpapers: Vec<Wallpaper>,
    /// Folders that could not be read, with the error.
    pub problems: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Done(Library),
    /// Headers read after the first results, so the grid shows up at once.
    Meta(Vec<(PathBuf, meta::Meta)>),
}

/// Scans in a background thread and reports through `send` (false: receiver gone).
pub fn scan_in_background(
    discovery: Arc<dyn Discovery>,
    folders: Vec<PathBuf>,
    send: impl Fn(ScanEvent) -> bool + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("lumenwall-ui-scan".into())
        .spawn(move || {
            let library = scan(&*discovery, &folders);
            let todo: Vec<(PathBuf, Kind, Option<String>)> = library
                .wallpapers
                .iter()
                .map(|w| (w.dir.clone(), w.project.kind, w.project.file.clone()))
                .collect();
            if !send(ScanEvent::Done(library)) {
                return;
            }
            let found: Vec<_> = todo
                .into_iter()
                .map(|(dir, kind, file)| {
                    let m = meta::read(&dir, kind, file.as_deref());
                    (dir, m)
                })
                .collect();
            if !found.is_empty() {
                send(ScanEvent::Meta(found));
            }
        });
    if let Err(e) = spawned {
        tracing::error!("cannot start the library scan: {e}");
    }
}

pub fn scan(discovery: &dyn Discovery, folders: &[PathBuf]) -> Library {
    let steam = discovery.discover();
    let mut lib = Library::default();
    let mut roots: Vec<(PathBuf, Origin)> = Vec::new();
    roots.extend(
        steam
            .workshop_dirs
            .iter()
            .map(|d| (d.clone(), Origin::Workshop)),
    );
    roots.extend(
        steam
            .we_project_dirs()
            .into_iter()
            .map(|d| (d, Origin::WallpaperEngine)),
    );
    roots.extend(folders.iter().map(|d| (d.clone(), Origin::Local)));
    for (root, origin) in roots {
        if root.join("project.json").is_file() {
            add(&mut lib, &root, origin);
            continue;
        }
        match std::fs::read_dir(&root) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let dir = entry.path();
                    if dir.join("project.json").is_file() {
                        add(&mut lib, &dir, origin);
                    }
                }
            }
            Err(e) => lib.problems.push((root.clone(), e.to_string())),
        }
    }
    // The same folder can be reached twice (a local folder inside a library).
    lib.wallpapers.sort_by(|a, b| a.dir.cmp(&b.dir));
    lib.wallpapers.dedup_by(|a, b| a.dir == b.dir);
    lib.steam = steam;
    lib
}

fn add(lib: &mut Library, dir: &Path, origin: Origin) {
    match load(dir, origin) {
        Ok(w) => lib.wallpapers.push(w),
        Err(e) => lib.problems.push((dir.to_owned(), e)),
    }
}

pub fn load(dir: &Path, origin: Origin) -> Result<Wallpaper, String> {
    let text = std::fs::read_to_string(dir.join("project.json")).map_err(|e| e.to_string())?;
    let project = project::parse(&text)?;
    let preview = project
        .preview
        .as_deref()
        .map(|p| dir.join(p))
        .filter(|p| p.is_file())
        .or_else(|| {
            ["preview.gif", "preview.jpg", "preview.png", "preview.webp"]
                .iter()
                .map(|n| dir.join(n))
                .find(|p| p.is_file())
        });
    let meta = std::fs::metadata(dir).map_err(|e| e.to_string())?;
    let added = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let mut search_text = project.title.to_lowercase();
    for t in &project.tags {
        search_text.push('\n');
        search_text.push_str(&t.to_lowercase());
    }
    if let Some(id) = &project.workshop_id {
        search_text.push('\n');
        search_text.push_str(id);
    }
    Ok(Wallpaper {
        dir: std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned()),
        project,
        preview,
        origin,
        added,
        resolution: None,
        sound: None,
        search_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(SteamInstall);
    impl Discovery for Fixed {
        fn discover(&self) -> SteamInstall {
            self.0.clone()
        }
    }

    #[test]
    fn scans_workshop_and_local_folders() {
        let tmp = std::env::temp_dir().join(format!("lumenwall-ui-scan-{}", std::process::id()));
        let ws = tmp.join("431960");
        for (id, json) in [
            (
                "1",
                r#"{"title":"Scene A","type":"scene","tags":["Anime"],"preview":"preview.gif"}"#,
            ),
            ("2", r#"{"title":"Video B","type":"video","file":"b.mp4"}"#),
            ("3", "not json"),
        ] {
            std::fs::create_dir_all(ws.join(id)).unwrap();
            std::fs::write(ws.join(id).join("project.json"), json).unwrap();
        }
        std::fs::write(ws.join("1/preview.gif"), b"GIF89a").unwrap();
        let single = tmp.join("mine");
        std::fs::create_dir_all(&single).unwrap();
        std::fs::write(
            single.join("project.json"),
            r#"{"title":"Mine","type":"web"}"#,
        )
        .unwrap();

        let discovery = Fixed(SteamInstall {
            workshop_dirs: vec![ws.clone()],
            ..Default::default()
        });
        let lib = scan(&discovery, &[single.clone(), tmp.join("missing")]);
        let mut titles: Vec<&str> = lib.wallpapers.iter().map(Wallpaper::title).collect();
        titles.sort_unstable();
        assert_eq!(titles, ["Mine", "Scene A", "Video B"]);
        assert_eq!(lib.problems.len(), 2, "{:?}", lib.problems);
        let a = lib
            .wallpapers
            .iter()
            .find(|w| w.title() == "Scene A")
            .unwrap();
        assert!(a.preview.as_ref().unwrap().ends_with("preview.gif"));
        assert!(a.search_text.contains("anime"));
        assert_eq!(a.blocker(false), Some(Blocker::NeedsWallpaperEngine));
        assert_eq!(a.blocker(true), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolution_classes() {
        assert_eq!(ResClass::of(Some((1920, 1080))), ResClass::UpTo1080);
        assert_eq!(ResClass::of(Some((2560, 1440))), ResClass::Qhd);
        assert_eq!(ResClass::of(Some((3840, 2160))), ResClass::Uhd);
        assert_eq!(ResClass::of(Some((1080, 1920))), ResClass::UpTo1080);
        assert_eq!(ResClass::of(None), ResClass::Unknown);
    }

    /// Reads the real Workshop folder when `LUMENWALL_WE_WORKSHOP` is set (docs/TESTING.md).
    #[test]
    #[ignore = "needs LUMENWALL_WE_WORKSHOP"]
    fn real_workshop_folder() {
        let lib = scan(&NativeDiscovery, &[]);
        assert!(!lib.wallpapers.is_empty(), "{:?}", lib.steam);
        for w in &lib.wallpapers {
            println!(
                "{:?} {:?} {:?} {}",
                w.project.kind,
                w.project.workshop_id,
                w.preview,
                w.title()
            );
        }
    }
}

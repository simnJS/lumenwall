//! Steam folder discovery: workshop content, Wallpaper Engine installation and the
//! `assets` folder (spec 01 §5).
//!
//! Linux follows the spec's fixed candidate lists first (native, `~/.steam`,
//! Flatpak, Snap), so the same folder wins as in the reference; only when none
//! matches are the additional libraries declared in `libraryfolders.vdf` (and the
//! installations `steamlocate` finds) searched. Windows reads the registry
//! (QO-30 proposal).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

use thiserror::Error;

/// Steam application id of Wallpaper Engine.
pub const WALLPAPER_ENGINE_APP_ID: u32 = 431_960;

/// Installation folder name of Wallpaper Engine under `steamapps/common`.
pub const INSTALL_DIR_NAME: &str = "wallpaper_engine";

/// Discovery failures.
#[derive(Debug, Error)]
pub enum SteamError {
    /// `HOME` is not set (§5.5).
    #[error("home folder not found (HOME is not set)")]
    HomeNotSet,
    /// `HOME` is not an existing folder (§5.5).
    #[error("{} is not a directory", .0.display())]
    HomeNotDir(PathBuf),
    /// No workshop folder for this id (§5.2).
    #[error("workshop folder not found for application 431960 and content \"{id}\"")]
    WorkshopNotFound {
        /// Workshop id.
        id: String,
    },
    /// No installation sub-folder (§5.3).
    #[error("folder not found for application {INSTALL_DIR_NAME}: \"{sub}\"")]
    InstallNotFound {
        /// Requested sub-path.
        sub: String,
    },
    /// The executable path is unknown (assets fallback, §5.4).
    #[error("cannot locate the executable: {0}")]
    CurrentExe(#[source] io::Error),
}

/// Positional wallpaper argument (§5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperArg {
    /// No path separator: a workshop id (not checked to be numeric).
    WorkshopId(String),
    /// A path, used as is (relative to the current directory if relative).
    Path(PathBuf),
}

impl WallpaperArg {
    /// Classifies an argument: anything without a separator is a workshop id. On
    /// Windows `\` also counts as a separator (QO-30).
    pub fn parse(arg: &str) -> Self {
        let has_separator = arg.contains('/') || (cfg!(windows) && arg.contains('\\'));
        if has_separator {
            Self::Path(PathBuf::from(arg))
        } else {
            Self::WorkshopId(arg.to_owned())
        }
    }
}

/// Which search order to use: the first two Linux candidates are swapped between
/// workshop and installation lookups (§5.2, §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    /// Workshop content.
    Workshop,
    /// Installation folder.
    Install,
}

#[cfg(not(windows))]
const LINUX_ROOTS_WORKSHOP: [&str; 4] = [
    ".local/share/Steam",
    ".steam/steam",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam",
    "snap/steam/common/.local/share/Steam",
];

#[cfg(not(windows))]
const LINUX_ROOTS_INSTALL: [&str; 4] = [
    ".steam/steam",
    ".local/share/Steam",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam",
    "snap/steam/common/.local/share/Steam",
];

/// Locates Steam folders.
#[derive(Debug, Clone)]
pub struct SteamLocator {
    home: Option<PathBuf>,
    extra_roots: Vec<PathBuf>,
    system_discovery: bool,
}

impl Default for SteamLocator {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SteamLocator {
    /// Uses `HOME` (read lazily, only when a search happens) and system discovery
    /// (`steamlocate`, registry on Windows).
    pub fn from_env() -> Self {
        Self {
            home: None,
            extra_roots: Vec::new(),
            system_discovery: true,
        }
    }

    /// Uses `home` instead of `HOME` and disables system discovery (tests, tools).
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: Some(home.into()),
            extra_roots: Vec::new(),
            system_discovery: false,
        }
    }

    /// Adds Steam root folders searched after the standard ones.
    pub fn with_extra_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.extra_roots.extend(roots);
        self
    }

    /// Enables or disables system discovery.
    pub fn with_system_discovery(mut self, enabled: bool) -> Self {
        self.system_discovery = enabled;
        self
    }

    /// Home folder `H` (§5.5).
    pub fn home(&self) -> Result<PathBuf, SteamError> {
        let home = match &self.home {
            Some(home) => home.clone(),
            None => env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
                .ok_or(SteamError::HomeNotSet)?,
        };
        if home.is_dir() {
            Ok(home)
        } else {
            Err(SteamError::HomeNotDir(home))
        }
    }

    /// Steam root folders in priority order: the spec's fixed candidates first
    /// (Linux), or the registry entries (Windows), then extra and discovered roots.
    pub fn steam_roots(&self, kind: SearchKind) -> Result<Vec<PathBuf>, SteamError> {
        let mut roots = self.primary_roots(kind)?;
        roots.extend(self.extra_roots.iter().cloned());
        if self.system_discovery
            && let Ok(found) = steamlocate::locate_all()
        {
            roots.extend(found.into_iter().map(|dir| dir.path().to_path_buf()));
        }
        Ok(dedup_paths(roots))
    }

    #[cfg(not(windows))]
    fn primary_roots(&self, kind: SearchKind) -> Result<Vec<PathBuf>, SteamError> {
        let home = self.home()?;
        let list = match kind {
            SearchKind::Workshop => LINUX_ROOTS_WORKSHOP,
            SearchKind::Install => LINUX_ROOTS_INSTALL,
        };
        Ok(list.iter().map(|rel| home.join(rel)).collect())
    }

    #[cfg(windows)]
    fn primary_roots(&self, _kind: SearchKind) -> Result<Vec<PathBuf>, SteamError> {
        let mut roots = Vec::new();
        if self.system_discovery {
            use winreg::RegKey;
            use winreg::enums::HKEY_CURRENT_USER;
            if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam")
                && let Ok(path) = key.get_value::<String, _>("SteamPath")
            {
                roots.push(PathBuf::from(path));
            }
            if let Ok(dir) = steamlocate::locate() {
                roots.push(dir.path().to_path_buf());
            }
            roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
        }
        if let Some(home) = &self.home {
            roots.push(home.join("Steam"));
        }
        Ok(roots)
    }

    /// Additional libraries declared in each root's `steamapps/libraryfolders.vdf`,
    /// in file order, without the roots themselves.
    pub fn additional_libraries(&self, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut libraries = Vec::new();
        for root in roots {
            let Ok(dir) = steamlocate::SteamDir::from_dir(root) else {
                continue;
            };
            match dir.library_paths() {
                Ok(paths) => libraries.extend(paths),
                Err(e) => tracing::debug!(root = %root.display(), error = %e, "no library list"),
            }
        }
        let known: HashSet<PathBuf> = roots.iter().map(|p| canonical_key(p)).collect();
        dedup_paths(libraries)
            .into_iter()
            .filter(|p| !known.contains(&canonical_key(p)))
            .collect()
    }

    /// First existing folder `<library>/<rel>`, roots first, then additional
    /// libraries (read only when the roots fail).
    fn find_in_libraries(
        &self,
        kind: SearchKind,
        rel: &Path,
    ) -> Result<Option<PathBuf>, SteamError> {
        let roots = self.steam_roots(kind)?;
        if let Some(found) = roots.iter().map(|r| r.join(rel)).find(|c| c.is_dir()) {
            return Ok(Some(found));
        }
        Ok(self
            .additional_libraries(&roots)
            .into_iter()
            .map(|l| l.join(rel))
            .find(|c| c.is_dir()))
    }

    /// Workshop folder of item `id` (§5.2):
    /// `<library>/steamapps/workshop/content/431960/<id>`.
    pub fn workshop_dir(&self, id: &str) -> Result<PathBuf, SteamError> {
        let not_found = || SteamError::WorkshopNotFound { id: id.to_owned() };
        // An empty, `.` or `..` id would name the content folder or its parent.
        if id.is_empty() || id == "." || id == ".." {
            return Err(not_found());
        }
        let rel = Path::new("steamapps/workshop/content")
            .join(WALLPAPER_ENGINE_APP_ID.to_string())
            .join(id);
        self.find_in_libraries(SearchKind::Workshop, &rel)?
            .ok_or_else(not_found)
    }

    /// Installation sub-folder `sub` (§5.3):
    /// `<library>/steamapps/common/wallpaper_engine/<sub>`; empty `sub` gives the
    /// installation folder itself.
    pub fn install_dir(&self, sub: &str) -> Result<PathBuf, SteamError> {
        let mut rel = Path::new("steamapps/common").join(INSTALL_DIR_NAME);
        if !sub.is_empty() {
            rel.push(sub);
        }
        self.find_in_libraries(SearchKind::Install, &rel)?
            .ok_or_else(|| SteamError::InstallNotFound {
                sub: sub.to_owned(),
            })
    }

    /// Path of a wallpaper argument (§5.1): workshop ids go through
    /// [`workshop_dir`](Self::workshop_dir), paths are returned as is.
    pub fn resolve_wallpaper(&self, arg: &str) -> Result<PathBuf, SteamError> {
        match WallpaperArg::parse(arg) {
            WallpaperArg::WorkshopId(id) => self.workshop_dir(&id),
            WallpaperArg::Path(path) => Ok(path),
        }
    }
}

/// Where the assets folder came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetsDirOrigin {
    /// `--assets-dir`.
    CommandLine,
    /// Wallpaper Engine installation.
    Installation,
    /// `assets` next to the executable.
    ExecutableDir,
}

/// The chosen assets folder; it is only checked when mounted (§4.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetsDir {
    /// Folder (or `.pkg`) to mount.
    pub path: PathBuf,
    /// Origin.
    pub origin: AssetsDirOrigin,
}

/// Picks the assets folder (§5.4): `--assets-dir` when non-empty (unchecked), else
/// the installation's `assets`, else `assets` next to the canonical executable.
pub fn resolve_assets_dir(
    cli: Option<&Path>,
    locator: &SteamLocator,
) -> Result<AssetsDir, SteamError> {
    if let Some(path) = cli.filter(|p| !p.as_os_str().is_empty()) {
        tracing::info!(path = %path.display(), "assets folder from --assets-dir");
        return Ok(AssetsDir {
            path: path.to_path_buf(),
            origin: AssetsDirOrigin::CommandLine,
        });
    }
    match locator.install_dir("assets") {
        Ok(path) => Ok(AssetsDir {
            path,
            origin: AssetsDirOrigin::Installation,
        }),
        Err(e) => {
            tracing::debug!(error = %e, "no installation assets folder; using the executable's folder");
            let exe = env::current_exe().map_err(SteamError::CurrentExe)?;
            let exe = fs::canonicalize(&exe).unwrap_or(exe);
            let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
            Ok(AssetsDir {
                path: dir.join("assets"),
                origin: AssetsDirOrigin::ExecutableDir,
            })
        }
    }
}

fn canonical_key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Removes later duplicates (same canonical folder, e.g. `~/.steam/steam` linking
/// to `~/.local/share/Steam`), keeping order.
fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|p| seen.insert(canonical_key(p)))
        .collect()
}

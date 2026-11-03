//! Asset access: wallpaper resolution (Workshop id or path, Steam discovery including the Snap
//! install), the assets folder, and the adapters from the `lumen-assets` VFS to the readers of
//! the other crates.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lumen_assets::steam::{SteamLocator, WallpaperArg, resolve_assets_dir};
use lumen_assets::{Vfs, WallpaperVfsOptions};

/// Turns a Workshop id or a path into a wallpaper folder (spec 01 §5.1, spec 07 §3.1).
#[derive(Debug, Clone, Default)]
pub struct WallpaperResolver {
    locator: SteamLocator,
}

impl WallpaperResolver {
    /// Uses `HOME`, the standard Steam roots (native, `~/.steam`, Flatpak, Snap), their
    /// `libraryfolders.vdf` libraries and `steamlocate`.
    pub fn new() -> Self {
        Self {
            locator: SteamLocator::from_env(),
        }
    }

    /// Resolver with an explicit locator (tests).
    pub fn with_locator(locator: SteamLocator) -> Self {
        Self { locator }
    }

    /// Workshop folder of `id`.
    pub fn workshop_dir(&self, id: &str) -> Result<PathBuf, String> {
        self.locator.workshop_dir(id).map_err(|e| e.to_string())
    }

    /// A value without `/` is a Workshop id, anything else a path used as is.
    pub fn resolve(&self, arg: &str) -> Result<PathBuf, String> {
        match WallpaperArg::parse(arg) {
            WallpaperArg::WorkshopId(id) => self.workshop_dir(&id),
            WallpaperArg::Path(p) => {
                if p.is_dir() || p.is_file() {
                    Ok(p)
                } else {
                    Err(format!("wallpaper folder '{}' not found", p.display()))
                }
            }
        }
    }

    /// The Wallpaper Engine installation folder (playlists in `config.json`).
    pub fn install_dir(&self) -> Result<PathBuf, String> {
        self.locator.install_dir("").map_err(|e| e.to_string())
    }

    /// The `assets` folder: `--assets-dir`, else the installation's, else next to the binary.
    pub fn assets_dir(&self, cli: Option<&Path>) -> Result<PathBuf, String> {
        resolve_assets_dir(cli, &self.locator)
            .map(|a| a.path)
            .map_err(|e| e.to_string())
    }
}

/// Opens the VFS of a wallpaper (§4.8 mount order).
pub fn open_vfs(wallpaper: &Path, assets: &Path) -> Result<Vfs, String> {
    let options = WallpaperVfsOptions::default();
    Vfs::for_wallpaper(wallpaper, assets, &options).map_err(|e| e.to_string())
}

/// `lumen-scene` reader over the VFS (archive entries are borrowed, never copied).
pub struct SceneSource<'a>(pub &'a Vfs);

impl lumen_scene::AssetSource for SceneSource<'_> {
    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>, lumen_scene::AssetError> {
        self.0.read(path).map_err(|e| {
            if e.is_not_found() {
                lumen_scene::AssetError::NotFound
            } else {
                lumen_scene::AssetError::Io(std::io::Error::other(e.to_string()))
            }
        })
    }

    fn exists(&self, path: &str) -> bool {
        self.0.exists(path)
    }
}

/// Bytes of a VFS file kept alive without copying: archive entries are read again from the
/// shared mapping on access; loose files are read once into memory.
pub struct VfsBytes {
    vfs: Arc<Vfs>,
    path: String,
    owned: Option<Vec<u8>>,
}

impl VfsBytes {
    /// `None` when the file does not resolve.
    pub fn open(vfs: &Arc<Vfs>, path: &str) -> Option<Self> {
        match vfs.read(path).ok()? {
            Cow::Borrowed(_) => Some(Self {
                vfs: vfs.clone(),
                path: path.to_owned(),
                owned: None,
            }),
            Cow::Owned(v) => Some(Self {
                vfs: vfs.clone(),
                path: path.to_owned(),
                owned: Some(v),
            }),
        }
    }
}

impl AsRef<[u8]> for VfsBytes {
    fn as_ref(&self) -> &[u8] {
        if let Some(v) = &self.owned {
            return v;
        }
        match self.vfs.read(&self.path) {
            Ok(Cow::Borrowed(b)) => b,
            _ => &[],
        }
    }
}

#[cfg(feature = "we-shaders")]
/// `lumen-shader` file access over the VFS.
pub struct ShaderSource<'a>(pub &'a Vfs);

#[cfg(feature = "we-shaders")]
impl lumen_shader::ShaderFs for ShaderSource<'_> {
    fn read(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        self.0.read(path).ok()
    }
}

//! Virtual file system: adapters, mount points and lookup (spec 01 §4).

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lumenwall_formats::pkg::{OpenMode, PkgArchive};
use lumenwall_formats::tex::{self, ParseOptions, Tex, TexError};

use crate::builtin::builtin_files;
use crate::error::AssetError;
use crate::path::{
    ShaderStage, normalize, normalize_with, rooted_relative, shader_relative_name,
    texture_json_path, texture_path, workshop_compat_path,
};

/// Source string and root of the media thumbnail mount (§4.5).
pub const MEDIA_THUMBNAIL: &str = "$mediaThumbnail";

/// Provider of the artwork URL of the media being played (MPRIS `mpris:artUrl` on
/// Linux). Implemented by the platform layer.
pub trait MediaArtSource: Send + Sync {
    /// Current artwork URL, if any.
    fn art_url(&self) -> Option<String>;
}

/// Memory adapter content: normalized path → bytes (§4.4).
#[derive(Debug, Clone, Default)]
pub struct MemoryFiles {
    files: HashMap<String, Cow<'static, [u8]>>,
}

impl MemoryFiles {
    /// Adds or replaces a file. The key is normalized, so paths compare by component.
    pub fn insert(&mut self, path: &str, content: impl Into<Cow<'static, [u8]>>) {
        let key = normalize_with(path, false).into_owned();
        self.files.insert(key, content.into());
    }

    /// Adds or replaces a file with static content (no copy).
    pub fn insert_static(&mut self, path: &str, content: &'static [u8]) {
        self.insert(path, Cow::Borrowed(content));
    }

    /// Content of a file.
    pub fn get(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|c| &**c)
    }

    /// Number of files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether there is no file.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Adapter over a real folder (§4.2).
#[derive(Debug, Clone)]
pub struct DirectoryAdapter {
    root: PathBuf,
}

impl DirectoryAdapter {
    /// Adapter rooted at the canonical form of `path`, which must be a folder.
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let root = fs::canonicalize(path)?;
        if !root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        Ok(Self { root })
    }

    /// Canonical root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Canonical target of `rel` if it exists and stays inside the root. Containment
    /// is checked by path component, not by string prefix (QO-20).
    fn contained(&self, rel: &str) -> Option<PathBuf> {
        let canonical = fs::canonicalize(self.root.join(rel)).ok()?;
        canonical.starts_with(&self.root).then_some(canonical)
    }

    fn exists(&self, rel: &str) -> bool {
        self.contained(rel).is_some_and(|p| p.is_file())
    }

    fn open(&self, rel: &str, query: &str) -> Result<Vec<u8>, AssetError> {
        let path = self
            .contained(rel)
            .ok_or_else(|| AssetError::not_found(query, "missing or outside the folder"))?;
        if !path.is_file() {
            return Err(AssetError::not_found(query, "directory instead of a file"));
        }
        // Rust always opens files in binary mode (QO-21).
        fs::read(&path).map_err(|source| AssetError::Io {
            path: query.to_owned(),
            source,
        })
    }

    fn physical_path(&self, rel: &str, query: &str) -> Result<PathBuf, AssetError> {
        self.contained(rel)
            .ok_or_else(|| AssetError::not_found(query, "missing or outside the folder"))
    }
}

/// Adapter serving the artwork of the media being played (§4.5).
#[derive(Clone, Default)]
pub struct MediaThumbnailAdapter {
    source: Option<Arc<dyn MediaArtSource>>,
}

impl fmt::Debug for MediaThumbnailAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaThumbnailAdapter")
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl MediaThumbnailAdapter {
    /// Adapter reading URLs from `source` (no source: never any artwork).
    pub fn new(source: Option<Arc<dyn MediaArtSource>>) -> Self {
        Self { source }
    }

    fn open(&self, rel: &str, query: &str) -> Result<Vec<u8>, AssetError> {
        if !rel.is_empty() {
            return Err(AssetError::not_found(query, "not the media thumbnail"));
        }
        let url = self
            .source
            .as_ref()
            .and_then(|s| s.art_url())
            .ok_or_else(|| AssetError::not_found(query, "no artwork for the current media"))?;
        let encoded = url
            .strip_prefix("file://")
            .ok_or_else(|| AssetError::not_found(query, "artwork URL is not a file:// URL"))?;
        // Percent-escapes are decoded (the reference does not, QO-23).
        let path = path_from_bytes(percent_decode(encoded));
        let path = std::path::absolute(&path)
            .map_err(|_| AssetError::not_found(query, "artwork path cannot be made absolute"))?;
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            _ => return Err(AssetError::not_found(query, "artwork file missing")),
        }
        fs::read(&path).map_err(|source| AssetError::Io {
            path: query.to_owned(),
            source,
        })
    }
}

fn percent_decode(input: &str) -> Vec<u8> {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// The four adapter kinds of §4.1.1.
#[derive(Debug)]
pub enum Adapter {
    /// In-memory files.
    Memory(MemoryFiles),
    /// `.pkg` archive.
    Archive(PkgArchive),
    /// Real folder.
    Directory(DirectoryAdapter),
    /// Media artwork.
    MediaThumbnail(MediaThumbnailAdapter),
}

impl Adapter {
    /// Whether `rel` exists in this adapter.
    pub fn exists(&self, rel: &str) -> bool {
        match self {
            Self::Memory(files) => files.files.contains_key(rel),
            Self::Archive(pkg) => pkg.contains(rel),
            Self::Directory(dir) => dir.exists(rel),
            Self::MediaThumbnail(_) => rel.is_empty(),
        }
    }

    /// Contents of `rel`; archive and memory contents are borrowed. `query` is used
    /// in error messages.
    pub fn open(&self, rel: &str, query: &str) -> Result<Cow<'_, [u8]>, AssetError> {
        match self {
            Self::Memory(files) => files
                .get(rel)
                .map(Cow::Borrowed)
                .ok_or_else(|| AssetError::not_found(query, "not in memory files")),
            // A.4: an out-of-bounds entry still exists (lookup stopped here) but
            // fails alone with an `Invalid` error; other entries stay readable.
            Self::Archive(pkg) => match pkg.read(rel) {
                None => Err(AssetError::not_found(query, "not in archive")),
                Some(Ok(data)) => Ok(Cow::Borrowed(data)),
                Some(Err(source)) => Err(AssetError::PkgEntry {
                    path: query.to_owned(),
                    source,
                }),
            },
            Self::Directory(dir) => dir.open(rel, query).map(Cow::Owned),
            Self::MediaThumbnail(thumb) => thumb.open(rel, query).map(Cow::Owned),
        }
    }

    /// Path on disk of `rel`; only folders have one.
    pub fn physical_path(&self, rel: &str, query: &str) -> Result<PathBuf, AssetError> {
        match self {
            Self::Directory(dir) => dir.physical_path(rel, query),
            Self::Memory(_) | Self::Archive(_) => Err(AssetError::not_found(
                query,
                "no physical path for this mount",
            )),
            // QO-24: `NotFound` rather than a generic error.
            Self::MediaThumbnail(_) => Err(AssetError::not_found(
                query,
                "no physical path for the media thumbnail",
            )),
        }
    }
}

/// A mount point: a virtual root prefix and an adapter.
#[derive(Debug)]
pub struct Mount {
    root: String,
    adapter: Adapter,
}

impl Mount {
    /// Virtual root (`/` or `$mediaThumbnail`).
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The adapter.
    pub fn adapter(&self) -> &Adapter {
        &self.adapter
    }
}

/// Options of [`Vfs::for_wallpaper`].
#[derive(Clone)]
pub struct WallpaperVfsOptions {
    /// Artwork source for `$mediaThumbnail`.
    pub media: Option<Arc<dyn MediaArtSource>>,
    /// Mount the process's current directory last (§4.8 priority 7, QO-27).
    pub mount_current_dir: bool,
    /// How `.pkg` archives are loaded.
    pub pkg_open_mode: OpenMode,
}

impl Default for WallpaperVfsOptions {
    fn default() -> Self {
        Self {
            media: None,
            mount_current_dir: true,
            pkg_open_mode: OpenMode::Map,
        }
    }
}

impl fmt::Debug for WallpaperVfsOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WallpaperVfsOptions")
            .field("media", &self.media.is_some())
            .field("mount_current_dir", &self.mount_current_dir)
            .field("pkg_open_mode", &self.pkg_open_mode)
            .finish()
    }
}

/// Ordered list of mount points of one wallpaper (§4.1).
#[derive(Debug)]
pub struct Vfs {
    mounts: Vec<Mount>,
    media: MediaThumbnailAdapter,
    pkg_open_mode: OpenMode,
    generation: u64,
}

/// A value no other VFS of the process has had.
fn next_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Default for Vfs {
    fn default() -> Self {
        Self {
            mounts: Vec::new(),
            media: MediaThumbnailAdapter::default(),
            pkg_open_mode: OpenMode::default(),
            generation: next_generation(),
        }
    }
}

impl Vfs {
    /// Empty VFS.
    pub fn new() -> Self {
        Self::default()
    }

    /// Identity of what this VFS serves: unique in the process, and renewed by every
    /// change of its mounts. Readers may keep what they read under one generation
    /// (shader units and includes during a scene load) instead of reading it again;
    /// files of folder mounts are assumed not to change meanwhile.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Sets the artwork source used when `$mediaThumbnail` is mounted.
    pub fn set_media_source(&mut self, source: Option<Arc<dyn MediaArtSource>>) {
        self.media = MediaThumbnailAdapter::new(source);
        self.generation = next_generation();
    }

    /// Sets how `.pkg` archives mounted afterwards are loaded.
    pub fn set_pkg_open_mode(&mut self, mode: OpenMode) {
        self.pkg_open_mode = mode;
    }

    /// Mount points in priority order.
    pub fn mounts(&self) -> &[Mount] {
        &self.mounts
    }

    /// Mounts `source` at `root`, at the end of the list (§4.1.2). Factories are
    /// tried in order: memory (`virtual`), archive (`.pkg` file), folder,
    /// `$mediaThumbnail`. A factory that accepts but fails propagates its error.
    pub fn mount(&mut self, source: impl AsRef<Path>, root: &str) -> Result<(), AssetError> {
        let adapter = self.create_adapter(source.as_ref())?;
        self.mount_adapter(root, adapter);
        Ok(())
    }

    /// Appends an already-built adapter.
    pub fn mount_adapter(&mut self, root: &str, adapter: Adapter) {
        self.mounts.push(Mount {
            root: root.to_owned(),
            adapter,
        });
        self.generation = next_generation();
    }

    fn create_adapter(&self, source: &Path) -> Result<Adapter, AssetError> {
        // QO-19: the exact string `virtual` creates an empty memory adapter.
        if source.as_os_str() == "virtual" {
            return Ok(Adapter::Memory(MemoryFiles::default()));
        }
        if let Ok(canonical) = fs::canonicalize(source)
            && let Ok(meta) = fs::metadata(&canonical)
        {
            // Extension checked on the resolved path, case-sensitive.
            if meta.is_file() && canonical.extension().is_some_and(|e| e == "pkg") {
                return PkgArchive::open_with(&canonical, self.pkg_open_mode)
                    .map(Adapter::Archive)
                    .map_err(|source| AssetError::Pkg {
                        path: canonical,
                        source,
                    });
            }
            if meta.is_dir() {
                return Ok(Adapter::Directory(DirectoryAdapter { root: canonical }));
            }
        }
        if source.as_os_str() == MEDIA_THUMBNAIL {
            return Ok(Adapter::MediaThumbnail(self.media.clone()));
        }
        Err(AssetError::not_found(
            source.to_string_lossy(),
            "no adapter can mount this path",
        ))
    }

    /// VFS of a wallpaper, mounted in the §4.8 order: built-in memory files,
    /// `$mediaThumbnail`, the wallpaper (folder or `.pkg`; errors propagate),
    /// `scene.pkg` and `gifscene.pkg` (skipped when absent, logged when invalid),
    /// the assets folder (fatal on failure), then the current directory.
    pub fn for_wallpaper(
        wallpaper: &Path,
        assets_dir: &Path,
        options: &WallpaperVfsOptions,
    ) -> Result<Self, AssetError> {
        let mut vfs = Self::new();
        vfs.set_media_source(options.media.clone());
        vfs.set_pkg_open_mode(options.pkg_open_mode);

        vfs.mount_adapter("/", Adapter::Memory(builtin_files()));
        vfs.mount_adapter(MEDIA_THUMBNAIL, Adapter::MediaThumbnail(vfs.media.clone()));
        vfs.mount(wallpaper, "/")?;
        for name in ["scene.pkg", "gifscene.pkg"] {
            let path = wallpaper.join(name);
            match vfs.mount(&path, "/") {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                // QO-32: an invalid archive is skipped with a log line.
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "ignoring invalid archive")
                }
            }
        }
        vfs.mount(assets_dir, "/")
            .map_err(|source| AssetError::NoAssetsDir {
                path: fs::canonicalize(assets_dir).unwrap_or_else(|_| assets_dir.to_path_buf()),
                source: Box::new(source),
            })?;
        if options.mount_current_dir
            && let Ok(cwd) = std::env::current_dir()
            && let Err(e) = vfs.mount(&cwd, "/")
        {
            tracing::debug!(error = %e, "current directory not mounted");
        }
        Ok(vfs)
    }

    /// Releases the memory mappings of every mounted archive (see
    /// [`PkgArchive::release_mapping`]).
    pub fn release_mappings(&mut self) {
        for mount in &mut self.mounts {
            if let Adapter::Archive(pkg) = &mut mount.adapter {
                pkg.release_mapping();
            }
        }
        self.generation = next_generation();
    }

    /// Lookup of §4.7 on a normalized query; returns the mount and the path relative
    /// to its root. Allocation-free.
    fn locate<'q>(&self, normalized: &'q str) -> Option<(&Mount, &'q str)> {
        for mount in &self.mounts {
            if let Some(rel) = normalized.strip_prefix(mount.root.as_str())
                && mount.adapter.exists(rel)
            {
                return Some((mount, rel));
            }
        }
        // Step 3: one retry with "/" + n, whose normalization drops leading `..`.
        if !normalized.starts_with('/') {
            let rooted = rooted_relative(normalized);
            for mount in &self.mounts {
                if let Some(root) = mount.root.strip_prefix('/')
                    && let Some(rel) = rooted.strip_prefix(root)
                    && mount.adapter.exists(rel)
                {
                    return Some((mount, rel));
                }
            }
        }
        None
    }

    /// Index of the mount that serves `path` and the path relative to its root.
    pub fn resolve(&self, path: &str) -> Option<(usize, String)> {
        let normalized = normalize(path);
        let (mount, rel) = self.locate(&normalized)?;
        let index = self.mounts.iter().position(|m| std::ptr::eq(m, mount))?;
        Some((index, rel.to_owned()))
    }

    /// Whether `path` resolves.
    pub fn exists(&self, path: &str) -> bool {
        self.locate(&normalize(path)).is_some()
    }

    /// Reads a whole file (§4.9). Bytes are raw (no BOM removal). Archive and memory
    /// contents are borrowed, folder files are read into memory.
    ///
    /// The adapter receives the path that matched during lookup. The reference
    /// passes the original query instead, which makes absolute and `..` queries
    /// fail after a successful lookup (QO-26); Lumenwall does not reproduce that.
    pub fn read(&self, path: &str) -> Result<Cow<'_, [u8]>, AssetError> {
        let normalized = normalize(path);
        let (mount, rel) = self
            .locate(&normalized)
            .ok_or_else(|| AssetError::not_found(path, "file absent from every mount point"))?;
        mount.adapter.open(rel, path)
    }

    /// Path on disk of `path` (§4.9); only folder mounts have one.
    pub fn physical_path(&self, path: &str) -> Result<PathBuf, AssetError> {
        let normalized = normalize(path);
        let (mount, rel) = self
            .locate(&normalized)
            .ok_or_else(|| AssetError::not_found(path, "file absent from every mount point"))?;
        mount.adapter.physical_path(rel, path)
    }

    /// Reads a shader source (§4.10): `shaders/<name>` with its last extension
    /// replaced by the stage's, after trying the workshop compatibility substitute
    /// (§4.10.1). Only a `NotFound` substitute is skipped.
    pub fn read_shader(&self, name: &str, stage: ShaderStage) -> Result<Cow<'_, [u8]>, AssetError> {
        let relative = shader_relative_name(name, stage);
        if let Some(compat) = workshop_compat_path(&relative) {
            match self.read(&compat) {
                Ok(data) => {
                    tracing::info!(original = %relative, replacement = %compat, "workshop shader replaced by its compatibility version");
                    return Ok(data);
                }
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e),
            }
        }
        self.read(&format!("shaders/{relative}"))
    }

    /// Reads `materials/<name>.tex` and, if present, `materials/<name>.tex-json`
    /// (§4.10, §2.9.2). Errors on the companion are ignored.
    pub fn read_texture(&self, name: &str) -> Result<TextureFiles<'_>, AssetError> {
        let tex = self.read(&texture_path(name))?;
        let tex_json = self.read(&texture_json_path(name)).ok();
        Ok(TextureFiles { tex, tex_json })
    }
}

/// Bytes of a texture and of its optional `.tex-json` companion.
#[derive(Debug, Clone)]
pub struct TextureFiles<'v> {
    /// `.tex` contents.
    pub tex: Cow<'v, [u8]>,
    /// `.tex-json` contents, if the file resolved.
    pub tex_json: Option<Cow<'v, [u8]>>,
}

impl TextureFiles<'_> {
    /// Parses the texture and applies the companion's spritesheet values.
    pub fn parse(&self) -> Result<Tex<'_>, TexError> {
        self.parse_with(&ParseOptions::default())
    }

    /// [`parse`](Self::parse) with explicit limits.
    pub fn parse_with(&self, options: &ParseOptions) -> Result<Tex<'_>, TexError> {
        let mut texture = tex::parse_with(&self.tex, options)?;
        if let Some(json) = &self.tex_json {
            texture.apply_tex_json(json);
        }
        Ok(texture)
    }
}

/// Texture names that never map to a file (§4.11).
pub fn is_reserved_texture_name(name: &str) -> bool {
    name == "$mediaThumbnail"
        || name == "$mediaPreviousThumbnail"
        || name.starts_with("_rt_")
        || name.starts_with("_alias_")
}

/// Texture lookup across every loaded wallpaper (§4.11): VFSs are tried in byte
/// order of their keys (`default`, a screen name, `span:<screen>`); a `NotFound`
/// moves to the next one, any other error is returned. Returns the index of the
/// providing wallpaper in `wallpapers` and the files.
pub fn find_texture<'v, K: AsRef<str>>(
    wallpapers: &[(K, &'v Vfs)],
    name: &str,
) -> Result<(usize, TextureFiles<'v>), AssetError> {
    let mut order: Vec<usize> = (0..wallpapers.len()).collect();
    order.sort_by(|&a, &b| {
        wallpapers[a]
            .0
            .as_ref()
            .as_bytes()
            .cmp(wallpapers[b].0.as_ref().as_bytes())
    });
    for index in order {
        match wallpapers[index].1.read_texture(name) {
            Ok(files) => return Ok((index, files)),
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e),
        }
    }
    Err(AssetError::not_found(
        texture_path(name),
        "texture not provided by any wallpaper",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("/a%20b/%C3%A9"), "/a b/é".as_bytes());
        assert_eq!(percent_decode("/50%/%zz/%4"), b"/50%/%zz/%4");
    }
}

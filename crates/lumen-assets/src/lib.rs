//! Virtual file system of a wallpaper and asset discovery (spec 01 §4 and §5).
//!
//! - [`Vfs`]: ordered mount points (built-in memory files, media thumbnail, the
//!   wallpaper folder or `.pkg`, `scene.pkg`, `gifscene.pkg`, the assets folder,
//!   the current directory) with the spec's lookup order.
//! - [`path`]: lexical normalization and per-asset path conventions.
//! - [`steam`]: workshop, installation and `assets` folder discovery on Linux and
//!   Windows.

mod builtin;
mod error;
pub mod path;
pub mod steam;
mod vfs;

pub use builtin::{BUILTIN_FILES, builtin_files};
pub use error::{AssetError, ErrorCategory};
pub use vfs::{
    Adapter, DirectoryAdapter, MEDIA_THUMBNAIL, MediaArtSource, MediaThumbnailAdapter, MemoryFiles,
    Mount, TextureFiles, Vfs, WallpaperVfsOptions, find_texture, is_reserved_texture_name,
};

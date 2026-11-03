//! Access to wallpaper files. `lumen-assets` implements [`AssetSource`] over the VFS; tests use the
//! in-memory implementation on `HashMap`.

use std::borrow::Cow;
use std::collections::HashMap;

/// Error returned by an [`AssetSource`].
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    /// No file at this path.
    #[error("not found")]
    NotFound,
    /// The file exists but could not be read.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Read-only file access by asset path (relative to the VFS root, `/` separators, used verbatim).
pub trait AssetSource {
    /// Reads a whole file. Implementations backed by a memory map return `Cow::Borrowed`.
    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>, AssetError>;

    /// Whether a file exists. The default implementation reads it.
    fn exists(&self, path: &str) -> bool {
        self.read(path).is_ok()
    }
}

impl<T: AssetSource + ?Sized> AssetSource for &T {
    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>, AssetError> {
        (**self).read(path)
    }

    fn exists(&self, path: &str) -> bool {
        (**self).exists(path)
    }
}

impl<S: std::hash::BuildHasher> AssetSource for HashMap<String, Vec<u8>, S> {
    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>, AssetError> {
        self.get(path)
            .map(|v| Cow::Borrowed(v.as_slice()))
            .ok_or(AssetError::NotFound)
    }

    fn exists(&self, path: &str) -> bool {
        self.contains_key(path)
    }
}

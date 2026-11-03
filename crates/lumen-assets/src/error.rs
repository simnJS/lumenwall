//! Error type and the spec's two error categories (spec 01 §0.4).

use std::io;
use std::path::PathBuf;

use lumen_formats::pkg::PkgError;
use thiserror::Error;

/// The two error families of spec 01 §0.4. Fallback loops (texture search across
/// wallpapers, workshop shader compatibility) only skip `NotFound`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    /// `Introuvable`: no mount point has the path, or the adapter refuses it.
    NotFound,
    /// `Invalide`: the file exists but its content or its reading failed.
    Invalid,
}

/// Errors of the virtual file system.
#[derive(Debug, Error)]
pub enum AssetError {
    /// The path resolves nowhere or the adapter refuses it.
    #[error("not found: \"{path}\": {reason}")]
    NotFound {
        /// Requested path.
        path: String,
        /// Why.
        reason: &'static str,
    },
    /// Reading an existing file failed.
    #[error("cannot read \"{path}\": {source}")]
    Io {
        /// Requested path.
        path: String,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// A `.pkg` could not be mounted.
    #[error("invalid archive {}: {source}", path.display())]
    Pkg {
        /// Archive path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: PkgError,
    },
    /// The assets folder cannot be mounted (fatal, spec 01 §4.8 priority 6).
    #[error("no valid assets folder at {}: {source}", path.display())]
    NoAssetsDir {
        /// Resolved path.
        path: PathBuf,
        /// Mount error.
        #[source]
        source: Box<AssetError>,
    },
}

impl AssetError {
    pub(crate) fn not_found(path: impl Into<String>, reason: &'static str) -> Self {
        Self::NotFound {
            path: path.into(),
            reason,
        }
    }

    /// Category of the error.
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::NotFound { .. } => ErrorCategory::NotFound,
            Self::Io { .. } | Self::Pkg { .. } | Self::NoAssetsDir { .. } => ErrorCategory::Invalid,
        }
    }

    /// Whether the error is in the `NotFound` category.
    pub fn is_not_found(&self) -> bool {
        self.category() == ErrorCategory::NotFound
    }
}

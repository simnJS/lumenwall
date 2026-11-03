use std::path::PathBuf;

/// Errors of [`crate::VideoPlayer`].
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    /// Built without the `ffmpeg` feature.
    #[error("video decoding is not available in this build (lumenwall-video without FFmpeg)")]
    Unsupported,
    /// The FFmpeg shared libraries could not be loaded (not installed, other major version).
    #[error("video decoding is not available: {0}")]
    Libraries(String),
    #[error("{path}: {message}")]
    Open { path: PathBuf, message: String },
    #[error("{path}: no video stream")]
    NoVideoStream { path: PathBuf },
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("out of memory")]
    OutOfMemory,
}

use std::path::PathBuf;

/// Errors of [`crate::VideoPlayer`].
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    /// Built without the `ffmpeg` feature.
    #[error("video decoding is not available in this build (lumen-video without FFmpeg)")]
    Unsupported,
    #[error("{path}: {message}")]
    Open { path: PathBuf, message: String },
    #[error("{path}: no video stream")]
    NoVideoStream { path: PathBuf },
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("out of memory")]
    OutOfMemory,
}

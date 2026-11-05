//! Error type of the render crate.

use std::path::PathBuf;

/// Every fallible operation of `lumenwall-render` returns this error.
///
/// Errors only happen at load time (context creation, uploads, pipeline
/// compilation) or on surface loss; the per-frame path never fails.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no compatible GPU adapter found (backends: {0:?})")]
    NoAdapter(wgpu::Backends),
    #[error("GPU device request failed: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),
    #[error("surface creation failed: {0}")]
    CreateSurface(#[from] wgpu::CreateSurfaceError),
    #[error("the selected GPU cannot present to this surface")]
    SurfaceUnsupported,
    #[error("surface lost and could not be reconfigured")]
    SurfaceLost,
    #[error("texture `{label}`: {reason}")]
    InvalidTexture { label: String, reason: String },
    #[error("BC compressed textures are not supported by this GPU; upload RGBA instead")]
    BcUnsupported,
    #[error("shader `{label}`: {reason}")]
    Shader { label: String, reason: String },
    #[error("pass `{label}`: {reason}")]
    Pass { label: String, reason: String },
    #[error("GPU validation error in `{label}`: {message}")]
    Validation { label: String, message: String },
    #[error("unknown handle: {0}")]
    BadHandle(&'static str),
    #[error("I/O error on {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("PNG encoding failed: {0}")]
    Png(#[from] png::EncodingError),
    #[error("GPU readback failed: {0}")]
    Readback(String),
    /// The scene's backend cannot do this (another backend may: the caller falls back).
    #[error("unsupported by this backend: {0}")]
    Unsupported(String),
    /// The backend's device was removed or reset: drop everything made on it and start again.
    #[error("GPU device lost: {0}")]
    DeviceLost(String),
    #[error("out of GPU memory")]
    OutOfMemory,
}

pub type Result<T, E = RenderError> = std::result::Result<T, E>;

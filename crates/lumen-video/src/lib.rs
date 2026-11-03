//! Video wallpaper decoding to GPU textures (spec 07 §14).
//!
//! [`VideoPlayer`] demuxes and decodes a video file with FFmpeg on its own thread, hands a
//! small bounded queue of frames to the render thread and converts the frame due now from
//! YUV to RGBA on the GPU (`lumen_render::yuv`) into a render target that the compositor
//! scales to the outputs like a scene's `_rt_FullFrameBuffer`.
//!
//! - Decoding path chosen at open time, with automatic fallback: FFmpeg's Vulkan Video
//!   decoder on wgpu's own device, sampled in place (zero copy); else the hardware decoder of
//!   the rendering GPU (NVDEC through CUDA, VA-API, Vulkan Video, D3D11VA on Windows) with a
//!   copy of the decoded planes; else software decoding.
//! - Infinite loop with a continuous timeline (no visible jump), cadence = video frame rate
//!   capped to the wallpaper fps (frames that would never be shown are dropped before they
//!   are copied), decoding stopped while paused (the decoder blocks on the full queue, no
//!   timer), memory bounded by the queue length.
//!
//! The crate never touches the window system or the audio device: the integration plays the
//! sound track (`map_file` + `lumen-audio`) and drives pause and wake-ups.

mod error;
#[cfg(feature = "ffmpeg")]
mod ffmpeg;
pub mod pacing;
mod player;
// Used by the FFmpeg player only (kept compiled and tested without it).
#[cfg_attr(not(feature = "ffmpeg"), allow(dead_code))]
mod queue;
pub mod select;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use error::VideoError;
pub use player::{VideoPlayer, VideoStats};
pub use queue::Notify;

/// `--hwdec` (spec 07 §2.2, §14.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HwDecode {
    /// Best hardware decoder of the rendering GPU, software otherwise.
    #[default]
    Auto,
    /// NVDEC with a copy of the decoded frame (CUDA), software otherwise.
    NvdecCopy,
    /// Software decoding only.
    No,
}

/// Hardware decoder families.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HwKind {
    /// NVIDIA NVDEC through FFmpeg's CUDA device.
    Cuda,
    /// VA-API (Mesa: AMD, Intel; also NVIDIA through nvidia-vaapi-driver).
    Vaapi,
    /// Vulkan Video.
    Vulkan,
    /// Direct3D 11 video decoding (Windows).
    D3d11va,
}

impl HwKind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Cuda => "NVDEC (cuda)",
            Self::Vaapi => "VA-API",
            Self::Vulkan => "Vulkan Video",
            Self::D3d11va => "D3D11VA",
        }
    }
}

/// How frames actually reach the GPU (known once the first frame is decoded).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodePath {
    /// libavcodec on the CPU, planes uploaded with one copy.
    Software,
    /// Hardware decoder, planes downloaded to system memory then uploaded (two copies).
    HardwareCopy(HwKind),
    /// Hardware decoder on the render device, decoded images sampled in place (no copy).
    ZeroCopy(HwKind),
}

impl std::fmt::Display for DecodePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Software => f.write_str("software decoding, CPU upload"),
            Self::HardwareCopy(k) => {
                write!(
                    f,
                    "{} hardware decoding, copy through system memory",
                    k.name()
                )
            }
            Self::ZeroCopy(k) => {
                write!(
                    f,
                    "{} hardware decoding on the render device, zero copy",
                    k.name()
                )
            }
        }
    }
}

/// Playback settings.
#[derive(Clone, Debug)]
pub struct VideoOptions {
    pub hwdec: HwDecode,
    /// Frame-rate cap (wallpaper `--fps`); frames that no display instant `k / max_fps`
    /// would show are dropped on the decoding thread. 0 = every frame (offline renders).
    pub max_fps: u32,
    /// Decoded frames waiting for display (memory bound: one 4K NV12 frame = 12 MiB on the
    /// copy paths, a reference to a GPU image on the zero-copy path).
    pub queue_frames: usize,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            hwdec: HwDecode::Auto,
            max_fps: 30,
            queue_frames: 3,
        }
    }
}

/// Stream properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoInfo {
    /// Codec name (`h264`, `hevc`, `vp9`, `av1`…).
    pub codec: String,
    /// Coded picture size.
    pub coded_size: (u32, u32),
    /// Display size (sample aspect ratio applied): the wallpaper size (spec 07 §4.5).
    pub display_size: (u32, u32),
    /// Average frame rate, 0 when unknown.
    pub fps: f64,
    /// Duration in seconds, 0 when unknown.
    pub duration: f64,
    /// The file has an audio stream.
    pub has_audio: bool,
    /// Pixel format name of the decoded frames.
    pub pixel_format: String,
}

/// Bytes of a video shared without copying (memory-mapped file, slice of a mapped `.pkg`).
pub type VideoBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// Where a video is read from.
#[derive(Clone)]
pub enum VideoInput {
    /// A file on disk (`video` wallpapers, spec 07 §14.1).
    File(PathBuf),
    /// A complete video file in memory (videos embedded in `.tex` textures, spec 07 §14.2);
    /// `name` is only used in messages.
    Memory { name: String, data: VideoBytes },
}

impl VideoInput {
    /// Name used in messages.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::File(p) => p.display().to_string(),
            Self::Memory { name, .. } => name.clone(),
        }
    }
}

/// Memory-mapped bytes of a file (the video's sound track is played from the same file by
/// the integration without reading it into memory).
pub fn map_file(path: &Path) -> std::io::Result<VideoBytes> {
    let file = std::fs::File::open(path)?;
    // SAFETY: the mapping is read-only; Workshop content is not rewritten while a wallpaper
    // runs (a truncation would only make reads fail with SIGBUS, like for `.pkg` files).
    let map = unsafe { memmap2::Mmap::map(&file)? };
    Ok(Arc::new(map))
}

/// Whether this build can decode video at all.
#[must_use]
pub fn is_supported() -> bool {
    cfg!(feature = "ffmpeg")
}

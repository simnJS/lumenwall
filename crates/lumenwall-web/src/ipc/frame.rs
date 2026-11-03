//! Exchange structures for browser frames shared on the GPU.
//!
//! The browser paints into a pool of GPU textures and hands one out per frame
//! (`OnAcceleratedPaint`). The texture is only valid until the host acknowledges it, so the
//! consumer (lumenwall-render) must, as soon as it receives a [`SharedFrame`]:
//!
//! 1. import the handle into its wgpu device (Linux: DMA-BUF planes → Vulkan external memory;
//!    Windows: NT handle → `ID3D12Device::OpenSharedHandle`, or Vulkan `VK_KHR_external_memory_win32`),
//! 2. copy it into a texture it owns and wait for that copy to finish on the GPU,
//! 3. call [`SharedFrame::release`] (dropping the frame also releases it).
//!
//! The host waits for the release (bounded by a timeout) before returning the texture to the
//! browser, so a slow consumer slows the page down instead of reading a reused buffer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// Pixel layout of a shared frame (8 bits per channel, premultiplied alpha, sRGB-encoded).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PixelFormat {
    /// wgpu `Bgra8Unorm`, Vulkan `B8G8R8A8_UNORM`, DRM `ARGB8888`.
    Bgra8,
    /// wgpu `Rgba8Unorm`, Vulkan `R8G8B8A8_UNORM`, DRM `ABGR8888`.
    Rgba8,
}

impl PixelFormat {
    /// DRM fourcc code (little-endian `AR24` / `AB24`) for DMA-BUF import.
    pub fn drm_fourcc(self) -> u32 {
        let code = match self {
            Self::Bgra8 => *b"AR24",
            Self::Rgba8 => *b"AB24",
        };
        u32::from_le_bytes(code)
    }
}

/// One DMA-BUF plane, without its file descriptor (sent out of band).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneInfo {
    pub offset: u64,
    pub stride: u32,
    pub size: u64,
}

/// Wire form of a frame (host → main).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameInfo {
    /// Increasing frame number.
    pub id: u64,
    /// Full texture size (coded size).
    pub width: u32,
    pub height: u32,
    /// Visible rectangle inside the texture: x, y, width, height.
    pub visible: [i32; 4],
    pub format: PixelFormat,
    /// DRM format modifier (Linux).
    pub modifier: u64,
    /// DMA-BUF planes (Linux); their descriptors travel with the message, in order.
    pub planes: Vec<PlaneInfo>,
    /// Windows: shared NT handle value in the host process (0 elsewhere).
    pub handle: u64,
}

/// A DMA-BUF plane owned by the main process.
#[cfg(unix)]
#[derive(Debug)]
pub struct DmaBufPlane {
    pub fd: std::os::fd::OwnedFd,
    pub offset: u64,
    pub stride: u32,
    pub size: u64,
}

/// GPU handle of a shared frame, owned by the receiving process.
#[derive(Debug)]
pub enum SharedTexture {
    /// Linux: import with `VK_EXT_external_memory_dma_buf` + `VK_EXT_image_drm_format_modifier`
    /// (all planes share one allocation in practice), format [`PixelFormat::drm_fourcc`].
    #[cfg(unix)]
    DmaBuf {
        planes: Vec<DmaBufPlane>,
        modifier: u64,
    },
    /// Windows: NT handle of an `ID3D11Texture2D` created without keyed mutex; open with
    /// `ID3D12Device::OpenSharedHandle` (wgpu DX12) or `ID3D11Device1::OpenSharedResource1`.
    #[cfg(windows)]
    D3D11 {
        handle: std::os::windows::io::OwnedHandle,
    },
}

/// Called with the frame id when the consumer is done with it.
pub(crate) type ReleaseFn = Box<dyn FnOnce(u64) + Send>;

/// A browser frame ready to be imported by the renderer. See the module documentation.
pub struct SharedFrame {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub visible: [i32; 4],
    pub format: PixelFormat,
    pub texture: SharedTexture,
    revoked: Arc<AtomicBool>,
    release: Option<ReleaseFn>,
}

impl std::fmt::Debug for SharedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedFrame")
            .field("id", &self.id)
            .field("size", &(self.width, self.height))
            .field("format", &self.format)
            .field("texture", &self.texture)
            .finish_non_exhaustive()
    }
}

impl SharedFrame {
    pub(crate) fn new(
        info: &FrameInfo,
        texture: SharedTexture,
        revoked: Arc<AtomicBool>,
        release: ReleaseFn,
    ) -> Self {
        Self {
            id: info.id,
            width: info.width,
            height: info.height,
            visible: info.visible,
            format: info.format,
            texture,
            revoked,
            release: Some(release),
        }
    }

    /// True once the host gave up waiting for this frame: its texture may already be reused, so
    /// skip the import and wait for the next frame.
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }

    /// Tells the host the copy is complete. Call after the GPU finished reading the texture.
    pub fn release(mut self) {
        if let Some(release) = self.release.take() {
            release(self.id);
        }
    }
}

impl Drop for SharedFrame {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_values() {
        assert_eq!(PixelFormat::Bgra8.drm_fourcc(), 0x3432_5241);
        assert_eq!(PixelFormat::Rgba8.drm_fourcc(), 0x3432_4241);
    }
}

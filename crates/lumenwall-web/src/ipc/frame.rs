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
    /// The single plane is a shared memory buffer with the pixels (software painting,
    /// [`crate::ipc::FrameTransport::Memory`]), not a GPU texture.
    #[serde(default)]
    pub memory: bool,
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

#[cfg(unix)]
impl DmaBufPlane {
    /// Identity of the buffer behind the descriptor (device, inode): the same for every
    /// descriptor the host sends for one buffer of the browser's pool, so an import can be
    /// cached and reused instead of being redone for every frame.
    pub fn buffer_id(&self) -> std::io::Result<(u64, u64)> {
        buffer_id(&self.fd)
    }
}

/// (device, inode) of an open descriptor.
#[cfg(unix)]
pub fn buffer_id(fd: &std::os::fd::OwnedFd) -> std::io::Result<(u64, u64)> {
    let stat = rustix::fs::fstat(fd)?;
    #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
    Ok((stat.st_dev as u64, stat.st_ino as u64))
}

/// Read-only mapping of a shared memory frame ([`SharedTexture::Memory`]).
#[cfg(unix)]
#[derive(Debug)]
pub struct MappedMemory {
    ptr: std::ptr::NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is read-only and owned by this value; it is only unmapped on drop.
#[cfg(unix)]
unsafe impl Send for MappedMemory {}

#[cfg(unix)]
impl MappedMemory {
    /// Maps `len` bytes of `fd` read-only. The host keeps writing into the same buffer between
    /// frames, so the contents are only stable between a [`SharedFrame`] and its release.
    pub fn map(fd: &std::os::fd::OwnedFd, len: usize) -> std::io::Result<Self> {
        use rustix::mm::{MapFlags, ProtFlags, mmap};
        if len == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty mapping",
            ));
        }
        // SAFETY: a fresh shared, read-only mapping chosen by the kernel; it aliases no Rust
        // object and is unmapped in Drop.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ,
                MapFlags::SHARED,
                fd,
                0,
            )?
        };
        let ptr = std::ptr::NonNull::new(ptr.cast::<u8>())
            .ok_or_else(|| std::io::Error::other("null mapping"))?;
        Ok(Self { ptr, len })
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `len` readable bytes mapped in `map`, alive until drop.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

#[cfg(unix)]
impl Drop for MappedMemory {
    fn drop(&mut self) {
        // SAFETY: the mapping created in `map`, unmapped once.
        let _ = unsafe { rustix::mm::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

/// Read-only view of a shared memory frame ([`SharedTexture::Memory`], a section handle).
#[cfg(windows)]
#[derive(Debug)]
pub struct MappedMemory {
    ptr: std::ptr::NonNull<u8>,
    len: usize,
}

// SAFETY: the view is read-only and owned by this value; it is only unmapped on drop.
#[cfg(windows)]
unsafe impl Send for MappedMemory {}

#[cfg(windows)]
impl MappedMemory {
    /// Maps the first `len` bytes of the section read-only. The host keeps writing into the
    /// same section between frames, so the contents are only stable between a [`SharedFrame`]
    /// and its release. The view stays valid after the handle is closed.
    pub fn map(section: &std::os::windows::io::OwnedHandle, len: usize) -> std::io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Memory::{FILE_MAP_READ, MapViewOfFile};
        if len == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty mapping",
            ));
        }
        // SAFETY: a fresh read-only view chosen by the system; it aliases no Rust object and
        // is unmapped in Drop. A section smaller than `len` makes the call fail.
        let view = unsafe { MapViewOfFile(section.as_raw_handle(), FILE_MAP_READ, 0, 0, len) };
        let ptr = std::ptr::NonNull::new(view.Value.cast::<u8>())
            .ok_or_else(std::io::Error::last_os_error)?;
        Ok(Self { ptr, len })
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `len` readable bytes mapped in `map`, alive until drop.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

#[cfg(windows)]
impl Drop for MappedMemory {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        // SAFETY: the view created in `map`, unmapped once.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ptr.as_ptr().cast(),
            });
        }
    }
}

/// Whether two handles refer to the same kernel object (Windows 10 and later). The main process
/// receives a new duplicate for every frame; this tells which buffer of the browser's pool it
/// is, so an import or a mapping can be reused instead of being redone for every frame.
#[cfg(windows)]
pub fn same_object(
    a: &std::os::windows::io::OwnedHandle,
    b: &std::os::windows::io::OwnedHandle,
) -> bool {
    use std::os::windows::io::AsRawHandle;
    // SAFETY: both handles are open for the duration of the call.
    unsafe {
        windows_sys::Win32::Foundation::CompareObjectHandles(a.as_raw_handle(), b.as_raw_handle())
            != 0
    }
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
    /// Linux, software painting: pixels in a shared memory buffer (`stride` bytes per row,
    /// `size` bytes), to map with [`MappedMemory`] and upload.
    #[cfg(unix)]
    Memory {
        fd: std::os::fd::OwnedFd,
        stride: u32,
        size: u64,
    },
    /// Windows: NT handle of an `ID3D11Texture2D` created without keyed mutex; import with
    /// Vulkan `VK_KHR_external_memory_win32` (handle type `D3D11_TEXTURE`, wgpu-hal
    /// `texture_from_d3d11_shared_handle`), `ID3D12Device::OpenSharedHandle` (DX12) or
    /// `ID3D11Device1::OpenSharedResource1`. [`same_object`] identifies the pool buffer.
    #[cfg(windows)]
    D3D11 {
        handle: std::os::windows::io::OwnedHandle,
    },
    /// Windows, software painting: pixels in a shared memory section (`stride` bytes per row,
    /// `size` bytes), to map with [`MappedMemory`] and upload.
    #[cfg(windows)]
    Memory {
        handle: std::os::windows::io::OwnedHandle,
        stride: u32,
        size: u64,
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

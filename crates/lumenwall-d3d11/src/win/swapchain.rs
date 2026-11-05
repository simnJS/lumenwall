//! Flip-model swap chain of one window (one per monitor): `DXGI_SWAP_EFFECT_FLIP_DISCARD`, two
//! buffers, never tearing, presented with `Present(0, 0)` (the wallpaper paces itself; a frame is
//! shown at the next vertical blank and a newer one replaces it before, like Vulkan's Mailbox).

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_MWA_NO_ALT_ENTER, DXGI_MWA_NO_WINDOW_CHANGES, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGISwapChain1,
};
use windows::core::BOOL;

use super::device::Device;
use crate::error::{Result, hresult, out};

/// Format of every swap chain: what DWM composes natively, never sRGB (the wgpu path presents
/// through a non-sRGB view too).
pub const SWAP_FORMAT: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_UNORM;
const BUFFERS: u32 = 2;

/// Result of a present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentStatus {
    Presented,
    /// The window is not visible (`DXGI_STATUS_OCCLUDED`): stop rendering this output until the
    /// platform says it is visible again (no polling).
    Occluded,
}

/// The swap chain of one window.
pub struct SwapChain {
    swap: IDXGISwapChain1,
    back: Option<ID3D11Texture2D>,
    rtv: Option<ID3D11RenderTargetView>,
    size: (u32, u32),
}

impl SwapChain {
    /// Creates the swap chain of `hwnd` at `size` (clamped to 1×1).
    pub fn for_hwnd(device: &Device, hwnd: HWND, size: (u32, u32)) -> Result<Self> {
        let size = (size.0.max(1), size.1.max(1));
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: size.0,
            Height: size.1,
            Format: SWAP_FORMAT,
            Stereo: BOOL(0),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: BUFFERS,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };
        // SAFETY: valid device, window handle owned by the caller and descriptor.
        let swap = unsafe {
            device
                .factory()
                .CreateSwapChainForHwnd(device.raw(), hwnd, &desc, None, None)
        }
        .map_err(|e| device.error("CreateSwapChainForHwnd", &e))?;
        // The wallpaper window never goes full screen through DXGI.
        // SAFETY: plain call on the factory that made the swap chain.
        let _ = unsafe {
            device
                .factory()
                .MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES)
        };
        let mut s = Self {
            swap,
            back: None,
            rtv: None,
            size,
        };
        s.create_view(device)?;
        Ok(s)
    }

    fn create_view(&mut self, device: &Device) -> Result<()> {
        // SAFETY: buffer 0 of a flip-model swap chain is the current back buffer; the view is
        // created on the swap chain's own device.
        unsafe {
            let back: ID3D11Texture2D = self
                .swap
                .GetBuffer(0)
                .map_err(|e| device.error("GetBuffer", &e))?;
            let mut rtv = None;
            device
                .raw()
                .CreateRenderTargetView(&back, None, Some(&mut rtv))
                .map_err(|e| device.error("CreateRenderTargetView", &e))?;
            self.rtv = Some(out(rtv, "CreateRenderTargetView")?);
            self.back = Some(back);
        }
        Ok(())
    }

    /// Resizes the buffers (window resized, monitor mode changed). Every reference to the old
    /// buffers held by this swap chain is dropped first; the context's bindings are cleared.
    pub fn resize(&mut self, device: &Device, size: (u32, u32)) -> Result<()> {
        let size = (size.0.max(1), size.1.max(1));
        if size == self.size {
            return Ok(());
        }
        self.rtv = None;
        self.back = None;
        // SAFETY: the context drops its references to the back buffer before the resize, as
        // `ResizeBuffers` requires.
        unsafe {
            device.context().ClearState();
            device.context().Flush();
            self.swap
                .ResizeBuffers(BUFFERS, size.0, size.1, SWAP_FORMAT, DXGI_SWAP_CHAIN_FLAG(0))
        }
        .map_err(|e| device.error("ResizeBuffers", &e))?;
        self.size = size;
        self.create_view(device)
    }

    /// Render target view of the current back buffer. Bind it again after every present (the
    /// flip model unbinds it).
    #[must_use]
    pub fn rtv(&self) -> &ID3D11RenderTargetView {
        self.rtv.as_ref().expect("swap chain view exists between resizes")
    }

    /// The current back buffer (copies, readback in tests).
    #[must_use]
    pub fn back_buffer(&self) -> &ID3D11Texture2D {
        self.back.as_ref().expect("swap chain buffer exists between resizes")
    }

    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    #[must_use]
    pub fn format(&self) -> DXGI_FORMAT {
        SWAP_FORMAT
    }

    #[must_use]
    pub fn raw(&self) -> &IDXGISwapChain1 {
        &self.swap
    }

    /// Presents at the next vertical blank without blocking (`Present(0, 0)`).
    pub fn present(&self, device: &Device) -> Result<PresentStatus> {
        // SAFETY: plain call; the HRESULT is checked below.
        let hr = unsafe { self.swap.Present(0, DXGI_PRESENT(0)) };
        match hr.0 {
            hresult::DXGI_STATUS_OCCLUDED => Ok(PresentStatus::Occluded),
            code if code < 0 => Err(device.error("Present", &windows::core::Error::from_hresult(hr))),
            // S_OK and the other success codes (mode change in progress…).
            _ => Ok(PresentStatus::Presented),
        }
    }
}

impl Drop for SwapChain {
    fn drop(&mut self) {
        // Views before the swap chain (`windows` releases fields in declaration order anyway).
        self.rtv = None;
        self.back = None;
    }
}


//! Textures made outside the renderer, used without a CPU copy:
//! - video frames FFmpeg's D3D11VA decoder writes on the renderer's own device (task D3D11 6/n
//!   produces [`DecodedFrame`]s, task 4/n's YUV pass samples them through [`plane_views`]);
//! - the browser's frames, shared as NT handles by CEF's accelerated paint
//!   ([`Device::open_shared_texture`], task 7/n).

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2DARRAY;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_ARRAY_SRV,
    ID3D11Multithread, ID3D11ShaderResourceView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R16_UNORM,
    DXGI_FORMAT_R16G16_UNORM,
};
use windows::core::Interface;

use super::device::Device;
use crate::error::{Error, Result, out};

/// Layout of a decoded frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaneFormat {
    /// 8-bit 4:2:0: luma R8, interleaved chroma R8G8 at half size.
    Nv12,
    /// 10-bit 4:2:0 in 16-bit words (MSB-aligned): luma R16, chroma R16G16.
    P010,
}

impl PlaneFormat {
    /// View formats of the `(luma, chroma)` planes.
    #[must_use]
    pub fn plane_formats(self) -> (DXGI_FORMAT, DXGI_FORMAT) {
        match self {
            Self::Nv12 => (DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM),
            Self::P010 => (DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM),
        }
    }
}

/// One frame of the hardware decoder: a slice of its texture array (FFmpeg's D3D11VA pool),
/// created with `D3D11_BIND_SHADER_RESOURCE` so it can be sampled.
#[derive(Clone, Debug)]
pub struct DecodedFrame {
    pub texture: ID3D11Texture2D,
    /// Array slice of the frame.
    pub slice: u32,
    pub format: PlaneFormat,
    /// Visible size in pixels (the texture may be padded by the decoder).
    pub size: (u32, u32),
}

/// `(luma, chroma)` shader resource views of one slice of a decoded frame.
pub fn plane_views(
    device: &Device,
    frame: &DecodedFrame,
) -> Result<(ID3D11ShaderResourceView, ID3D11ShaderResourceView)> {
    let (luma, chroma) = frame.format.plane_formats();
    let view = |format: DXGI_FORMAT| -> Result<ID3D11ShaderResourceView> {
        let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2DARRAY,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                    FirstArraySlice: frame.slice,
                    ArraySize: 1,
                },
            },
        };
        let mut srv = None;
        // SAFETY: a texture of this device; the descriptor names one plane of one slice; live
        // out-parameter. The runtime refuses a texture without the shader-resource bind flag.
        unsafe {
            device
                .raw()
                .CreateShaderResourceView(&frame.texture, Some(&desc), Some(&mut srv))
        }
        .map_err(|e| device.error("CreateShaderResourceView", &e))?;
        out(srv, "CreateShaderResourceView")
    };
    Ok((view(luma)?, view(chroma)?))
}

impl Device {
    /// Opens a texture another device shared as an NT handle
    /// (`D3D11_RESOURCE_MISC_SHARED_NTHANDLE`: CEF's accelerated paint). The texture must live on
    /// this device's adapter. The caller keeps owning the handle.
    pub fn open_shared_texture(&self, handle: HANDLE) -> Result<ID3D11Texture2D> {
        // SAFETY: `OpenSharedResource1` only reads the handle; an invalid one is an error.
        unsafe { self.raw().OpenSharedResource1(handle) }
            .map_err(|e| self.error("OpenSharedResource1", &e))
    }

    /// Turns the runtime's lock around the immediate context on or off
    /// (`ID3D11Multithread::SetMultithreadProtected`): on before FFmpeg's decoder uses the
    /// device from its own thread. Returns the previous state.
    pub fn set_multithread_protected(&self, on: bool) -> Result<bool> {
        let mt: ID3D11Multithread = self
            .context()
            .cast()
            .map_err(|_| Error::Unsupported("ID3D11Multithread (Windows 10)".into()))?;
        // SAFETY: plain setter on a live context.
        Ok(unsafe { mt.SetMultithreadProtected(on) }.as_bool())
    }
}

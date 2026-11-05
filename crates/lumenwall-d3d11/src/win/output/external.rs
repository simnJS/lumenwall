//! Copy of an external frame (CEF's accelerated paint, a shared BGRA or RGBA texture) into a
//! target, by draw: the view hides the channel order, so BGRA sources land as RGBA.

use naga::Module;
use windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2D;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0,
    D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC, ID3D11SamplerState, ID3D11ShaderResourceView,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_TYPELESS, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8X8_TYPELESS,
    DXGI_FORMAT_B8G8R8X8_UNORM, DXGI_FORMAT_R8G8B8A8_TYPELESS, DXGI_FORMAT_R8G8B8A8_UNORM,
};

use super::pass::{Destination, FullscreenPass, f32_bytes};
use crate::cache::BytecodeCache;
use crate::error::{Error, Result, out};
use crate::state::SamplerKey;
use crate::win::{Device, RenderTarget, StateCache};

/// WGSL of the copy program (pass block `b0`, texture `t0`, sampler `s0`).
pub const COPY_WGSL: &str = include_str!("shaders/copy.wgsl");

/// Copy of an external texture (a shared BGRA or RGBA frame) into a target, by draw.
pub struct ExternalCopy {
    pass: FullscreenPass,
    sampler: ID3D11SamplerState,
}

impl ExternalCopy {
    /// `module` is [`COPY_WGSL`] parsed by naga.
    pub fn new(
        device: &Device,
        states: &mut StateCache,
        cache: Option<&BytecodeCache>,
        module: &Module,
    ) -> Result<Self> {
        Ok(Self {
            pass: FullscreenPass::new(device, states, cache, module, 16, 0b1)?,
            sampler: states.sampler(device, SamplerKey::CLAMP_LINEAR)?,
        })
    }

    /// A shader resource view of `source` (8-bit BGRA or RGBA, typed or typeless, created with
    /// `D3D11_BIND_SHADER_RESOURCE`). Callers that copy the same texture every frame (opened
    /// shared handles are cached by identity) keep the view and use [`ExternalCopy::copy_view`].
    pub fn view(device: &Device, source: &ID3D11Texture2D) -> Result<ID3D11ShaderResourceView> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: plain query into a live out-parameter.
        unsafe { source.GetDesc(&mut desc) };
        if desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 == 0 {
            return Err(Error::InvalidData(
                "external texture lacks D3D11_BIND_SHADER_RESOURCE".into(),
            ));
        }
        if desc.SampleDesc.Count > 1 || desc.ArraySize != 1 {
            return Err(Error::Unsupported(
                "external texture must be single-sample and not an array".into(),
            ));
        }
        let format = match desc.Format {
            DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_TYPELESS => {
                DXGI_FORMAT_B8G8R8A8_UNORM
            }
            DXGI_FORMAT_B8G8R8X8_UNORM | DXGI_FORMAT_B8G8R8X8_TYPELESS => {
                DXGI_FORMAT_B8G8R8X8_UNORM
            }
            DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_TYPELESS => {
                DXGI_FORMAT_R8G8B8A8_UNORM
            }
            f => {
                return Err(Error::Unsupported(format!(
                    "external texture of DXGI format {}",
                    f.0
                )));
            }
        };
        let view_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                },
            },
        };
        let mut srv = None;
        // SAFETY: a texture with the shader-resource bind flag (checked above); the view format
        // is in the texture's format family; live out-parameter.
        unsafe {
            device
                .raw()
                .CreateShaderResourceView(source, Some(&view_desc), Some(&mut srv))
        }
        .map_err(|e| device.error("CreateShaderResourceView", &e))?;
        out(srv, "CreateShaderResourceView")
    }

    /// Copies `source` into `target`: exact when the sizes are equal, linear otherwise.
    pub fn copy(
        &mut self,
        device: &Device,
        source: &ID3D11Texture2D,
        target: &RenderTarget,
    ) -> Result<()> {
        self.copy_view(device, &Self::view(device, source)?, target)
    }

    /// [`ExternalCopy::copy`] with a view from [`ExternalCopy::view`].
    pub fn copy_view(
        &mut self,
        device: &Device,
        source: &ID3D11ShaderResourceView,
        target: &RenderTarget,
    ) -> Result<()> {
        let (w, h) = target.size();
        let (w, h) = (w as f32, h as f32);
        self.pass.draw(
            device,
            &f32_bytes(&[w, h, 1.0 / w, 1.0 / h]),
            &[source],
            &self.sampler,
            Destination {
                rtv: target.draw_view(),
                size: target.size(),
                clear: None,
            },
        )?;
        target.resolve(device);
        Ok(())
    }
}

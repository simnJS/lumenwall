//! Render targets: a sampled single-sample texture, optionally drawn through a multisampled
//! texture resolved into it (MSAA), optionally with a `D32_FLOAT` depth buffer (3D scenes).
//!
//! Aliasing (Perf A) needs nothing here: targets whose frame lifetimes do not overlap share one
//! `RenderTarget` (Direct3D 11 has no placed resources; sharing the texture is the aliasing).

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DEPTH_STENCIL, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_CLEAR_DEPTH, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
    ID3D11DepthStencilView, ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_D32_FLOAT, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
};

use super::device::Device;
use crate::error::{Error, Result, out};

/// Colour format of a target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TargetFormat {
    /// Every target of the wgpu path today (`RT_FORMAT`).
    #[default]
    Rgba8Unorm,
    /// HDR targets (bloom and HDR scenes, spec 04 Q23 when it lands).
    Rgba16Float,
    /// `fbos` declaring one or two channels.
    R8Unorm,
    Rg8Unorm,
}

impl TargetFormat {
    #[must_use]
    pub fn dxgi(self) -> DXGI_FORMAT {
        match self {
            Self::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
            Self::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
            Self::R8Unorm => DXGI_FORMAT_R8_UNORM,
            Self::Rg8Unorm => DXGI_FORMAT_R8G8_UNORM,
        }
    }

    #[must_use]
    pub fn bytes_per_pixel(self) -> u64 {
        match self {
            Self::Rgba8Unorm => 4,
            Self::Rgba16Float => 8,
            Self::R8Unorm => 1,
            Self::Rg8Unorm => 2,
        }
    }
}

/// What to create.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TargetDesc {
    pub size: (u32, u32),
    pub format: TargetFormat,
    /// 1 = no MSAA; 2, 4 or 8 (checked against the device).
    pub samples: u32,
    pub depth: bool,
}

impl TargetDesc {
    #[must_use]
    pub fn new(size: (u32, u32)) -> Self {
        Self {
            size,
            format: TargetFormat::Rgba8Unorm,
            samples: 1,
            depth: false,
        }
    }
}

/// Depth buffer of a target (same size and sample count).
pub struct DepthBuffer {
    texture: ID3D11Texture2D,
    dsv: ID3D11DepthStencilView,
}

impl DepthBuffer {
    #[must_use]
    pub fn view(&self) -> &ID3D11DepthStencilView {
        &self.dsv
    }

    #[must_use]
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }
}

/// A render target.
pub struct RenderTarget {
    desc: TargetDesc,
    texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    rtv: ID3D11RenderTargetView,
    msaa: Option<(ID3D11Texture2D, ID3D11RenderTargetView)>,
    depth: Option<DepthBuffer>,
}

fn texture2d(
    device: &Device,
    size: (u32, u32),
    format: DXGI_FORMAT,
    samples: u32,
    bind: u32,
) -> Result<ID3D11Texture2D> {
    let mut t = None;
    // SAFETY: valid descriptor without initial data; live out-parameter.
    unsafe {
        device.raw().CreateTexture2D(
            &D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: samples,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: bind,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            },
            None,
            Some(&mut t),
        )
    }
    .map_err(|e| device.error("CreateTexture2D", &e))?;
    out(t, "CreateTexture2D")
}

fn rtv(device: &Device, t: &ID3D11Texture2D) -> Result<ID3D11RenderTargetView> {
    let mut v = None;
    // SAFETY: the texture has the render-target bind flag; null descriptor = whole texture.
    unsafe { device.raw().CreateRenderTargetView(t, None, Some(&mut v)) }
        .map_err(|e| device.error("CreateRenderTargetView", &e))?;
    out(v, "CreateRenderTargetView")
}

impl RenderTarget {
    /// Creates the target (sizes clamped to 1×1, as the wgpu path does).
    pub fn new(device: &Device, desc: TargetDesc) -> Result<Self> {
        let desc = TargetDesc {
            size: (desc.size.0.max(1), desc.size.1.max(1)),
            samples: desc.samples.max(1),
            ..desc
        };
        let format = desc.format.dxgi();
        if desc.samples > 1 && device.msaa_quality_levels(format, desc.samples) == 0 {
            return Err(Error::Unsupported(format!(
                "{}× MSAA on {:?}",
                desc.samples, desc.format
            )));
        }
        let rt = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
        let texture = texture2d(device, desc.size, format, 1, rt)?;
        let mut srv = None;
        // SAFETY: shader-resource bind flag set above.
        unsafe {
            device
                .raw()
                .CreateShaderResourceView(&texture, None, Some(&mut srv))
        }
        .map_err(|e| device.error("CreateShaderResourceView", &e))?;
        let rtv_single = rtv(device, &texture)?;
        let msaa = if desc.samples > 1 {
            let t = texture2d(
                device,
                desc.size,
                format,
                desc.samples,
                D3D11_BIND_RENDER_TARGET.0 as u32,
            )?;
            let v = rtv(device, &t)?;
            Some((t, v))
        } else {
            None
        };
        let depth = if desc.depth {
            let texture = texture2d(
                device,
                desc.size,
                DXGI_FORMAT_D32_FLOAT,
                desc.samples,
                D3D11_BIND_DEPTH_STENCIL.0 as u32,
            )?;
            let mut dsv = None;
            // SAFETY: depth-stencil bind flag set above.
            unsafe {
                device
                    .raw()
                    .CreateDepthStencilView(&texture, None, Some(&mut dsv))
            }
            .map_err(|e| device.error("CreateDepthStencilView", &e))?;
            Some(DepthBuffer {
                texture,
                dsv: out(dsv, "CreateDepthStencilView")?,
            })
        } else {
            None
        };
        Ok(Self {
            desc,
            texture,
            srv: out(srv, "CreateShaderResourceView")?,
            rtv: rtv_single,
            msaa,
            depth,
        })
    }

    #[must_use]
    pub fn desc(&self) -> TargetDesc {
        self.desc
    }

    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.desc.size
    }

    /// The view draws go to: the multisampled one with MSAA.
    #[must_use]
    pub fn draw_view(&self) -> &ID3D11RenderTargetView {
        self.msaa.as_ref().map_or(&self.rtv, |(_, v)| v)
    }

    /// The single-sample texture shaders sample (after [`RenderTarget::resolve`] with MSAA).
    #[must_use]
    pub fn srv(&self) -> &ID3D11ShaderResourceView {
        &self.srv
    }

    #[must_use]
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    #[must_use]
    pub fn depth(&self) -> Option<&DepthBuffer> {
        self.depth.as_ref()
    }

    /// Binds the target (and its depth buffer) with a viewport covering it. Unbind it from the
    /// shader stages first when it was sampled (the runtime unbinds conflicting views itself,
    /// with a debug-layer warning).
    pub fn bind(&self, device: &Device) {
        let views = [Some(self.draw_view().clone())];
        // SAFETY: views of this device; the viewport is a plain value.
        unsafe {
            device
                .context()
                .OMSetRenderTargets(Some(&views), self.depth.as_ref().map(|d| &d.dsv));
            device.context().RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.desc.size.0 as f32,
                Height: self.desc.size.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
        }
    }

    /// Clears the colour (and the depth to 1, the "less or equal" start value).
    pub fn clear(&self, device: &Device, color: [f32; 4]) {
        // SAFETY: views of this device.
        unsafe {
            device
                .context()
                .ClearRenderTargetView(self.draw_view(), &color);
            if let Some(d) = &self.depth {
                device
                    .context()
                    .ClearDepthStencilView(&d.dsv, D3D11_CLEAR_DEPTH.0, 1.0, 0);
            }
        }
    }

    /// Resolves the multisampled texture into the sampled one (no-op without MSAA). Once per
    /// frame, after the last draw into the target and before it is sampled.
    pub fn resolve(&self, device: &Device) {
        if let Some((msaa, _)) = &self.msaa {
            // SAFETY: same size and format, single-sample destination.
            unsafe {
                device.context().ResolveSubresource(
                    &self.texture,
                    0,
                    msaa,
                    0,
                    self.desc.format.dxgi(),
                );
            }
        }
    }

    /// GPU bytes (colour, MSAA colour, depth).
    #[must_use]
    pub fn bytes(&self) -> u64 {
        let px = u64::from(self.desc.size.0) * u64::from(self.desc.size.1);
        let s = u64::from(self.desc.samples);
        let colour = px * self.desc.format.bytes_per_pixel();
        let msaa = if s > 1 { colour * s } else { 0 };
        let depth = if self.desc.depth { px * 4 * s } else { 0 };
        colour + msaa + depth
    }
}

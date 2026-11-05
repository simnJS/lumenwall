//! One Direct3D state object per distinct key, created at load time and shared by every draw.
//! (Direct3D 11 also deduplicates identical descriptors itself, up to 4096 objects per kind;
//! the cache keeps the lookup out of the driver and the count visible.)

use std::collections::HashMap;

use windows::Win32::Graphics::Direct3D11::*;
use windows::core::BOOL;

use super::device::Device;
use crate::error::{Result, out};
use crate::state::{
    AddressMode, BlendFactor, BlendKey, BlendOp, Cull, DepthKey, RasterKey, SamplerKey,
};

fn address(a: AddressMode) -> D3D11_TEXTURE_ADDRESS_MODE {
    match a {
        AddressMode::Repeat => D3D11_TEXTURE_ADDRESS_WRAP,
        AddressMode::Clamp => D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressMode::Border => D3D11_TEXTURE_ADDRESS_BORDER,
    }
}

fn factor(f: BlendFactor) -> D3D11_BLEND {
    match f {
        BlendFactor::Zero => D3D11_BLEND_ZERO,
        BlendFactor::One => D3D11_BLEND_ONE,
        BlendFactor::SrcColor => D3D11_BLEND_SRC_COLOR,
        BlendFactor::InvSrcColor => D3D11_BLEND_INV_SRC_COLOR,
        BlendFactor::SrcAlpha => D3D11_BLEND_SRC_ALPHA,
        BlendFactor::InvSrcAlpha => D3D11_BLEND_INV_SRC_ALPHA,
        BlendFactor::DstColor => D3D11_BLEND_DEST_COLOR,
        BlendFactor::InvDstColor => D3D11_BLEND_INV_DEST_COLOR,
        BlendFactor::DstAlpha => D3D11_BLEND_DEST_ALPHA,
        BlendFactor::InvDstAlpha => D3D11_BLEND_INV_DEST_ALPHA,
    }
}

fn op(o: BlendOp) -> D3D11_BLEND_OP {
    match o {
        BlendOp::Add => D3D11_BLEND_OP_ADD,
        BlendOp::Subtract => D3D11_BLEND_OP_SUBTRACT,
        BlendOp::RevSubtract => D3D11_BLEND_OP_REV_SUBTRACT,
        BlendOp::Min => D3D11_BLEND_OP_MIN,
        BlendOp::Max => D3D11_BLEND_OP_MAX,
    }
}

/// The descriptor of a sampler key (exposed for tests).
#[must_use]
pub(crate) fn sampler_desc(key: SamplerKey) -> D3D11_SAMPLER_DESC {
    let anisotropic = !key.nearest && key.anisotropy > 1;
    let filter = match (key.nearest, anisotropic) {
        (true, _) => D3D11_FILTER_MIN_MAG_MIP_POINT,
        (false, true) => D3D11_FILTER_ANISOTROPIC,
        (false, false) => D3D11_FILTER_MIN_MAG_MIP_LINEAR,
    };
    let a = address(key.address);
    D3D11_SAMPLER_DESC {
        Filter: filter,
        AddressU: a,
        AddressV: a,
        AddressW: a,
        MipLODBias: 0.0,
        MaxAnisotropy: if anisotropic {
            u32::from(key.anisotropy.min(16))
        } else {
            1
        },
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        // Transparent black, the wgpu path's border colour.
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        // `mipmaps: false` samples level 0 only (wgpu: `lod_max_clamp = 0`).
        MaxLOD: if key.mipmaps { f32::MAX } else { 0.0 },
    }
}

/// Load-time cache of state objects.
#[derive(Default)]
pub struct StateCache {
    samplers: HashMap<SamplerKey, ID3D11SamplerState>,
    blends: HashMap<BlendKey, ID3D11BlendState>,
    rasters: HashMap<RasterKey, ID3D11RasterizerState>,
    depths: HashMap<DepthKey, ID3D11DepthStencilState>,
}

impl StateCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sampler(&mut self, device: &Device, key: SamplerKey) -> Result<ID3D11SamplerState> {
        if let Some(s) = self.samplers.get(&key) {
            return Ok(s.clone());
        }
        let mut s = None;
        // SAFETY: valid descriptor, live out-parameter.
        unsafe {
            device
                .raw()
                .CreateSamplerState(&sampler_desc(key), Some(&mut s))
        }
        .map_err(|e| device.error("CreateSamplerState", &e))?;
        let s = out(s, "CreateSamplerState")?;
        self.samplers.insert(key, s.clone());
        Ok(s)
    }

    pub fn blend(&mut self, device: &Device, key: BlendKey) -> Result<ID3D11BlendState> {
        if let Some(s) = self.blends.get(&key) {
            return Ok(s.clone());
        }
        let mut desc = D3D11_BLEND_DESC::default();
        desc.RenderTarget[0] = match key.blend {
            None => D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: BOOL(0),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_ZERO,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_ZERO,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: key.write_mask,
            },
            Some((c, a)) => D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: BOOL(1),
                SrcBlend: factor(c.src),
                DestBlend: factor(c.dst),
                BlendOp: op(c.op),
                SrcBlendAlpha: factor(a.src),
                DestBlendAlpha: factor(a.dst),
                BlendOpAlpha: op(a.op),
                RenderTargetWriteMask: key.write_mask,
            },
        };
        let mut s = None;
        // SAFETY: valid descriptor, live out-parameter.
        unsafe { device.raw().CreateBlendState(&desc, Some(&mut s)) }
            .map_err(|e| device.error("CreateBlendState", &e))?;
        let s = out(s, "CreateBlendState")?;
        self.blends.insert(key, s.clone());
        Ok(s)
    }

    pub fn raster(&mut self, device: &Device, key: RasterKey) -> Result<ID3D11RasterizerState> {
        if let Some(s) = self.rasters.get(&key) {
            return Ok(s.clone());
        }
        let desc = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: match key.cull {
                Cull::None => D3D11_CULL_NONE,
                Cull::Front => D3D11_CULL_FRONT,
                Cull::Back => D3D11_CULL_BACK,
            },
            FrontCounterClockwise: BOOL::from(key.front_ccw),
            DepthBias: 0,
            DepthBiasClamp: 0.0,
            SlopeScaledDepthBias: 0.0,
            DepthClipEnable: BOOL::from(key.depth_clip),
            ScissorEnable: BOOL::from(key.scissor),
            MultisampleEnable: BOOL::from(key.multisample),
            AntialiasedLineEnable: BOOL(0),
        };
        let mut s = None;
        // SAFETY: valid descriptor, live out-parameter.
        unsafe { device.raw().CreateRasterizerState(&desc, Some(&mut s)) }
            .map_err(|e| device.error("CreateRasterizerState", &e))?;
        let s = out(s, "CreateRasterizerState")?;
        self.rasters.insert(key, s.clone());
        Ok(s)
    }

    pub fn depth(&mut self, device: &Device, key: DepthKey) -> Result<ID3D11DepthStencilState> {
        if let Some(s) = self.depths.get(&key) {
            return Ok(s.clone());
        }
        let keep = D3D11_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D11_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D11_STENCIL_OP_KEEP,
            StencilPassOp: D3D11_STENCIL_OP_KEEP,
            StencilFunc: D3D11_COMPARISON_ALWAYS,
        };
        let desc = D3D11_DEPTH_STENCIL_DESC {
            DepthEnable: BOOL::from(key.test || key.write),
            DepthWriteMask: if key.write {
                D3D11_DEPTH_WRITE_MASK_ALL
            } else {
                D3D11_DEPTH_WRITE_MASK_ZERO
            },
            DepthFunc: if key.test {
                D3D11_COMPARISON_LESS_EQUAL
            } else {
                D3D11_COMPARISON_ALWAYS
            },
            StencilEnable: BOOL(0),
            StencilReadMask: 0xFF,
            StencilWriteMask: 0xFF,
            FrontFace: keep,
            BackFace: keep,
        };
        let mut s = None;
        // SAFETY: valid descriptor, live out-parameter.
        unsafe { device.raw().CreateDepthStencilState(&desc, Some(&mut s)) }
            .map_err(|e| device.error("CreateDepthStencilState", &e))?;
        let s = out(s, "CreateDepthStencilState")?;
        self.depths.insert(key, s.clone());
        Ok(s)
    }

    /// Objects created so far (samplers, blend, rasteriser, depth).
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.samplers.len(),
            self.blends.len(),
            self.rasters.len(),
            self.depths.len(),
        )
    }
}

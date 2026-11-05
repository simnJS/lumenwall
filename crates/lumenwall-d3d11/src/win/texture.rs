//! Sampled textures for every texel format `lumenwall-formats` produces: 2D or volume, all mips,
//! immutable with their pixels given at creation (one copy in GPU memory, no CPU copy kept), or
//! writable for content that changes (video frames uploaded from the CPU, cover art).

use std::ffi::c_void;

use lumenwall_formats::tex::GpuFormat;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE3D_DESC,
    D3D11_USAGE, D3D11_USAGE_DEFAULT, D3D11_USAGE_IMMUTABLE, ID3D11Resource,
    ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11Texture3D,
};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

use super::device::Device;
use crate::error::{Error, Result, out};
use crate::layout::{Dimension, MipLevel, pitches, validate_upload};

/// The DXGI format of a `lumenwall-formats` texel format (always UNORM/float, never sRGB).
#[must_use]
pub fn dxgi_format(f: GpuFormat) -> DXGI_FORMAT {
    match f {
        GpuFormat::R8Unorm => DXGI_FORMAT_R8_UNORM,
        GpuFormat::Rg8Unorm => DXGI_FORMAT_R8G8_UNORM,
        GpuFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
        GpuFormat::Bc1RgbaUnorm => DXGI_FORMAT_BC1_UNORM,
        GpuFormat::Bc2RgbaUnorm => DXGI_FORMAT_BC2_UNORM,
        GpuFormat::Bc3RgbaUnorm => DXGI_FORMAT_BC3_UNORM,
        GpuFormat::Bc7RgbaUnorm => DXGI_FORMAT_BC7_UNORM,
        GpuFormat::R16Float => DXGI_FORMAT_R16_FLOAT,
        GpuFormat::Rg16Float => DXGI_FORMAT_R16G16_FLOAT,
        GpuFormat::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
        GpuFormat::Rgb10a2Unorm => DXGI_FORMAT_R10G10B10A2_UNORM,
    }
}

/// A sampled texture.
pub struct Texture {
    resource: ID3D11Resource,
    srv: ID3D11ShaderResourceView,
    format: GpuFormat,
    dimension: Dimension,
    size: (u32, u32, u32),
    mip_levels: u32,
    writable: bool,
}

impl Texture {
    /// An immutable texture with every level (validated first: a bad upload is an error, not a
    /// driver crash).
    pub fn new(
        device: &Device,
        format: GpuFormat,
        dimension: Dimension,
        levels: &[MipLevel<'_>],
    ) -> Result<Self> {
        validate_upload(format, dimension, levels)?;
        Self::create(device, format, dimension, levels, D3D11_USAGE_IMMUTABLE)
    }

    /// A writable 2D texture of one level, updated with [`Texture::write_level0`]; `initial`
    /// fills it at creation (zeros when `None`).
    pub fn new_writable(
        device: &Device,
        format: GpuFormat,
        size: (u32, u32),
        initial: Option<&[u8]>,
    ) -> Result<Self> {
        let bytes = format.level_size(size.0, size.1) as usize;
        let zeros;
        let data = match initial {
            Some(d) => d,
            None => {
                zeros = vec![0u8; bytes];
                &zeros
            }
        };
        let level = [MipLevel {
            width: size.0,
            height: size.1,
            depth: 1,
            data,
        }];
        validate_upload(format, Dimension::D2, &level)?;
        Self::create(device, format, Dimension::D2, &level, D3D11_USAGE_DEFAULT)
    }

    fn create(
        device: &Device,
        format: GpuFormat,
        dimension: Dimension,
        levels: &[MipLevel<'_>],
        usage: D3D11_USAGE,
    ) -> Result<Self> {
        let init: Vec<D3D11_SUBRESOURCE_DATA> = levels
            .iter()
            .map(|l| {
                let (row, slice) = pitches(format, l.width, l.height);
                D3D11_SUBRESOURCE_DATA {
                    pSysMem: l.data.as_ptr().cast::<c_void>(),
                    SysMemPitch: row,
                    SysMemSlicePitch: slice,
                }
            })
            .collect();
        let base = levels[0];
        let dxgi = dxgi_format(format);
        let mip_levels = levels.len() as u32;
        let raw = device.raw();
        // SAFETY: descriptors match `init`: one entry per level, each pointing to exactly the
        // bytes `validate_upload` checked; `levels` outlives the calls; out-parameters are live.
        let resource: ID3D11Resource = unsafe {
            match dimension {
                Dimension::D2 => {
                    let mut t: Option<ID3D11Texture2D> = None;
                    raw.CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: base.width,
                            Height: base.height,
                            MipLevels: mip_levels,
                            ArraySize: 1,
                            Format: dxgi,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: usage,
                            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                            CPUAccessFlags: 0,
                            MiscFlags: 0,
                        },
                        Some(init.as_ptr()),
                        Some(&mut t),
                    )
                    .map_err(|e| device.error("CreateTexture2D", &e))?;
                    out(t, "CreateTexture2D")?
                        .cast()
                        .map_err(|e| device.error("cast", &e))?
                }
                Dimension::D3 => {
                    let mut t: Option<ID3D11Texture3D> = None;
                    raw.CreateTexture3D(
                        &D3D11_TEXTURE3D_DESC {
                            Width: base.width,
                            Height: base.height,
                            Depth: base.depth,
                            MipLevels: mip_levels,
                            Format: dxgi,
                            Usage: usage,
                            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                            CPUAccessFlags: 0,
                            MiscFlags: 0,
                        },
                        Some(init.as_ptr()),
                        Some(&mut t),
                    )
                    .map_err(|e| device.error("CreateTexture3D", &e))?;
                    out(t, "CreateTexture3D")?
                        .cast()
                        .map_err(|e| device.error("cast", &e))?
                }
            }
        };
        let mut srv = None;
        // SAFETY: the resource was created with the shader-resource bind flag; a null
        // descriptor views every level.
        unsafe { raw.CreateShaderResourceView(&resource, None, Some(&mut srv)) }
            .map_err(|e| device.error("CreateShaderResourceView", &e))?;
        Ok(Self {
            resource,
            srv: out(srv, "CreateShaderResourceView")?,
            format,
            dimension,
            size: (base.width, base.height, base.depth),
            mip_levels,
            writable: usage == D3D11_USAGE_DEFAULT,
        })
    }

    /// Replaces level 0 of a writable texture (tightly packed rows). The copy is queued on the
    /// context: no CPU copy is kept, nothing waits.
    pub fn write_level0(&self, device: &Device, data: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(Error::InvalidData("texture is immutable".into()));
        }
        let (w, h, _) = self.size;
        let (row, slice) = pitches(self.format, w, h);
        if data.len() != slice as usize {
            return Err(Error::InvalidData(format!(
                "level 0 needs {slice} bytes, got {}",
                data.len()
            )));
        }
        // SAFETY: a DEFAULT texture of this device; `data` holds exactly one level.
        unsafe {
            device.context().UpdateSubresource(
                &self.resource,
                0,
                None,
                data.as_ptr().cast(),
                row,
                slice,
            );
        }
        Ok(())
    }

    #[must_use]
    pub fn srv(&self) -> &ID3D11ShaderResourceView {
        &self.srv
    }

    #[must_use]
    pub fn resource(&self) -> &ID3D11Resource {
        &self.resource
    }

    #[must_use]
    pub fn format(&self) -> GpuFormat {
        self.format
    }

    #[must_use]
    pub fn dimension(&self) -> Dimension {
        self.dimension
    }

    /// `(width, height, depth)` of level 0.
    #[must_use]
    pub fn size(&self) -> (u32, u32, u32) {
        self.size
    }

    #[must_use]
    pub fn mip_levels(&self) -> u32 {
        self.mip_levels
    }

    /// GPU bytes of the pixels (counters).
    #[must_use]
    pub fn bytes(&self) -> u64 {
        let (w, h, d) = self.size;
        (0..self.mip_levels)
            .map(|k| {
                let depth = if self.dimension == Dimension::D3 {
                    (d >> k).max(1)
                } else {
                    1
                };
                self.format
                    .volume_size((w >> k).max(1), (h >> k).max(1), depth)
            })
            .sum()
    }
}

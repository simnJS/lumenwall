//! Reading textures back to the CPU (headless renders, screenshots, tests): a staging copy,
//! `Map(READ)` (which waits for the GPU), tightly packed rows. Never on the frame path; the staging
//! texture is released before returning, so it leaves no system memory behind.

use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_RESOURCE_DIMENSION_TEXTURE2D, D3D11_RESOURCE_DIMENSION_TEXTURE3D, D3D11_TEXTURE2D_DESC,
    D3D11_TEXTURE3D_DESC, D3D11_USAGE_STAGING, ID3D11Resource, ID3D11Texture2D, ID3D11Texture3D,
};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

use super::device::Device;
use crate::error::{Error, Result, out};

/// `(block edge, bytes per block)` of the formats this crate creates.
fn block_info(f: DXGI_FORMAT) -> Option<(u32, u32)> {
    Some(match f {
        DXGI_FORMAT_R8_UNORM => (1, 1),
        DXGI_FORMAT_R8G8_UNORM | DXGI_FORMAT_R16_FLOAT => (1, 2),
        DXGI_FORMAT_R8G8B8A8_UNORM
        | DXGI_FORMAT_B8G8R8A8_UNORM
        | DXGI_FORMAT_R16G16_FLOAT
        | DXGI_FORMAT_R10G10B10A2_UNORM
        | DXGI_FORMAT_D32_FLOAT => (1, 4),
        DXGI_FORMAT_R16G16B16A16_FLOAT => (1, 8),
        DXGI_FORMAT_BC1_UNORM => (4, 8),
        DXGI_FORMAT_BC2_UNORM | DXGI_FORMAT_BC3_UNORM | DXGI_FORMAT_BC7_UNORM => (4, 16),
        _ => return None,
    })
}

/// Mip level `mip` of a single-sample 2D texture or volume, tightly packed (rows of blocks,
/// slices one after the other), and its `(width, height, depth)`.
pub fn read_subresource(
    device: &Device,
    resource: &ID3D11Resource,
    mip: u32,
) -> Result<(Vec<u8>, (u32, u32, u32), DXGI_FORMAT)> {
    // SAFETY: plain query of a live resource.
    let kind = unsafe { resource.GetType() };
    let raw = device.raw();
    // A staging copy of the whole resource (copying one BC level into a texture of its own size
    // would break the "multiple of 4" rule of BC level 0).
    let (staging, format, size): (ID3D11Resource, DXGI_FORMAT, (u32, u32, u32)) = if kind
        == D3D11_RESOURCE_DIMENSION_TEXTURE2D
    {
        let t: ID3D11Texture2D = resource.cast().map_err(|e| device.error("cast", &e))?;
        let mut d = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: live out-parameter.
        unsafe { t.GetDesc(&mut d) };
        if d.SampleDesc.Count > 1 {
            return Err(Error::Unsupported("reading a multisampled texture (resolve it first)".into()));
        }
        if mip >= d.MipLevels {
            return Err(Error::InvalidData(format!("mip {mip} of {}", d.MipLevels)));
        }
        let size = ((d.Width >> mip).max(1), (d.Height >> mip).max(1), 1);
        let desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            ArraySize: 1,
            ..d
        };
        let mut s = None;
        // SAFETY: valid staging descriptor, live out-parameter.
        unsafe { raw.CreateTexture2D(&desc, None, Some(&mut s)) }
            .map_err(|e| device.error("CreateTexture2D", &e))?;
        let s: ID3D11Resource = out(s, "CreateTexture2D")?.cast().map_err(|e| device.error("cast", &e))?;
        (s, d.Format, size)
    } else if kind == D3D11_RESOURCE_DIMENSION_TEXTURE3D {
        let t: ID3D11Texture3D = resource.cast().map_err(|e| device.error("cast", &e))?;
        let mut d = D3D11_TEXTURE3D_DESC::default();
        // SAFETY: live out-parameter.
        unsafe { t.GetDesc(&mut d) };
        if mip >= d.MipLevels {
            return Err(Error::InvalidData(format!("mip {mip} of {}", d.MipLevels)));
        }
        let size = (
            (d.Width >> mip).max(1),
            (d.Height >> mip).max(1),
            (d.Depth >> mip).max(1),
        );
        let desc = D3D11_TEXTURE3D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            ..d
        };
        let mut s = None;
        // SAFETY: valid staging descriptor, live out-parameter.
        unsafe { raw.CreateTexture3D(&desc, None, Some(&mut s)) }
            .map_err(|e| device.error("CreateTexture3D", &e))?;
        let s: ID3D11Resource = out(s, "CreateTexture3D")?.cast().map_err(|e| device.error("cast", &e))?;
        (s, d.Format, size)
    } else {
        return Err(Error::Unsupported("reading back a buffer or 1D texture".into()));
    };
    let (block, block_bytes) =
        block_info(format).ok_or_else(|| Error::Unsupported(format!("reading format {}", format.0)))?;
    let row_bytes = size.0.div_ceil(block) * block_bytes;
    let rows = size.1.div_ceil(block);
    let ctx = device.context();
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: same-sized resources of one device; `Map(READ)` on a CPU-readable staging
    // resource waits for the copy; rows are read inside the mapping, within the pitches the
    // runtime returned, before `Unmap`.
    let data = unsafe {
        ctx.CopyResource(&staging, resource);
        ctx.Map(&staging, mip, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| device.error("Map", &e))?;
        let mut data = Vec::with_capacity((row_bytes * rows * size.2) as usize);
        let base = mapped.pData.cast::<u8>().cast_const();
        for z in 0..size.2 as usize {
            for y in 0..rows as usize {
                let row = base.add(z * mapped.DepthPitch as usize + y * mapped.RowPitch as usize);
                data.extend_from_slice(std::slice::from_raw_parts(row, row_bytes as usize));
            }
        }
        ctx.Unmap(&staging, mip);
        data
    };
    Ok((data, size, format))
}

/// Level 0 of an RGBA8 or BGRA8 texture as RGBA8 rows (swap chain buffers are BGRA8).
pub fn read_texture_rgba8(device: &Device, texture: &ID3D11Texture2D) -> Result<(u32, u32, Vec<u8>)> {
    let resource: ID3D11Resource = texture.cast().map_err(|e| device.error("cast", &e))?;
    let (mut data, (w, h, _), format) = read_subresource(device, &resource, 0)?;
    match format {
        DXGI_FORMAT_R8G8B8A8_UNORM => {}
        DXGI_FORMAT_B8G8R8A8_UNORM => {
            for px in data.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        f => return Err(Error::Unsupported(format!("RGBA8 readback of format {}", f.0))),
    }
    Ok((w, h, data))
}

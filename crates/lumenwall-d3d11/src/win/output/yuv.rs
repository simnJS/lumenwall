//! YUV → RGBA conversion of video frames (spec 07 §14): hardware frames (D3D11VA texture array
//! slices, NV12 and P010) and CPU frames (writable textures) into an RGBA8 target.

use naga::Module;
use windows::Win32::Graphics::Direct3D11::{ID3D11SamplerState, ID3D11ShaderResourceView};

use super::pass::{Destination, FullscreenPass, f32_bytes};
use crate::cache::BytecodeCache;
use crate::error::Result;
use crate::state::SamplerKey;
use crate::win::{DecodedFrame, Device, PlaneFormat, RenderTarget, StateCache, plane_views};

/// WGSL of the conversion program: `lumenwall-render`'s `yuv_float.wgsl` under this crate's
/// binding contract (pass block `b0`, planes `t0` `t1` `t2`, sampler `s0`).
pub const YUV_WGSL: &str = include_str!("shaders/yuv.wgsl");

/// Colour conversion of a video frame (`lumenwall-render`'s `yuv::conversion_matrix`, chroma
/// siting and decoder padding crop).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YuvParams {
    /// Rows of the YUV → RGB matrix (with offsets), as `yuv::conversion_matrix` returns them:
    /// with `PlaneDepth::U8` for 8-bit planes, with `PlaneDepth::U16` for 16-bit planes (P010:
    /// `bits: 10, shift: 6`). The converter rescales the columns of 16-bit planes itself, since
    /// the views are UNORM (`value / 65535`) while that matrix expects the raw container value.
    pub matrix: [[f32; 4]; 3],
    /// Chroma sample offset in chroma texels.
    pub chroma_offset: [f32; 2],
    /// Visible part of the decoded texture, `(u1, v1)` (padding crop); `[1.0, 1.0]` for none.
    pub crop: [f32; 2],
}

/// How the chroma samples are stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChromaLayout {
    /// One plane with both chroma samples per texel (NV12, P010).
    #[default]
    Interleaved,
    /// Like `Interleaved` with V before U (NV21).
    InterleavedSwapped,
    /// Two planes, U then V (I420).
    Planar,
}

/// Plane views of one frame. 8-bit planes are `R8_UNORM` / `R8G8_UNORM` views, 16-bit planes
/// `R16_UNORM` / `R16G16_UNORM` views (`DXGI_FORMAT`s of [`PlaneFormat::plane_formats`]); a CPU
/// frame in 8 bits uses `Texture::new_writable` with `R8Unorm` / `Rg8Unorm` / `R8Unorm`.
pub struct YuvPlanes<'a> {
    pub luma: &'a ID3D11ShaderResourceView,
    /// The interleaved chroma plane, or U for [`ChromaLayout::Planar`].
    pub chroma: &'a ID3D11ShaderResourceView,
    /// V for [`ChromaLayout::Planar`].
    pub chroma_v: Option<&'a ID3D11ShaderResourceView>,
    pub layout: ChromaLayout,
    /// 16-bit views (P010 and friends) instead of 8-bit.
    pub sixteen_bit: bool,
}

/// YUV → RGBA draw into a target.
pub struct YuvConverter {
    pass: FullscreenPass,
    sampler: ID3D11SamplerState,
}

impl YuvConverter {
    /// `module` is [`YUV_WGSL`] parsed by naga.
    pub fn new(
        device: &Device,
        states: &mut StateCache,
        cache: Option<&BytecodeCache>,
        module: &Module,
    ) -> Result<Self> {
        Ok(Self {
            pass: FullscreenPass::new(device, states, cache, module, 96, 0b111)?,
            sampler: states.sampler(device, SamplerKey::CLAMP_LINEAR)?,
        })
    }

    /// Converts one hardware frame (planes from `external::plane_views`).
    pub fn convert(
        &mut self,
        device: &Device,
        frame: &DecodedFrame,
        params: &YuvParams,
        target: &RenderTarget,
    ) -> Result<()> {
        let (luma, chroma) = plane_views(device, frame)?;
        self.convert_planes(
            device,
            &YuvPlanes {
                luma: &luma,
                chroma: &chroma,
                chroma_v: None,
                layout: ChromaLayout::Interleaved,
                sixteen_bit: frame.format == PlaneFormat::P010,
            },
            params,
            target,
        )
    }

    /// Converts a frame given as plane views (hardware frames whose views the caller caches, or
    /// CPU frames in writable textures).
    pub fn convert_planes(
        &mut self,
        device: &Device,
        planes: &YuvPlanes<'_>,
        params: &YuvParams,
        target: &RenderTarget,
    ) -> Result<()> {
        let (w, h) = target.size();
        let (w, h) = (w as f32, h as f32);
        let scale = if planes.sixteen_bit { 65535.0 } else { 1.0 };
        let mut block = Vec::with_capacity(24);
        for row in &params.matrix {
            block.extend([row[0] * scale, row[1] * scale, row[2] * scale, row[3]]);
        }
        block.extend([
            params.chroma_offset[0],
            params.chroma_offset[1],
            f32::from(u8::from(planes.layout == ChromaLayout::Planar)),
            f32::from(u8::from(planes.layout == ChromaLayout::InterleavedSwapped)),
        ]);
        block.extend([w, h, 1.0 / w, 1.0 / h]);
        block.extend([params.crop[0], params.crop[1], 0.0, 0.0]);
        // Slot 2 is only read for three planes; any view keeps the slot valid otherwise.
        let third = planes.chroma_v.unwrap_or(planes.chroma);
        self.pass.draw(
            device,
            &f32_bytes(&block),
            &[planes.luma, planes.chroma, third],
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

//! Output passes: composition into a swap chain or a target, headless output, YUV → RGBA
//! conversion of video frames, copies of external (shared) textures.
//!
//! **Owned by task D3D11 4/n.** This file is the interface the engine (task 5/n), the video
//! (6/n) and the web (7/n) tasks code against. Until task 4/n implements it, every operation
//! returns [`Error::Unsupported`]; the owning task may change these signatures. What it implements
//! is in the crate README ("4/n: Direct3D 11 output passes").

use windows::Win32::Graphics::Direct3D11::{
    ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D,
};

use super::device::Device;
use super::external::DecodedFrame;
use super::state::StateCache;
use super::target::RenderTarget;
use crate::cache::BytecodeCache;
use crate::error::{Error, Result};
use crate::state::AddressMode;

fn pending(what: &str) -> Error {
    Error::Unsupported(format!("{what}: not implemented yet (task D3D11 4/n)"))
}

/// Parameters of one composition draw (`lumenwall-render`'s `OutputComposer` state).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComposeParams {
    /// `(u0, v0, u1, v1)` window of the scene shown on the output (`compose::uv_range`).
    pub uv: [f32; 4],
    /// Sampling outside `[0, 1]` (clamp, transparent border, repeat).
    pub clamp: AddressMode,
    pub flip_vertical: bool,
    /// Size of the destination in pixels.
    pub size: (u32, u32),
    /// Clear colour of the destination before the draw (the scene's clear colour), alpha 1.
    pub clear: Option<[f32; 4]>,
}

/// The composition program (one fullscreen triangle, blending off, alpha 1).
pub struct Compositor {
    _private: (),
}

impl Compositor {
    pub fn new(
        _device: &Device,
        _states: &mut StateCache,
        _cache: Option<&BytecodeCache>,
    ) -> Result<Self> {
        Err(pending("Compositor::new"))
    }

    /// Draws `source` into `target` (a swap chain view or a target's draw view).
    pub fn encode(
        &self,
        _device: &Device,
        _source: &ID3D11ShaderResourceView,
        _target: &ID3D11RenderTargetView,
        _params: &ComposeParams,
    ) -> Result<()> {
        Err(pending("Compositor::encode"))
    }
}

/// Off-screen output of headless renders: an RGBA8 target and its readback.
pub struct HeadlessOutput {
    _private: (),
}

impl HeadlessOutput {
    pub fn new(_device: &Device, _size: (u32, u32)) -> Result<Self> {
        Err(pending("HeadlessOutput::new"))
    }

    #[must_use]
    pub fn target(&self) -> Option<&RenderTarget> {
        None
    }

    /// RGBA8 rows of the output.
    pub fn read_rgba8(&self, _device: &Device) -> Result<Vec<u8>> {
        Err(pending("HeadlessOutput::read_rgba8"))
    }
}

/// Colour conversion of a video frame (`lumenwall-render`'s `yuv::conversion_matrix`, chroma
/// siting and decoder padding crop).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YuvParams {
    /// Rows of the YUV → RGB matrix (with offsets), as `yuv::conversion_matrix` returns them.
    pub matrix: [[f32; 4]; 3],
    /// Chroma sample offset in chroma texels.
    pub chroma_offset: [f32; 2],
    /// Visible part of the decoded texture, `(u1, v1)` (padding crop).
    pub crop: [f32; 2],
}

/// YUV → RGBA draw into a target.
pub struct YuvConverter {
    _private: (),
}

impl YuvConverter {
    pub fn new(
        _device: &Device,
        _states: &mut StateCache,
        _cache: Option<&BytecodeCache>,
    ) -> Result<Self> {
        Err(pending("YuvConverter::new"))
    }

    /// Converts one decoded frame (planes from `external::plane_views`).
    pub fn convert(
        &self,
        _device: &Device,
        _frame: &DecodedFrame,
        _params: &YuvParams,
        _target: &RenderTarget,
    ) -> Result<()> {
        Err(pending("YuvConverter::convert"))
    }
}

/// Copy of an external texture (a shared BGRA or RGBA frame) into a target, by draw.
pub struct ExternalCopy {
    _private: (),
}

impl ExternalCopy {
    pub fn new(
        _device: &Device,
        _states: &mut StateCache,
        _cache: Option<&BytecodeCache>,
    ) -> Result<Self> {
        Err(pending("ExternalCopy::new"))
    }

    pub fn copy(
        &self,
        _device: &Device,
        _source: &ID3D11Texture2D,
        _target: &RenderTarget,
    ) -> Result<()> {
        Err(pending("ExternalCopy::copy"))
    }
}

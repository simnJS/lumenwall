//! Scene executor: a [`crate::plan::ScenePlan`] and its uploads → draws on the immediate context.
//!
//! **Owned by task D3D11 3/n.** This file is the interface the bridge (task 5/n) codes against.
//! Until task 3/n implements it, every operation returns [`Error::Unsupported`]; the owning task may
//! change these signatures. What it implements is in the crate README ("3/n: Direct3D 11 scene
//! executor").

use std::marker::PhantomData;

use lumenwall_formats::tex::GpuFormat;
use windows::Win32::Graphics::Direct3D11::{ID3D11ShaderResourceView, ID3D11Texture2D};

use super::device::Device;
use crate::cache::{BytecodeCache, CacheKey};
use crate::error::{Error, Result};
use crate::hlsl::HlslProgram;
use crate::layout::{Dimension, MipLevel};
use crate::plan::{FrameInput, MeshLayoutPlan, OpPlan, PassPlan, ScenePlan};

fn pending(what: &str) -> Error {
    Error::Unsupported(format!("{what}: not implemented yet (task D3D11 3/n)"))
}

/// Load-time builder: uploads first (each returns its index in the plan), then
/// [`SceneBuilder::build`].
pub struct SceneBuilder<'d> {
    _device: PhantomData<&'d Device>,
}

impl<'d> SceneBuilder<'d> {
    #[must_use]
    pub fn new(_device: &'d Device, _cache: Option<&'d BytecodeCache>) -> Self {
        Self {
            _device: PhantomData,
        }
    }

    /// An immutable texture (2D or volume); its index for `TextureSource::Texture`.
    pub fn add_texture(
        &mut self,
        _format: GpuFormat,
        _dimension: Dimension,
        _levels: &[MipLevel<'_>],
    ) -> Result<usize> {
        Err(pending("SceneBuilder::add_texture"))
    }

    /// A program through the bytecode cache; its index for `PassPlan::program`.
    pub fn add_program(&mut self, _program: &HlslProgram, _key: Option<CacheKey>) -> Result<usize> {
        Err(pending("SceneBuilder::add_program"))
    }

    /// A static mesh; its index for `GeometryPlan::Mesh`.
    pub fn add_mesh(
        &mut self,
        _layout: &MeshLayoutPlan,
        _vertices: &[u8],
        _indices: &[u16],
    ) -> Result<usize> {
        Err(pending("SceneBuilder::add_mesh"))
    }

    /// Creates every target, buffer, state and input layout of the plan (and trims the device).
    pub fn build(self, _plan: &ScenePlan) -> Result<Scene> {
        Err(pending("SceneBuilder::build"))
    }
}

/// A built scene.
pub struct Scene {
    _private: (),
}

impl Scene {
    /// Records one frame from the scene's CPU state.
    pub fn render(&mut self, _device: &Device, _frame: &FrameInput<'_>) -> Result<()> {
        Err(pending("Scene::render"))
    }

    /// Passes added after load (scripts' layers); their indices follow the existing ones.
    pub fn extend(&mut self, _device: &Device, _passes: Vec<PassPlan>) -> Result<()> {
        Err(pending("Scene::extend"))
    }

    /// A new frame order (visibility changes of layers, reordering).
    pub fn set_ops(&mut self, _ops: Vec<OpPlan>) -> Result<()> {
        Err(pending("Scene::set_ops"))
    }

    /// The sampled (resolved) texture of a target: the composition reads `_rt_FullFrameBuffer`.
    #[must_use]
    pub fn target_srv(&self, _target: usize) -> Option<&ID3D11ShaderResourceView> {
        None
    }

    /// The texture of a target, for readbacks and uploads into it (cover art).
    #[must_use]
    pub fn target_texture(&self, _target: usize) -> Option<&ID3D11Texture2D> {
        None
    }
}

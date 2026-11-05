//! The draw every output pass shares: one oversized triangle (`SV_VertexID`, no vertex buffer, no
//! input layout), a pass block in a one-window constant arena, textures from slot 0 and one
//! sampler, blending off, no depth, one render target.

use naga::Module;
use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_VIEWPORT, ID3D11BlendState, ID3D11RasterizerState, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView,
};

use crate::cache::{BytecodeCache, CompileOptions};
use crate::error::{Error, Result};
use crate::hlsl;
use crate::layout::ConstantLayout;
use crate::layout::ConstantRegion;
use crate::state::{BlendKey, RasterKey};
use crate::win::{ConstantArena, Device, ShaderProgram, StateCache, compile_program};

/// Where a pass draws.
pub(super) struct Destination<'a> {
    pub rtv: &'a ID3D11RenderTargetView,
    /// Size in pixels of the target the view belongs to.
    pub size: (u32, u32),
    /// Clear colour applied before the draw.
    pub clear: Option<[f32; 4]>,
}

pub(super) struct FullscreenPass {
    program: ShaderProgram,
    arena: ConstantArena,
    block: ConstantRegion,
    block_len: usize,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    /// Bit N: the program samples slot N.
    textures: u8,
}

impl FullscreenPass {
    /// Translates `module` (entry points `vs_main` / `fs_main`), compiles it (through `cache`) and
    /// checks the pass block holds exactly `block_len` bytes and the textures are `textures`.
    pub(super) fn new(
        device: &Device,
        states: &mut StateCache,
        cache: Option<&BytecodeCache>,
        module: &Module,
        block_len: usize,
        textures: u8,
    ) -> Result<Self> {
        let hlsl = hlsl::translate(module, "vs_main", "fs_main")?;
        let declared = hlsl.resources.uniform_block.map(|b| b as usize);
        if declared != Some(block_len) || hlsl.resources.textures != textures {
            return Err(Error::Shader(format!(
                "output shader declares a {declared:?}-byte pass block and textures {:#b}, \
                 expected {block_len} bytes and {textures:#b}",
                hlsl.resources.textures
            )));
        }
        if !hlsl.vertex_inputs.is_empty() {
            return Err(Error::Shader(
                "output shaders take no vertex input (SV_VertexID)".into(),
            ));
        }
        let (program, _) = compile_program(device, &hlsl, cache, None, CompileOptions::default())?;
        let mut layout = ConstantLayout::new();
        let block = layout.alloc(block_len as u32)?;
        Ok(Self {
            program,
            arena: ConstantArena::new(device, &layout)?,
            block,
            block_len,
            blend: states.blend(device, BlendKey::OPAQUE)?,
            raster: states.raster(device, RasterKey::default())?,
            textures,
        })
    }

    /// Draws into `dest`: `block` is the pass block,
    /// `views` the textures of slots 0.. and `sampler` the one at slot 0.
    pub(super) fn draw(
        &mut self,
        device: &Device,
        block: &[u8],
        views: &[&ID3D11ShaderResourceView],
        sampler: &ID3D11SamplerState,
        dest: Destination<'_>,
    ) -> Result<()> {
        if block.len() != self.block_len || views.len() != self.textures.count_ones() as usize {
            return Err(Error::InvalidData(format!(
                "output pass needs a {}-byte block and {} textures, got {} and {}",
                self.block_len,
                self.textures.count_ones(),
                block.len(),
                views.len()
            )));
        }
        let Destination { rtv, size, clear } = dest;
        if size.0 == 0 || size.1 == 0 {
            return Err(Error::InvalidData("output of size 0".into()));
        }
        self.arena.write(self.block, 0, block)?;
        self.arena.upload(device)?;
        let ctx = device.context();
        let srvs: Vec<Option<ID3D11ShaderResourceView>> =
            views.iter().map(|v| Some((*v).clone())).collect();
        // Slot numbers follow the register of each texture: slots are dense from 0 here (the
        // output shaders declare `t0..tN`), checked by the `textures` mask in `new`.
        // SAFETY: every object belongs to this device; the viewport is a plain value; the
        // triangle needs no vertex data (`SV_VertexID`), so no buffer or layout is bound; the
        // views are unbound again before returning so a later pass may render into a source.
        unsafe {
            self.program.bind(device);
            self.arena.bind(device, 0, self.block);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.OMSetDepthStencilState(None, 0);
            ctx.OMSetBlendState(&self.blend, None, 0xFFFF_FFFF);
            ctx.RSSetState(&self.raster);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: size.0 as f32,
                Height: size.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            if let Some(c) = clear {
                ctx.ClearRenderTargetView(rtv, &c);
            }
            ctx.PSSetShaderResources(0, Some(&srvs));
            ctx.PSSetSamplers(0, Some(&[Some(sampler.clone())]));
            ctx.Draw(3, 0);
            ctx.PSSetShaderResources(0, Some(&vec![None; srvs.len()]));
        }
        Ok(())
    }
}

/// `f32`s as little-endian bytes (pass blocks).
pub(super) fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

//! Composition of the scene target into an output (spec 04 §21): UV window, clamp / border /
//! repeat, vertical flip, alpha forced to 1. One draw per output.

use naga::Module;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView,
};

use super::pass::{Destination, FullscreenPass, f32_bytes};
use crate::cache::BytecodeCache;
use crate::error::Result;
use crate::state::{AddressMode, SamplerKey};
use crate::win::{Device, StateCache};

/// WGSL of the composition program: a copy of `lumenwall-render`'s `compose.wgsl` under this
/// crate's binding contract (pass block `b0`, texture `t0`, sampler `s0`). Parse it with naga's
/// WGSL front end and hand the module to [`Compositor::new`].
pub const COMPOSE_WGSL: &str = include_str!("shaders/compose.wgsl");

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

impl ComposeParams {
    /// The pass block: `(u0, u1, v0, v1)` then the mode (`x` = 1: transparent border emulated in
    /// the shader, as the wgpu path does, so the sampler itself clamps).
    #[must_use]
    pub fn block(&self) -> [f32; 8] {
        let [u0, v0, u1, v1] = self.uv;
        let (v0, v1) = if self.flip_vertical {
            (1.0 - v0, 1.0 - v1)
        } else {
            (v0, v1)
        };
        let border = if self.clamp == AddressMode::Border {
            1.0
        } else {
            0.0
        };
        [u0, u1, v0, v1, border, 0.0, 0.0, 0.0]
    }
}

/// The composition program (one fullscreen triangle, blending off, alpha 1).
pub struct Compositor {
    pass: FullscreenPass,
    clamp: ID3D11SamplerState,
    repeat: ID3D11SamplerState,
}

impl Compositor {
    /// `module` is [`COMPOSE_WGSL`] parsed by naga.
    pub fn new(
        device: &Device,
        states: &mut StateCache,
        cache: Option<&BytecodeCache>,
        module: &Module,
    ) -> Result<Self> {
        Ok(Self {
            pass: FullscreenPass::new(device, states, cache, module, 32, 0b1)?,
            clamp: states.sampler(device, SamplerKey::CLAMP_LINEAR)?,
            repeat: states.sampler(
                device,
                SamplerKey {
                    address: AddressMode::Repeat,
                    ..SamplerKey::CLAMP_LINEAR
                },
            )?,
        })
    }

    /// Draws `source` (a single-sample view of the scene target) into `target` (a swap chain view
    /// or a target's draw view).
    pub fn encode(
        &mut self,
        device: &Device,
        source: &ID3D11ShaderResourceView,
        target: &ID3D11RenderTargetView,
        params: &ComposeParams,
    ) -> Result<()> {
        let sampler = if params.clamp == AddressMode::Repeat {
            &self.repeat
        } else {
            &self.clamp
        };
        self.pass.draw(
            device,
            &f32_bytes(&params.block()),
            &[source],
            sampler,
            Destination {
                rtv: target,
                size: params.size,
                clear: params.clear,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_orders_the_window_and_flips() {
        let p = ComposeParams {
            uv: [0.1, 0.25, 0.9, 0.75],
            clamp: AddressMode::Clamp,
            flip_vertical: false,
            size: (4, 4),
            clear: None,
        };
        assert_eq!(p.block()[..5], [0.1, 0.9, 0.25, 0.75, 0.0]);
        let f = ComposeParams {
            flip_vertical: true,
            clamp: AddressMode::Border,
            ..p
        };
        assert_eq!(f.block()[..5], [0.1, 0.9, 0.75, 0.25, 1.0]);
    }
}

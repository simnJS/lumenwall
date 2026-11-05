//! Direct3D 11 backend of Lumenwall's renderer on Windows (the design, the seam with
//! `lumenwall-render` and the follow-up tasks are in the crate README).
//!
//! This crate is the foundation: the building blocks the scene renderer on Direct3D 11 is made of,
//! ported from what `tools/d3d11-probe` measured.
//!
//! * Everywhere (pure, tested on Linux too): [`hlsl`] (naga → HLSL shader model 5.0 under the
//!   binding contract, sampler heap rewrite), [`cache`] (bytecode on disk, keyed and versioned),
//!   [`layout`] (constant arena windows, linear arenas, upload rules), [`state`] (fixed-function
//!   state keys), [`error`].
//! * Windows only: [`Device`] (feature level 11_0+, D3D11.1 context, device-loss detection,
//!   `Trim`), [`SwapChain`] (flip model per window, resize, occlusion), textures for every
//!   `lumenwall-formats` texel format, render targets with MSAA and depth, buffer arenas (one
//!   dynamic constant buffer bound with offsets, shared static vertex/index buffers, dynamic
//!   streams), state caches, shader compilation (`D3DCompile`) with the cache, GPU timers,
//!   process GPU memory (`QueryVideoMemoryInfo`), readback and an off-screen window.
//!
//! Performance rules (docs/PERFORMANCE.md, tools/d3d11-probe "Recommended design"): every object
//! is created at load time; a frame maps the constant arena once (`WRITE_DISCARD`) and binds
//! windows of it; few buffer objects (each one costs shared GPU memory while frames run);
//! `Device::trim` after every large load.

pub mod cache;
pub mod error;
pub mod hlsl;
pub mod layout;
pub mod state;

#[cfg(windows)]
mod win;

pub use cache::{Bytecode, BytecodeCache, CacheKey, CompileOptions};
pub use error::{Error, LostReason, Result};
pub use hlsl::{HlslProgram, InputKind, Register, Resources, VertexInput};
pub use layout::{ConstantLayout, ConstantRegion, Dimension, LinearArena, MipLevel};
pub use state::{AddressMode, BlendKey, Cull, DepthKey, RasterKey, SamplerKey};

#[cfg(windows)]
pub use win::*;

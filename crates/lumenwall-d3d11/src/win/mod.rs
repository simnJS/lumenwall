//! Everything that calls Direct3D 11 or DXGI (Windows only).
//!
//! Unsafe code: every Direct3D call is an FFI call through the `windows` crate. Each `unsafe`
//! block says what makes it sound; the common ground is that interfaces are owned (`windows`
//! reference-counts them), descriptors and initial data outlive the call, and out-parameters
//! point to live locals.

mod buffer;
mod compile;
mod device;
mod external;
mod memory;
mod query;
mod readback;
mod state;
mod swapchain;
mod target;
mod texture;
mod window;

/// Output passes (task D3D11 4/n owns this folder).
pub mod output;
/// Scene executor (task D3D11 3/n owns this folder).
pub mod scene;

pub use buffer::{ConstantArena, DynamicBuffer, StaticBuffer, StaticBufferKind};
pub use compile::{
    CacheOutcome, DEFAULT_ATTRIBUTES, ShaderProgram, VertexElement, compile_hlsl, compile_program,
    create_input_layout,
};
pub use device::{AdapterChoice, AdapterInfo, Caps, Device, DeviceOptions, Luid};
pub use external::{DecodedFrame, PlaneFormat, plane_views};
pub use memory::{GpuMemory, SegmentMemory};
pub use query::GpuTimer;
pub use readback::{Subresource, read_subresource, read_texture_rgba8};
pub use state::StateCache;
pub use swapchain::{PresentStatus, SwapChain};
pub use target::{DepthBuffer, RenderTarget, TargetDesc};
pub use texture::{Texture, dxgi_format};
pub use window::OffscreenWindow;

/// The raw `windows` crate, for callers that need an interface this crate does not wrap.
pub use windows;

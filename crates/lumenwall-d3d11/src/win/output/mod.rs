//! Output passes (task D3D11 4/n): composition into a swap chain or a target, headless output,
//! YUV → RGBA conversion of video frames, copies of external (shared) textures.
//!
//! Every pass is one oversized triangle drawn from `SV_VertexID` with blending off, a pass block
//! in a one-window constant arena and its textures from slot 0, so none of them needs a vertex
//! buffer or an input layout. Their shaders are WGSL (copies of `lumenwall-render`'s
//! `compose.wgsl` and `yuv_float.wgsl`, adapted to the binding contract) translated by
//! [`crate::hlsl::translate`]. This crate's naga has no WGSL front end, so constructors take the
//! parsed module: parse [`COMPOSE_WGSL`], [`YUV_WGSL`] or [`COPY_WGSL`] once and pass it in.
//! Bytecode goes through the optional [`crate::BytecodeCache`].

mod compose;
mod external;
mod headless;
mod pass;
mod yuv;

pub use compose::{COMPOSE_WGSL, ComposeParams, Compositor};
pub use external::{COPY_WGSL, ExternalCopy};
pub use headless::{HeadlessOutput, encode_png_rgba8};
pub use yuv::{ChromaLayout, YUV_WGSL, YuvConverter, YuvParams, YuvPlanes};

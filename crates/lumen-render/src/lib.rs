//! GPU scene renderer of Lumen: images, effects, final composition.
//!
//! Built from `specs/04-rendu-scene-effets.md` (main), `specs/03-shaders.md`
//! (engine uniforms) and `specs/05-particules.md` (vertex formats), plus the
//! public wgpu/naga documentation. See the crate README for the integration
//! contract and the decisions on the spec's open questions.
//!
//! Layers, from pure math to GPU:
//!
//! * [`math`], [`chain`], [`compose`] (UV windows), [`blend`]: spec formulas, unit tested;
//! * [`context`], [`surface`], [`texture`], [`targets`]: device, outputs, uploads, render targets;
//! * [`shader`], [`dynbuf`], [`graph`]: pass contract, uniforms, the compiled pass graph;
//! * [`image`]: image objects (chain plan → passes, per-frame geometry and matrices);
//! * [`bloom`], [`headless`], [`stats`]: bloom chain, off-screen readback, debug counters.
//!
//! Performance rules (docs/PERFORMANCE.md): all GPU objects are created at load
//! time; a steady-state frame performs at most one `write_buffer`, records
//! precomputed segments (render bundles where possible), allocates nothing and
//! can be skipped entirely when [`graph::RenderScene::needs_redraw`] is false.

pub mod blend;
pub mod bloom;
pub mod chain;
pub mod compose;
pub mod context;
pub mod dynbuf;
pub mod error;
pub mod graph;
pub mod headless;
pub mod image;
pub mod math;
pub mod shader;
pub mod stats;
pub mod surface;
pub mod targets;
pub mod texture;

pub use blend::{BlendMode, CullMode, WriteMask};
pub use compose::{ClampMode, ComposeSettings, Compositor, OutputComposer, ScalingMode};
pub use context::{ContextOptions, GpuCaps, GpuContext, GpuInstance};
pub use dynbuf::{FrameInputs, UniformSlot};
pub use error::{RenderError, Result};
pub use graph::{
    Geometry, Op, PassDesc, PassId, RenderScene, SceneBuilder, ShaderId, SkinRange, SlotBinding,
    StreamAttribute, StreamId, StreamLayout, TextureId, TextureRef,
};
pub use headless::HeadlessOutput;
pub use image::{
    ImageFrame, ImageHandle, ImagePass, ImageSpec, PuppetGeometry, SlotSource, add_image,
    add_image_with_puppet,
};
pub use math::{SkinVertex, UvRange, Vertex};
pub use shader::ShaderDesc;
pub use surface::OutputSurface;
pub use targets::{RenderTargetPool, RtId};
pub use texture::{MipLevel, TexFlags, TexelFormat, TextureUpload, VolumeLevel, VolumeUpload};

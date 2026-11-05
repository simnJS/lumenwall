//! The seam between the backend-neutral scene front ([`crate::graph`]) and the GPU backends.
//!
//! The front ([`crate::graph::SceneBuilder`], [`crate::graph::RenderScene`]) keeps everything
//! that does not depend on a device: passes and frame operations, the render target plan
//! (names, scopes, sizes, aliasing, transient marks), the uniform shadow and its patch lists,
//! the vertices, indices and skin records, stream and mesh layouts, quality settings, slot-0
//! variants, enabled flags, dirty flags and `needs_redraw`. It talks to the GPU only through
//! [`SceneBackend`], an object-safe trait, for what must happen at once (uploads, shaders,
//! meshes, streams), at build, when the structure changes, once per frame and for tools
//! (readback, profiling).
//!
//! Every call hands the backend a borrowed [`SceneDesc`]: the front's whole state, plain data.
//! Per frame the front makes one virtual call ([`SceneBackend::render`]) with that view and
//! allocates nothing.
//!
//! Backends:
//! * wgpu ([`crate::wgpu_scene::WgpuScene`], in this crate): Vulkan (recorded directly through
//!   wgpu-hal when it can, [`crate::hal`]) and Direct3D 12. [`crate::graph::SceneBuilder::new`]
//!   builds on it.
//! * Direct3D 11: `lumenwall-d3d11` runs a plain-data plan; the engine's bridge implements
//!   this trait by mapping a [`SceneDesc`] to that plan, and builds with
//!   [`crate::graph::SceneBuilder::with_backend`].

use std::any::Any;

use crate::context::GpuContext;
use crate::desc::{MeshLayout, Op, PassDesc, StreamLayout, TextureId, TextureRef};
use crate::dynbuf::{DynamicRegion, UniformBlock, UniformSlot};
use crate::error::Result;
use crate::math::SkinVertex;
use crate::quality::QualitySettings;
use crate::shader::{MAX_SLOTS, PreparedShader, ShaderInterface};
use crate::targets::{RenderTargetPool, RtId};
use crate::texture::{
    DEFAULT_TEXTURE_SAMPLER, SamplerKey, TextureInfo, TextureUpload, VolumeUpload,
};

/// The device a call runs on.
#[derive(Clone, Copy)]
pub enum Gpu<'a> {
    /// The wgpu device: the wgpu backend works on it, and render target pools allocate their
    /// textures on it.
    Wgpu(&'a GpuContext),
    /// A backend that holds its own device (Direct3D 11): nothing is created on wgpu, and render
    /// target pools only plan (names, sizes, aliasing).
    Other,
}

impl<'a> Gpu<'a> {
    /// The wgpu context, if the call runs on wgpu.
    #[must_use]
    pub fn wgpu(self) -> Option<&'a GpuContext> {
        match self {
            Gpu::Wgpu(ctx) => Some(ctx),
            Gpu::Other => None,
        }
    }
}

impl<'a> From<&'a GpuContext> for Gpu<'a> {
    fn from(ctx: &'a GpuContext) -> Self {
        Gpu::Wgpu(ctx)
    }
}

impl<'a, 'b> From<&'b &'a GpuContext> for Gpu<'a> {
    fn from(ctx: &'b &'a GpuContext) -> Self {
        Gpu::Wgpu(ctx)
    }
}

/// Command buffers of a frame recorded directly in Vulkan
/// ([`crate::graph::RenderScene::encode_frame`]): the caller's commands up to the scene with the
/// frame's uploads, then the scene. Empty on the wgpu path (everything went into the caller's
/// encoder) and on other backends.
#[derive(Default)]
pub struct FrameCommands {
    pub(crate) buffers: [Option<wgpu::CommandBuffer>; 2],
}

impl FrameCommands {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffers.iter().all(Option::is_none)
    }
}

impl IntoIterator for FrameCommands {
    type Item = wgpu::CommandBuffer;
    type IntoIter = std::iter::Flatten<std::array::IntoIter<Option<wgpu::CommandBuffer>, 2>>;

    fn into_iter(self) -> Self::IntoIter {
        self.buffers.into_iter().flatten()
    }
}

/// Where a frame goes.
pub enum Frame<'a> {
    /// wgpu: the frame's uploads and draws go into `encoder`. A scene recorded directly in
    /// Vulkan puts `encoder`'s commands so far and its own command buffer into `commands`, in
    /// submission order, and leaves a fresh `encoder` for what follows (composition).
    Wgpu {
        ctx: &'a GpuContext,
        encoder: &'a mut wgpu::CommandEncoder,
        commands: &'a mut FrameCommands,
    },
    /// Another backend: whatever its owner hands over for the frame (Direct3D 11 records on its
    /// immediate context and needs nothing: `&mut ()`).
    Other(&'a mut dyn Any),
}

impl<'a> Frame<'a> {
    /// The device of the frame.
    #[must_use]
    pub fn gpu(&self) -> Gpu<'a> {
        match self {
            Frame::Wgpu { ctx, .. } => Gpu::Wgpu(ctx),
            Frame::Other(_) => Gpu::Other,
        }
    }
}

/// A pass as the front keeps it: its descriptor and its run-time state.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PassState {
    pub desc: PassDesc,
    /// Its uniform block in the dynamic region (bound at `@group(0) @binding(0)` with the
    /// block's offset).
    pub uniform: Option<UniformBlock>,
    /// Slot-0 variants (at least one).
    pub variants: usize,
    /// Selected slot-0 variant ([`crate::graph::RenderScene::set_variant`]).
    pub variant: usize,
    /// Visibility ([`crate::graph::RenderScene::set_enabled`]): disabled passes are not drawn.
    pub enabled: bool,
    /// The target's memory is shared with other transient targets and this pass writes it first
    /// in the frame: its drawing starts from a clear to transparent black.
    pub clear_first: bool,
    /// The pass samples its own target: it reads this scratch copy instead, copied from the
    /// target right before the pass (spec 04 §24.6).
    pub hazard_scratch: Option<RtId>,
    /// `g_Texture0Resolution` of the pass block (updated with the variant).
    pub(crate) res0: Option<UniformSlot>,
}

/// A vertex stream as the front keeps it: layout, capacity and this frame's CPU data.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct StreamState {
    pub layout: StreamLayout,
    /// Capacity in floats.
    pub max_floats: usize,
    /// Indices uploaded at creation.
    pub max_indices: u32,
    /// Indices to draw this frame (0 = nothing).
    pub index_count: u32,
    /// CPU copy of the vertices written since the last frame: the first `pending` floats are to
    /// be uploaded with the frame.
    pub vertices: Vec<f32>,
    pub pending: usize,
}

/// A static mesh as the front keeps it (its data went to the backend at creation).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct MeshInfo {
    pub layout: MeshLayout,
    pub index_count: u32,
}

/// What a texture slot of a pass reads, resolved by the front's rules
/// ([`SceneDesc::pass_slots`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundTexture {
    Texture(TextureId),
    /// A render target (the hazard scratch when the pass samples its own target).
    Target(RtId),
    /// The 1×1 `(0, 0, 0, 1)` texture.
    Default,
    /// The 1×1×1 `(0, 0, 0, 1)` volume (a `texture_3d` slot without a volume texture).
    DefaultVolume,
}

/// One texture slot of a pass (bindings `2 * slot` and `2 * slot + 1` of group 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedSlot {
    pub slot: u32,
    pub texture: BoundTexture,
    pub sampler: SamplerKey,
    /// Declared `texture_3d` (sampled without anisotropy).
    pub volume: bool,
}

/// Everything the front knows about a scene, borrowed (no allocation): the structure a backend
/// compiles at build ("the scene export"), and the frame state it reads every frame.
///
/// Indices are positions in the lists: `passes[p]` is `PassId` `p`, `shaders[s]` is
/// `ShaderId` `s`, and so on. Uniform blocks and vertices are byte ranges of
/// [`Self::dynamic`], which the backend mirrors byte for byte.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct SceneDesc<'a> {
    /// Projection size `(W, H)`.
    pub scene_size: (f32, f32),
    pub quality: QualitySettings,
    /// Render targets: sizes, nominal sizes, format, samples, sampling, the aliasing plan
    /// ([`crate::targets::RenderTarget::shares`]). On wgpu the pool holds the textures too.
    pub targets: &'a RenderTargetPool,
    /// Targets with a depth buffer of their size.
    pub depth_targets: &'a [RtId],
    pub textures: &'a [TextureInfo],
    pub shaders: &'a [ShaderInterface],
    pub passes: &'a [PassState],
    /// Frame operations in order (draws refer to `passes`).
    pub ops: &'a [Op],
    /// Frame globals, pass blocks and vertices, with this frame's dirty range.
    pub dynamic: &'a DynamicRegion,
    /// Static 16-bit indices (at build; empty afterwards: they never change).
    pub indices: &'a [u16],
    /// Static skin records (at build; empty afterwards).
    pub skins: &'a [SkinVertex],
    pub streams: &'a [StreamState],
    pub meshes: &'a [MeshInfo],
    /// Scene textures or targets were written outside the scene since the last frame
    /// ([`crate::graph::RenderScene::mark_dirty`]).
    pub textures_written: bool,
}

impl SceneDesc<'_> {
    /// The texture slots pass `pass` declares, for slot-0 variant `variant`, in slot order: what
    /// each binds and with which sampler. A `texture_3d` slot only takes a volume texture, a 2D
    /// slot only 2D textures and targets; anything else reads the default of the right
    /// dimension. A slot sampling the pass's own target reads its hazard scratch.
    pub fn pass_slots(
        &self,
        pass: usize,
        variant: usize,
    ) -> impl Iterator<Item = ResolvedSlot> + '_ {
        let p = &self.passes[pass];
        let (slots, volumes) = self
            .shaders
            .get(p.desc.shader.index())
            .map_or((0, 0), |s| (s.slots, s.volume_slots));
        let variant = p.desc.slot0_variants.get(variant).copied();
        let hazard = p.hazard_scratch.map(|s| (p.desc.target, s));
        (0..MAX_SLOTS)
            .filter(move |n| slots & (1 << n) != 0)
            .map(move |n| {
                let mut b = p.desc.slots[n];
                if n == 0
                    && let Some(t) = variant
                {
                    b = Some(crate::desc::SlotBinding {
                        texture: t,
                        sampler: b.and_then(|b| b.sampler),
                    });
                }
                let volume = volumes & (1 << n) != 0;
                let (texture, sampler) = self.resolve(b, hazard, volume);
                ResolvedSlot {
                    slot: n as u32,
                    texture,
                    sampler,
                    volume,
                }
            })
    }

    fn resolve(
        &self,
        b: Option<crate::desc::SlotBinding>,
        hazard: Option<(RtId, RtId)>,
        volume: bool,
    ) -> (BoundTexture, SamplerKey) {
        let b = b.unwrap_or(crate::desc::SlotBinding {
            texture: TextureRef::Default,
            sampler: None,
        });
        let texture = |t: TextureId| self.textures.get(t.index());
        let (bound, key) = if volume {
            match b.texture {
                TextureRef::Texture(t) if texture(t).is_some_and(|i| i.volume) => {
                    (BoundTexture::Texture(t), texture(t).map(|i| i.sampler))
                }
                _ => (BoundTexture::DefaultVolume, None),
            }
        } else {
            match b.texture {
                TextureRef::Texture(t) if texture(t).is_some_and(|i| !i.volume) => {
                    (BoundTexture::Texture(t), texture(t).map(|i| i.sampler))
                }
                TextureRef::Target(r) => {
                    let key = self.targets.get(r).sampler_key();
                    let r = match hazard {
                        Some((target, scratch)) if target == r => scratch,
                        _ => r,
                    };
                    (BoundTexture::Target(r), Some(key))
                }
                _ => (BoundTexture::Default, None),
            }
        };
        (
            bound,
            b.sampler.unwrap_or(key.unwrap_or(DEFAULT_TEXTURE_SAMPLER)),
        )
    }
}

/// GPU time of one render pass or copy of a frame ([`crate::graph::RenderScene::profile_frame`]).
#[derive(Clone, Debug)]
pub struct SegmentTime {
    pub target: String,
    pub size: (u32, u32),
    /// Draws in the render pass (0 for a copy).
    pub passes: usize,
    pub first_pass: String,
    pub ms: f64,
}

/// What a backend's device can do, as the scene code asks it (texture formats and sizes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendCaps {
    /// BC1/BC2/BC3 textures upload as they are (otherwise the caller decodes them to RGBA8).
    pub bc: bool,
    /// Largest width or height of a 2D texture or render target.
    pub max_texture_2d: u32,
}

/// Debug counters of a compiled graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphCounts {
    pub passes: usize,
    pub render_passes: usize,
    pub copies: usize,
    pub bundles: usize,
}

/// A GPU backend of the scene front. Object-safe: the front holds a `Box<dyn SceneBackend>`.
///
/// Contract:
/// * The front calls the `add_*` methods in its own order and keeps the ids: the n-th
///   successful `add_texture` / `add_volume_texture` is `TextureId` n, and so on. A failed call
///   adds nothing.
/// * [`Self::build`] is called once, then [`Self::extend`] for passes added after load,
///   [`Self::rebind`] after target reallocations, [`Self::replan`] after visibility or order
///   changes, and [`Self::render`] once per frame. The [`SceneDesc`] of each call is the
///   front's current state.
/// * The dynamic region: at build the backend creates its copy from
///   [`DynamicRegion::bytes`]; at an extension where [`DynamicRegion::grew`], it reallocates it
///   with the whole region. Every frame it uploads [`crate::dynbuf::Shadow::dirty_range`] and
///   the streams' pending vertices; the front clears both after the call.
/// * Errors are values: nothing panics on a GPU failure.
pub trait SceneBackend: Any {
    /// Alignment of the pass uniform blocks in the dynamic region (bound at their offset).
    fn uniform_alignment(&self, gpu: Gpu<'_>) -> u32;

    /// Whether render targets of `format` can be drawn with `samples` samples per pixel.
    fn supports_samples(&self, gpu: Gpu<'_>, format: wgpu::TextureFormat, samples: u32) -> bool;

    /// Texture formats and sizes the device takes.
    fn caps(&self, gpu: Gpu<'_>) -> BackendCaps;

    /// The quality settings, the targets' format or their sample counts changed (before build).
    fn quality_changed(
        &mut self,
        _gpu: Gpu<'_>,
        _quality: &QualitySettings,
        _targets: &RenderTargetPool,
    ) {
    }

    /// Upload a 2D texture with all its levels (the caller drops its CPU copy afterwards).
    fn add_texture(&mut self, gpu: Gpu<'_>, upload: &TextureUpload<'_>) -> Result<TextureInfo>;

    /// Upload a volume texture with all its levels.
    fn add_volume_texture(
        &mut self,
        gpu: Gpu<'_>,
        upload: &VolumeUpload<'_>,
    ) -> Result<TextureInfo>;

    /// Add a pass shader, prepared by the front (reflection, clip fix and validation done).
    fn add_shader(&mut self, gpu: Gpu<'_>, shader: PreparedShader) -> Result<()>;

    /// Upload a static mesh: interleaved `vertices` laid out as `layout`, 16-bit triangle-list
    /// `indices`.
    fn add_mesh(
        &mut self,
        gpu: Gpu<'_>,
        label: &str,
        layout: &MeshLayout,
        vertices: &[u8],
        indices: &[u16],
    ) -> Result<()>;

    /// Allocate a vertex stream of at most `max_vertices` vertices of `layout`, drawn with the
    /// constant 32-bit `indices`.
    fn add_stream(
        &mut self,
        gpu: Gpu<'_>,
        label: &str,
        layout: &StreamLayout,
        max_vertices: u32,
        indices: &[u32],
    ) -> Result<()>;

    /// Compile the scene: everything a frame needs is created here.
    fn build(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>) -> Result<()>;

    /// Passes `first..` were added after load (their shaders, textures and streams came through
    /// the `add_*` methods). On error the backend keeps no pass from `first` on (the front drops
    /// them too).
    fn extend(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>, first: usize) -> Result<()>;

    /// Targets were reallocated (resize, unshare), samplers or hazard scratches changed:
    /// recreate what refers to them, then [`Self::replan`].
    fn rebind(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>) -> Result<()>;

    /// The frame operations or enabled flags changed: plan the frame again.
    fn replan(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>);

    /// Upload this frame's dirty data and record the frame. Returns `true` when the backend had to
    /// plan the frame again on its own (the front counts the frame as a change). Called once per
    /// scene per frame: no allocation expected.
    fn render(&mut self, frame: &mut Frame<'_>, scene: &SceneDesc<'_>) -> bool;

    /// RGBA8 rows of a render target, tightly packed (screenshots, tests).
    fn read_target(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>, target: RtId)
    -> Result<Vec<u8>>;

    /// Debug profile of one frame (tools, never the live loop): uploads the dirty uniform range,
    /// renders the frame alone with timestamps and waits. `None` without timestamp queries.
    fn profile(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>) -> Option<Vec<SegmentTime>>;

    /// Debug counters (render passes, copies, bundles: whatever the backend has).
    fn counts(&self) -> GraphCounts;

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl std::fmt::Debug for Gpu<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Gpu::Wgpu(_) => "Gpu::Wgpu",
            Gpu::Other => "Gpu::Other",
        })
    }
}

//! Scene executor: a [`ScenePlan`] and its uploads → draws on the immediate context (task D3D11
//! 3/n). The bridge in `lumenwall-engine` (task 5/n) maps `lumenwall-render`'s scene description
//! to a plan; this module turns the plan into Direct3D 11 objects at load time and records each
//! frame from the borrowed CPU state ([`FrameInput`]).
//!
//! # Load time ([`SceneBuilder`], [`Scene::extend`])
//!
//! - Uploads first: textures and volumes ([`SceneBuilder::add_texture`]), programs through the
//!   bytecode cache ([`SceneBuilder::add_program`]), static meshes ([`SceneBuilder::add_mesh`]).
//!   Their indices are what the plan refers to.
//! - [`SceneBuilder::build`] validates the plan, then creates:
//!   - one [`RenderTarget`] per target that owns its texture; a target with `shares` draws into its
//!     owner's (aliasing). MSAA levels the device lacks are clamped down (the next supported level,
//!     else 1), as `Msaa::clamp_to` does on wgpu: [`Scene::target_samples`];
//!   - the 1×1 `(0, 0, 0, 1)` defaults (2D always, the volume when a program has a `texture_3d`);
//!   - few buffer objects: the constant arena (the shadow mirrored byte for byte), the image vertex
//!     region (one `DynamicBuffer`), one immutable vertex buffer (the stride-0 defaults of unfed
//!     inputs, every mesh, the skin records), one immutable index buffer (mesh and puppet 16-bit
//!     indices, stream 32-bit indices) and one `DynamicBuffer` per stream;
//!   - per pass: blend, rasteriser and depth states, its arena window, one SRV/sampler table per
//!     slot-0 variant, the input layout of its program and geometry (attributes matched by name,
//!     unfed inputs from the defaults), its hazard copy;
//!   - then it waits for the GPU and trims the device (`Device::trim`).
//! - The rasteriser's depth clip follows the target (on with a depth buffer, off without: spec 04
//!   Q4) and multisampling follows the target's sample count, whatever the plan's key says.
//!
//! # Frames ([`Scene::render`])
//!
//! - The shadow is mirrored into the arena: at most one constant `Map` per frame, none when no
//!   byte changed. The vertex region is rewritten (`WRITE_DISCARD`, whole) when the frame reports a
//!   dirty range; a stream when the frame gives it vertices.
//! - The ops run in order, as `lumenwall-render`'s segments: a clear, a change of target or a
//!   `clear_first` pass starts a segment (the target is bound once, its depth cleared to 1 by the
//!   first segment of the frame on it); a copy or a hazard copy ends it; a multisampled target is
//!   resolved at the end of each of its segments. Sampled views that alias the next target are
//!   unbound first.
//! - Nothing is created and nothing is allocated; every binding is tracked so that a draw sets only
//!   what changed. At the end of the frame the render target and the views are unbound, so the
//!   composition can sample any target.
//!
//! Errors are values: a lost device, an out-of-memory or a plan that does not match the uploads
//! is an [`Error`], never a panic.

mod build;
mod frame;

use std::collections::HashMap;

use lumenwall_formats::tex::GpuFormat;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11BlendState, ID3D11DepthStencilState, ID3D11InputLayout, ID3D11Query,
    ID3D11RasterizerState, ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D,
};

use super::buffer::{ConstantArena, DynamicBuffer, StaticBuffer};
use super::compile::{CacheOutcome, DEFAULT_ATTRIBUTES, ShaderProgram, compile_program};
use super::device::Device;
use super::state::StateCache;
use super::target::RenderTarget;
use super::texture::Texture;
use crate::cache::{BytecodeCache, CacheKey, CompileOptions};
use crate::error::{Error, Result};
use crate::hlsl::{HlslProgram, MAX_SLOTS};
use crate::layout::{ConstantRegion, Dimension, LinearArena, MipLevel, TargetFormat};
use crate::plan::{
    FrameInput, MeshLayoutPlan, OpPlan, PassPlan, ScenePlan, StreamPlan, TargetPlan,
};
use crate::state::SamplerKey;

pub use frame::SegmentTime;

/// Input slot of the geometry (vertex region, stream or mesh).
const GEOMETRY_SLOT: u32 = 0;
/// Input slot of the skin records (skinned geometry).
const SKIN_SLOT: u32 = 1;
/// Input slot of the stride-0 defaults (`DEFAULT_ATTRIBUTES`) that feed unfed inputs.
const DEFAULTS_SLOT: u32 = 2;
/// Offset of `DEFAULT_ATTRIBUTES` in the static vertex buffer.
const DEFAULTS_OFFSET: u32 = 0;
/// Alignment of the data in the static buffers.
const STATIC_ALIGN: usize = 16;

/// Load-time builder: uploads first (each returns its index in the plan), then
/// [`SceneBuilder::build`].
pub struct SceneBuilder<'d> {
    device: &'d Device,
    cache: Option<&'d BytecodeCache>,
    uploads: Uploads,
    /// Static vertex data: the defaults, then every mesh (skin records follow at build).
    static_vertices: LinearArena,
    /// Static index data: every mesh (puppet and stream indices follow at build).
    static_indices: LinearArena,
    meshes: Vec<Mesh>,
}

/// What the builder and the scene upload (textures and programs can be added after `build` too).
#[derive(Default)]
struct Uploads {
    textures: Vec<Texture>,
    programs: Vec<ShaderProgram>,
    created: u64,
    cache_hits: usize,
    cache_compiled: usize,
}

impl Uploads {
    fn add_texture(
        &mut self,
        device: &Device,
        format: GpuFormat,
        dimension: Dimension,
        levels: &[MipLevel<'_>],
    ) -> Result<usize> {
        let t = Texture::new(device, format, dimension, levels)?;
        self.created += 2;
        self.textures.push(t);
        Ok(self.textures.len() - 1)
    }

    fn add_program(
        &mut self,
        device: &Device,
        cache: Option<&BytecodeCache>,
        program: &HlslProgram,
        key: Option<CacheKey>,
    ) -> Result<usize> {
        let (p, outcome) = compile_program(device, program, cache, key, CompileOptions::default())?;
        self.created += 2;
        match outcome {
            CacheOutcome::Hit => self.cache_hits += 1,
            CacheOutcome::Compiled | CacheOutcome::Uncached => self.cache_compiled += 1,
        }
        self.programs.push(p);
        Ok(self.programs.len() - 1)
    }
}

/// A static mesh: its layout and where its data sits in the static buffers.
struct Mesh {
    layout: MeshLayoutPlan,
    vertex_offset: u32,
    index_offset: u32,
    index_count: u32,
}

impl<'d> SceneBuilder<'d> {
    #[must_use]
    pub fn new(device: &'d Device, cache: Option<&'d BytecodeCache>) -> Self {
        let mut static_vertices = LinearArena::new();
        let at = static_vertices.push(&DEFAULT_ATTRIBUTES, STATIC_ALIGN);
        debug_assert_eq!(at, DEFAULTS_OFFSET);
        Self {
            device,
            cache,
            uploads: Uploads::default(),
            static_vertices,
            static_indices: LinearArena::new(),
            meshes: Vec::new(),
        }
    }

    /// An immutable texture (2D or volume, every level given); its index for
    /// `TextureSource::Texture`. An invalid upload is an error, not a driver crash.
    pub fn add_texture(
        &mut self,
        format: GpuFormat,
        dimension: Dimension,
        levels: &[MipLevel<'_>],
    ) -> Result<usize> {
        self.uploads
            .add_texture(self.device, format, dimension, levels)
    }

    /// A program through the bytecode cache (`key` defaults to `CacheKey::for_hlsl`); its index
    /// for `PassPlan::program`.
    pub fn add_program(&mut self, program: &HlslProgram, key: Option<CacheKey>) -> Result<usize> {
        self.uploads
            .add_program(self.device, self.cache, program, key)
    }

    /// A static mesh (interleaved `vertices` laid out as `layout`, 16-bit triangle-list
    /// `indices`), drawn whole by `GeometryPlan::Mesh`; its index. The bytes go to the scene's
    /// one static vertex buffer and one static index buffer, created by `build`.
    pub fn add_mesh(
        &mut self,
        layout: &MeshLayoutPlan,
        vertices: &[u8],
        indices: &[u16],
    ) -> Result<usize> {
        let bad = |m: String| {
            Err(Error::InvalidData(format!(
                "mesh {}: {m}",
                self.meshes.len()
            )))
        };
        if layout.stride == 0
            || layout
                .attributes
                .iter()
                .any(|a| a.offset + a.format.size() > layout.stride)
        {
            return bad(format!("layout {layout:?}"));
        }
        if !vertices.len().is_multiple_of(layout.stride as usize) {
            return bad(format!(
                "{} vertex bytes for a stride of {}",
                vertices.len(),
                layout.stride
            ));
        }
        let count = vertices.len() / layout.stride as usize;
        if let Some(i) = indices.iter().find(|&&i| usize::from(i) >= count) {
            return bad(format!("index {i} beyond {count} vertices"));
        }
        let index_count = u32::try_from(indices.len())
            .map_err(|_| Error::InvalidData("mesh with over 4G indices".into()))?;
        let vertex_offset = self.static_vertices.push(vertices, STATIC_ALIGN);
        let bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let index_offset = self.static_indices.push(&bytes, 4);
        self.meshes.push(Mesh {
            layout: layout.clone(),
            vertex_offset,
            index_offset,
            index_count,
        });
        Ok(self.meshes.len() - 1)
    }

    /// Textures, programs and meshes added so far, as `ScenePlan::validate` counts them.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize) {
        (
            self.uploads.textures.len(),
            self.uploads.programs.len(),
            self.meshes.len(),
        )
    }

    /// Creates every target, buffer, state and input layout of the plan, then waits for the GPU
    /// and trims the device (the frame creates nothing).
    pub fn build(self, plan: &ScenePlan) -> Result<Scene> {
        build::build(self, plan)
    }
}

/// Passes, targets and streams added to a built scene ([`Scene::extend`]: layers created by
/// scripts). Indices follow the existing ones; the op order is set afterwards with
/// [`Scene::set_ops`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScenePlanExtension {
    /// New targets (their `shares`, if any, names an owner among all targets).
    pub targets: Vec<TargetPlan>,
    /// New passes. As on the wgpu path, layers added after load draw image vertices, streams or
    /// meshes: indexed and skinned geometry (puppets) cannot be added.
    pub passes: Vec<PassPlan>,
    /// New streams.
    pub streams: Vec<StreamPlan>,
    /// Bytes of the whole shadow after the extension (at least the current size).
    pub constants_len: u32,
    /// Bytes of the whole vertex region after the extension (at least the current size).
    pub vertex_bytes: u32,
}

/// What the scene holds (load-time objects: the frame creates none).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneStats {
    pub textures: usize,
    pub programs: usize,
    pub meshes: usize,
    pub passes: usize,
    /// Targets of the plan, and the textures they draw into (fewer with aliasing).
    pub targets: usize,
    pub target_textures: usize,
    pub input_layouts: usize,
    /// Buffer objects: constant arena, vertex region, static vertex data, static index data,
    /// streams.
    pub buffers: usize,
    /// `(samplers, blend, rasteriser, depth)` state objects.
    pub states: (usize, usize, usize, usize),
    /// Every Direct3D object this scene created so far: load time, `extend`, a `set_ops` that
    /// unshares aliased targets, the queries of the first profiling frame. Never a frame.
    pub objects_created: u64,
    /// Programs read from the bytecode cache, and compiled.
    pub cache_hits: usize,
    pub cache_compiled: usize,
    /// GPU bytes of the textures (with the defaults), of the targets (colour, MSAA colour,
    /// depth) and of the buffers.
    pub texture_bytes: u64,
    pub target_bytes: u64,
    pub buffer_bytes: u64,
}

/// What the last frame did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// `Map` of the constant arena: 1 when a byte of the shadow changed, else 0.
    pub constant_maps: u32,
    /// `Map` of the vertex region (0 or 1) and of streams.
    pub vertex_maps: u32,
    pub stream_maps: u32,
    pub draws: u32,
    /// Clear ops and `clear_first` clears (depth clears not counted).
    pub clears: u32,
    /// Copy ops and hazard copies.
    pub copies: u32,
    pub resolves: u32,
    /// Segments: runs of draws on one target (the render passes of the wgpu path) and copies.
    pub segments: u32,
}

/// What a slot binds as shader resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Srv {
    Texture(usize),
    /// A plan target (its owner's texture with aliasing, resolved with MSAA).
    Target(usize),
    Default2D,
    Default3D,
}

/// One slot of a pass: `tN` and `sN` (each only when the program declares it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SlotBind {
    srv: Option<Srv>,
    /// Index in [`Scene::samplers`].
    sampler: Option<usize>,
}

/// How a pass draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Draw {
    /// Image vertices of the region.
    Vertices {
        first: u32,
        count: u32,
    },
    /// Puppet indices relative to `first_vertex` (base vertex).
    Indexed {
        first_vertex: u32,
        first_index: u32,
        count: u32,
    },
    /// Indexed, with the region and the skin records bound at the mesh's first vertex and record.
    Skinned {
        vertex_offset: u32,
        skin_offset: u32,
        first_index: u32,
        count: u32,
    },
    Stream(usize),
    Mesh(usize),
}

/// A pass compiled at load time.
struct Pass {
    label: String,
    program: usize,
    target: usize,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    depth: ID3D11DepthStencilState,
    /// Index in [`Scene::layouts`].
    layout: usize,
    uniform: Option<ConstantRegion>,
    /// Slots the program declares (texture or sampler).
    slots: u8,
    /// One table per slot-0 variant (at least one).
    tables: Vec<[SlotBind; MAX_SLOTS as usize]>,
    draw: Draw,
    /// Scratch target the pass's own target is copied into before the draw (the program samples
    /// its target).
    hazard: Option<usize>,
    clear_first: bool,
}

/// A target of the plan.
struct Target {
    plan: TargetPlan,
    /// Index in [`Scene::textures_rt`] of the texture it draws into.
    physical: usize,
    /// Samples after clamping to what the device supports.
    samples: u32,
}

/// A vertex stream.
struct Stream {
    plan: StreamPlan,
    buffer: DynamicBuffer,
    /// Index in [`Scene::index_buffers`] and byte offset of its 32-bit indices.
    index_buffer: usize,
    index_offset: u32,
    max_indices: u32,
    /// Indices drawn (set by each frame).
    index_count: u32,
}

/// Layout of an input layout's geometry, for deduplication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LayoutKind {
    Image,
    Skinned,
    /// The first stream with this layout.
    Stream(usize),
    /// The first mesh with this layout.
    Mesh(usize),
}

/// A built scene.
pub struct Scene {
    textures: Vec<Texture>,
    programs: Vec<ShaderProgram>,
    meshes: Vec<Mesh>,
    targets: Vec<Target>,
    textures_rt: Vec<RenderTarget>,
    default_2d: Texture,
    default_3d: Option<Texture>,
    arena: ConstantArena,
    constants_len: u32,
    frame_block: Option<ConstantRegion>,
    vertex_bytes: u32,
    vertices: Option<DynamicBuffer>,
    /// The vertex region was (re)created: the next frame writes it whatever its dirty range.
    vertices_stale: bool,
    /// Defaults, meshes and skin records.
    static_vertices: StaticBuffer,
    /// Byte offset of the skin records in `static_vertices`.
    skin_offset: u32,
    /// The build's index data (meshes, puppets, streams), then one buffer per extension with
    /// streams.
    index_buffers: Vec<StaticBuffer>,
    /// Index in `index_buffers` of the build's (none when the plan has no index at all).
    main_index: Option<usize>,
    /// Byte offset of the puppet indices in the build's index buffer.
    puppet_index_offset: u32,
    streams: Vec<Stream>,
    states: StateCache,
    samplers: Vec<ID3D11SamplerState>,
    sampler_index: HashMap<SamplerKey, usize>,
    layouts: Vec<Option<ID3D11InputLayout>>,
    layout_index: HashMap<(usize, LayoutKind), usize>,
    passes: Vec<Pass>,
    ops: Vec<OpPlan>,
    /// Depth buffers cleared in the current frame, per target (no allocation per frame).
    depth_cleared: Vec<bool>,
    /// Timestamp queries of [`Scene::profile_frame`] (created at its first call, reused).
    profile_queries: Vec<ID3D11Query>,
    created: u64,
    cache_hits: usize,
    cache_compiled: usize,
    last: FrameStats,
}

impl Scene {
    /// Records one frame from the scene's CPU state. `frame.shadow` has `constants_len` bytes,
    /// `frame.vertices` `vertex_bytes`, and `variants`, `enabled` and `streams` one entry per
    /// pass or stream at least.
    pub fn render(&mut self, device: &Device, frame: &FrameInput<'_>) -> Result<()> {
        frame::render(self, device, frame)
    }

    /// Debug profile of one frame (tools, never the live loop): records it like
    /// [`Scene::render`] with a timestamp pair around each segment, waits for the GPU and returns
    /// the segments in frame order. `None` when the GPU clock was disjoint or the results did not
    /// come within 5 s. The queries are created at the first call and reused.
    pub fn profile_frame(
        &mut self,
        device: &Device,
        frame: &FrameInput<'_>,
    ) -> Result<Option<Vec<SegmentTime>>> {
        frame::profile(self, device, frame)
    }

    /// What the last frame did.
    #[must_use]
    pub fn last_frame(&self) -> FrameStats {
        self.last
    }

    /// A texture added after `build` (layers created by scripts); its index.
    pub fn add_texture(
        &mut self,
        device: &Device,
        format: GpuFormat,
        dimension: Dimension,
        levels: &[MipLevel<'_>],
    ) -> Result<usize> {
        let t = Texture::new(device, format, dimension, levels)?;
        self.created += 2;
        self.textures.push(t);
        Ok(self.textures.len() - 1)
    }

    /// A program added after `build`; its index.
    pub fn add_program(
        &mut self,
        device: &Device,
        cache: Option<&BytecodeCache>,
        program: &HlslProgram,
        key: Option<CacheKey>,
    ) -> Result<usize> {
        let mut u = Uploads::default();
        u.add_program(device, cache, program, key)?;
        self.created += u.created;
        self.cache_hits += u.cache_hits;
        self.cache_compiled += u.cache_compiled;
        self.programs.extend(u.programs);
        if self
            .programs
            .last()
            .is_some_and(|p| p.resources().volumes != 0)
        {
            self.ensure_default_3d(device)?;
        }
        Ok(self.programs.len() - 1)
    }

    /// Targets, passes and streams added after load (scripts' layers); their indices follow the
    /// existing ones. Nothing is drawn until [`Scene::set_ops`] places the new passes. On error the
    /// scene is unchanged, except for objects already created.
    pub fn extend(&mut self, device: &Device, extension: ScenePlanExtension) -> Result<()> {
        build::extend(self, device, extension)
    }

    /// A new frame order (visibility changes of layers, reordering). Passes left out stay
    /// compiled for a later reuse. Target lifetimes were planned on the old order: a scene with
    /// aliased targets gives each its own texture again (and no pass clears first any more), as
    /// `lumenwall-render` does.
    pub fn set_ops(&mut self, device: &Device, ops: Vec<OpPlan>) -> Result<()> {
        build::set_ops(self, device, ops)
    }

    /// The frame order.
    #[must_use]
    pub fn ops(&self) -> &[OpPlan] {
        &self.ops
    }

    /// The sampled (resolved) texture of a target: the composition reads `_rt_FullFrameBuffer`.
    #[must_use]
    pub fn target_srv(&self, target: usize) -> Option<&ID3D11ShaderResourceView> {
        self.render_target(target).map(RenderTarget::srv)
    }

    /// The texture of a target (single-sample, resolved), for readbacks and uploads into it
    /// (cover art).
    #[must_use]
    pub fn target_texture(&self, target: usize) -> Option<&ID3D11Texture2D> {
        self.render_target(target).map(RenderTarget::texture)
    }

    /// The render target a plan target draws into (its owner's with aliasing).
    #[must_use]
    pub fn render_target(&self, target: usize) -> Option<&RenderTarget> {
        let t = self.targets.get(target)?;
        self.textures_rt.get(t.physical)
    }

    /// Samples per pixel of a target after clamping to the device (1 = no MSAA).
    #[must_use]
    pub fn target_samples(&self, target: usize) -> Option<u32> {
        self.targets.get(target).map(|t| t.samples)
    }

    /// Colour format of a target.
    #[must_use]
    pub fn target_format(&self, target: usize) -> Option<TargetFormat> {
        self.targets.get(target).map(|t| t.plan.format)
    }

    /// Whether two targets draw into the same texture (aliasing).
    #[must_use]
    pub fn shares_texture(&self, a: usize, b: usize) -> bool {
        match (self.targets.get(a), self.targets.get(b)) {
            (Some(a), Some(b)) => a.physical == b.physical,
            _ => false,
        }
    }

    /// What the scene holds.
    #[must_use]
    pub fn stats(&self) -> SceneStats {
        let buffers = 2
            + usize::from(self.vertices.is_some())
            + self.index_buffers.len()
            + self.streams.len();
        let buffer_bytes = self.arena.len() as u64
            + self
                .vertices
                .as_ref()
                .map_or(0, |b| u64::from(b.capacity()))
            + u64::from(self.static_vertices.len())
            + self
                .index_buffers
                .iter()
                .map(|b| u64::from(b.len()))
                .sum::<u64>()
            + self
                .streams
                .iter()
                .map(|s| u64::from(s.buffer.capacity()))
                .sum::<u64>();
        SceneStats {
            textures: self.textures.len(),
            programs: self.programs.len(),
            meshes: self.meshes.len(),
            passes: self.passes.len(),
            targets: self.targets.len(),
            target_textures: self.textures_rt.len(),
            input_layouts: self.layouts.iter().flatten().count(),
            buffers,
            states: self.states.counts(),
            objects_created: self.created,
            cache_hits: self.cache_hits,
            cache_compiled: self.cache_compiled,
            texture_bytes: self
                .textures
                .iter()
                .chain(std::iter::once(&self.default_2d))
                .chain(self.default_3d.as_ref())
                .map(Texture::bytes)
                .sum(),
            target_bytes: self.textures_rt.iter().map(RenderTarget::bytes).sum(),
            buffer_bytes,
        }
    }

    /// The 1×1×1 volume that a `texture_3d` slot given no volume reads.
    fn ensure_default_3d(&mut self, device: &Device) -> Result<()> {
        if self.default_3d.is_none() {
            self.default_3d = Some(default_texture(device, Dimension::D3)?);
            self.created += 2;
        }
        Ok(())
    }
}

/// A 1×1 (×1) `(0, 0, 0, 1)` RGBA8 texture.
fn default_texture(device: &Device, dimension: Dimension) -> Result<Texture> {
    Texture::new(
        device,
        GpuFormat::Rgba8Unorm,
        dimension,
        &[MipLevel {
            width: 1,
            height: 1,
            depth: 1,
            data: &[0, 0, 0, 255],
        }],
    )
}

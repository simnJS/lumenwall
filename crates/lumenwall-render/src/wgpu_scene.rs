//! The wgpu backend of the scene front ([`crate::backend::SceneBackend`]): pipelines
//! deduplicated by key, bind groups per slot-0 variant, hazard copies, consecutive draws on one
//! target merged into one render pass, frozen passes pre-recorded in render bundles, per-frame
//! uploads through a recycled staging belt.
//!
//! On a Vulkan device the frame is recorded directly in Vulkan ([`crate::hal`]): the same
//! segments, pipelines and bind groups created through wgpu-hal, replayed without wgpu-core's
//! per-pass validation and tracking. Direct3D 12, `LUMENWALL_DIRECT_VULKAN=0`,
//! [`WgpuScene::set_recording`] and anything wgpu-hal refuses keep the wgpu path.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::backend::{
    BackendCaps, BoundTexture, Frame, Gpu, GraphCounts, PassState, SceneBackend, SceneDesc,
    SegmentTime, StreamState,
};
use crate::blend::{BlendMode, CullMode, WriteMask};
use crate::context::{GpuContext, MemoryToken};
use crate::desc::{
    DEPTH_FORMAT, DepthState, Geometry, MeshLayout, PassDesc, SKIN_ATTRIBUTES, ShaderId,
    StreamLayout, TextureId,
};
use crate::dynbuf::{DynamicRegion, ScalarKind, frame_globals};
use crate::error::{RenderError, Result};
use crate::hal::{
    DirectGraph, DrawCall, DrawStep, FrameState, PipelineSpec, Recording, SlotView, Step,
    VertexBuffer, WgpuReason,
};
use crate::math::{SKIN_VERTEX_SIZE, VERTEX_SIZE};
use crate::quality::{Msaa, QualitySettings};
use crate::shader::{PassShader, PreparedShader, VertexAttr};
use crate::targets::{RT_FORMAT_HDR, RenderTargetPool, RtId};
use crate::texture::{
    GpuTexture, SamplerKey, TextureInfo, TextureUpload, VolumeUpload, default_texture,
    default_volume, upload_texture, upload_volume,
};

fn wgpu_ctx(gpu: Gpu<'_>) -> Result<&GpuContext> {
    gpu.wgpu().ok_or_else(|| {
        RenderError::Unsupported("the wgpu scene backend needs the wgpu context".into())
    })
}

/// The dynamic region's GPU copy: one buffer for the frame globals, every pass block (dynamic
/// offsets) and the vertices.
struct GpuDynamic {
    buffer: wgpu::Buffer,
    _memory: MemoryToken,
}

impl GpuDynamic {
    /// A buffer holding the whole region.
    fn new(ctx: &GpuContext, region: &DynamicRegion) -> Self {
        let total = region.byte_len();
        let buffer = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumenwall.dynamic"),
            size: total,
            usage: wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue().write_buffer(&buffer, 0, region.bytes());
        ctx.note_resources(1);
        Self {
            buffer,
            _memory: ctx.memory().track(total),
        }
    }
}

/// The dirty range of the region, 4-byte aligned (buffer copies).
fn dirty_bytes(region: &DynamicRegion) -> Option<(u32, u32)> {
    let (start, end) = region.shadow().dirty_range()?;
    let start = start & !3;
    let end = end.next_multiple_of(4).min(region.byte_len() as u32);
    Some((start, end))
}

/// Vertex and 16-bit index buffers of a static mesh, uploaded once.
struct StaticMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    _memory: MemoryToken,
}

/// Buffers of a vertex stream, allocated once at their maximum size.
struct GpuStream {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    _memory: MemoryToken,
}

/// Depth attachment of a render target (spec 09 §3.1).
struct DepthBuffer {
    target: RtId,
    size: (u32, u32),
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    _memory: MemoryToken,
}

fn create_depth(ctx: &GpuContext, target: RtId, size: (u32, u32), samples: u32) -> DepthBuffer {
    let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("lumenwall.depth"),
        size: wgpu::Extent3d {
            width: size.0.max(1),
            height: size.1.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    ctx.note_resources(2);
    DepthBuffer {
        target,
        size,
        view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        texture,
        _memory: ctx
            .memory()
            .track(u64::from(size.0.max(1)) * u64::from(size.1.max(1)) * 4),
    }
}

/// Format of a skinned input fed from the skin buffer, `None` = not a skin attribute (or
/// a float-typed index input, which the buffer cannot feed).
fn skin_attribute(name: &str, attr: VertexAttr) -> Option<(wgpu::VertexFormat, u64)> {
    let (_, offset) = SKIN_ATTRIBUTES.iter().find(|(n, _)| *n == name)?;
    let VertexAttr::Unfed { components, kind } = attr else {
        return None;
    };
    let integer = kind != ScalarKind::Float;
    if integer != (*offset == 0) {
        return None;
    }
    Some((unfed_format(kind, components), *offset))
}

/// Format of a default-fed vertex input.
fn unfed_format(kind: ScalarKind, components: u8) -> wgpu::VertexFormat {
    match (kind, components) {
        (ScalarKind::Float, 1) => wgpu::VertexFormat::Float32,
        (ScalarKind::Float, 2) => wgpu::VertexFormat::Float32x2,
        (ScalarKind::Float, 3) => wgpu::VertexFormat::Float32x3,
        (ScalarKind::Float, _) => wgpu::VertexFormat::Float32x4,
        (ScalarKind::Sint, 1) => wgpu::VertexFormat::Sint32,
        (ScalarKind::Sint, 2) => wgpu::VertexFormat::Sint32x2,
        (ScalarKind::Sint, 3) => wgpu::VertexFormat::Sint32x3,
        (ScalarKind::Sint, _) => wgpu::VertexFormat::Sint32x4,
        (ScalarKind::Uint, 1) => wgpu::VertexFormat::Uint32,
        (ScalarKind::Uint, 2) => wgpu::VertexFormat::Uint32x2,
        (ScalarKind::Uint, 3) => wgpu::VertexFormat::Uint32x3,
        (ScalarKind::Uint, _) => wgpu::VertexFormat::Uint32x4,
    }
}

fn uniform_entry(binding: u32, dynamic: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Uniform bind group: pass blocks (dynamic offset) and the frame globals block.
fn uniform_bind_group(
    ctx: &GpuContext,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    binding_size: u32,
) -> wgpu::BindGroup {
    let bg = ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lumenwall.uniforms.bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(u64::from(binding_size)),
                }),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(u64::from(frame_globals::SIZE)),
                }),
            },
        ],
    });
    ctx.note_resources(1);
    bg
}

/// Everything a pipeline is created from (both paths).
struct PipelinePlan {
    key: PipelineKey,
    buffers: Vec<VertexBuffer>,
    /// Vertex buffer slots (≥ 1) fed with defaults: `(slot, integer?)`.
    unfed: Vec<(u32, bool)>,
    primitive: wgpu::PrimitiveState,
    depth_stencil: Option<wgpu::DepthStencilState>,
    target: wgpu::ColorTargetState,
    /// `(slots, volume slots)` of the shader.
    layout: (u8, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PipelineKey {
    shader: ShaderId,
    blend: BlendMode,
    write_mask: WriteMask,
    cull: CullMode,
    /// First stream with the same vertex layout, for stream passes.
    stream_layout: Option<usize>,
    /// Skin buffer bound in slot 1 (attribute layout differs).
    skinned: bool,
    /// First mesh with the same vertex layout, for mesh passes.
    mesh_layout: Option<usize>,
    front_face: wgpu::FrontFace,
    /// Depth state when the target has a depth buffer.
    depth: Option<DepthState>,
    /// Samples per pixel of the pass's target.
    samples: u32,
}

/// The wgpu side of a pass (its descriptor and run-time state are the front's
/// [`PassState`]).
struct WgpuPass {
    /// wgpu path (`None` while the scene records directly in Vulkan).
    pipeline: Option<wgpu::RenderPipeline>,
    /// Direct path: index of the wgpu-hal pipeline.
    direct_pipeline: Option<usize>,
    /// wgpu path: one per variant (empty while the scene records directly in Vulkan).
    bind_groups: Vec<wgpu::BindGroup>,
    /// Vertex buffer slots (≥ 1) fed with defaults: `(slot, integer?)`.
    unfed: Vec<(u32, bool)>,
    /// The wgpu path refused its pipeline: the pass is left out of the frame.
    failed: bool,
}

enum Item {
    Bundle(usize),
    Pass(usize),
}

/// What one texture slot of a pass binds (bindings `2 * slot` and `2 * slot + 1`).
struct BoundSlot<'a> {
    slot: u32,
    view: &'a wgpu::TextureView,
    sampler: SamplerKey,
    /// Sampled as a volume (no anisotropy).
    volume: bool,
    /// The render target behind the view.
    target: Option<RtId>,
}

/// Per-frame state the direct path reads while recording.
struct FrameView<'a> {
    passes: &'a [PassState],
    streams: &'a [StreamState],
}

impl FrameState for FrameView<'_> {
    fn variant(&self, pass: u32) -> usize {
        self.passes.get(pass as usize).map_or(0, |p| p.variant)
    }

    fn stream_indices(&self, stream: u32) -> u32 {
        self.streams
            .get(stream as usize)
            .map_or(0, |s| s.index_count)
    }
}

/// CPU time of a frame recorded directly in Vulkan, beyond the uploads (debug counters).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectTimes {
    /// Recording the frame into its Vulkan command buffer (encoder creation and `finish`
    /// included).
    pub record: Duration,
    /// The submission [`crate::graph::RenderScene::encode`] makes (zero with
    /// [`crate::graph::RenderScene::encode_frame`]).
    pub submit: Duration,
}

enum Segment {
    Copy {
        src: RtId,
        dst: RtId,
    },
    Render {
        target: RtId,
        clear: Option<wgpu::Color>,
        /// Depth attachment `(index in WgpuScene::depths, clear to 1?)`.
        depth: Option<(usize, bool)>,
        items: Vec<Item>,
    },
}

/// Something both a render pass and a bundle encoder can record into.
trait Recorder<'a> {
    fn pipeline(&mut self, p: &'a wgpu::RenderPipeline);
    fn bind_group(&mut self, index: u32, bg: &'a wgpu::BindGroup, offsets: &[u32]);
    fn vertex_buffer(&mut self, slot: u32, s: wgpu::BufferSlice<'a>);
    fn index_buffer(&mut self, s: wgpu::BufferSlice<'a>, format: wgpu::IndexFormat);
    fn draw(&mut self, v: std::ops::Range<u32>);
    fn draw_indexed(&mut self, i: std::ops::Range<u32>, base: i32);
}

impl<'a> Recorder<'a> for wgpu::RenderPass<'_> {
    fn pipeline(&mut self, p: &'a wgpu::RenderPipeline) {
        self.set_pipeline(p);
    }
    fn bind_group(&mut self, index: u32, bg: &'a wgpu::BindGroup, offsets: &[u32]) {
        self.set_bind_group(index, bg, offsets);
    }
    fn vertex_buffer(&mut self, slot: u32, s: wgpu::BufferSlice<'a>) {
        self.set_vertex_buffer(slot, s);
    }
    fn index_buffer(&mut self, s: wgpu::BufferSlice<'a>, format: wgpu::IndexFormat) {
        self.set_index_buffer(s, format);
    }
    fn draw(&mut self, v: std::ops::Range<u32>) {
        wgpu::RenderPass::draw(self, v, 0..1);
    }
    fn draw_indexed(&mut self, i: std::ops::Range<u32>, base: i32) {
        wgpu::RenderPass::draw_indexed(self, i, base, 0..1);
    }
}

impl<'a> Recorder<'a> for wgpu::RenderBundleEncoder<'a> {
    fn pipeline(&mut self, p: &'a wgpu::RenderPipeline) {
        self.set_pipeline(p);
    }
    fn bind_group(&mut self, index: u32, bg: &'a wgpu::BindGroup, offsets: &[u32]) {
        self.set_bind_group(index, bg, offsets);
    }
    fn vertex_buffer(&mut self, slot: u32, s: wgpu::BufferSlice<'a>) {
        self.set_vertex_buffer(slot, s);
    }
    fn index_buffer(&mut self, s: wgpu::BufferSlice<'a>, format: wgpu::IndexFormat) {
        self.set_index_buffer(s, format);
    }
    fn draw(&mut self, v: std::ops::Range<u32>) {
        wgpu::RenderBundleEncoder::draw(self, v, 0..1);
    }
    fn draw_indexed(&mut self, i: std::ops::Range<u32>, base: i32) {
        wgpu::RenderBundleEncoder::draw_indexed(self, i, base, 0..1);
    }
}

/// A scene on wgpu: what [`crate::graph::SceneBuilder::new`] builds on.
pub struct WgpuScene {
    /// Direct Vulkan recording; `None` on the wgpu path. First field: dropped (after a GPU
    /// wait) before the resources its frames used.
    direct: Option<DirectGraph>,
    /// Whether the scene may (still) record directly in Vulkan.
    allowed: std::result::Result<(), WgpuReason>,
    /// `(shader id, final module, validation info)` of the shaders added since the direct
    /// graph last took them, for wgpu-hal.
    direct_modules: Vec<(usize, naga::Module, naga::valid::ModuleInfo)>,
    /// wgpu-hal pipelines by key (direct path).
    direct_pipelines: HashMap<PipelineKey, usize>,
    /// Why the scene records through wgpu (`None` on the direct path).
    wgpu_reason: Option<WgpuReason>,
    adapter_backend: wgpu::Backend,
    textures: Vec<GpuTexture>,
    default_texture: GpuTexture,
    /// Bound to `texture_3d` slots without a volume texture (only when a shader has one).
    default_volume: Option<GpuTexture>,
    /// Compiled shaders and pipelines, reused by passes added after load.
    shaders: Vec<PassShader>,
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,
    meshes: Vec<StaticMesh>,
    streams: Vec<GpuStream>,
    depths: Vec<DepthBuffer>,
    /// Created by [`SceneBackend::build`].
    dynbuf: Option<GpuDynamic>,
    index_buffer: Option<wgpu::Buffer>,
    skin_buffer: Option<wgpu::Buffer>,
    default_attrs: wgpu::Buffer,
    uniform_layout: wgpu::BindGroupLayout,
    uniform_bg: Option<wgpu::BindGroup>,
    /// By `(slots, volume slots)` of the shader interface.
    texture_layouts: HashMap<(u8, u8), (wgpu::BindGroupLayout, wgpu::PipelineLayout)>,
    passes: Vec<WgpuPass>,
    segments: Vec<Segment>,
    bundles: Vec<wgpu::RenderBundle>,
    /// Passes recorded in each of `bundles`: a rebuild reuses the bundle of an unchanged
    /// run. Emptied when bind groups or the dynamic buffer change.
    bundle_runs: Vec<Vec<usize>>,
    /// Recycled staging memory of the per-frame uploads (uniforms, vertex streams).
    uploads: Option<wgpu::util::StagingBelt>,
    /// CPU split of the last frame recorded directly in Vulkan.
    last_direct: DirectTimes,
    /// The scene went back to wgpu since the last frame (the front counts it as a change).
    fell_back: bool,
}

impl WgpuScene {
    /// A scene backend on `ctx` (load time).
    #[must_use]
    pub fn new(ctx: &GpuContext) -> Self {
        let device = ctx.device();
        // (0, 0, 0, 1) as floats, then as integers, for unfed attributes.
        let mut defaults = [0u8; 32];
        defaults[12..16].copy_from_slice(&1.0f32.to_le_bytes());
        defaults[28..32].copy_from_slice(&1i32.to_le_bytes());
        let default_attrs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumenwall.default-attributes"),
            size: 32,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue().write_buffer(&default_attrs, 0, &defaults);
        ctx.note_resources(1);
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumenwall.uniforms.bgl"),
            entries: &[uniform_entry(0, true), uniform_entry(1, false)],
        });
        ctx.note_resources(1);
        let adapter_backend = ctx.adapter().get_info().backend;
        Self {
            direct: None,
            allowed: crate::hal::choose(Recording::from_env(), adapter_backend),
            direct_modules: Vec::new(),
            direct_pipelines: HashMap::new(),
            wgpu_reason: None,
            adapter_backend,
            textures: Vec::new(),
            default_texture: default_texture(ctx),
            default_volume: None,
            shaders: Vec::new(),
            pipelines: HashMap::new(),
            meshes: Vec::new(),
            streams: Vec::new(),
            depths: Vec::new(),
            dynbuf: None,
            index_buffer: None,
            skin_buffer: None,
            default_attrs,
            uniform_layout,
            uniform_bg: None,
            texture_layouts: HashMap::new(),
            passes: Vec::new(),
            segments: Vec::new(),
            bundles: Vec::new(),
            bundle_runs: Vec::new(),
            uploads: None,
            last_direct: DirectTimes::default(),
            fell_back: false,
        }
    }

    /// How the scene records its frames ([`Recording::Auto`] by default, unless
    /// `LUMENWALL_DIRECT_VULKAN=0`). Before shaders are added: [`Recording::Auto`] set later
    /// cannot make shaders already added usable by the direct path.
    pub fn set_recording(&mut self, recording: Recording) {
        self.allowed = crate::hal::choose(recording, self.adapter_backend).and(self.allowed);
        if self.allowed.is_err() {
            self.direct_modules.clear();
        }
    }

    #[must_use]
    pub fn texture(&self, id: TextureId) -> Option<&GpuTexture> {
        self.textures.get(id.index())
    }

    /// Whether the scene records its frames directly in Vulkan ([`crate::hal`]).
    #[must_use]
    pub fn records_direct(&self) -> bool {
        self.direct.is_some()
    }

    /// Why the scene records through wgpu (`None` when it records directly in Vulkan).
    #[must_use]
    pub fn wgpu_reason(&self) -> Option<WgpuReason> {
        self.wgpu_reason
    }

    /// Counters of the direct path (`None` on the wgpu path).
    #[must_use]
    pub fn direct_stats(&self) -> Option<crate::hal::DirectStats> {
        self.direct.as_ref().map(DirectGraph::stats)
    }

    /// CPU split of the last frame recorded directly in Vulkan (zero on the wgpu path).
    #[must_use]
    pub fn last_direct_times(&self) -> DirectTimes {
        self.last_direct
    }

    pub(crate) fn set_submit_time(&mut self, submit: Duration) {
        self.last_direct.submit = submit;
    }

    fn texture_layout(
        &mut self,
        ctx: &GpuContext,
        key: (u8, u8),
    ) -> &(wgpu::BindGroupLayout, wgpu::PipelineLayout) {
        let (slots, volumes) = key;
        let uniform_layout = &self.uniform_layout;
        self.texture_layouts.entry(key).or_insert_with(|| {
            let entries = crate::hal::texture_layout_entries(slots, volumes);
            let bgl = ctx
                .device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("lumenwall.textures.bgl"),
                    entries: &entries,
                });
            let pl = ctx
                .device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("lumenwall.pass.layout"),
                    bind_group_layouts: &[Some(uniform_layout), Some(&bgl)],
                    immediate_size: 0,
                });
            ctx.note_resources(2);
            (bgl, pl)
        })
    }

    /// Vertex layouts, states and cache key of the pipeline of a pass (both paths).
    fn pipeline_plan(
        &self,
        ctx: &GpuContext,
        shader: &PassShader,
        desc: &PassDesc,
        scene: &SceneDesc<'_>,
    ) -> Result<PipelinePlan> {
        let iface = shader.interface();
        let err = |reason: String| RenderError::Pass {
            label: desc.label.clone(),
            reason,
        };
        if let Geometry::Indexed { .. } | Geometry::Skinned { .. } = desc.geometry
            && self.index_buffer.is_none()
        {
            return Err(err("indexed geometry without indices".into()));
        }
        let skinned = matches!(desc.geometry, Geometry::Skinned { .. });
        if skinned && self.skin_buffer.is_none() {
            return Err(err("skinned geometry without skin records".into()));
        }

        // Vertex layouts: slot 0 = dynamic vertices (or the stream), then the skin buffer for
        // skinned geometry, then one slot per default-fed input.
        let stream = match desc.geometry {
            Geometry::Stream(s) => Some(
                scene
                    .streams
                    .get(s.index())
                    .ok_or_else(|| err("unknown vertex stream".into()))?,
            ),
            _ => None,
        };
        let mesh = match desc.geometry {
            Geometry::Mesh(m) => Some(
                scene
                    .meshes
                    .get(m.index())
                    .ok_or_else(|| err("unknown mesh".into()))?,
            ),
            _ => None,
        };
        let mut main_attrs = Vec::new();
        let mut skin_attrs = Vec::new();
        let mut unfed_attrs = Vec::new();
        let unfed_attr = |location, attr: VertexAttr| {
            let (components, kind) = match attr {
                VertexAttr::Position => (3, ScalarKind::Float),
                VertexAttr::TexCoord => (2, ScalarKind::Float),
                VertexAttr::Unfed { components, kind } => (components, kind),
            };
            (
                wgpu::VertexAttribute {
                    format: unfed_format(kind, components),
                    offset: 0,
                    shader_location: location,
                },
                kind != ScalarKind::Float,
            )
        };
        for input in &iface.vertex_inputs {
            if let Some(mesh) = mesh {
                let name = iface.vertex_input_name(input.location).unwrap_or("");
                if let Some(a) = mesh.layout.attributes.iter().find(|a| a.name == name) {
                    main_attrs.push(wgpu::VertexAttribute {
                        format: a.format,
                        offset: u64::from(a.offset),
                        shader_location: input.location,
                    });
                } else {
                    unfed_attrs.push(unfed_attr(input.location, input.attr));
                }
                continue;
            }
            if skinned
                && let Some((format, offset)) = iface
                    .vertex_input_name(input.location)
                    .and_then(|name| skin_attribute(name, input.attr))
            {
                skin_attrs.push(wgpu::VertexAttribute {
                    format,
                    offset,
                    shader_location: input.location,
                });
                continue;
            }
            if let Some(stream) = stream {
                let name = iface.vertex_input_name(input.location).unwrap_or("");
                if let Some(a) = stream.layout.attributes.iter().find(|a| a.name == name) {
                    let format = match a.components {
                        1 => wgpu::VertexFormat::Float32,
                        2 => wgpu::VertexFormat::Float32x2,
                        3 => wgpu::VertexFormat::Float32x3,
                        _ => wgpu::VertexFormat::Float32x4,
                    };
                    main_attrs.push(wgpu::VertexAttribute {
                        format,
                        offset: u64::from(a.offset_floats) * 4,
                        shader_location: input.location,
                    });
                } else {
                    unfed_attrs.push(unfed_attr(input.location, input.attr));
                }
                continue;
            }
            match input.attr {
                VertexAttr::Position => main_attrs.push(wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 0,
                    shader_location: input.location,
                }),
                VertexAttr::TexCoord => main_attrs.push(wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 12,
                    shader_location: input.location,
                }),
                VertexAttr::Unfed { .. } => {
                    unfed_attrs.push(unfed_attr(input.location, input.attr));
                }
            }
        }
        let max_buffers = ctx.device().limits().max_vertex_buffers as usize;
        let first_unfed = 1 + u32::from(skinned);
        if first_unfed as usize + unfed_attrs.len() > max_buffers {
            return Err(err(format!(
                "{} unfed vertex inputs exceed the device limit",
                unfed_attrs.len()
            )));
        }
        let stride = match (stream, mesh) {
            (Some(s), _) => u64::from(s.layout.stride_floats) * 4,
            (_, Some(m)) => u64::from(m.layout.stride),
            _ => u64::from(VERTEX_SIZE),
        };
        // Pipelines of streams (meshes) with the same layout are shared.
        let layout_key =
            stream.and_then(|s| scene.streams.iter().position(|o| o.layout == s.layout));
        let mesh_layout = mesh.and_then(|m| scene.meshes.iter().position(|o| o.layout == m.layout));
        let depth = scene
            .depth_targets
            .contains(&desc.target)
            .then_some(desc.depth);
        let mut buffers = vec![VertexBuffer {
            stride,
            step: wgpu::VertexStepMode::Vertex,
            attributes: main_attrs,
        }];
        if skinned {
            buffers.push(VertexBuffer {
                stride: u64::from(SKIN_VERTEX_SIZE),
                step: wgpu::VertexStepMode::Vertex,
                attributes: skin_attrs,
            });
        }
        let unfed = unfed_attrs
            .iter()
            .enumerate()
            .map(|(i, (_, int))| (i as u32 + first_unfed, *int))
            .collect();
        for (a, _) in unfed_attrs {
            // Instance step with a single instance: every vertex reads element 0.
            buffers.push(VertexBuffer {
                stride: 16,
                step: wgpu::VertexStepMode::Instance,
                attributes: vec![a],
            });
        }
        // Passes into a target with a depth buffer clip against the near and far planes like
        // Wallpaper Engine's 3D passes; the others never discard geometry for its depth.
        let unclipped = ctx.caps().unclipped_depth && depth.is_none();
        Ok(PipelinePlan {
            key: PipelineKey {
                shader: desc.shader,
                blend: desc.blend,
                write_mask: desc.write_mask,
                cull: desc.cull,
                stream_layout: layout_key,
                skinned,
                mesh_layout,
                front_face: desc.front_face,
                depth,
                samples: scene.targets.get(desc.target).samples(),
            },
            buffers,
            unfed,
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: desc.front_face,
                cull_mode: desc.cull.face(),
                // Without a depth buffer, never discard geometry for its depth (spec 04 Q4).
                unclipped_depth: unclipped,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: depth.map(|d| wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(d.write),
                depth_compare: Some(if d.test {
                    wgpu::CompareFunction::LessEqual
                } else {
                    wgpu::CompareFunction::Always
                }),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            target: wgpu::ColorTargetState {
                format: scene.targets.format(),
                blend: desc.blend.state(),
                write_mask: desc.write_mask.writes(),
            },
            layout: (iface.slots, iface.volume_slots),
        })
    }

    /// The wgpu pipeline of a plan, from the cache or created.
    fn wgpu_pipeline(
        &mut self,
        ctx: &GpuContext,
        shader: &PassShader,
        plan: &PipelinePlan,
        label: &str,
        cache: &mut HashMap<PipelineKey, wgpu::RenderPipeline>,
    ) -> Result<wgpu::RenderPipeline> {
        if let Some(p) = cache.get(&plan.key) {
            return Ok(p.clone());
        }
        let (_, pl) = self.texture_layout(ctx, plan.layout);
        let pl = pl.clone();
        let buffers: Vec<Option<wgpu::VertexBufferLayout<'_>>> = plan
            .buffers
            .iter()
            .map(|b| {
                Some(wgpu::VertexBufferLayout {
                    array_stride: b.stride,
                    step_mode: b.step,
                    attributes: &b.attributes,
                })
            })
            .collect();
        let scope = ctx.device().push_error_scope(wgpu::ErrorFilter::Validation);
        let p = ctx
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader.module,
                    entry_point: Some(&shader.vertex_entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &buffers,
                },
                primitive: plan.primitive,
                depth_stencil: plan.depth_stencil.clone(),
                multisample: wgpu::MultisampleState {
                    count: plan.key.samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader.module,
                    entry_point: Some(&shader.fragment_entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(plan.target.clone())],
                }),
                multiview_mask: None,
                cache: ctx.pipeline_cache(),
            });
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(RenderError::Validation {
                label: label.to_owned(),
                message: e.to_string(),
            });
        }
        ctx.note_resources(1);
        cache.insert(plan.key, p.clone());
        Ok(p)
    }

    /// The direct (wgpu-hal) pipeline of a plan, from the cache or created; `None` sends the
    /// scene back to wgpu.
    fn direct_pipeline(&mut self, shader: &PassShader, plan: &PipelinePlan) -> Option<usize> {
        if let Some(i) = self.direct_pipelines.get(&plan.key) {
            return Some(*i);
        }
        let d = self.direct.as_mut()?;
        let spec = PipelineSpec {
            shader: plan.key.shader.index(),
            vertex_entry: &shader.vertex_entry,
            fragment_entry: &shader.fragment_entry,
            layout: plan.layout,
            buffers: &plan.buffers,
            primitive: plan.primitive,
            depth_stencil: plan.depth_stencil.clone(),
            target: plan.target.clone(),
        };
        match d.add_pipeline(&spec) {
            Ok(i) => {
                self.direct_pipelines.insert(plan.key, i);
                Some(i)
            }
            Err(e) => {
                self.abandon_direct(&format!("{}: {}", shader.label(), e.0));
                None
            }
        }
    }

    fn compile_pass(
        &mut self,
        ctx: &GpuContext,
        shader: &PassShader,
        desc: &PassDesc,
        scene: &SceneDesc<'_>,
        cache: &mut HashMap<PipelineKey, wgpu::RenderPipeline>,
    ) -> Result<WgpuPass> {
        let plan = self.pipeline_plan(ctx, shader, desc, scene)?;
        let direct = if self.direct.is_some() {
            self.direct_pipeline(shader, &plan)
        } else {
            None
        };
        let pipeline = match direct {
            Some(_) => None,
            None => Some(self.wgpu_pipeline(ctx, shader, &plan, &desc.label, cache)?),
        };
        Ok(WgpuPass {
            pipeline,
            direct_pipeline: direct,
            bind_groups: Vec::new(),
            unfed: plan.unfed,
            failed: false,
        })
    }

    /// Compiles passes `first..` of `scene` in order, stopping at the first failure.
    fn compile_passes(
        &mut self,
        ctx: &GpuContext,
        scene: &SceneDesc<'_>,
        first: usize,
    ) -> Result<()> {
        let shaders = std::mem::take(&mut self.shaders);
        let mut pipelines = std::mem::take(&mut self.pipelines);
        let mut compiled = Ok(());
        for st in scene.passes.get(first..).unwrap_or_default() {
            let Some(shader) = shaders.get(st.desc.shader.index()) else {
                compiled = Err(RenderError::BadHandle("shader"));
                break;
            };
            match self.compile_pass(ctx, shader, &st.desc, scene, &mut pipelines) {
                Ok(p) => self.passes.push(p),
                Err(e) => {
                    compiled = Err(e);
                    break;
                }
            }
        }
        // Kept for passes added later (extensions): modules and pipelines are reused.
        self.shaders = shaders;
        self.pipelines = pipelines;
        compiled
    }

    /// Sends the scene back to the wgpu path for good (a wgpu-hal object could not be created,
    /// or the frame cannot be planned): the direct graph is destroyed after a GPU wait, and the
    /// caller creates the missing wgpu objects ([`Self::ensure_wgpu`]).
    fn abandon_direct(&mut self, why: &str) {
        if self.direct.take().is_some() {
            tracing::warn!(target: "lumenwall_render::direct", "recording through wgpu: {why}");
            self.direct_pipelines.clear();
            self.wgpu_reason = Some(WgpuReason::Unsupported);
            self.allowed = Err(WgpuReason::Unsupported);
            self.direct_modules.clear();
            for p in &mut self.passes {
                p.direct_pipeline = None;
            }
        }
    }

    /// Creates the wgpu pipelines, bind groups, segments and bundles a scene abandoned by the
    /// direct path is missing.
    fn ensure_wgpu(&mut self, ctx: &GpuContext, scene: &SceneDesc<'_>) -> Result<()> {
        let shaders = std::mem::take(&mut self.shaders);
        let mut cache = std::mem::take(&mut self.pipelines);
        let mut res = Ok(());
        for i in 0..self.passes.len() {
            if self.passes[i].pipeline.is_some() {
                continue;
            }
            let desc = &scene.passes[i].desc;
            let Some(shader) = shaders.get(desc.shader.index()) else {
                continue;
            };
            match self
                .pipeline_plan(ctx, shader, desc, scene)
                .and_then(|plan| self.wgpu_pipeline(ctx, shader, &plan, &desc.label, &mut cache))
            {
                Ok(p) => {
                    self.passes[i].pipeline = Some(p);
                    self.passes[i].failed = false;
                }
                Err(e) => {
                    // The wgpu path would have refused the scene: the pass is left out.
                    tracing::error!(target: "lumenwall_render", "{}: {e}", desc.label);
                    self.passes[i].failed = true;
                    res = Err(e);
                }
            }
        }
        self.shaders = shaders;
        self.pipelines = cache;
        for i in 0..self.passes.len() {
            if self.passes[i].bind_groups.is_empty() {
                self.bind_groups_for(ctx, i, scene);
            }
        }
        self.bundle_runs.clear();
        self.bundles.clear();
        self.rebuild_frame(ctx, scene);
        self.fell_back = true;
        res
    }

    /// Views, samplers and render targets of each slot-0 variant of pass `i` (both paths).
    fn pass_slots<'a>(&'a self, i: usize, scene: &SceneDesc<'a>) -> Vec<Vec<BoundSlot<'a>>> {
        let targets = scene.targets;
        (0..scene.passes[i].variants)
            .map(|v| {
                scene
                    .pass_slots(i, v)
                    .map(|s| {
                        let default = || self.default_texture.view();
                        let (view, target) = match s.texture {
                            BoundTexture::Texture(t) => (
                                self.textures
                                    .get(t.index())
                                    .map_or_else(default, GpuTexture::view),
                                None,
                            ),
                            BoundTexture::Target(r) => (targets.get(r).view(), Some(r)),
                            BoundTexture::Default => (default(), None),
                            // Unreachable without the volume: it exists when a shader has a
                            // volume slot.
                            BoundTexture::DefaultVolume => (
                                self.default_volume
                                    .as_ref()
                                    .map_or_else(default, GpuTexture::view),
                                None,
                            ),
                        };
                        BoundSlot {
                            slot: s.slot,
                            view,
                            sampler: s.sampler,
                            volume: s.volume,
                            target,
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// Every texture the scene owns (targets, textures, depth buffers).
    // wgpu handles hash and compare by their resource id, which never changes.
    #[allow(clippy::mutable_key_type)]
    fn owned_textures(&self, targets: &RenderTargetPool) -> HashSet<wgpu::Texture> {
        let mut owned: HashSet<wgpu::Texture> = targets
            .iter()
            .map(|t| t.texture().clone())
            .chain(self.textures.iter().map(|t| t.texture().clone()))
            .chain(self.depths.iter().map(|d| d.texture.clone()))
            .collect();
        owned.insert(self.default_texture.texture().clone());
        if let Some(v) = &self.default_volume {
            owned.insert(v.texture().clone());
        }
        owned
    }

    /// Texture bind groups of pass `i` (one per slot-0 variant), on the scene's path.
    fn bind_groups_for(&mut self, ctx: &GpuContext, i: usize, scene: &SceneDesc<'_>) {
        let Some(iface) = scene.shaders.get(scene.passes[i].desc.shader.index()) else {
            return;
        };
        let key = (iface.slots, iface.volume_slots);
        if let Some(mut d) = self.direct.take() {
            let result = {
                let slots = self.pass_slots(i, scene);
                let views: Vec<Vec<SlotView<'_>>> = slots
                    .iter()
                    .map(|v| {
                        v.iter()
                            .map(|s| SlotView {
                                slot: s.slot,
                                view: s.view,
                                sampler: s.sampler,
                                volume: s.volume,
                            })
                            .collect()
                    })
                    .collect();
                d.set_pass_groups(i, key, &views)
            };
            self.direct = Some(d);
            match result {
                Ok(()) => return,
                Err(e) => {
                    let label = &scene.passes[i].desc.label;
                    self.abandon_direct(&format!("{label}: {}", e.0));
                }
            }
        }
        self.texture_layout(ctx, key);
        let groups = {
            let slots = self.pass_slots(i, scene);
            let (bgl, _) = &self.texture_layouts[&key];
            let label = &scene.passes[i].desc.label;
            let mut groups = Vec::with_capacity(slots.len());
            for v in &slots {
                let samplers: Vec<wgpu::Sampler> = v
                    .iter()
                    .map(|s| {
                        if s.volume {
                            ctx.volume_sampler(s.sampler)
                        } else {
                            ctx.sampler(s.sampler)
                        }
                    })
                    .collect();
                let mut entries = Vec::with_capacity(v.len() * 2);
                for (s, sampler) in v.iter().zip(&samplers) {
                    entries.push(wgpu::BindGroupEntry {
                        binding: 2 * s.slot,
                        resource: wgpu::BindingResource::TextureView(s.view),
                    });
                    entries.push(wgpu::BindGroupEntry {
                        binding: 2 * s.slot + 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    });
                }
                groups.push(ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: bgl,
                    entries: &entries,
                }));
                ctx.note_resources(1);
            }
            groups
        };
        self.passes[i].bind_groups = groups;
    }

    /// Segments, then the bundles (wgpu path) or the recorded frame (direct path).
    fn rebuild_frame(&mut self, ctx: &GpuContext, scene: &SceneDesc<'_>) {
        self.build_segments(scene);
        if self.direct.is_none() {
            self.build_bundles(ctx, scene);
            return;
        }
        if let Err(why) = self.build_program(ctx, scene) {
            self.abandon_direct(&why);
            // Builds the bundles through `rebuild_frame` again, on the wgpu path.
            if let Err(e) = self.ensure_wgpu(ctx, scene) {
                tracing::error!(target: "lumenwall_render", "back to wgpu: {e}");
            }
        }
    }

    /// The direct frame of the current segments.
    fn build_program(
        &mut self,
        ctx: &GpuContext,
        scene: &SceneDesc<'_>,
    ) -> std::result::Result<(), String> {
        let Some(mut d) = self.direct.take() else {
            return Ok(());
        };
        let r = self
            .direct_steps(scene)
            .and_then(|steps| d.build_program(ctx, &steps).map_err(|e| e.0));
        self.direct = Some(d);
        r
    }

    /// The segments as the direct path records them.
    fn direct_steps<'a>(
        &'a self,
        scene: &SceneDesc<'a>,
    ) -> std::result::Result<Vec<Step<'a>>, String> {
        let targets = scene.targets;
        let mut steps = Vec::with_capacity(self.segments.len());
        for seg in &self.segments {
            match seg {
                Segment::Copy { src, dst } => {
                    let (s, d) = (targets.get(*src), targets.get(*dst));
                    let (sw, sh) = s.size();
                    let (dw, dh) = d.size();
                    steps.push(Step::Copy {
                        src: s.texture(),
                        dst: d.texture(),
                        size: (sw.min(dw), sh.min(dh)),
                    });
                }
                Segment::Render {
                    target,
                    clear,
                    depth,
                    items,
                } => {
                    let rt = targets.get(*target);
                    let mut sampled: Vec<&wgpu::Texture> = Vec::new();
                    let mut draws = Vec::with_capacity(items.len());
                    for item in items {
                        let Item::Pass(p) = *item else {
                            return Err("render bundle on the direct path".into());
                        };
                        for v in self.pass_slots(p, scene) {
                            for s in v {
                                if let Some(t) = s.target {
                                    let t = targets.get(t).texture();
                                    if !sampled.contains(&t) {
                                        sampled.push(t);
                                    }
                                }
                            }
                        }
                        if let Some(d) = self.direct_draw(p, scene)? {
                            draws.push(d);
                        }
                    }
                    steps.push(Step::Render {
                        target: rt.texture(),
                        clear: *clear,
                        depth: depth
                            .map(|(d, c)| (&self.depths[d].texture, &self.depths[d].view, c)),
                        sampled,
                        draws,
                    });
                }
            }
        }
        Ok(steps)
    }

    /// The draw of pass `p` (what [`Self::record`] does on the wgpu path); `None` for a mesh
    /// without indices.
    fn direct_draw(
        &self,
        p: usize,
        scene: &SceneDesc<'_>,
    ) -> std::result::Result<Option<DrawStep<'_>>, String> {
        let pass = &self.passes[p];
        let st = &scene.passes[p];
        let label = &st.desc.label;
        let pipeline = pass
            .direct_pipeline
            .ok_or_else(|| format!("{label}: no wgpu-hal pipeline"))?;
        let dynbuf = &self
            .dynbuf
            .as_ref()
            .ok_or_else(|| format!("{label}: scene not built"))?
            .buffer;
        let vertex_offset = u64::from(scene.dynamic.vertex_offset());
        let mut vertex: Vec<(&wgpu::Buffer, u64)> = Vec::new();
        let mut index = None;
        let call = match st.desc.geometry {
            Geometry::Vertices(v) => {
                vertex.push((dynbuf, vertex_offset));
                DrawCall::Vertices {
                    first: v.first,
                    count: v.count,
                }
            }
            Geometry::Indexed { vertices, indices } => {
                let ib = self
                    .index_buffer
                    .as_ref()
                    .ok_or_else(|| format!("{label}: no index buffer"))?;
                vertex.push((dynbuf, vertex_offset));
                index = Some((ib, wgpu::IndexFormat::Uint16));
                DrawCall::Indexed {
                    first: indices.first,
                    count: indices.count,
                    base: vertices.first as i32,
                }
            }
            Geometry::Skinned {
                vertices,
                indices,
                skin,
            } => {
                let ib = self
                    .index_buffer
                    .as_ref()
                    .ok_or_else(|| format!("{label}: no index buffer"))?;
                let sb = self
                    .skin_buffer
                    .as_ref()
                    .ok_or_else(|| format!("{label}: no skin buffer"))?;
                // Both buffers start at the mesh: its indices are relative to its first vertex.
                vertex.push((
                    dynbuf,
                    vertex_offset + u64::from(vertices.first * VERTEX_SIZE),
                ));
                vertex.push((sb, u64::from(skin.first * SKIN_VERTEX_SIZE)));
                index = Some((ib, wgpu::IndexFormat::Uint16));
                DrawCall::Indexed {
                    first: indices.first,
                    count: indices.count,
                    base: 0,
                }
            }
            Geometry::Stream(s) => {
                let st = self
                    .streams
                    .get(s.index())
                    .ok_or_else(|| format!("{label}: unknown stream"))?;
                vertex.push((&st.vertices, 0));
                index = Some((&st.indices, wgpu::IndexFormat::Uint32));
                DrawCall::Stream(s.0)
            }
            Geometry::Mesh(m) => {
                let mesh = self
                    .meshes
                    .get(m.index())
                    .ok_or_else(|| format!("{label}: unknown mesh"))?;
                if mesh.index_count == 0 {
                    return Ok(None);
                }
                vertex.push((&mesh.vertices, 0));
                index = Some((&mesh.indices, wgpu::IndexFormat::Uint16));
                DrawCall::Indexed {
                    first: 0,
                    count: mesh.index_count,
                    base: 0,
                }
            }
        };
        for &(_, int) in &pass.unfed {
            vertex.push((&self.default_attrs, if int { 16 } else { 0 }));
        }
        Ok(Some(DrawStep {
            pass: p as u32,
            pipeline,
            uniform_offset: st.uniform.map_or(0, |u| u.offset),
            vertex,
            index,
            call,
        }))
    }

    fn build_segments(&mut self, scene: &SceneDesc<'_>) {
        let mut segments = Vec::new();
        let mut cur: Option<(RtId, Option<wgpu::Color>, Vec<usize>)> = None;
        let flush = |cur: &mut Option<(RtId, Option<wgpu::Color>, Vec<usize>)>,
                     segments: &mut Vec<Segment>| {
            if let Some((target, clear, passes)) = cur.take() {
                segments.push(Segment::Render {
                    target,
                    clear,
                    depth: None,
                    items: passes.into_iter().map(Item::Pass).collect(),
                });
            }
        };
        for op in scene.ops {
            match *op {
                crate::desc::Op::Clear { target, color } => {
                    flush(&mut cur, &mut segments);
                    cur = Some((
                        target,
                        Some(wgpu::Color {
                            r: color[0],
                            g: color[1],
                            b: color[2],
                            a: color[3],
                        }),
                        Vec::new(),
                    ));
                }
                crate::desc::Op::Copy { src, dst } => {
                    flush(&mut cur, &mut segments);
                    segments.push(Segment::Copy { src, dst });
                }
                crate::desc::Op::Draw(p) => {
                    let Some(pass) = scene.passes.get(p.index()).filter(|p| p.enabled) else {
                        continue;
                    };
                    if self.passes.get(p.index()).is_none_or(|w| w.failed) {
                        continue;
                    }
                    let target = pass.desc.target;
                    if let Some(scratch) = pass.hazard_scratch {
                        flush(&mut cur, &mut segments);
                        segments.push(Segment::Copy {
                            src: target,
                            dst: scratch,
                        });
                    }
                    if pass.clear_first {
                        // Shared texture: start from transparent black, as a fresh target.
                        flush(&mut cur, &mut segments);
                        cur = Some((target, Some(wgpu::Color::TRANSPARENT), Vec::new()));
                    } else if cur.as_ref().is_none_or(|c| c.0 != target) {
                        flush(&mut cur, &mut segments);
                        cur = Some((target, None, Vec::new()));
                    }
                    if let Some(c) = cur.as_mut() {
                        c.2.push(p.index());
                    }
                }
            }
        }
        flush(&mut cur, &mut segments);
        // Depth attachments: the first render pass of the frame on a target clears its depth.
        let mut seen: Vec<usize> = Vec::new();
        for seg in &mut segments {
            if let Segment::Render { target, depth, .. } = seg
                && let Some(d) = self.depths.iter().position(|d| d.target == *target)
            {
                *depth = Some((d, !seen.contains(&d)));
                seen.push(d);
            }
        }
        self.segments = segments;
    }

    fn record<'a, R: Recorder<'a>>(&'a self, r: &mut R, index: usize, scene: &SceneDesc<'_>) {
        let pass = &self.passes[index];
        let st = &scene.passes[index];
        let stream = match st.desc.geometry {
            Geometry::Stream(s) => {
                match (self.streams.get(s.index()), scene.streams.get(s.index())) {
                    (Some(g), Some(s)) if s.index_count > 0 => Some((g, s.index_count)),
                    _ => return,
                }
            }
            _ => None,
        };
        let mesh = match st.desc.geometry {
            Geometry::Mesh(m) => match self.meshes.get(m.index()) {
                Some(m) if m.index_count > 0 => Some(m),
                _ => return,
            },
            _ => None,
        };
        let (Some(pipeline), Some(group), Some(uniforms), Some(dynbuf)) = (
            &pass.pipeline,
            pass.bind_groups.get(st.variant),
            &self.uniform_bg,
            &self.dynbuf,
        ) else {
            return;
        };
        let dynbuf = &dynbuf.buffer;
        let vertex_offset = scene.dynamic.vertex_offset();
        r.pipeline(pipeline);
        r.bind_group(0, uniforms, &[st.uniform.map_or(0, |u| u.offset)]);
        r.bind_group(1, group, &[]);
        if let Some((s, _)) = stream {
            r.vertex_buffer(0, s.vertices.slice(..));
        } else if let Some(m) = mesh {
            r.vertex_buffer(0, m.vertices.slice(..));
        } else if let Geometry::Skinned { vertices, skin, .. } = st.desc.geometry {
            // Both buffers start at the mesh: its indices are relative to its first vertex.
            let start = vertex_offset + vertices.first * VERTEX_SIZE;
            r.vertex_buffer(0, dynbuf.slice(u64::from(start)..));
            if let Some(b) = &self.skin_buffer {
                r.vertex_buffer(1, b.slice(u64::from(skin.first * SKIN_VERTEX_SIZE)..));
            }
        } else {
            r.vertex_buffer(0, dynbuf.slice(u64::from(vertex_offset)..));
        }
        for &(slot, int) in &pass.unfed {
            let off = if int { 16 } else { 0 };
            r.vertex_buffer(slot, self.default_attrs.slice(off..off + 16));
        }
        match st.desc.geometry {
            Geometry::Vertices(v) => r.draw(v.first..v.first + v.count),
            Geometry::Indexed { vertices, indices } => {
                if let Some(ib) = &self.index_buffer {
                    r.index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
                    r.draw_indexed(
                        indices.first..indices.first + indices.count,
                        vertices.first as i32,
                    );
                }
            }
            Geometry::Skinned { indices, .. } => {
                if let Some(ib) = &self.index_buffer {
                    r.index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
                    r.draw_indexed(indices.first..indices.first + indices.count, 0);
                }
            }
            Geometry::Stream(_) => {
                if let Some((s, count)) = stream {
                    r.index_buffer(s.indices.slice(..), wgpu::IndexFormat::Uint32);
                    r.draw_indexed(0..count, 0);
                }
            }
            Geometry::Mesh(_) => {
                if let Some(m) = mesh {
                    r.index_buffer(m.indices.slice(..), wgpu::IndexFormat::Uint16);
                    r.draw_indexed(0..m.index_count, 0);
                }
            }
        }
    }

    /// Group runs of frozen passes (a single bind group) into render bundles. A run recorded
    /// by the previous build is reused as is (layers added, removed or hidden re-record only
    /// the runs they change).
    fn build_bundles(&mut self, ctx: &GpuContext, scene: &SceneDesc<'_>) {
        let mut bundles = Vec::new();
        let mut runs: Vec<Vec<usize>> = Vec::new();
        let mut previous: HashMap<Vec<usize>, wgpu::RenderBundle> =
            std::mem::take(&mut self.bundle_runs)
                .into_iter()
                .zip(std::mem::take(&mut self.bundles))
                .collect();
        let mut new_segments = Vec::with_capacity(self.segments.len());
        let segments = std::mem::take(&mut self.segments);
        for seg in segments {
            match seg {
                Segment::Render {
                    target,
                    clear,
                    depth,
                    items,
                } => {
                    let depth_stencil = depth.map(|_| wgpu::RenderBundleDepthStencil {
                        format: DEPTH_FORMAT,
                        depth_read_only: false,
                        stencil_read_only: true,
                    });
                    let mut out = Vec::new();
                    let mut run: Vec<usize> = Vec::new();
                    let mut close_run =
                        |run: &mut Vec<usize>,
                         out: &mut Vec<Item>,
                         bundles: &mut Vec<wgpu::RenderBundle>| {
                            if run.is_empty() {
                                return;
                            }
                            if let Some(b) = previous.remove(run.as_slice()) {
                                bundles.push(b);
                                runs.push(std::mem::take(run));
                                out.push(Item::Bundle(bundles.len() - 1));
                                return;
                            }
                            let mut enc = ctx.device().create_render_bundle_encoder(
                                &wgpu::RenderBundleEncoderDescriptor {
                                    label: Some("lumenwall.bundle"),
                                    color_formats: &[Some(scene.targets.format())],
                                    depth_stencil,
                                    sample_count: scene.targets.get(target).samples(),
                                    multiview: None,
                                },
                            );
                            for &p in run.iter() {
                                self.record(&mut enc, p, scene);
                            }
                            bundles.push(enc.finish(&wgpu::RenderBundleDescriptor {
                                label: Some("lumenwall.bundle"),
                            }));
                            ctx.note_resources(1);
                            runs.push(std::mem::take(run));
                            out.push(Item::Bundle(bundles.len() - 1));
                        };
                    for item in items {
                        let Item::Pass(p) = item else {
                            continue;
                        };
                        // Stream passes change their draw count every frame: never bundled.
                        if self.passes[p].bind_groups.len() == 1
                            && !matches!(scene.passes[p].desc.geometry, Geometry::Stream(_))
                        {
                            run.push(p);
                        } else {
                            close_run(&mut run, &mut out, &mut bundles);
                            out.push(Item::Pass(p));
                        }
                    }
                    close_run(&mut run, &mut out, &mut bundles);
                    new_segments.push(Segment::Render {
                        target,
                        clear,
                        depth,
                        items: out,
                    });
                }
                other => new_segments.push(other),
            }
        }
        self.segments = new_segments;
        self.bundles = bundles;
        self.bundle_runs = runs;
    }

    /// Records the frame's segments; with `queries`, timestamps around each one (indices `2i`
    /// and `2i + 1`, [`SceneBackend::profile`]).
    fn record_segments(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queries: Option<&wgpu::QuerySet>,
        scene: &SceneDesc<'_>,
    ) {
        let targets = scene.targets;
        for (si, seg) in self.segments.iter().enumerate() {
            let (begin, end) = (2 * si as u32, 2 * si as u32 + 1);
            match seg {
                Segment::Copy { src, dst } => {
                    if let Some(q) = queries {
                        encoder.write_timestamp(q, begin);
                    }
                    let (s, d) = (targets.get(*src), targets.get(*dst));
                    let (sw, sh) = s.size();
                    let (dw, dh) = d.size();
                    encoder.copy_texture_to_texture(
                        s.texture().as_image_copy(),
                        d.texture().as_image_copy(),
                        wgpu::Extent3d {
                            width: sw.min(dw),
                            height: sh.min(dh),
                            depth_or_array_layers: 1,
                        },
                    );
                    if let Some(q) = queries {
                        encoder.write_timestamp(q, end);
                    }
                }
                Segment::Render {
                    target,
                    clear,
                    depth,
                    items,
                } => {
                    let rt = targets.get(*target);
                    let depth_stencil_attachment =
                        depth.map(|(d, clear)| wgpu::RenderPassDepthStencilAttachment {
                            view: &self.depths[d].view,
                            depth_ops: Some(wgpu::Operations {
                                load: if clear {
                                    wgpu::LoadOp::Clear(1.0)
                                } else {
                                    wgpu::LoadOp::Load
                                },
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        });
                    let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            // Multisampled target: draw into the companion texture and
                            // resolve into the target's own after every render pass, so
                            // everything that samples or copies it sees the resolved image.
                            view: rt.msaa_view().unwrap_or_else(|| rt.view()),
                            depth_slice: None,
                            resolve_target: rt.msaa_view().map(|_| rt.view()),
                            ops: wgpu::Operations {
                                load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment,
                        timestamp_writes: queries.map(|q| wgpu::RenderPassTimestampWrites {
                            query_set: q,
                            beginning_of_pass_write_index: Some(begin),
                            end_of_pass_write_index: Some(end),
                        }),
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    for item in items {
                        match item {
                            Item::Bundle(b) => {
                                rp.execute_bundles(std::iter::once(&self.bundles[*b]))
                            }
                            Item::Pass(p) => self.record(&mut rp, *p, scene),
                        }
                    }
                }
            }
        }
    }

    /// GPU milliseconds of each segment of one frame recorded through wgpu.
    fn profile_wgpu(&mut self, ctx: &GpuContext, scene: &SceneDesc<'_>) -> Option<Vec<f64>> {
        let device = ctx.device();
        let count = 2 * self.segments.len() as u32;
        let set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("lumenwall.profile"),
            ty: wgpu::QueryType::Timestamp,
            count,
        });
        let size = u64::from(count) * 8;
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumenwall.profile.resolve"),
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumenwall.profile.read"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lumenwall.profile"),
        });
        self.record_segments(&mut enc, Some(&set), scene);
        enc.resolve_query_set(&set, 0..count, &resolve, 0);
        enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, size);
        ctx.queue().submit(std::iter::once(enc.finish()));
        read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        ctx.wait_idle();
        let period = f64::from(ctx.queue().get_timestamp_period()) / 1e6;
        let ticks: Vec<u64> = {
            let data = read.slice(..).get_mapped_range().ok()?;
            bytemuck::cast_slice::<u8, u64>(&data).to_vec()
        };
        read.unmap();
        Some(
            ticks
                .chunks(2)
                .map(|t| t[1].saturating_sub(t[0]) as f64 * period)
                .collect(),
        )
    }

    /// Sets up the direct path of a new scene (its wgpu-hal modules and uniform bind group).
    fn start_direct(&mut self, ctx: &GpuContext, buffer: &wgpu::Buffer, binding_size: u32) {
        let modules = std::mem::take(&mut self.direct_modules);
        let made = self.allowed.and_then(|()| {
            let fail = |e: crate::hal::Unsupported| {
                tracing::warn!(target: "lumenwall_render::direct", "recording through wgpu: {}", e.0);
                WgpuReason::Unsupported
            };
            let mut d = DirectGraph::new(ctx).map_err(fail)?;
            for (id, module, info) in modules {
                d.add_module(id, module, info).map_err(fail)?;
            }
            d.set_uniform_group(buffer, binding_size, frame_globals::SIZE)
                .map_err(fail)?;
            Ok(d)
        });
        match made {
            Ok(d) => self.direct = Some(d),
            Err(r) => {
                self.wgpu_reason = Some(r);
                self.allowed = Err(r);
            }
        }
    }

    /// Debug log of the path a built scene took.
    fn log_recording(&self) {
        match &self.direct {
            Some(d) => {
                let s = d.stats();
                tracing::debug!(
                    target: "lumenwall_render::direct",
                    "recording directly in Vulkan: {} passes, {} render passes, {} draws, {} pipelines, {} image barriers",
                    self.passes.len(),
                    s.render_passes,
                    s.draws,
                    s.pipelines,
                    s.barriers
                );
            }
            None => tracing::debug!(
                target: "lumenwall_render::direct",
                "recording through wgpu ({:?})",
                self.wgpu_reason
            ),
        }
    }

    /// The direct path has no frame to replay (after a rebuild that left it empty): plan it.
    fn ensure_program(&mut self, ctx: &GpuContext, scene: &SceneDesc<'_>) {
        if self.direct.as_ref().is_some_and(|d| !d.has_program()) {
            self.rebuild_frame(ctx, scene);
        }
    }
}

impl SceneBackend for WgpuScene {
    fn uniform_alignment(&self, gpu: Gpu<'_>) -> u32 {
        gpu.wgpu().map_or(256, |ctx| {
            ctx.device().limits().min_uniform_buffer_offset_alignment
        })
    }

    fn caps(&self, gpu: Gpu<'_>) -> BackendCaps {
        gpu.wgpu().map_or(
            BackendCaps {
                bc: false,
                max_texture_2d: 2048,
            },
            |ctx| BackendCaps {
                bc: ctx.supports_bc(),
                max_texture_2d: ctx.device().limits().max_texture_dimension_2d,
            },
        )
    }

    fn supports_samples(&self, gpu: Gpu<'_>, format: wgpu::TextureFormat, samples: u32) -> bool {
        gpu.wgpu().is_some_and(|ctx| {
            ctx.adapter()
                .get_texture_format_features(format)
                .flags
                .sample_count_supported(samples)
        })
    }

    fn quality_changed(
        &mut self,
        _gpu: Gpu<'_>,
        quality: &QualitySettings,
        targets: &RenderTargetPool,
    ) {
        if quality.msaa != Msaa::Off
            || targets.format() == RT_FORMAT_HDR
            || targets.iter().any(|t| t.samples() > 1)
        {
            // Multisampled and 16-bit float targets are recorded through wgpu only.
            self.allowed = Err(WgpuReason::Quality);
        }
    }

    fn add_texture(&mut self, gpu: Gpu<'_>, upload: &TextureUpload<'_>) -> Result<TextureInfo> {
        let t = upload_texture(wgpu_ctx(gpu)?, upload)?;
        let info = t.info();
        self.textures.push(t);
        Ok(info)
    }

    fn add_volume_texture(
        &mut self,
        gpu: Gpu<'_>,
        upload: &VolumeUpload<'_>,
    ) -> Result<TextureInfo> {
        let t = upload_volume(wgpu_ctx(gpu)?, upload)?;
        let info = t.info();
        self.textures.push(t);
        Ok(info)
    }

    fn add_shader(&mut self, gpu: Gpu<'_>, shader: PreparedShader) -> Result<()> {
        let ctx = wgpu_ctx(gpu)?;
        // The direct path hands wgpu-hal the module wgpu-core receives: clip fix applied,
        // validated.
        let direct = self
            .allowed
            .is_ok()
            .then(|| (shader.module.clone(), shader.info.clone()));
        let s = crate::shader::compile_prepared(ctx, shader)?;
        self.shaders.push(s);
        if let Some((module, info)) = direct {
            self.direct_modules
                .push((self.shaders.len() - 1, module, info));
        }
        Ok(())
    }

    fn add_mesh(
        &mut self,
        gpu: Gpu<'_>,
        label: &str,
        _layout: &MeshLayout,
        vertices: &[u8],
        indices: &[u16],
    ) -> Result<()> {
        let ctx = wgpu_ctx(gpu)?;
        let device = ctx.device();
        let vsize = (vertices.len() as u64)
            .max(16)
            .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: vsize,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !vertices.is_empty() {
            let len = vertices.len() - vertices.len() % 4;
            ctx.queue().write_buffer(&vbuf, 0, &vertices[..len]);
            if len < vertices.len() {
                let mut tail = [0u8; 4];
                tail[..vertices.len() - len].copy_from_slice(&vertices[len..]);
                ctx.queue().write_buffer(&vbuf, len as u64, &tail);
            }
        }
        // Even index count: buffer writes are 4-byte aligned.
        let mut padded = indices.to_vec();
        if padded.len() % 2 == 1 {
            padded.push(0);
        }
        let ibytes: &[u8] = bytemuck::cast_slice(&padded);
        let isize = (ibytes.len() as u64)
            .max(16)
            .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
        let ibuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: isize,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !ibytes.is_empty() {
            ctx.queue().write_buffer(&ibuf, 0, ibytes);
        }
        ctx.note_resources(2);
        self.meshes.push(StaticMesh {
            vertices: vbuf,
            indices: ibuf,
            index_count: indices.len() as u32,
            _memory: ctx.memory().track(vsize + isize),
        });
        Ok(())
    }

    fn add_stream(
        &mut self,
        gpu: Gpu<'_>,
        label: &str,
        layout: &StreamLayout,
        max_vertices: u32,
        indices: &[u32],
    ) -> Result<()> {
        let ctx = wgpu_ctx(gpu)?;
        let device = ctx.device();
        let max_floats = max_vertices as usize * layout.stride_floats as usize;
        let vsize = ((max_floats * 4) as u64)
            .max(16)
            .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: vsize,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ibytes: &[u8] = bytemuck::cast_slice(indices);
        let isize = (ibytes.len() as u64)
            .max(16)
            .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: isize,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !ibytes.is_empty() {
            ctx.queue().write_buffer(&index_buffer, 0, ibytes);
        }
        ctx.note_resources(2);
        self.streams.push(GpuStream {
            vertices,
            indices: index_buffer,
            _memory: ctx.memory().track(vsize + isize),
        });
        Ok(())
    }

    fn build(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>) -> Result<()> {
        let ctx = wgpu_ctx(gpu)?;
        let device = ctx.device();
        let dynbuf = GpuDynamic::new(ctx, scene.dynamic);
        // One staging chunk holds a whole frame's uploads: every uniform and every stream.
        let upload_chunk = (scene.dynamic.byte_len()
            + scene
                .streams
                .iter()
                .map(|s| s.max_floats as u64 * 4)
                .sum::<u64>())
        .next_multiple_of(4096)
        .max(16 * 1024);
        self.index_buffer = (!scene.indices.is_empty()).then(|| {
            let bytes: &[u8] = bytemuck::cast_slice(scene.indices);
            let size = (bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lumenwall.indices"),
                size,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            ctx.queue().write_buffer(&b, 0, bytes);
            ctx.note_resources(1);
            b
        });
        self.skin_buffer = (!scene.skins.is_empty()).then(|| {
            let bytes: &[u8] = bytemuck::cast_slice(scene.skins);
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lumenwall.skin"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            ctx.queue().write_buffer(&b, 0, bytes);
            ctx.note_resources(1);
            b
        });
        self.uniform_bg = Some(uniform_bind_group(
            ctx,
            &self.uniform_layout,
            &dynbuf.buffer,
            scene.dynamic.binding_size(),
        ));
        // The 1×1×1 volume exists only when a shader has a `texture_3d` slot.
        if scene.shaders.iter().any(|s| s.volume_slots != 0) {
            self.default_volume = Some(default_volume(ctx));
        }
        self.depths = scene
            .depth_targets
            .iter()
            .map(|&t| {
                let rt = scene.targets.get(t);
                create_depth(ctx, t, rt.size(), rt.samples())
            })
            .collect();
        self.uploads = Some(wgpu::util::StagingBelt::new(device.clone(), upload_chunk));
        self.start_direct(ctx, &dynbuf.buffer, scene.dynamic.binding_size());
        self.dynbuf = Some(dynbuf);
        self.compile_passes(ctx, scene, 0)?;
        if self.direct.is_none() && self.passes.iter().any(|p| p.pipeline.is_none()) {
            // Direct path abandoned while compiling: the passes compiled for it get wgpu
            // pipelines.
            self.ensure_wgpu(ctx, scene)?;
        }
        self.rebind(gpu, scene)?;
        self.log_recording();
        Ok(())
    }

    fn extend(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>, first: usize) -> Result<()> {
        let ctx = wgpu_ctx(gpu)?;
        if self.default_volume.is_none() && scene.shaders.iter().any(|s| s.volume_slots != 0) {
            self.default_volume = Some(default_volume(ctx));
        }
        // The region may have grown even if nothing is compiled below: the GPU buffer follows.
        if scene.dynamic.grew() {
            self.dynbuf = Some(GpuDynamic::new(ctx, scene.dynamic));
        }
        let rebound = scene.dynamic.grew() || scene.dynamic.binding_grew();
        if rebound && let Some(d) = &self.dynbuf {
            self.uniform_bg = Some(uniform_bind_group(
                ctx,
                &self.uniform_layout,
                &d.buffer,
                scene.dynamic.binding_size(),
            ));
            // Bundles recorded the old buffer or binding size: re-recorded before the next
            // frame (the front marks the frame stale).
            self.bundle_runs.clear();
        }
        // Direct path: wgpu-hal modules of the new shaders, uniform group over a new buffer.
        if let (Some(_), Err(r)) = (&self.direct, self.allowed) {
            self.abandon_direct(&format!(
                "a shader added to the scene cannot be prepared ({r:?})"
            ));
        }
        if let Some(d) = &mut self.direct {
            let mut failed = None;
            for (id, module, info) in std::mem::take(&mut self.direct_modules) {
                if let Err(e) = d.add_module(id, module, info) {
                    failed = Some(e.0);
                    break;
                }
            }
            if failed.is_none()
                && rebound
                && let Some(buffer) = &self.dynbuf
                && let Err(e) = d.set_uniform_group(
                    &buffer.buffer,
                    scene.dynamic.binding_size(),
                    frame_globals::SIZE,
                )
            {
                failed = Some(e.0);
            }
            if let Some(f) = failed {
                self.abandon_direct(&f);
            }
        }
        let compiled = self.compile_passes(ctx, scene, first);
        if compiled.is_err() {
            self.passes.truncate(first);
        }
        for i in first..self.passes.len() {
            self.bind_groups_for(ctx, i, scene);
        }
        if self.direct.is_none()
            && self
                .passes
                .iter()
                .any(|p| p.pipeline.is_none() || p.bind_groups.is_empty())
        {
            // Abandoned during this extension: the passes made for the direct path get theirs.
            let _ = self.ensure_wgpu(ctx, scene);
        }
        compiled
    }

    fn rebind(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>) -> Result<()> {
        let ctx = wgpu_ctx(gpu)?;
        // Direct path: the GPU finishes the recorded frames before what they use is replaced.
        if self.direct.is_some() {
            #[allow(clippy::mutable_key_type)] // Keys compared by resource id.
            let owned = self.owned_textures(scene.targets);
            if let Some(d) = &mut self.direct {
                d.idle(&|t| owned.contains(t));
            }
        }
        // Depth buffers follow their target's size.
        for i in 0..self.depths.len() {
            let target = self.depths[i].target;
            let rt = scene.targets.get(target);
            if self.depths[i].size != rt.size() {
                self.depths[i] = create_depth(ctx, target, rt.size(), rt.samples());
            }
        }
        for i in 0..self.passes.len() {
            self.bind_groups_for(ctx, i, scene);
        }
        if self.direct.is_none() && self.passes.iter().any(|p| p.pipeline.is_none()) {
            // Direct path abandoned while binding: its passes get wgpu pipelines.
            self.ensure_wgpu(ctx, scene)?;
        }
        self.bundle_runs.clear();
        self.rebuild_frame(ctx, scene);
        if let Some(d) = &mut self.direct {
            d.compact();
        }
        Ok(())
    }

    fn replan(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>) {
        if let Some(ctx) = gpu.wgpu() {
            self.rebuild_frame(ctx, scene);
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, scene: &SceneDesc<'_>) -> bool {
        let Frame::Wgpu {
            ctx,
            encoder,
            commands,
        } = frame
        else {
            return false;
        };
        let ctx: &GpuContext = ctx;
        self.ensure_program(ctx, scene);
        if let (Some(uploads), Some(dynbuf)) = (self.uploads.as_mut(), self.dynbuf.as_ref()) {
            // The previous frame was submitted: its staging chunks come back once the GPU is
            // done.
            uploads.recall();
            if let Some((start, end)) = dirty_bytes(scene.dynamic)
                && let Some(size) = wgpu::BufferSize::new(u64::from(end - start))
            {
                uploads
                    .write_buffer(encoder, &dynbuf.buffer, u64::from(start), size)
                    .copy_from_slice(&scene.dynamic.bytes()[start as usize..end as usize]);
            }
            for (s, gpu) in scene.streams.iter().zip(&self.streams) {
                if s.pending == 0 {
                    continue;
                }
                if let Some(size) = wgpu::BufferSize::new(s.pending as u64 * 4) {
                    uploads
                        .write_buffer(encoder, &gpu.vertices, 0, size)
                        .copy_from_slice(bytemuck::cast_slice(&s.vertices[..s.pending]));
                }
            }
            uploads.finish();
        }
        match &self.direct {
            None => self.record_segments(encoder, None, scene),
            Some(d) => {
                if scene.textures_written {
                    // Textures and targets written through wgpu (queue writes, copies) are in
                    // another state for wgpu-core: back to their sampled state before the frame
                    // samples them.
                    crate::hal::restore_textures(
                        encoder,
                        self.textures
                            .iter()
                            .map(GpuTexture::texture)
                            .chain(scene.targets.iter().map(|t| t.texture())),
                    );
                }
                let next = ctx
                    .device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("lumenwall.frame"),
                    });
                let before = std::mem::replace(&mut **encoder, next).finish();
                let t_record = Instant::now();
                let recorded = d.record(
                    ctx.device(),
                    &FrameView {
                        passes: scene.passes,
                        streams: scene.streams,
                    },
                    None,
                );
                self.last_direct.record = t_record.elapsed();
                commands.buffers = [Some(before), Some(recorded)];
            }
        }
        std::mem::take(&mut self.fell_back)
    }

    fn read_target(
        &mut self,
        gpu: Gpu<'_>,
        scene: &SceneDesc<'_>,
        target: RtId,
    ) -> Result<Vec<u8>> {
        let ctx = wgpu_ctx(gpu)?;
        let rt = scene
            .targets
            .try_get(target)
            .ok_or(RenderError::BadHandle("render target"))?;
        crate::headless::read_texture_rgba8(ctx, rt.texture())
    }

    fn profile(&mut self, gpu: Gpu<'_>, scene: &SceneDesc<'_>) -> Option<Vec<SegmentTime>> {
        let ctx = gpu.wgpu()?;
        // The frame's dirty uniforms go first (the profiled frame uploads nothing else).
        if let Some(d) = &self.dynbuf
            && let Some((start, end)) = dirty_bytes(scene.dynamic)
        {
            ctx.queue().write_buffer(
                &d.buffer,
                u64::from(start),
                &scene.dynamic.bytes()[start as usize..end as usize],
            );
        }
        if !ctx.caps().timestamps || self.segments.is_empty() {
            return None;
        }
        self.ensure_program(ctx, scene);
        let ms: Vec<f64> = match &self.direct {
            Some(d) => {
                let uploads = ctx
                    .device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("lumenwall.profile"),
                    })
                    .finish();
                d.profile(
                    ctx,
                    &FrameView {
                        passes: scene.passes,
                        streams: scene.streams,
                    },
                    uploads,
                )?
            }
            None => self.profile_wgpu(ctx, scene)?,
        };
        let mut out = Vec::with_capacity(self.segments.len());
        for (seg, ms) in self.segments.iter().zip(ms) {
            let (target, passes, first) = match seg {
                Segment::Copy { dst, .. } => (*dst, 0, "copy".to_owned()),
                Segment::Render { target, items, .. } => {
                    let ids: Vec<usize> = items
                        .iter()
                        .flat_map(|i| match i {
                            Item::Pass(p) => vec![*p],
                            Item::Bundle(b) => {
                                self.bundle_runs.get(*b).cloned().unwrap_or_default()
                            }
                        })
                        .collect();
                    let first = ids
                        .first()
                        .map_or_else(String::new, |&p| scene.passes[p].desc.label.clone());
                    (*target, ids.len(), first)
                }
            };
            let rt = scene.targets.get(target);
            out.push(SegmentTime {
                target: rt.label().to_owned(),
                size: rt.size(),
                passes,
                first_pass: first,
                ms,
            });
        }
        Some(out)
    }

    fn counts(&self) -> GraphCounts {
        GraphCounts {
            passes: self.passes.len(),
            render_passes: self
                .segments
                .iter()
                .filter(|s| matches!(s, Segment::Render { .. }))
                .count(),
            copies: self
                .segments
                .iter()
                .filter(|s| matches!(s, Segment::Copy { .. }))
                .count(),
            bundles: self.bundles.len(),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

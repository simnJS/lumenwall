//! Generic pass graph: a pass = pipeline (shader + vertex inputs + blend +
//! write mask + culling) + texture/sampler bindings + a uniform block.
//!
//! Everything is created once by [`SceneBuilder::build`]: pipelines, bind
//! groups, hazard copies, render-pass grouping and render bundles for frozen
//! passes. A frame then performs at most one `write_buffer` and records the
//! precomputed segments, with no allocation and no GPU object creation.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::blend::{BlendMode, CullMode, FRONT_FACE, WriteMask};
use crate::context::GpuContext;
use crate::dynbuf::{
    DynamicBuffer, DynamicBufferBuilder, FrameInputs, Patch, ScalarKind, Shadow, UniformBlock,
    UniformSlot, UniformType, VertexRange, frame_globals, patches_for,
};
use crate::error::{RenderError, Result};
use crate::math::{SKIN_VERTEX_SIZE, SkinVertex, VERTEX_SIZE, Vertex};
use crate::shader::{MAX_SLOTS, PassShader, ShaderDesc, ShaderInterface, VertexAttr};
use crate::targets::{RT_FORMAT, RenderTargetPool, RtId};
use crate::texture::{GpuTexture, SamplerKey, TextureUpload, default_texture, upload_texture};

/// Handle of a compiled shader in a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShaderId(u32);

/// Handle of an uploaded texture in a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureId(u32);

/// Handle of a pass in a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PassId(u32);

/// Indices of the static index buffer (puppet meshes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexRange {
    pub first: u32,
    pub count: u32,
}

/// What a texture slot samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureRef {
    Texture(TextureId),
    Target(RtId),
    /// 1×1 `(0, 0, 0, 1)`.
    Default,
}

/// Binding of one slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotBinding {
    pub texture: TextureRef,
    /// Sampler override; `None` = the texture's or target's own sampling.
    pub sampler: Option<SamplerKey>,
}

impl SlotBinding {
    #[must_use]
    pub fn texture(id: TextureId) -> Self {
        Self {
            texture: TextureRef::Texture(id),
            sampler: None,
        }
    }

    #[must_use]
    pub fn target(id: RtId) -> Self {
        Self {
            texture: TextureRef::Target(id),
            sampler: None,
        }
    }
}

/// Geometry of a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Geometry {
    /// Non-indexed triangles of the dynamic vertex region.
    Vertices(VertexRange),
    /// Indexed triangles (16-bit indices relative to `vertices.first`).
    Indexed {
        vertices: VertexRange,
        indices: IndexRange,
    },
    /// Indexed triangles of a vertex stream (particles): its own vertex layout,
    /// 32-bit indices, draw count set every frame by [`RenderScene::write_stream`].
    Stream(StreamId),
    /// [`Geometry::Indexed`] plus per-vertex skinning attributes (puppets, spec 08 §9.8):
    /// shader inputs `a_BlendIndices`, `a_BlendWeights`, `a_Normal` and `a_Tangent4` read the
    /// static skin buffer, record `k` belonging to vertex `vertices.first + k`.
    Skinned {
        vertices: VertexRange,
        indices: IndexRange,
        skin: SkinRange,
    },
}

/// Records of the static skin buffer ([`SceneBuilder::add_skin`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkinRange {
    pub first: u32,
    pub count: u32,
}

/// Skin buffer layout of a shader input: `(name, byte offset in SkinVertex)`.
const SKIN_ATTRIBUTES: [(&str, u64); 4] = [
    ("a_BlendIndices", 0),
    ("a_BlendWeights", 16),
    ("a_Normal", 32),
    ("a_Tangent4", 48),
];

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

/// Handle of a vertex stream of a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u32);

/// One float attribute of a vertex stream, matched to shader inputs by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamAttribute {
    pub name: String,
    pub offset_floats: u32,
    /// 1 to 4 floats.
    pub components: u32,
}

/// Vertex layout of a stream (e.g. the 17-float sprite vertex of spec 05 §12.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamLayout {
    pub stride_floats: u32,
    pub attributes: Vec<StreamAttribute>,
}

/// Vertices rewritten every frame from the CPU (particles) with a constant
/// index buffer, both allocated once at their maximum size.
struct VertexStream {
    layout: StreamLayout,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    max_floats: usize,
    max_indices: u32,
    index_count: u32,
    _memory: crate::context::MemoryToken,
}

/// Render-ready description of a pass.
#[derive(Clone, Debug)]
pub struct PassDesc {
    pub label: String,
    pub shader: ShaderId,
    pub target: RtId,
    pub blend: BlendMode,
    pub write_mask: WriteMask,
    pub cull: CullMode,
    pub geometry: Geometry,
    /// Slots 0..8. A slot the shader samples without binding gets the default texture.
    pub slots: [Option<SlotBinding>; MAX_SLOTS],
    /// Alternatives for slot 0 (frames of an animated texture), selected per
    /// frame with [`RenderScene::set_variant`]. Empty = no variants.
    pub slot0_variants: Vec<TextureRef>,
}

impl PassDesc {
    /// A pass with default states and no bindings.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        shader: ShaderId,
        target: RtId,
        geometry: Geometry,
    ) -> Self {
        Self {
            label: label.into(),
            shader,
            target,
            blend: BlendMode::Normal,
            write_mask: WriteMask::All,
            cull: CullMode::None,
            geometry,
            slots: [None; MAX_SLOTS],
            slot0_variants: Vec::new(),
        }
    }

    /// Builder-style slot binding.
    #[must_use]
    pub fn with_slot(mut self, slot: usize, binding: SlotBinding) -> Self {
        if slot < MAX_SLOTS {
            self.slots[slot] = Some(binding);
        }
        self
    }
}

/// One frame operation, in order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    /// Clear a target (merged with the following draws into one render pass).
    Clear {
        target: RtId,
        color: [f64; 4],
    },
    /// Copy a target into another (`copy` effect command, spec 04 §9.4 alternative).
    Copy {
        src: RtId,
        dst: RtId,
    },
    Draw(PassId),
}

struct PassRecord {
    desc: PassDesc,
    uniform: Option<UniformBlock>,
}

/// Engine uniforms with a fixed identity default.
const IDENTITY_MATRICES: &[&str] = &[
    "g_ModelViewProjectionMatrix",
    "g_ModelViewProjectionMatrixInverse",
    "g_EffectModelViewProjectionMatrix",
    "g_ModelMatrix",
    "g_EffectModelMatrix",
    "g_ViewProjectionMatrix",
    "g_NormalModelMatrix",
    "g_EffectTextureProjectionMatrix",
    "g_EffectTextureProjectionMatrixInverse",
];

/// Engine uniforms whose default is all ones (image defaults, spec 04 §13.2).
const ONE_DEFAULTS: &[&str] = &[
    "g_TextureReductionScale",
    "g_Alpha",
    "g_UserAlpha",
    "g_Brightness",
    "g_Color",
    "g_Color4",
];

const RESOLUTION_NAMES: [&str; MAX_SLOTS] = [
    "g_Texture0Resolution",
    "g_Texture1Resolution",
    "g_Texture2Resolution",
    "g_Texture3Resolution",
    "g_Texture4Resolution",
    "g_Texture5Resolution",
    "g_Texture6Resolution",
    "g_Texture7Resolution",
];

/// Load-time builder of a [`RenderScene`].
pub struct SceneBuilder<'c> {
    ctx: &'c GpuContext,
    /// Render targets: create scene, image and effect targets here.
    pub targets: RenderTargetPool,
    textures: Vec<GpuTexture>,
    shaders: Vec<PassShader>,
    dynbuf: DynamicBufferBuilder,
    indices: Vec<u16>,
    skins: Vec<SkinVertex>,
    streams: Vec<VertexStream>,
    passes: Vec<PassRecord>,
    ops: Vec<Op>,
    scene_size: (f32, f32),
}

impl<'c> SceneBuilder<'c> {
    /// Allocate a vertex stream of at most `max_vertices` vertices drawn with
    /// the constant 32-bit `indices` (load time). Feed it every frame with
    /// [`RenderScene::write_stream`].
    pub fn add_stream(
        &mut self,
        label: &str,
        layout: StreamLayout,
        max_vertices: u32,
        indices: &[u32],
    ) -> StreamId {
        let device = self.ctx.device();
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
            self.ctx.queue().write_buffer(&index_buffer, 0, ibytes);
        }
        self.ctx.note_resources(2);
        self.streams.push(VertexStream {
            layout,
            vertices,
            indices: index_buffer,
            max_floats,
            max_indices: indices.len() as u32,
            index_count: 0,
            _memory: self.ctx.memory().track(vsize + isize),
        });
        StreamId(self.streams.len() as u32 - 1)
    }

    /// `scene_size` = projection size `(W, H)`, used for `g_TexelSize`.
    #[must_use]
    pub fn new(ctx: &'c GpuContext, scene_size: (f32, f32)) -> Self {
        let align = ctx.device().limits().min_uniform_buffer_offset_alignment;
        let mut dynbuf = DynamicBufferBuilder::new(align);
        let ts = [1.0 / scene_size.0, 1.0 / scene_size.1];
        let tsh = [0.5 / scene_size.0, 0.5 / scene_size.1];
        dynbuf
            .shadow_mut()
            .write_bytes(frame_globals::TEXEL_SIZE, bytemuck::cast_slice(&ts));
        dynbuf
            .shadow_mut()
            .write_bytes(frame_globals::TEXEL_SIZE_HALF, bytemuck::cast_slice(&tsh));
        Self {
            ctx,
            targets: RenderTargetPool::new(),
            textures: Vec::new(),
            shaders: Vec::new(),
            dynbuf,
            indices: Vec::new(),
            skins: Vec::new(),
            streams: Vec::new(),
            passes: Vec::new(),
            ops: Vec::new(),
            scene_size,
        }
    }

    #[must_use]
    pub fn context(&self) -> &'c GpuContext {
        self.ctx
    }

    #[must_use]
    pub fn scene_size(&self) -> (f32, f32) {
        self.scene_size
    }

    /// Compile a shader (reflection, clip fix, validation). An error means the
    /// object using it must be dropped (spec 03 §10.4); the scene goes on.
    pub fn add_shader(&mut self, desc: ShaderDesc) -> Result<ShaderId> {
        let s = crate::shader::compile(self.ctx, desc)?;
        self.shaders.push(s);
        Ok(ShaderId(self.shaders.len() as u32 - 1))
    }

    #[must_use]
    pub fn shader_interface(&self, id: ShaderId) -> Option<&ShaderInterface> {
        self.shaders.get(id.0 as usize).map(PassShader::interface)
    }

    /// Upload a texture; the caller may drop its CPU copy right after.
    pub fn add_texture(&mut self, upload: &TextureUpload<'_>) -> Result<TextureId> {
        let t = upload_texture(self.ctx, upload)?;
        self.textures.push(t);
        Ok(TextureId(self.textures.len() as u32 - 1))
    }

    #[must_use]
    pub fn texture(&self, id: TextureId) -> Option<&GpuTexture> {
        self.textures.get(id.0 as usize)
    }

    /// Vertices in the dynamic region (rewritable per frame).
    pub fn add_vertices(&mut self, v: &[Vertex]) -> VertexRange {
        self.dynbuf.alloc_vertices(v)
    }

    /// Static 16-bit indices.
    pub fn add_indices(&mut self, i: &[u16]) -> IndexRange {
        let first = self.indices.len() as u32;
        self.indices.extend_from_slice(i);
        if self.indices.len() % 2 == 1 {
            // Keep every range 4-byte aligned for buffer slicing.
            self.indices.push(0);
        }
        IndexRange {
            first,
            count: i.len() as u32,
        }
    }

    /// Static skinning records of a puppet mesh, one per vertex (see [`Geometry::Skinned`]).
    pub fn add_skin(&mut self, s: &[SkinVertex]) -> SkinRange {
        let first = self.skins.len() as u32;
        self.skins.extend_from_slice(s);
        SkinRange {
            first,
            count: s.len() as u32,
        }
    }

    fn resolution(&self, r: TextureRef) -> [f32; 4] {
        match r {
            TextureRef::Texture(t) => self
                .textures
                .get(t.0 as usize)
                .map_or([1.0; 4], GpuTexture::resolution),
            TextureRef::Target(t) => self
                .targets
                .try_get(t)
                .map_or([1.0; 4], |rt| rt.resolution()),
            TextureRef::Default => [1.0; 4],
        }
    }

    /// Add a pass: allocates its uniform block and writes engine defaults
    /// (identity matrices, ones, texel size, texture resolutions).
    pub fn add_pass(&mut self, desc: PassDesc) -> Result<PassId> {
        let shader = self
            .shaders
            .get(desc.shader.0 as usize)
            .ok_or(RenderError::BadHandle("shader"))?;
        if self.targets.try_get(desc.target).is_none() {
            return Err(RenderError::BadHandle("render target"));
        }
        if let Geometry::Stream(s) = desc.geometry
            && self.streams.get(s.0 as usize).is_none()
        {
            return Err(RenderError::BadHandle("vertex stream"));
        }
        let uniform = shader
            .interface()
            .uniform_size
            .map(|s| self.dynbuf.alloc_uniform(s));
        if let Some(block) = uniform {
            let fields = shader.interface().uniforms.clone();
            let ts = [1.0 / self.scene_size.0, 1.0 / self.scene_size.1];
            for f in &fields {
                let slot = f.slot(block.offset);
                let name = f.name.as_str();
                let sh = self.dynbuf.shadow_mut();
                if IDENTITY_MATRICES.contains(&name) {
                    sh.write_floats(&slot, &identity_for(&slot.ty));
                } else if ONE_DEFAULTS.contains(&name) {
                    sh.write_floats(&slot, &[1.0; 4]);
                } else if name == "g_TexelSize" {
                    sh.write_floats(&slot, &ts);
                } else if name == "g_TexelSizeHalf" {
                    sh.write_floats(&slot, &[ts[0] * 0.5, ts[1] * 0.5]);
                } else if let Some(n) = RESOLUTION_NAMES.iter().position(|r| *r == name) {
                    let r = match (n, desc.slot0_variants.first()) {
                        (0, Some(v)) => Some(*v),
                        _ => desc.slots[n].map(|b| b.texture),
                    };
                    if let Some(r) = r {
                        let res = self.resolution(r);
                        self.dynbuf.shadow_mut().write_floats(&slot, &res);
                    }
                }
            }
        }
        self.passes.push(PassRecord { desc, uniform });
        Ok(PassId(self.passes.len() as u32 - 1))
    }

    /// Write handle of a uniform of a pass (load time: resolve names once).
    #[must_use]
    pub fn uniform(&self, pass: PassId, name: &str) -> Option<UniformSlot> {
        let rec = self.passes.get(pass.0 as usize)?;
        let block = rec.uniform?;
        let shader = self.shaders.get(rec.desc.shader.0 as usize)?;
        shader.interface().field(name).map(|f| f.slot(block.offset))
    }

    /// Initial values (material constants, image values…).
    pub fn shadow_mut(&mut self) -> &mut Shadow {
        self.dynbuf.shadow_mut()
    }

    /// Append a frame operation.
    pub fn push(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// Compile everything.
    pub fn build(mut self) -> Result<RenderScene> {
        let ctx = self.ctx;
        let device = ctx.device();

        // Frame-global copies into pass blocks.
        let mut patches = Vec::new();
        for rec in &self.passes {
            let (Some(block), Some(shader)) =
                (rec.uniform, self.shaders.get(rec.desc.shader.0 as usize))
            else {
                continue;
            };
            for f in &shader.interface().uniforms {
                if frame_globals::FIELDS.iter().any(|g| g.0 == f.name) {
                    let slot = f.slot(block.offset);
                    if !patches_for(&f.name, &slot, &mut patches) {
                        tracing::warn!(target: "lumen_render", "{}: unsupported layout for {}", rec.desc.label, f.name);
                    }
                }
            }
        }
        // Texel size is static: apply its copies once now.
        let dynbuf = self.dynbuf.build(ctx);

        let index_buffer = (!self.indices.is_empty()).then(|| {
            let bytes: &[u8] = bytemuck::cast_slice(&self.indices);
            let size = (bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lumen.indices"),
                size,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            ctx.queue().write_buffer(&b, 0, bytes);
            ctx.note_resources(1);
            b
        });
        let skin_buffer = (!self.skins.is_empty()).then(|| {
            let bytes: &[u8] = bytemuck::cast_slice(&self.skins);
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lumen.skin"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            ctx.queue().write_buffer(&b, 0, bytes);
            ctx.note_resources(1);
            b
        });
        // (0, 0, 0, 1) as floats, then as integers, for unfed attributes.
        let mut defaults = [0u8; 32];
        defaults[12..16].copy_from_slice(&1.0f32.to_le_bytes());
        defaults[28..32].copy_from_slice(&1i32.to_le_bytes());
        let default_attrs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumen.default-attributes"),
            size: 32,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue().write_buffer(&default_attrs, 0, &defaults);
        ctx.note_resources(1);

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumen.uniforms.bgl"),
            entries: &[uniform_entry(0, true), uniform_entry(1, false)],
        });
        let uniform_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lumen.uniforms.bg"),
            layout: &uniform_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: dynbuf.buffer(),
                        offset: 0,
                        size: wgpu::BufferSize::new(u64::from(dynbuf.binding_size())),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: dynbuf.buffer(),
                        offset: 0,
                        size: wgpu::BufferSize::new(u64::from(frame_globals::SIZE)),
                    }),
                },
            ],
        });
        ctx.note_resources(2);

        let mut scene = RenderScene {
            default_texture: default_texture(ctx),
            targets: std::mem::take(&mut self.targets),
            textures: std::mem::take(&mut self.textures),
            interfaces: self.shaders.iter().map(|s| s.interface().clone()).collect(),
            dynbuf,
            index_buffer,
            skin_buffer,
            streams: std::mem::take(&mut self.streams),
            default_attrs,
            uniform_layout,
            uniform_bg,
            texture_layouts: HashMap::new(),
            passes: Vec::with_capacity(self.passes.len()),
            ops: std::mem::take(&mut self.ops),
            segments: Vec::new(),
            bundles: Vec::new(),
            patches,
            dirty: true,
            settle_left: 0,
            settle_frames: 0,
            feedback: false,
            accumulates: false,
            last_encode: Duration::ZERO,
        };

        let mut pipelines: HashMap<PipelineKey, wgpu::RenderPipeline> = HashMap::new();
        for rec in &self.passes {
            let shader = &self.shaders[rec.desc.shader.0 as usize];
            let pass = scene.compile_pass(ctx, shader, rec, &mut pipelines)?;
            scene.passes.push(pass);
        }
        scene.analyse_ops();
        scene.rebuild_bindings(ctx)?;
        Ok(scene)
    }
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

fn identity_for(ty: &UniformType) -> Vec<f32> {
    match ty {
        UniformType::Matrix { columns, rows } => {
            let (c, r) = (usize::from(*columns), usize::from(*rows));
            (0..c * r)
                .map(|i| if i / r == i % r { 1.0 } else { 0.0 })
                .collect()
        }
        _ => Vec::new(),
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
}

struct CompiledPass {
    label: String,
    shader: ShaderId,
    desc: PassDesc,
    uniform: Option<UniformBlock>,
    pipeline: wgpu::RenderPipeline,
    /// One per variant (at least one).
    bind_groups: Vec<wgpu::BindGroup>,
    variant: usize,
    /// Vertex buffer slots (≥ 1) fed with defaults: `(slot, integer?)`.
    unfed: Vec<(u32, bool)>,
    /// Scratch copy of the target when the pass samples its own target.
    hazard_scratch: Option<RtId>,
    res0: Option<UniformSlot>,
    enabled: bool,
}

enum Item {
    Bundle(usize),
    Pass(usize),
}

enum Segment {
    Copy {
        src: RtId,
        dst: RtId,
    },
    Render {
        target: RtId,
        clear: Option<wgpu::Color>,
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

/// A compiled scene graph ready to render frames.
pub struct RenderScene {
    targets: RenderTargetPool,
    textures: Vec<GpuTexture>,
    default_texture: GpuTexture,
    interfaces: Vec<ShaderInterface>,
    dynbuf: DynamicBuffer,
    index_buffer: Option<wgpu::Buffer>,
    skin_buffer: Option<wgpu::Buffer>,
    streams: Vec<VertexStream>,
    default_attrs: wgpu::Buffer,
    uniform_layout: wgpu::BindGroupLayout,
    uniform_bg: wgpu::BindGroup,
    texture_layouts: HashMap<u8, (wgpu::BindGroupLayout, wgpu::PipelineLayout)>,
    passes: Vec<CompiledPass>,
    ops: Vec<Op>,
    segments: Vec<Segment>,
    bundles: Vec<wgpu::RenderBundle>,
    patches: Vec<Patch>,
    dirty: bool,
    settle_left: u32,
    settle_frames: u32,
    feedback: bool,
    accumulates: bool,
    last_encode: Duration,
}

/// Default number of extra frames rendered after the last change when the
/// graph accumulates across frames (translucent/additive passes into targets
/// that are never cleared, spec 04 §15): enough for 8-bit convergence.
pub const ACCUMULATION_SETTLE_FRAMES: u32 = 120;

impl RenderScene {
    fn texture_layout(
        &mut self,
        ctx: &GpuContext,
        slots: u8,
    ) -> &(wgpu::BindGroupLayout, wgpu::PipelineLayout) {
        let uniform_layout = &self.uniform_layout;
        self.texture_layouts.entry(slots).or_insert_with(|| {
            let mut entries = Vec::new();
            for n in 0..MAX_SLOTS as u32 {
                if slots & (1 << n) == 0 {
                    continue;
                }
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding: 2 * n,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                });
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding: 2 * n + 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                });
            }
            let bgl = ctx
                .device()
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("lumen.textures.bgl"),
                    entries: &entries,
                });
            let pl = ctx
                .device()
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("lumen.pass.layout"),
                    bind_group_layouts: &[Some(uniform_layout), Some(&bgl)],
                    immediate_size: 0,
                });
            ctx.note_resources(2);
            (bgl, pl)
        })
    }

    fn compile_pass(
        &mut self,
        ctx: &GpuContext,
        shader: &PassShader,
        rec: &PassRecord,
        cache: &mut HashMap<PipelineKey, wgpu::RenderPipeline>,
    ) -> Result<CompiledPass> {
        let desc = &rec.desc;
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
                self.streams
                    .get(s.0 as usize)
                    .ok_or_else(|| err("unknown vertex stream".into()))?,
            ),
            _ => None,
        };
        let mut main_attrs = Vec::new();
        let mut skin_attrs = Vec::new();
        let mut unfed_attrs = Vec::new();
        for input in &iface.vertex_inputs {
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
                    continue;
                }
                let (components, kind) = match input.attr {
                    VertexAttr::Position => (3, ScalarKind::Float),
                    VertexAttr::TexCoord => (2, ScalarKind::Float),
                    VertexAttr::Unfed { components, kind } => (components, kind),
                };
                unfed_attrs.push((
                    [wgpu::VertexAttribute {
                        format: unfed_format(kind, components),
                        offset: 0,
                        shader_location: input.location,
                    }],
                    kind != ScalarKind::Float,
                ));
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
                VertexAttr::Unfed { components, kind } => {
                    unfed_attrs.push((
                        [wgpu::VertexAttribute {
                            format: unfed_format(kind, components),
                            offset: 0,
                            shader_location: input.location,
                        }],
                        kind != ScalarKind::Float,
                    ));
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
        let stride = stream.map_or(u64::from(VERTEX_SIZE), |s| {
            u64::from(s.layout.stride_floats) * 4
        });
        // Pipelines of streams with the same layout are shared.
        let layout_key =
            stream.and_then(|s| self.streams.iter().position(|o| o.layout == s.layout));
        let mut layouts = vec![Some(wgpu::VertexBufferLayout {
            array_stride: stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &main_attrs,
        })];
        if skinned {
            layouts.push(Some(wgpu::VertexBufferLayout {
                array_stride: u64::from(SKIN_VERTEX_SIZE),
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &skin_attrs,
            }));
        }
        for (a, _) in &unfed_attrs {
            // Instance step with a single instance: every vertex reads element 0.
            layouts.push(Some(wgpu::VertexBufferLayout {
                array_stride: 16,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: a,
            }));
        }
        let unfed = unfed_attrs
            .iter()
            .enumerate()
            .map(|(i, (_, int))| (i as u32 + first_unfed, *int))
            .collect();

        let key = PipelineKey {
            shader: desc.shader,
            blend: desc.blend,
            write_mask: desc.write_mask,
            cull: desc.cull,
            stream_layout: layout_key,
            skinned,
        };
        let pipeline = if let Some(p) = cache.get(&key) {
            p.clone()
        } else {
            let unclipped = ctx.caps().unclipped_depth;
            let (_, pl) = self.texture_layout(ctx, iface.slots);
            let pl = pl.clone();
            let scope = ctx.device().push_error_scope(wgpu::ErrorFilter::Validation);
            let p = ctx
                .device()
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(&desc.label),
                    layout: Some(&pl),
                    vertex: wgpu::VertexState {
                        module: &shader.module,
                        entry_point: Some(&shader.vertex_entry),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &layouts,
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        strip_index_format: None,
                        front_face: FRONT_FACE,
                        cull_mode: desc.cull.face(),
                        // No depth buffer exists: never discard geometry for its depth (spec 04 Q4).
                        unclipped_depth: unclipped,
                        polygon_mode: wgpu::PolygonMode::Fill,
                        conservative: false,
                    },
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader.module,
                        entry_point: Some(&shader.fragment_entry),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: RT_FORMAT,
                            blend: desc.blend.state(),
                            write_mask: desc.write_mask.writes(),
                        })],
                    }),
                    multiview_mask: None,
                    cache: ctx.pipeline_cache(),
                });
            if let Some(e) = pollster::block_on(scope.pop()) {
                return Err(RenderError::Validation {
                    label: desc.label.clone(),
                    message: e.to_string(),
                });
            }
            ctx.note_resources(1);
            cache.insert(key, p.clone());
            p
        };

        let res0 = rec
            .uniform
            .and_then(|b| iface.field(RESOLUTION_NAMES[0]).map(|f| f.slot(b.offset)));
        Ok(CompiledPass {
            label: desc.label.clone(),
            shader: desc.shader,
            desc: desc.clone(),
            uniform: rec.uniform,
            pipeline,
            bind_groups: Vec::new(),
            variant: 0,
            unfed,
            hazard_scratch: None,
            res0,
            enabled: true,
        })
    }

    /// Detect cross-frame dependencies (feedback, accumulation).
    fn analyse_ops(&mut self) {
        let mut cleared = Vec::new();
        let mut written_so_far = Vec::new();
        let mut written_total = Vec::new();
        for op in &self.ops {
            match op {
                Op::Clear { target, .. } => cleared.push(*target),
                Op::Copy { dst, .. } => written_total.push(*dst),
                Op::Draw(p) => {
                    if let Some(pass) = self.passes.get(p.0 as usize) {
                        written_total.push(pass.desc.target);
                    }
                }
            }
        }
        for op in &self.ops {
            let (reads, writes): (Vec<RtId>, Option<RtId>) = match op {
                Op::Clear { target, .. } => (Vec::new(), Some(*target)),
                Op::Copy { src, dst } => (vec![*src], Some(*dst)),
                Op::Draw(p) => {
                    let Some(pass) = self.passes.get(p.0 as usize) else {
                        continue;
                    };
                    if pass.desc.blend.reads_destination() && !cleared.contains(&pass.desc.target) {
                        self.accumulates = true;
                    }
                    let reads = pass
                        .desc
                        .slots
                        .iter()
                        .flatten()
                        .map(|b| b.texture)
                        .chain(pass.desc.slot0_variants.iter().copied())
                        .filter_map(|t| match t {
                            TextureRef::Target(r) => Some(r),
                            _ => None,
                        })
                        .collect();
                    (reads, Some(pass.desc.target))
                }
            };
            for r in reads {
                if !written_so_far.contains(&r)
                    && written_total.contains(&r)
                    && !cleared.contains(&r)
                {
                    self.feedback = true;
                }
            }
            if let Some(w) = writes {
                written_so_far.push(w);
            }
        }
        self.settle_frames = if self.accumulates {
            ACCUMULATION_SETTLE_FRAMES
        } else if self.feedback {
            2
        } else {
            0
        };
    }

    fn view_and_sampler(
        &self,
        ctx: &GpuContext,
        b: Option<SlotBinding>,
        hazard: Option<(RtId, RtId)>,
    ) -> (&wgpu::TextureView, wgpu::Sampler) {
        let b = b.unwrap_or(SlotBinding {
            texture: TextureRef::Default,
            sampler: None,
        });
        let (view, key) = match b.texture {
            TextureRef::Texture(t) => match self.textures.get(t.0 as usize) {
                Some(tex) => (tex.view(), tex.sampler_key()),
                None => (
                    self.default_texture.view(),
                    self.default_texture.sampler_key(),
                ),
            },
            TextureRef::Target(r) => {
                let key = self.targets.get(r).sampler_key();
                let r = match hazard {
                    Some((target, scratch)) if target == r => scratch,
                    _ => r,
                };
                (self.targets.get(r).view(), key)
            }
            TextureRef::Default => (
                self.default_texture.view(),
                self.default_texture.sampler_key(),
            ),
        };
        (view, ctx.sampler(b.sampler.unwrap_or(key)))
    }

    /// Rebuild bind groups, hazard copies, segments and bundles. Call after
    /// any target reallocation or sampler change (event-driven, not per frame).
    pub fn rebuild_bindings(&mut self, ctx: &GpuContext) -> Result<()> {
        // Hazards: a pass sampling its own destination reads a copy (spec 04 §24.6).
        for i in 0..self.passes.len() {
            let desc = &self.passes[i].desc;
            let target = desc.target;
            let samples_target = desc
                .slots
                .iter()
                .flatten()
                .map(|b| b.texture)
                .chain(desc.slot0_variants.iter().copied())
                .any(|t| t == TextureRef::Target(target));
            let scratch = if samples_target {
                let size = self.targets.get(target).size();
                Some(self.targets.scratch(ctx, size))
            } else {
                None
            };
            self.passes[i].hazard_scratch = scratch;
        }

        for i in 0..self.passes.len() {
            let slots_mask = self.interfaces[self.passes[i].shader.0 as usize].slots;
            self.texture_layout(ctx, slots_mask);
            let pass = &self.passes[i];
            let hazard = pass.hazard_scratch.map(|s| (pass.desc.target, s));
            let variants: Vec<Option<TextureRef>> = if pass.desc.slot0_variants.is_empty() {
                vec![None]
            } else {
                pass.desc.slot0_variants.iter().copied().map(Some).collect()
            };
            let (bgl, _) = &self.texture_layouts[&slots_mask];
            let mut groups = Vec::with_capacity(variants.len());
            for v in variants {
                let mut views = Vec::new();
                for n in 0..MAX_SLOTS {
                    if slots_mask & (1 << n) == 0 {
                        continue;
                    }
                    let mut b = pass.desc.slots[n];
                    if n == 0
                        && let Some(t) = v
                    {
                        b = Some(SlotBinding {
                            texture: t,
                            sampler: b.and_then(|b| b.sampler),
                        });
                    }
                    views.push((n as u32, self.view_and_sampler(ctx, b, hazard)));
                }
                let mut entries = Vec::with_capacity(views.len() * 2);
                for (n, (view, sampler)) in &views {
                    entries.push(wgpu::BindGroupEntry {
                        binding: 2 * n,
                        resource: wgpu::BindingResource::TextureView(view),
                    });
                    entries.push(wgpu::BindGroupEntry {
                        binding: 2 * n + 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    });
                }
                groups.push(ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(&pass.label),
                    layout: bgl,
                    entries: &entries,
                }));
                ctx.note_resources(1);
            }
            self.passes[i].bind_groups = groups;
            self.passes[i].variant = self.passes[i]
                .variant
                .min(self.passes[i].bind_groups.len() - 1);
        }

        self.build_segments();
        self.build_bundles(ctx);
        self.dirty = true;
        Ok(())
    }

    fn build_segments(&mut self) {
        let mut segments = Vec::new();
        let mut cur: Option<(RtId, Option<wgpu::Color>, Vec<usize>)> = None;
        let flush = |cur: &mut Option<(RtId, Option<wgpu::Color>, Vec<usize>)>,
                     segments: &mut Vec<Segment>| {
            if let Some((target, clear, passes)) = cur.take() {
                segments.push(Segment::Render {
                    target,
                    clear,
                    items: passes.into_iter().map(Item::Pass).collect(),
                });
            }
        };
        for op in &self.ops {
            match *op {
                Op::Clear { target, color } => {
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
                Op::Copy { src, dst } => {
                    flush(&mut cur, &mut segments);
                    segments.push(Segment::Copy { src, dst });
                }
                Op::Draw(p) => {
                    let Some(pass) = self.passes.get(p.0 as usize).filter(|p| p.enabled) else {
                        continue;
                    };
                    let target = pass.desc.target;
                    if let Some(scratch) = pass.hazard_scratch {
                        flush(&mut cur, &mut segments);
                        segments.push(Segment::Copy {
                            src: target,
                            dst: scratch,
                        });
                    }
                    if cur.as_ref().is_none_or(|c| c.0 != target) {
                        flush(&mut cur, &mut segments);
                        cur = Some((target, None, Vec::new()));
                    }
                    if let Some(c) = cur.as_mut() {
                        c.2.push(p.0 as usize);
                    }
                }
            }
        }
        flush(&mut cur, &mut segments);
        self.segments = segments;
    }

    fn record<'a, R: Recorder<'a>>(&'a self, r: &mut R, index: usize) {
        let pass = &self.passes[index];
        let stream = match pass.desc.geometry {
            Geometry::Stream(s) => match self.streams.get(s.0 as usize) {
                Some(s) if s.index_count > 0 => Some(s),
                _ => return,
            },
            _ => None,
        };
        r.pipeline(&pass.pipeline);
        r.bind_group(0, &self.uniform_bg, &[pass.uniform.map_or(0, |u| u.offset)]);
        r.bind_group(1, &pass.bind_groups[pass.variant], &[]);
        if let Some(s) = stream {
            r.vertex_buffer(0, s.vertices.slice(..));
        } else if let Geometry::Skinned { vertices, skin, .. } = pass.desc.geometry {
            // Both buffers start at the mesh: its indices are relative to its first vertex.
            let start = self.dynbuf.vertex_offset() + vertices.first * VERTEX_SIZE;
            r.vertex_buffer(0, self.dynbuf.buffer().slice(u64::from(start)..));
            if let Some(b) = &self.skin_buffer {
                r.vertex_buffer(1, b.slice(u64::from(skin.first * SKIN_VERTEX_SIZE)..));
            }
        } else {
            r.vertex_buffer(
                0,
                self.dynbuf
                    .buffer()
                    .slice(u64::from(self.dynbuf.vertex_offset())..),
            );
        }
        for &(slot, int) in &pass.unfed {
            let off = if int { 16 } else { 0 };
            r.vertex_buffer(slot, self.default_attrs.slice(off..off + 16));
        }
        match pass.desc.geometry {
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
                if let Some(s) = stream {
                    r.index_buffer(s.indices.slice(..), wgpu::IndexFormat::Uint32);
                    r.draw_indexed(0..s.index_count, 0);
                }
            }
        }
    }

    /// Group runs of frozen passes (a single bind group) into render bundles.
    fn build_bundles(&mut self, ctx: &GpuContext) {
        let mut bundles = Vec::new();
        let mut new_segments = Vec::with_capacity(self.segments.len());
        let segments = std::mem::take(&mut self.segments);
        for seg in segments {
            match seg {
                Segment::Render {
                    target,
                    clear,
                    items,
                } => {
                    let mut out = Vec::new();
                    let mut run: Vec<usize> = Vec::new();
                    let close_run =
                        |run: &mut Vec<usize>,
                         out: &mut Vec<Item>,
                         bundles: &mut Vec<wgpu::RenderBundle>| {
                            if run.is_empty() {
                                return;
                            }
                            let mut enc = ctx.device().create_render_bundle_encoder(
                                &wgpu::RenderBundleEncoderDescriptor {
                                    label: Some("lumen.bundle"),
                                    color_formats: &[Some(RT_FORMAT)],
                                    depth_stencil: None,
                                    sample_count: 1,
                                    multiview: None,
                                },
                            );
                            for &p in run.iter() {
                                self.record(&mut enc, p);
                            }
                            bundles.push(enc.finish(&wgpu::RenderBundleDescriptor {
                                label: Some("lumen.bundle"),
                            }));
                            ctx.note_resources(1);
                            out.push(Item::Bundle(bundles.len() - 1));
                            run.clear();
                        };
                    for item in items {
                        let Item::Pass(p) = item else {
                            continue;
                        };
                        // Stream passes change their draw count every frame: never bundled.
                        if self.passes[p].bind_groups.len() == 1
                            && !matches!(self.passes[p].desc.geometry, Geometry::Stream(_))
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
                        items: out,
                    });
                }
                other => new_segments.push(other),
            }
        }
        self.segments = new_segments;
        self.bundles = bundles;
    }

    /// Feed the per-frame engine values (frame globals + pass copies).
    /// No allocation; marks the scene dirty only if a value changed.
    pub fn update_frame(&mut self, inputs: &FrameInputs<'_>) {
        self.dynbuf.update_frame(inputs, &self.patches);
    }

    /// Uniform writes by slot (matrices, colours, properties).
    pub fn shadow_mut(&mut self) -> &mut Shadow {
        self.dynbuf.shadow_mut()
    }

    /// Rewrite the vertices of a range (image rectangle moved).
    pub fn set_vertices(&mut self, range: VertexRange, v: &[Vertex]) {
        self.dynbuf.set_vertices(range, v);
    }

    /// Upload this frame's vertices of a stream and set how many of its
    /// indices to draw (clamped to the buffer capacities). One
    /// `write_buffer`, no allocation; an empty stream draws nothing.
    pub fn write_stream(
        &mut self,
        ctx: &GpuContext,
        id: StreamId,
        vertices: &[f32],
        index_count: u32,
    ) {
        let Some(s) = self.streams.get_mut(id.0 as usize) else {
            return;
        };
        let floats = vertices.len().min(s.max_floats);
        let count = index_count.min(s.max_indices);
        if count == 0 && s.index_count == 0 {
            return;
        }
        if floats > 0 && count > 0 {
            ctx.queue()
                .write_buffer(&s.vertices, 0, bytemuck::cast_slice(&vertices[..floats]));
        }
        s.index_count = count;
        self.dirty = true;
    }

    /// Select the slot-0 variant of a pass (frame of an animated texture).
    /// Also updates `g_Texture0Resolution` (spec 04 §12.5).
    pub fn set_variant(&mut self, pass: PassId, variant: usize) {
        let Some(p) = self.passes.get_mut(pass.0 as usize) else {
            return;
        };
        let v = variant.min(p.bind_groups.len().saturating_sub(1));
        if p.variant == v {
            return;
        }
        p.variant = v;
        self.dirty = true;
        if let (Some(slot), Some(r)) = (p.res0.clone(), p.desc.slot0_variants.get(v).copied()) {
            let res = match r {
                TextureRef::Texture(t) => self
                    .textures
                    .get(t.0 as usize)
                    .map_or([1.0; 4], GpuTexture::resolution),
                TextureRef::Target(t) => self.targets.get(t).resolution(),
                TextureRef::Default => [1.0; 4],
            };
            self.dynbuf.shadow_mut().write_floats(&slot, &res);
        }
    }

    /// Enable or disable passes (image visibility, spec 04 Q8). Disabled
    /// passes are not recorded. Rebuilds the render-pass grouping and the
    /// bundles when something changed: call it on visibility events, not
    /// every frame.
    pub fn set_enabled(&mut self, ctx: &GpuContext, passes: &[PassId], enabled: bool) {
        let mut changed = false;
        for p in passes {
            if let Some(cp) = self.passes.get_mut(p.0 as usize)
                && cp.enabled != enabled
            {
                cp.enabled = enabled;
                changed = true;
            }
        }
        if changed {
            self.build_segments();
            self.build_bundles(ctx);
            self.dirty = true;
        }
    }

    /// Something outside the uniform buffer changed (video frame written into
    /// a texture, thumbnail loaded…).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Whether the next frame would differ from the last one. When `false`,
    /// the caller can skip rendering and presenting entirely.
    #[must_use]
    pub fn needs_redraw(&self) -> bool {
        self.dirty || self.dynbuf.shadow().is_dirty() || self.settle_left > 0
    }

    /// Override the number of frames rendered after the last change.
    pub fn set_settle_frames(&mut self, frames: u32) {
        self.settle_frames = frames;
    }

    /// Whether some pass accumulates across frames (spec 04 §15).
    #[must_use]
    pub fn accumulates(&self) -> bool {
        self.accumulates
    }

    /// Upload the dirty uniforms (one `write_buffer`) and record the frame.
    pub fn encode(&mut self, ctx: &GpuContext, encoder: &mut wgpu::CommandEncoder) {
        let t0 = Instant::now();
        let changed = self.dynbuf.flush(ctx.queue()) || self.dirty;
        self.dirty = false;
        if changed {
            self.settle_left = self.settle_frames;
        } else {
            self.settle_left = self.settle_left.saturating_sub(1);
        }
        for seg in &self.segments {
            match seg {
                Segment::Copy { src, dst } => {
                    let (s, d) = (self.targets.get(*src), self.targets.get(*dst));
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
                }
                Segment::Render {
                    target,
                    clear,
                    items,
                } => {
                    let rt = self.targets.get(*target);
                    let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: rt.view(),
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    for item in items {
                        match item {
                            Item::Bundle(b) => {
                                rp.execute_bundles(std::iter::once(&self.bundles[*b]))
                            }
                            Item::Pass(p) => self.record(&mut rp, *p),
                        }
                    }
                }
            }
        }
        self.last_encode = t0.elapsed();
    }

    /// Encode and submit in one command buffer (convenience).
    pub fn render(&mut self, ctx: &GpuContext) -> wgpu::SubmissionIndex {
        let mut encoder = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumen.frame"),
            });
        self.encode(ctx, &mut encoder);
        ctx.queue().submit(std::iter::once(encoder.finish()))
    }

    /// CPU time of the last [`Self::encode`].
    #[must_use]
    pub fn last_encode_time(&self) -> Duration {
        self.last_encode
    }

    #[must_use]
    pub fn targets(&self) -> &RenderTargetPool {
        &self.targets
    }

    /// Mutable access for resizes and sampler changes; call
    /// [`Self::rebuild_bindings`] afterwards.
    pub fn targets_mut(&mut self) -> &mut RenderTargetPool {
        &mut self.targets
    }

    #[must_use]
    pub fn texture(&self, id: TextureId) -> Option<&GpuTexture> {
        self.textures.get(id.0 as usize)
    }

    /// Write handle of a uniform of a pass.
    #[must_use]
    pub fn uniform(&self, pass: PassId, name: &str) -> Option<UniformSlot> {
        let p = self.passes.get(pass.0 as usize)?;
        let block = p.uniform?;
        self.interfaces[p.shader.0 as usize]
            .field(name)
            .map(|f| f.slot(block.offset))
    }

    /// Number of passes, render passes and bundles (debug counters).
    #[must_use]
    pub fn counts(&self) -> GraphCounts {
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
}

/// Debug counters of a compiled graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphCounts {
    pub passes: usize,
    pub render_passes: usize,
    pub copies: usize,
    pub bundles: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_layouts() {
        assert_eq!(
            identity_for(&UniformType::Matrix {
                columns: 3,
                rows: 3
            }),
            vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(
            identity_for(&UniformType::Matrix {
                columns: 4,
                rows: 4
            })[15],
            1.0
        );
        assert!(identity_for(&UniformType::Scalar(ScalarKind::Float)).is_empty());
    }
}

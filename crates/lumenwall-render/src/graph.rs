//! Generic pass graph: a pass = pipeline (shader + vertex inputs + blend +
//! write mask + culling) + texture/sampler bindings + a uniform block.
//!
//! Everything is created once by [`SceneBuilder::build`]: pipelines, bind
//! groups, hazard copies, render-pass grouping and render bundles for frozen
//! passes. A frame then performs at most one `write_buffer` and records the
//! precomputed segments, with no allocation and no GPU object creation.
//!
//! On a Vulkan device the frame is recorded directly in Vulkan ([`crate::hal`]): the same
//! segments, pipelines and bind groups created through wgpu-hal, replayed without wgpu-core's
//! per-pass validation and tracking. Direct3D 12, `LUMENWALL_DIRECT_VULKAN=0`,
//! [`SceneBuilder::set_recording`] and anything wgpu-hal refuses keep the wgpu path.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::blend::{BlendMode, CullMode, FRONT_FACE, WriteMask};
use crate::context::GpuContext;
use crate::context::MemoryToken;
use crate::dynbuf::{
    DynamicBuffer, DynamicBufferBuilder, FrameInputs, Patch, ScalarKind, Shadow, UniformBlock,
    UniformSlot, UniformType, VertexRange, frame_globals, patches_for,
};
use crate::error::{RenderError, Result};
use crate::hal::{
    DirectGraph, DrawCall, DrawStep, FrameState, PipelineSpec, Recording, SlotView, Step,
    VertexBuffer, WgpuReason,
};
use crate::math::{SKIN_VERTEX_SIZE, SkinVertex, VERTEX_SIZE, Vertex};
use crate::shader::{MAX_SLOTS, PassShader, ShaderDesc, ShaderInterface, VertexAttr};
use crate::targets::{
    ALIAS_AREA_SLACK, Access, AliasPlan, AliasSize, RT_FORMAT, RenderTargetPool, RtId,
    plan_aliasing,
};
use crate::texture::{
    GpuTexture, SamplerKey, TextureUpload, VolumeUpload, default_texture, default_volume,
    upload_texture, upload_volume,
};

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
    /// Every triangle of a static mesh ([`SceneBuilder::add_mesh`]): its own interleaved
    /// vertex layout, 16-bit indices.
    Mesh(MeshId),
}

/// Format of the depth buffers (spec 09 §3.1).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Depth test and write of a pass whose target has a depth buffer ([`SceneBuilder::add_depth`],
/// spec 09 §3.1). Ignored for targets without one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DepthState {
    /// Test "less or equal" against the buffer; `false` = always passes.
    pub test: bool,
    /// Write the fragment depth.
    pub write: bool,
}

/// Handle of a static mesh of a scene ([`SceneBuilder::add_mesh`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeshId(u32);

/// One attribute of a static mesh, matched to shader inputs by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshAttribute {
    pub name: String,
    pub format: wgpu::VertexFormat,
    /// Byte offset in the vertex.
    pub offset: u32,
}

/// Interleaved vertex layout of a static mesh (3D model meshes, spec 09 §1.14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshLayout {
    /// Bytes per vertex.
    pub stride: u32,
    pub attributes: Vec<MeshAttribute>,
}

/// Vertex and 16-bit index buffers of a static mesh, uploaded once.
struct StaticMesh {
    layout: MeshLayout,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
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

fn create_depth(ctx: &GpuContext, target: RtId, size: (u32, u32)) -> DepthBuffer {
    let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("lumenwall.depth"),
        size: wgpu::Extent3d {
            width: size.0.max(1),
            height: size.1.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
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
    /// Vertices written since the last encode, uploaded with the frame (`pending` floats).
    cpu: Vec<f32>,
    pending: usize,
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
    /// Winding of front faces in wgpu clip space: [`FRONT_FACE`] for passes in the upstream
    /// convention, `Ccw` for 3D passes in the Wallpaper Engine convention (spec 09 §3.5.1),
    /// `Cw` for their mirrored reflection (§3.7).
    pub front_face: wgpu::FrontFace,
    /// Depth test and write when the target has a depth buffer.
    pub depth: DepthState,
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
            front_face: FRONT_FACE,
            depth: DepthState::default(),
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

/// Load-time builder of a [`RenderScene`], also used to add passes to a built scene
/// ([`RenderScene::extend`]).
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
    meshes: Vec<StaticMesh>,
    depth_targets: Vec<RtId>,
    passes: Vec<PassRecord>,
    ops: Vec<Op>,
    scene_size: (f32, f32),
    /// The built scene being extended: uniform blocks and vertices go to its dynamic buffer.
    live: Option<&'c mut RenderScene>,
    /// Id of the first pass added by this builder (passes of the extended scene come first).
    pass_base: u32,
    /// `Ok` while the scene may record directly in Vulkan (then the final naga module of each
    /// shader is kept for wgpu-hal).
    direct: std::result::Result<(), WgpuReason>,
    /// `(shader id, final module, validation info)` of the shaders added, for wgpu-hal.
    direct_modules: Vec<(usize, naga::Module, naga::valid::ModuleInfo)>,
}

/// Passes and frame operations added by [`RenderScene::extend`]. The operations are not
/// placed in the frame: the caller inserts them where the layer belongs
/// ([`RenderScene::set_ops`]).
#[derive(Clone, Debug, Default)]
pub struct Extension {
    pub passes: Vec<PassId>,
    pub ops: Vec<Op>,
}

impl<'c> SceneBuilder<'c> {
    /// Uploads a static mesh (load time): interleaved `vertices` laid out as `layout`, 16-bit
    /// triangle-list `indices`. Shader inputs are fed by attribute name; others read
    /// (0, 0, 0, 1). Drawn whole by [`Geometry::Mesh`].
    pub fn add_mesh(
        &mut self,
        label: &str,
        layout: MeshLayout,
        vertices: &[u8],
        indices: &[u16],
    ) -> MeshId {
        let device = self.ctx.device();
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
            self.ctx.queue().write_buffer(&vbuf, 0, &vertices[..len]);
            if len < vertices.len() {
                let mut tail = [0u8; 4];
                tail[..vertices.len() - len].copy_from_slice(&vertices[len..]);
                self.ctx.queue().write_buffer(&vbuf, len as u64, &tail);
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
            self.ctx.queue().write_buffer(&ibuf, 0, ibytes);
        }
        self.ctx.note_resources(2);
        self.meshes.push(StaticMesh {
            layout,
            vertices: vbuf,
            indices: ibuf,
            index_count: indices.len() as u32,
            _memory: self.ctx.memory().track(vsize + isize),
        });
        MeshId(self.meshes.len() as u32 - 1)
    }

    /// Gives `target` a depth buffer of its size (spec 09 §3.1), shared by every pass drawn
    /// into it during a frame. It is cleared to 1 by the first render pass of the frame on
    /// that target; passes into it test and write depth according to [`PassDesc::depth`].
    /// Targets without one keep pipelines without depth (2D scenes are unchanged).
    pub fn add_depth(&mut self, target: RtId) {
        if !self.depth_targets.contains(&target) {
            self.depth_targets.push(target);
        }
    }

    /// Whether `target` has a depth buffer.
    #[must_use]
    pub fn has_depth(&self, target: RtId) -> bool {
        self.depth_targets.contains(&target)
    }

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
            cpu: vec![0.0; max_floats],
            pending: 0,
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
            // Allocated by `build`, so that transient targets can share memory.
            targets: RenderTargetPool::deferred(),
            textures: Vec::new(),
            shaders: Vec::new(),
            dynbuf,
            indices: Vec::new(),
            skins: Vec::new(),
            streams: Vec::new(),
            meshes: Vec::new(),
            depth_targets: Vec::new(),
            passes: Vec::new(),
            ops: Vec::new(),
            scene_size,
            live: None,
            pass_base: 0,
            direct: crate::hal::choose(Recording::from_env(), ctx.adapter().get_info().backend),
            direct_modules: Vec::new(),
        }
    }

    /// How the built scene records its frames ([`Recording::Auto`] by default, unless
    /// `LUMENWALL_DIRECT_VULKAN=0`). Call before adding shaders: [`Recording::Auto`] set later
    /// cannot make shaders already added usable by the direct path.
    pub fn set_recording(&mut self, recording: Recording) {
        self.direct = crate::hal::choose(recording, self.ctx.adapter().get_info().backend)
            .and(self.direct);
        if self.direct.is_err() {
            self.direct_modules.clear();
        }
    }

    /// Frame operations appended so far ([`Self::push`] and the image helpers).
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// The uniform shadow passes write into: the extended scene's when extending.
    fn dyn_shadow(&mut self) -> &mut Shadow {
        match &mut self.live {
            Some(scene) => scene.dynbuf.shadow_mut(),
            None => self.dynbuf.shadow_mut(),
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
        // The direct path hands wgpu-hal the module wgpu-core receives: clip fix applied,
        // validated. A module it cannot prepare sends the scene to wgpu at build.
        let direct = self.direct.is_ok().then(|| {
            let mut module = desc.module.clone();
            if desc.upstream_clip_space {
                crate::shader::apply_clip_space_fix(&mut module, &desc.vertex_entry).ok()?;
            }
            let info = crate::shader::validate(&module, &desc.label).ok()?;
            Some((module, info))
        });
        let s = crate::shader::compile(self.ctx, desc)?;
        self.shaders.push(s);
        let id = self.shaders.len() - 1;
        match direct {
            Some(Some((module, info))) => self.direct_modules.push((id, module, info)),
            Some(None) => self.direct = Err(WgpuReason::Unsupported),
            None => {}
        }
        Ok(ShaderId(id as u32))
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

    /// Upload a volume (3D) texture, bound like any texture: slots the shader declares as
    /// `texture_3d` sample it, 2D slots read the default texture instead.
    pub fn add_volume_texture(&mut self, upload: &VolumeUpload<'_>) -> Result<TextureId> {
        let t = upload_volume(self.ctx, upload)?;
        self.textures.push(t);
        Ok(TextureId(self.textures.len() as u32 - 1))
    }

    #[must_use]
    pub fn texture(&self, id: TextureId) -> Option<&GpuTexture> {
        self.textures.get(id.0 as usize)
    }

    /// Vertices in the dynamic region (rewritable per frame).
    pub fn add_vertices(&mut self, v: &[Vertex]) -> VertexRange {
        match &mut self.live {
            Some(scene) => scene.dynbuf.alloc_vertices(v),
            None => self.dynbuf.alloc_vertices(v),
        }
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
        if let Geometry::Mesh(m) = desc.geometry
            && self.meshes.get(m.0 as usize).is_none()
        {
            return Err(RenderError::BadHandle("mesh"));
        }
        let size = shader.interface().uniform_size;
        let uniform = size.map(|s| match &mut self.live {
            Some(scene) => scene.dynbuf.alloc_uniform(s),
            None => self.dynbuf.alloc_uniform(s),
        });
        if let Some(block) = uniform {
            let fields = self.shaders[desc.shader.0 as usize]
                .interface()
                .uniforms
                .clone();
            let ts = [1.0 / self.scene_size.0, 1.0 / self.scene_size.1];
            for f in &fields {
                let slot = f.slot(block.offset);
                let name = f.name.as_str();
                let res = RESOLUTION_NAMES
                    .iter()
                    .position(|r| *r == name)
                    .and_then(|n| match (n, desc.slot0_variants.first()) {
                        (0, Some(v)) => Some(*v),
                        _ => desc.slots[n].map(|b| b.texture),
                    })
                    .map(|r| self.resolution(r));
                let sh = self.dyn_shadow();
                if IDENTITY_MATRICES.contains(&name) {
                    sh.write_floats(&slot, &identity_for(&slot.ty));
                } else if ONE_DEFAULTS.contains(&name) {
                    sh.write_floats(&slot, &[1.0; 4]);
                } else if name == "g_TexelSize" {
                    sh.write_floats(&slot, &ts);
                } else if name == "g_TexelSizeHalf" {
                    sh.write_floats(&slot, &[ts[0] * 0.5, ts[1] * 0.5]);
                } else if let Some(res) = res {
                    sh.write_floats(&slot, &res);
                }
            }
        }
        self.passes.push(PassRecord { desc, uniform });
        Ok(PassId(self.pass_base + self.passes.len() as u32 - 1))
    }

    /// Write handle of a uniform of a pass (load time: resolve names once).
    #[must_use]
    pub fn uniform(&self, pass: PassId, name: &str) -> Option<UniformSlot> {
        let Some(local) = pass.0.checked_sub(self.pass_base) else {
            return self.live.as_ref()?.uniform(pass, name);
        };
        let rec = self.passes.get(local as usize)?;
        let block = rec.uniform?;
        let shader = self.shaders.get(rec.desc.shader.0 as usize)?;
        shader.interface().field(name).map(|f| f.slot(block.offset))
    }

    /// Initial values (material constants, image values…).
    pub fn shadow_mut(&mut self) -> &mut Shadow {
        self.dyn_shadow()
    }

    /// Append a frame operation.
    pub fn push(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// Compile everything.
    pub fn build(mut self) -> Result<RenderScene> {
        let ctx = self.ctx;
        let device = ctx.device();
        // Fewer render passes (chains first, scene draws together), then GPU memory of the
        // render targets, transient ones sharing textures over the new lifetimes.
        self.reorder_ops();
        let alias = self.alias_plan();
        self.targets.materialize(ctx, &alias.shares, &alias.sizes);

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
                        tracing::warn!(target: "lumenwall_render", "{}: unsupported layout for {}", rec.desc.label, f.name);
                    }
                }
            }
        }
        // Texel size is static: apply its copies once now.
        let dynbuf = self.dynbuf.build(ctx);
        // One staging chunk holds a whole frame's uploads: every uniform and every stream.
        let upload_chunk = (dynbuf.byte_len()
            + self
                .streams
                .iter()
                .map(|s| s.max_floats as u64 * 4)
                .sum::<u64>())
        .next_multiple_of(4096)
        .max(16 * 1024);

        let index_buffer = (!self.indices.is_empty()).then(|| {
            let bytes: &[u8] = bytemuck::cast_slice(&self.indices);
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
        let skin_buffer = (!self.skins.is_empty()).then(|| {
            let bytes: &[u8] = bytemuck::cast_slice(&self.skins);
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
        let uniform_bg = uniform_bind_group(ctx, &uniform_layout, &dynbuf);

        // The 1×1×1 volume exists only when a shader has a `texture_3d` slot.
        let default_volume = self
            .shaders
            .iter()
            .any(|s| s.interface().volume_slots != 0)
            .then(|| default_volume(ctx));
        let depths = self
            .depth_targets
            .iter()
            .map(|&t| create_depth(ctx, t, self.targets.get(t).size()))
            .collect();
        let mut scene = RenderScene {
            direct: None,
            direct_pipelines: HashMap::new(),
            wgpu_reason: None,
            textures_written: false,
            default_texture: default_texture(ctx),
            default_volume,
            depths,
            meshes: std::mem::take(&mut self.meshes),
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
            shaders: Vec::new(),
            pipelines: HashMap::new(),
            scene_size: self.scene_size,
            bundles_stale: false,
            bundle_runs: Vec::new(),
            uploads: wgpu::util::StagingBelt::new(device.clone(), upload_chunk),
            aliased: !alias.shares.is_empty(),
            unshare_pending: false,
        };
        scene.start_direct(ctx, self.direct, std::mem::take(&mut self.direct_modules));

        let mut pipelines: HashMap<PipelineKey, wgpu::RenderPipeline> = HashMap::new();
        for rec in &self.passes {
            let shader = &self.shaders[rec.desc.shader.0 as usize];
            let pass = scene.compile_pass(ctx, shader, rec, &mut pipelines)?;
            scene.passes.push(pass);
        }
        // The first draw into each shared texture clears it (see `AliasPlan::clear_first`).
        let mut pending = alias.clear_first;
        for op in &scene.ops {
            if let Op::Draw(p) = op
                && let Some(pass) = scene.passes.get_mut(p.0 as usize)
                && pending.remove(&pass.desc.target)
            {
                pass.clear_first = true;
            }
        }
        // Kept for passes added later (RenderScene::extend): modules and pipelines are reused.
        scene.shaders = std::mem::take(&mut self.shaders);
        scene.pipelines = pipelines;
        if scene.direct.is_none() && scene.passes.iter().any(|p| p.pipeline.is_none()) {
            // Direct path abandoned while compiling: the passes compiled for it get wgpu pipelines.
            scene.ensure_wgpu(ctx)?;
        }
        scene.analyse_ops();
        scene.rebuild_bindings(ctx)?;
        scene.log_recording();
        Ok(scene)
    }

    /// Render targets a pass samples: bound in a slot its shader declares (a material may
    /// name `_rt_FullFrameBuffer` for a slot the compiled combination never samples; the
    /// graph does not bind it, so it is no dependency), or among its slot-0 variants.
    fn sampled_targets<'a>(&'a self, rec: &'a PassRecord) -> impl Iterator<Item = RtId> + 'a {
        let mask = self
            .shaders
            .get(rec.desc.shader.0 as usize)
            .map_or(0, |s| s.interface().slots);
        let slot0 = mask & 1 != 0;
        rec.desc
            .slots
            .iter()
            .enumerate()
            .filter(move |(n, _)| mask & (1 << n) != 0)
            .filter_map(|(_, b)| b.map(|b| b.texture))
            .chain(
                rec.desc
                    .slot0_variants
                    .iter()
                    .copied()
                    .filter(move |_| slot0),
            )
            .filter_map(|t| match t {
                TextureRef::Target(r) => Some(r),
                _ => None,
            })
    }

    /// What each frame operation reads and writes (targets only).
    fn op_accesses(&self) -> Vec<crate::schedule::OpAccess> {
        self.ops
            .iter()
            .map(|op| match *op {
                Op::Clear { target, .. } => crate::schedule::OpAccess {
                    reads: Vec::new(),
                    writes: Some(target.0),
                },
                Op::Copy { src, dst } => crate::schedule::OpAccess {
                    reads: vec![src.0],
                    writes: Some(dst.0),
                },
                Op::Draw(p) => {
                    let Some(rec) =
                        p.0.checked_sub(self.pass_base)
                            .and_then(|l| self.passes.get(l as usize))
                    else {
                        return crate::schedule::OpAccess::default();
                    };
                    crate::schedule::OpAccess {
                        reads: self.sampled_targets(rec).map(|r| r.0).collect(),
                        writes: Some(rec.desc.target.0),
                    }
                }
            })
            .collect()
    }

    /// Reorders the frame operations of a new scene whose owner marked transient targets
    /// ([`crate::schedule`]): chains first, draws into the other targets (the scene) together.
    fn reorder_ops(&mut self) {
        if self.live.is_some() || !self.targets.has_transient() || self.ops.len() < 3 {
            return;
        }
        // `LUMENWALL_REORDER_BUDGET_MIB` (measurements; 0 keeps the spec order).
        let budget_mib = std::env::var("LUMENWALL_REORDER_BUDGET_MIB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(REORDER_BUDGET_MIB);
        if budget_mib == 0 {
            return;
        }
        let accesses = self.op_accesses();
        let targets = &self.targets;
        if tracing::enabled!(target: "lumenwall_render::schedule", tracing::Level::TRACE) {
            for (i, a) in accesses.iter().enumerate() {
                let reads_batched: Vec<&str> = a
                    .reads
                    .iter()
                    .filter(|&&t| !targets.is_transient(RtId(t)))
                    .map(|&t| targets.get(RtId(t)).label())
                    .collect();
                if !reads_batched.is_empty() {
                    let label = match self.ops[i] {
                        Op::Draw(p) => {
                            p.0.checked_sub(self.pass_base)
                                .and_then(|l| self.passes.get(l as usize))
                                .map_or("?", |r| r.desc.label.as_str())
                        }
                        _ => "clear/copy",
                    };
                    tracing::trace!(target: "lumenwall_render::schedule", "op {i} ({label}) reads {reads_batched:?}");
                }
            }
        }
        let order = crate::schedule::schedule(
            &accesses,
            |t| !targets.is_transient(RtId(t)),
            |t| {
                let id = RtId(t);
                targets.is_transient(id).then(|| {
                    let (w, h) = targets.get(id).size();
                    u64::from(w) * u64::from(h) * 4
                })
            },
            budget_mib << 20,
        );
        self.ops = order.into_iter().map(|i| self.ops[i]).collect();
    }

    /// Which transient targets share a texture ([`plan_aliasing`] over the frame operations).
    /// Only a deferred pool (a new scene) is planned; an extension allocates as it goes.
    fn alias_plan(&self) -> AliasPlan {
        if !self.targets.is_deferred() {
            return AliasPlan::default();
        }
        let mut accesses = Vec::new();
        for (i, op) in self.ops.iter().enumerate() {
            match *op {
                Op::Clear { target, .. } => accesses.push((i, target, Access::Clear)),
                // A copy may cover only part of its destination: counted as a read of both.
                Op::Copy { src, dst } => {
                    accesses.push((i, src, Access::Read));
                    accesses.push((i, dst, Access::Read));
                }
                Op::Draw(p) => {
                    let Some(rec) =
                        p.0.checked_sub(self.pass_base)
                            .and_then(|l| self.passes.get(l as usize))
                    else {
                        continue;
                    };
                    let d = &rec.desc;
                    for r in self.sampled_targets(rec) {
                        accesses.push((i, r, Access::Read));
                    }
                    let write = if d.blend.reads_destination() || d.write_mask != WriteMask::All {
                        Access::Read
                    } else {
                        Access::Overwrite
                    };
                    accesses.push((i, d.target, write));
                }
            }
        }
        // `LUMENWALL_ALIAS_SLACK` (measurements): pixel slack allowed when sharing.
        let slack = std::env::var("LUMENWALL_ALIAS_SLACK")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|s| *s >= 1.0)
            .unwrap_or(ALIAS_AREA_SLACK);
        let plan = plan_aliasing(
            &accesses,
            |t| {
                (self.targets.is_transient(t) && !self.depth_targets.contains(&t)).then(|| {
                    let rt = self.targets.get(t);
                    AliasSize {
                        size: rt.size(),
                        flexible: rt.is_flexible(),
                    }
                })
            },
            slack,
        );
        if tracing::enabled!(target: "lumenwall_render::alias", tracing::Level::TRACE) {
            let mut seen = std::collections::HashSet::new();
            for &(i, t, a) in &accesses {
                if !seen.insert(t) || !self.targets.is_transient(t) {
                    continue;
                }
                let last = accesses.iter().rev().find(|x| x.1 == t).map_or(i, |x| x.0);
                let rt = self.targets.get(t);
                tracing::trace!(
                    target: "lumenwall_render::alias",
                    "{} {:?} ops {i}..={last} first {a:?} shares {:?}",
                    rt.label(),
                    rt.size(),
                    plan.shares.get(&t).map(|o| self.targets.get(*o).label())
                );
            }
        }
        plan
    }

    /// Compiles the passes added to a scene being extended and gives the scene its resources
    /// back (see [`RenderScene::extend`]).
    fn finish_extension(mut self) -> Result<Extension> {
        let ctx = self.ctx;
        let scene = self
            .live
            .take()
            .expect("finish_extension on an extending builder");
        scene.targets = std::mem::take(&mut self.targets);
        scene.textures = std::mem::take(&mut self.textures);
        scene.streams = std::mem::take(&mut self.streams);
        let shaders = std::mem::take(&mut self.shaders);
        for s in &shaders[scene.interfaces.len()..] {
            scene.interfaces.push(s.interface().clone());
        }
        if scene.default_volume.is_none() && shaders.iter().any(|s| s.interface().volume_slots != 0)
        {
            scene.default_volume = Some(default_volume(ctx));
        }
        // The shadow may have grown even if nothing is compiled below: the GPU buffer follows.
        let rebound = scene.dynbuf.sync(ctx);
        if rebound {
            scene.uniform_bg = uniform_bind_group(ctx, &scene.uniform_layout, &scene.dynbuf);
            // Bundles recorded the old buffer or binding size: re-recorded before the next frame.
            scene.bundles_stale = true;
            scene.bundle_runs.clear();
        }
        // Direct path: wgpu-hal modules of the new shaders, uniform group over a new buffer.
        if let (Some(_), Err(r)) = (&scene.direct, self.direct) {
            scene.abandon_direct(&format!("a shader added to the scene cannot be prepared ({r:?})"));
        }
        if let Some(d) = &mut scene.direct {
            let mut failed = None;
            for (id, module, info) in std::mem::take(&mut self.direct_modules) {
                if let Err(e) = d.add_module(id, module, info) {
                    failed = Some(e.0);
                    break;
                }
            }
            if failed.is_none()
                && rebound
                && let Err(e) = d.set_uniform_group(
                    scene.dynbuf.buffer(),
                    scene.dynbuf.binding_size(),
                    frame_globals::SIZE,
                )
            {
                failed = Some(e.0);
            }
            if let Some(f) = failed {
                scene.abandon_direct(&f);
            }
        }
        if !self.indices.is_empty() || !self.skins.is_empty() {
            scene.shaders = shaders;
            return Err(RenderError::Pass {
                label: "extension".into(),
                reason: "indexed or skinned geometry cannot be added to a built scene".into(),
            });
        }
        for rec in &self.passes {
            let (Some(block), Some(shader)) =
                (rec.uniform, shaders.get(rec.desc.shader.0 as usize))
            else {
                continue;
            };
            for f in &shader.interface().uniforms {
                if frame_globals::FIELDS.iter().any(|g| g.0 == f.name) {
                    patches_for(&f.name, &f.slot(block.offset), &mut scene.patches);
                }
            }
        }
        let first = scene.passes.len();
        let mut pipelines = std::mem::take(&mut scene.pipelines);
        let mut compiled = Ok(());
        for rec in &self.passes {
            let shader = &shaders[rec.desc.shader.0 as usize];
            match scene.compile_pass(ctx, shader, rec, &mut pipelines) {
                Ok(p) => scene.passes.push(p),
                Err(e) => {
                    compiled = Err(e);
                    break;
                }
            }
        }
        scene.pipelines = pipelines;
        scene.shaders = shaders;
        for i in first..scene.passes.len() {
            scene.hazard_for(ctx, i);
            scene.bind_groups_for(ctx, i);
        }
        if scene.direct.is_none()
            && scene
                .passes
                .iter()
                .any(|p| p.pipeline.is_none() || p.bind_groups.is_empty())
        {
            // Abandoned during this extension: the passes made for the direct path get theirs.
            let _ = scene.ensure_wgpu(ctx);
        }
        scene.dirty = true;
        compiled?;
        Ok(Extension {
            passes: (first..scene.passes.len())
                .map(|i| PassId(i as u32))
                .collect(),
            ops: std::mem::take(&mut self.ops),
        })
    }
}

/// Uniform bind group: pass blocks (dynamic offset) and the frame globals block.
fn uniform_bind_group(
    ctx: &GpuContext,
    layout: &wgpu::BindGroupLayout,
    dynbuf: &DynamicBuffer,
) -> wgpu::BindGroup {
    let bg = ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lumenwall.uniforms.bg"),
        layout,
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
    ctx.note_resources(1);
    bg
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
}

struct CompiledPass {
    label: String,
    shader: ShaderId,
    desc: PassDesc,
    uniform: Option<UniformBlock>,
    /// wgpu path (`None` while the scene records directly in Vulkan).
    pipeline: Option<wgpu::RenderPipeline>,
    /// Direct path: index of the wgpu-hal pipeline.
    direct_pipeline: Option<usize>,
    /// wgpu path: one per variant (empty while the scene records directly in Vulkan).
    bind_groups: Vec<wgpu::BindGroup>,
    /// Slot-0 variants (at least one).
    variants: usize,
    variant: usize,
    /// Vertex buffer slots (≥ 1) fed with defaults: `(slot, integer?)`.
    unfed: Vec<(u32, bool)>,
    /// Scratch copy of the target when the pass samples its own target.
    hazard_scratch: Option<RtId>,
    res0: Option<UniformSlot>,
    enabled: bool,
    /// The target's texture is shared with other transient targets and this pass writes it
    /// first in the frame: its render pass starts with a clear to transparent black.
    clear_first: bool,
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
    passes: &'a [CompiledPass],
    streams: &'a [VertexStream],
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

/// Command buffers of a frame recorded directly in Vulkan
/// ([`RenderScene::encode_frame`]): the caller's commands up to the scene with the frame's
/// uploads, then the scene. Empty on the wgpu path (everything went into the caller's encoder).
#[derive(Default)]
pub struct FrameCommands {
    buffers: [Option<wgpu::CommandBuffer>; 2],
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

/// GPU time of one render pass or copy of a frame ([`RenderScene::profile_frame`]).
#[derive(Clone, Debug)]
pub struct SegmentTime {
    pub target: String,
    pub size: (u32, u32),
    /// Draws in the render pass (0 for a copy).
    pub passes: usize,
    pub first_pass: String,
    pub ms: f64,
}

enum Segment {
    Copy {
        src: RtId,
        dst: RtId,
    },
    Render {
        target: RtId,
        clear: Option<wgpu::Color>,
        /// Depth attachment `(index in RenderScene::depths, clear to 1?)`.
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

/// A compiled scene graph ready to render frames.
pub struct RenderScene {
    /// Direct Vulkan recording; `None` on the wgpu path. First field: dropped (after a GPU
    /// wait) before the resources its frames used.
    direct: Option<DirectGraph>,
    /// wgpu-hal pipelines by key (direct path).
    direct_pipelines: HashMap<PipelineKey, usize>,
    /// Why the scene records through wgpu (`None` on the direct path).
    wgpu_reason: Option<WgpuReason>,
    /// [`Self::mark_dirty`] since the last frame: textures may have been written by the queue.
    textures_written: bool,
    targets: RenderTargetPool,
    textures: Vec<GpuTexture>,
    default_texture: GpuTexture,
    /// Bound to `texture_3d` slots without a volume texture (only when a shader has one).
    default_volume: Option<GpuTexture>,
    interfaces: Vec<ShaderInterface>,
    dynbuf: DynamicBuffer,
    index_buffer: Option<wgpu::Buffer>,
    skin_buffer: Option<wgpu::Buffer>,
    streams: Vec<VertexStream>,
    meshes: Vec<StaticMesh>,
    depths: Vec<DepthBuffer>,
    default_attrs: wgpu::Buffer,
    uniform_layout: wgpu::BindGroupLayout,
    uniform_bg: wgpu::BindGroup,
    /// By `(slots, volume slots)` of the shader interface.
    texture_layouts: HashMap<(u8, u8), (wgpu::BindGroupLayout, wgpu::PipelineLayout)>,
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
    /// Compiled shaders and pipelines, reused by passes added after load.
    shaders: Vec<PassShader>,
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,
    scene_size: (f32, f32),
    /// Segments and bundles must be rebuilt before the next frame (structure changed).
    bundles_stale: bool,
    /// Passes recorded in each of `bundles`: a rebuild reuses the bundle of an unchanged
    /// run. Emptied when bind groups or the dynamic buffer change.
    bundle_runs: Vec<Vec<usize>>,
    /// Recycled staging memory of the per-frame uploads (uniforms, vertex streams).
    uploads: wgpu::util::StagingBelt,
    /// Some transient targets share a texture (planned on the frame order of the build).
    aliased: bool,
    /// The frame order changed: shared targets get their own textures before the next frame.
    unshare_pending: bool,
}

/// Memory of the transient targets that deferred scene draws may keep alive when the frame is
/// reordered ([`crate::schedule`]). 3463520581 at 2560x1440: budget 0 (spec order) 89 render
/// passes, 192 MiB reserved; 8 MiB: 65 passes, 192 MiB; 32 MiB: 59 passes, 224 MiB.
pub const REORDER_BUDGET_MIB: u64 = 8;

/// Default number of extra frames rendered after the last change when the
/// graph accumulates across frames (translucent/additive passes into targets
/// that are never cleared, spec 04 §15): enough for 8-bit convergence.
pub const ACCUMULATION_SETTLE_FRAMES: u32 = 120;

impl RenderScene {
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
                self.streams
                    .get(s.0 as usize)
                    .ok_or_else(|| err("unknown vertex stream".into()))?,
            ),
            _ => None,
        };
        let mesh = match desc.geometry {
            Geometry::Mesh(m) => Some(
                self.meshes
                    .get(m.0 as usize)
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
            stream.and_then(|s| self.streams.iter().position(|o| o.layout == s.layout));
        let mesh_layout = mesh.and_then(|m| self.meshes.iter().position(|o| o.layout == m.layout));
        let depth = self
            .depths
            .iter()
            .any(|d| d.target == desc.target)
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
                format: RT_FORMAT,
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
                multisample: wgpu::MultisampleState::default(),
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
            shader: plan.key.shader.0 as usize,
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
        rec: &PassRecord,
        cache: &mut HashMap<PipelineKey, wgpu::RenderPipeline>,
    ) -> Result<CompiledPass> {
        let desc = &rec.desc;
        let plan = self.pipeline_plan(ctx, shader, desc)?;
        let direct = if self.direct.is_some() {
            self.direct_pipeline(shader, &plan)
        } else {
            None
        };
        let pipeline = match direct {
            Some(_) => None,
            None => Some(self.wgpu_pipeline(ctx, shader, &plan, &desc.label, cache)?),
        };
        let res0 = rec.uniform.and_then(|b| {
            shader
                .interface()
                .field(RESOLUTION_NAMES[0])
                .map(|f| f.slot(b.offset))
        });
        Ok(CompiledPass {
            label: desc.label.clone(),
            shader: desc.shader,
            desc: desc.clone(),
            uniform: rec.uniform,
            pipeline,
            direct_pipeline: direct,
            bind_groups: Vec::new(),
            variants: desc.slot0_variants.len().max(1),
            variant: 0,
            unfed: plan.unfed,
            hazard_scratch: None,
            res0,
            enabled: true,
            clear_first: false,
        })
    }

    /// Sends the scene back to the wgpu path for good (a wgpu-hal object could not be created,
    /// or the frame cannot be planned): the direct graph is destroyed after a GPU wait, and the
    /// caller creates the missing wgpu objects ([`Self::ensure_wgpu`]).
    fn abandon_direct(&mut self, why: &str) {
        if self.direct.take().is_some() {
            tracing::warn!(target: "lumenwall_render::direct", "recording through wgpu: {why}");
            self.direct_pipelines.clear();
            self.wgpu_reason = Some(WgpuReason::Unsupported);
            for p in &mut self.passes {
                p.direct_pipeline = None;
            }
        }
    }

    /// Creates the wgpu pipelines, bind groups, segments and bundles a scene abandoned by the
    /// direct path is missing.
    fn ensure_wgpu(&mut self, ctx: &GpuContext) -> Result<()> {
        let shaders = std::mem::take(&mut self.shaders);
        let mut cache = std::mem::take(&mut self.pipelines);
        let mut res = Ok(());
        for i in 0..self.passes.len() {
            if self.passes[i].pipeline.is_some() {
                continue;
            }
            let Some(shader) = shaders.get(self.passes[i].shader.0 as usize) else {
                continue;
            };
            let desc = self.passes[i].desc.clone();
            match self
                .pipeline_plan(ctx, shader, &desc)
                .and_then(|plan| self.wgpu_pipeline(ctx, shader, &plan, &desc.label, &mut cache))
            {
                Ok(p) => self.passes[i].pipeline = Some(p),
                Err(e) => {
                    // The wgpu path would have refused the scene: the pass is left out.
                    tracing::error!(target: "lumenwall_render", "{}: {e}", desc.label);
                    self.passes[i].enabled = false;
                    res = Err(e);
                }
            }
        }
        self.shaders = shaders;
        self.pipelines = cache;
        for i in 0..self.passes.len() {
            if self.passes[i].bind_groups.is_empty() {
                self.bind_groups_for(ctx, i);
            }
        }
        self.bundle_runs.clear();
        self.bundles.clear();
        self.rebuild_frame(ctx);
        self.dirty = true;
        res
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

    /// View, sampler key and render target of a slot. A `volume` slot (`texture_3d`) only
    /// takes a volume texture, a 2D slot only 2D textures and targets; anything else reads the
    /// default (0, 0, 0, 1) of the right dimension. The target is the one behind the view (the
    /// hazard scratch when the pass samples its own target).
    fn slot_view(
        &self,
        b: Option<SlotBinding>,
        hazard: Option<(RtId, RtId)>,
        volume: bool,
    ) -> (&wgpu::TextureView, SamplerKey, Option<RtId>) {
        let b = b.unwrap_or(SlotBinding {
            texture: TextureRef::Default,
            sampler: None,
        });
        if volume {
            let tex = match b.texture {
                TextureRef::Texture(t) => self
                    .textures
                    .get(t.0 as usize)
                    .filter(|tex| tex.is_volume()),
                _ => None,
            };
            let tex = match (tex, &self.default_volume) {
                (Some(tex), _) | (None, Some(tex)) => tex,
                // Unreachable: the default volume exists when a shader has a volume slot.
                (None, None) => &self.default_texture,
            };
            return (tex.view(), b.sampler.unwrap_or(tex.sampler_key()), None);
        }
        let (view, key, rt) = match b.texture {
            TextureRef::Texture(t) => match self.textures.get(t.0 as usize) {
                Some(tex) if !tex.is_volume() => (tex.view(), tex.sampler_key(), None),
                _ => (
                    self.default_texture.view(),
                    self.default_texture.sampler_key(),
                    None,
                ),
            },
            TextureRef::Target(r) => {
                let key = self.targets.get(r).sampler_key();
                let r = match hazard {
                    Some((target, scratch)) if target == r => scratch,
                    _ => r,
                };
                (self.targets.get(r).view(), key, Some(r))
            }
            TextureRef::Default => (
                self.default_texture.view(),
                self.default_texture.sampler_key(),
                None,
            ),
        };
        (view, b.sampler.unwrap_or(key), rt)
    }

    /// Bindings of each slot-0 variant of pass `i` (both paths).
    fn pass_slots(&self, i: usize) -> Vec<Vec<BoundSlot<'_>>> {
        let pass = &self.passes[i];
        let iface = &self.interfaces[pass.shader.0 as usize];
        let (slots_mask, volume_mask) = (iface.slots, iface.volume_slots);
        let hazard = pass.hazard_scratch.map(|s| (pass.desc.target, s));
        let variants: Vec<Option<TextureRef>> = if pass.desc.slot0_variants.is_empty() {
            vec![None]
        } else {
            pass.desc.slot0_variants.iter().copied().map(Some).collect()
        };
        variants
            .into_iter()
            .map(|v| {
                (0..MAX_SLOTS)
                    .filter(|n| slots_mask & (1 << n) != 0)
                    .map(|n| {
                        let mut b = pass.desc.slots[n];
                        if n == 0
                            && let Some(t) = v
                        {
                            b = Some(SlotBinding {
                                texture: t,
                                sampler: b.and_then(|b| b.sampler),
                            });
                        }
                        let volume = volume_mask & (1 << n) != 0;
                        let (view, sampler, target) = self.slot_view(b, hazard, volume);
                        BoundSlot {
                            slot: n as u32,
                            view,
                            sampler,
                            volume,
                            target,
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// Every texture the scene owns (targets, textures, depth buffers).
    fn owned_textures(&self) -> HashSet<wgpu::Texture> {
        let mut owned: HashSet<wgpu::Texture> = self
            .targets
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

    /// Rebuild bind groups, hazard copies, segments and bundles. Call after
    /// any target reallocation or sampler change (event-driven, not per frame).
    pub fn rebuild_bindings(&mut self, ctx: &GpuContext) -> Result<()> {
        // Direct path: the GPU finishes the recorded frames before what they use is replaced.
        if self.direct.is_some() {
            let owned = self.owned_textures();
            if let Some(d) = &mut self.direct {
                d.idle(&|t| owned.contains(t));
            }
        }
        // Depth buffers follow their target's size.
        for i in 0..self.depths.len() {
            let target = self.depths[i].target;
            let size = self.targets.get(target).size();
            if self.depths[i].size != size {
                self.depths[i] = create_depth(ctx, target, size);
            }
        }
        // Hazards: a pass sampling its own destination reads a copy (spec 04 §24.6).
        for i in 0..self.passes.len() {
            self.hazard_for(ctx, i);
        }
        for i in 0..self.passes.len() {
            self.bind_groups_for(ctx, i);
        }
        if self.direct.is_none() && self.passes.iter().any(|p| p.pipeline.is_none()) {
            // Direct path abandoned while binding: its passes get wgpu pipelines.
            self.ensure_wgpu(ctx)?;
        }
        self.bundle_runs.clear();
        self.rebuild_frame(ctx);
        if let Some(d) = &mut self.direct {
            d.compact();
        }
        self.bundles_stale = false;
        self.dirty = true;
        Ok(())
    }

    /// Scratch copy of the target of pass `i` when it samples it (spec 04 §24.6).
    fn hazard_for(&mut self, ctx: &GpuContext, i: usize) {
        let desc = &self.passes[i].desc;
        let target = desc.target;
        // Only slots the shader samples (the others are not bound).
        let mask = self
            .interfaces
            .get(self.passes[i].shader.0 as usize)
            .map_or(0, |iface| iface.slots);
        let samples_target = desc
            .slots
            .iter()
            .enumerate()
            .filter(|(n, _)| mask & (1 << n) != 0)
            .filter_map(|(_, b)| b.map(|b| b.texture))
            .chain(
                desc.slot0_variants
                    .iter()
                    .copied()
                    .filter(|_| mask & 1 != 0),
            )
            .any(|t| t == TextureRef::Target(target));
        let scratch = if samples_target {
            let size = self.targets.get(target).size();
            Some(self.targets.scratch(ctx, size))
        } else {
            None
        };
        self.passes[i].hazard_scratch = scratch;
    }

    /// Texture bind groups of pass `i` (one per slot-0 variant), on the scene's path.
    fn bind_groups_for(&mut self, ctx: &GpuContext, i: usize) {
        let iface = &self.interfaces[self.passes[i].shader.0 as usize];
        let key = (iface.slots, iface.volume_slots);
        if let Some(mut d) = self.direct.take() {
            let result = {
                let slots = self.pass_slots(i);
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
                Ok(()) => {
                    let p = &mut self.passes[i];
                    p.variant = p.variant.min(p.variants - 1);
                    return;
                }
                Err(e) => {
                    let label = self.passes[i].label.clone();
                    self.abandon_direct(&format!("{label}: {}", e.0));
                }
            }
        }
        self.texture_layout(ctx, key);
        let groups = {
            let slots = self.pass_slots(i);
            let (bgl, _) = &self.texture_layouts[&key];
            let label = &self.passes[i].label;
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
        let p = &mut self.passes[i];
        p.bind_groups = groups;
        p.variant = p.variant.min(p.variants - 1);
    }

    /// Adds passes to the built scene (layers created after load, e.g. SceneScript
    /// `thisScene.createLayer`). `f` receives a builder whose `targets`, textures, shaders,
    /// streams and dynamic buffer are this scene's: already compiled shader modules and
    /// pipelines (by shader, blend, write mask, cull) and bind group layouts are reused, and
    /// only the new passes get pipelines (from the cache when possible) and bind groups.
    /// Uniform blocks and vertices are appended to the dynamic buffer, which grows with
    /// headroom (a reallocation re-records the bundles before the next frame).
    ///
    /// The frame operations `f` pushes are returned, not placed: put them where the layer
    /// belongs with [`Self::set_ops`]. Indexed and skinned geometry (puppet meshes) cannot be
    /// added. On error the passes compiled before the failure stay unused.
    pub fn extend<'a, R>(
        &'a mut self,
        ctx: &'a GpuContext,
        f: impl FnOnce(&mut SceneBuilder<'a>) -> R,
    ) -> (R, Result<Extension>) {
        let align = ctx.device().limits().min_uniform_buffer_offset_alignment;
        let mut b = SceneBuilder {
            ctx,
            targets: std::mem::take(&mut self.targets),
            textures: std::mem::take(&mut self.textures),
            shaders: std::mem::take(&mut self.shaders),
            dynbuf: DynamicBufferBuilder::new(align),
            indices: Vec::new(),
            skins: Vec::new(),
            streams: std::mem::take(&mut self.streams),
            // Layers created after load are 2D quads: no new mesh and no new depth target.
            meshes: Vec::new(),
            depth_targets: Vec::new(),
            passes: Vec::new(),
            ops: Vec::new(),
            scene_size: self.scene_size,
            pass_base: self.passes.len() as u32,
            direct: match self.wgpu_reason {
                None => Ok(()),
                Some(r) => Err(r),
            },
            direct_modules: Vec::new(),
            live: Some(self),
        };
        let r = f(&mut b);
        (r, b.finish_extension())
    }

    /// Frame operations in order.
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Replaces the frame operations (layers added, removed or reordered after load). Passes
    /// left out are kept, compiled, for a later reuse. Cross-frame dependencies are analysed
    /// again; segments and bundles are rebuilt once, before the next frame.
    pub fn set_ops(&mut self, ops: Vec<Op>) {
        self.ops = ops;
        self.accumulates = false;
        self.feedback = false;
        self.analyse_ops();
        self.bundles_stale = true;
        self.dirty = true;
        // Lifetimes were planned on the old order (lumenwall-engine marks no transient target
        // in scenes whose scripts reorder layers; this keeps any other caller correct).
        self.unshare_pending |= self.aliased;
    }

    /// Rebuilds segments and bundles now if the structure changed (also done by
    /// [`Self::encode`]).
    pub fn refresh(&mut self, ctx: &GpuContext) {
        if self.unshare_pending {
            self.unshare_pending = false;
            self.aliased = false;
            self.targets.unshare(ctx);
            for p in &mut self.passes {
                p.clear_first = false;
            }
            if let Err(e) = self.rebuild_bindings(ctx) {
                tracing::error!(target: "lumenwall_render", "render targets not rebound: {e}");
            }
            return;
        }
        if self.bundles_stale || self.direct.as_ref().is_some_and(|d| !d.has_program()) {
            self.rebuild_frame(ctx);
            self.bundles_stale = false;
        }
    }

    /// Segments, then the bundles (wgpu path) or the recorded frame (direct path).
    fn rebuild_frame(&mut self, ctx: &GpuContext) {
        self.build_segments();
        if self.direct.is_none() {
            self.build_bundles(ctx);
            return;
        }
        if let Err(why) = self.build_program(ctx) {
            self.abandon_direct(&why);
            // Builds the bundles through `rebuild_frame` again, on the wgpu path.
            if let Err(e) = self.ensure_wgpu(ctx) {
                tracing::error!(target: "lumenwall_render", "back to wgpu: {e}");
            }
        }
    }

    /// The direct frame of the current segments.
    fn build_program(&mut self, ctx: &GpuContext) -> std::result::Result<(), String> {
        let Some(mut d) = self.direct.take() else {
            return Ok(());
        };
        let r = self
            .direct_steps()
            .and_then(|steps| d.build_program(ctx, &steps).map_err(|e| e.0));
        self.direct = Some(d);
        r
    }

    /// The segments as the direct path records them.
    fn direct_steps(&self) -> std::result::Result<Vec<Step<'_>>, String> {
        let mut steps = Vec::with_capacity(self.segments.len());
        for seg in &self.segments {
            match seg {
                Segment::Copy { src, dst } => {
                    let (s, d) = (self.targets.get(*src), self.targets.get(*dst));
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
                    let rt = self.targets.get(*target);
                    let mut sampled: Vec<&wgpu::Texture> = Vec::new();
                    let mut draws = Vec::with_capacity(items.len());
                    for item in items {
                        let Item::Pass(p) = *item else {
                            return Err("render bundle on the direct path".into());
                        };
                        for v in self.pass_slots(p) {
                            for s in v {
                                if let Some(t) = s.target {
                                    let t = self.targets.get(t).texture();
                                    if !sampled.contains(&t) {
                                        sampled.push(t);
                                    }
                                }
                            }
                        }
                        if let Some(d) = self.direct_draw(p)? {
                            draws.push(d);
                        }
                    }
                    steps.push(Step::Render {
                        target: rt.texture(),
                        clear: *clear,
                        depth: depth.map(|(d, c)| (&self.depths[d].texture, &self.depths[d].view, c)),
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
    fn direct_draw(&self, p: usize) -> std::result::Result<Option<DrawStep<'_>>, String> {
        let pass = &self.passes[p];
        let pipeline = pass
            .direct_pipeline
            .ok_or_else(|| format!("{}: no wgpu-hal pipeline", pass.label))?;
        let dynbuf = self.dynbuf.buffer();
        let vertex_offset = u64::from(self.dynbuf.vertex_offset());
        let mut vertex: Vec<(&wgpu::Buffer, u64)> = Vec::new();
        let mut index = None;
        let call = match pass.desc.geometry {
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
                    .ok_or_else(|| format!("{}: no index buffer", pass.label))?;
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
                    .ok_or_else(|| format!("{}: no index buffer", pass.label))?;
                let sb = self
                    .skin_buffer
                    .as_ref()
                    .ok_or_else(|| format!("{}: no skin buffer", pass.label))?;
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
                    .get(s.0 as usize)
                    .ok_or_else(|| format!("{}: unknown stream", pass.label))?;
                vertex.push((&st.vertices, 0));
                index = Some((&st.indices, wgpu::IndexFormat::Uint32));
                DrawCall::Stream(s.0)
            }
            Geometry::Mesh(m) => {
                let mesh = self
                    .meshes
                    .get(m.0 as usize)
                    .ok_or_else(|| format!("{}: unknown mesh", pass.label))?;
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
            uniform_offset: pass.uniform.map_or(0, |u| u.offset),
            vertex,
            index,
            call,
        }))
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
                    depth: None,
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
                    if pass.clear_first {
                        // Shared texture: start from transparent black, as a fresh target.
                        flush(&mut cur, &mut segments);
                        cur = Some((target, Some(wgpu::Color::TRANSPARENT), Vec::new()));
                    } else if cur.as_ref().is_none_or(|c| c.0 != target) {
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

    fn record<'a, R: Recorder<'a>>(&'a self, r: &mut R, index: usize) {
        let pass = &self.passes[index];
        let stream = match pass.desc.geometry {
            Geometry::Stream(s) => match self.streams.get(s.0 as usize) {
                Some(s) if s.index_count > 0 => Some(s),
                _ => return,
            },
            _ => None,
        };
        let mesh = match pass.desc.geometry {
            Geometry::Mesh(m) => match self.meshes.get(m.0 as usize) {
                Some(m) if m.index_count > 0 => Some(m),
                _ => return,
            },
            _ => None,
        };
        let (Some(pipeline), Some(group)) = (&pass.pipeline, pass.bind_groups.get(pass.variant))
        else {
            return;
        };
        r.pipeline(pipeline);
        r.bind_group(0, &self.uniform_bg, &[pass.uniform.map_or(0, |u| u.offset)]);
        r.bind_group(1, group, &[]);
        if let Some(s) = stream {
            r.vertex_buffer(0, s.vertices.slice(..));
        } else if let Some(m) = mesh {
            r.vertex_buffer(0, m.vertices.slice(..));
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
    fn build_bundles(&mut self, ctx: &GpuContext) {
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
                                    color_formats: &[Some(RT_FORMAT)],
                                    depth_stencil,
                                    sample_count: 1,
                                    multiview: None,
                                },
                            );
                            for &p in run.iter() {
                                self.record(&mut enc, p);
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
        _ctx: &GpuContext,
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
        // Kept and uploaded with the frame (`encode`): no staging buffer per call.
        if floats > 0 && count > 0 {
            s.cpu[..floats].copy_from_slice(&vertices[..floats]);
            s.pending = floats;
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
        let v = variant.min(p.variants.saturating_sub(1));
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
            self.rebuild_frame(ctx);
            self.bundles_stale = false;
            self.dirty = true;
        }
    }

    /// Something outside the uniform buffer changed (video frame written into
    /// a texture, thumbnail loaded…). Call it after every write into one of the scene's
    /// textures through the queue (`GpuTexture::write_level0`): a scene recorded directly in
    /// Vulkan then puts its textures back in their sampled state before the next frame.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.textures_written = true;
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

    /// Sets up the direct path of a new scene (its wgpu-hal modules and uniform bind group).
    fn start_direct(
        &mut self,
        ctx: &GpuContext,
        allowed: std::result::Result<(), WgpuReason>,
        modules: Vec<(usize, naga::Module, naga::valid::ModuleInfo)>,
    ) {
        let made = allowed.and_then(|()| {
            let fail = |e: crate::hal::Unsupported| {
                tracing::warn!(target: "lumenwall_render::direct", "recording through wgpu: {}", e.0);
                WgpuReason::Unsupported
            };
            let mut d = DirectGraph::new(ctx).map_err(fail)?;
            for (id, module, info) in modules {
                d.add_module(id, module, info).map_err(fail)?;
            }
            d.set_uniform_group(
                self.dynbuf.buffer(),
                self.dynbuf.binding_size(),
                frame_globals::SIZE,
            )
            .map_err(fail)?;
            Ok(d)
        });
        match made {
            Ok(d) => self.direct = Some(d),
            Err(r) => self.wgpu_reason = Some(r),
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

    /// Upload the dirty uniforms and record the frame into `encoder`. A scene recorded
    /// directly in Vulkan submits `encoder`'s commands so far (with the frame's uploads) and its
    /// frame here, and gives the caller a fresh encoder for what follows (composition): use
    /// [`Self::encode_frame`] to submit everything at once instead.
    pub fn encode(&mut self, ctx: &GpuContext, encoder: &mut wgpu::CommandEncoder) {
        let commands = self.encode_frame(ctx, encoder);
        if !commands.is_empty() {
            ctx.queue().submit(commands);
        }
    }

    /// [`Self::encode`] without its submission. On the wgpu path the frame goes into `encoder`
    /// and nothing is returned. On the direct path `encoder` (the caller's commands so far, then
    /// the frame's uploads) and the scene's Vulkan frame are returned, in submission order, and
    /// `encoder` is replaced with a fresh one for the commands that follow (composition):
    /// `queue.submit(frame.into_iter().chain([encoder.finish()]))`.
    pub fn encode_frame(
        &mut self,
        ctx: &GpuContext,
        encoder: &mut wgpu::CommandEncoder,
    ) -> FrameCommands {
        let t0 = Instant::now();
        self.refresh(ctx);
        // The previous frame was submitted: its staging chunks come back once the GPU is done.
        self.uploads.recall();
        let mut changed = self.dynbuf.flush_into(encoder, &mut self.uploads);
        for s in &mut self.streams {
            if s.pending == 0 {
                continue;
            }
            if let Some(size) = wgpu::BufferSize::new(s.pending as u64 * 4) {
                self.uploads
                    .write_buffer(encoder, &s.vertices, 0, size)
                    .copy_from_slice(bytemuck::cast_slice(&s.cpu[..s.pending]));
            }
            s.pending = 0;
            changed = true;
        }
        self.uploads.finish();
        let changed = changed || self.dirty;
        self.dirty = false;
        if changed {
            self.settle_left = self.settle_frames;
        } else {
            self.settle_left = self.settle_left.saturating_sub(1);
        }
        let commands = match &self.direct {
            None => {
                self.record_segments(encoder, None);
                FrameCommands::default()
            }
            Some(d) => {
                if std::mem::take(&mut self.textures_written) {
                    // Textures written through the queue are in `COPY_DST` for wgpu-core: back
                    // to their sampled state before the frame samples them.
                    crate::hal::restore_textures(
                        encoder,
                        self.textures.iter().map(GpuTexture::texture),
                    );
                }
                let next = ctx
                    .device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("lumenwall.frame"),
                    });
                let before = std::mem::replace(encoder, next).finish();
                let frame = d.record(
                    ctx.device(),
                    &FrameView {
                        passes: &self.passes,
                        streams: &self.streams,
                    },
                    None,
                );
                FrameCommands {
                    buffers: [Some(before), Some(frame)],
                }
            }
        };
        self.last_encode = t0.elapsed();
        commands
    }

    /// Records the frame's segments; with `queries`, timestamps around each one (indices `2i`
    /// and `2i + 1`, [`Self::profile_frame`]).
    fn record_segments(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queries: Option<&wgpu::QuerySet>,
    ) {
        for (si, seg) in self.segments.iter().enumerate() {
            let (begin, end) = (2 * si as u32, 2 * si as u32 + 1);
            match seg {
                Segment::Copy { src, dst } => {
                    if let Some(q) = queries {
                        encoder.write_timestamp(q, begin);
                    }
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
                    let rt = self.targets.get(*target);
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
                            view: rt.view(),
                            depth_slice: None,
                            resolve_target: None,
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
                            Item::Pass(p) => self.record(&mut rp, *p),
                        }
                    }
                }
            }
        }
    }

    /// Debug profile of one frame (tools, never the live loop): renders the frame alone with
    /// a timestamp around each render pass and copy, waits for the GPU and returns, in frame
    /// order, `(target label, target size, passes, first pass label, milliseconds)`. `None`
    /// when the device has no timestamp queries.
    pub fn profile_frame(&mut self, ctx: &GpuContext) -> Option<Vec<SegmentTime>> {
        if !ctx.caps().timestamps || self.segments.is_empty() {
            return None;
        }
        self.refresh(ctx);
        let ms: Vec<f64> = match &self.direct {
            Some(d) => {
                self.dynbuf.flush(ctx.queue());
                let uploads = ctx
                    .device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("lumenwall.profile"),
                    })
                    .finish();
                d.profile(
                    ctx,
                    &FrameView {
                        passes: &self.passes,
                        streams: &self.streams,
                    },
                    uploads,
                )?
            }
            None => self.profile_wgpu(ctx)?,
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
                        .map_or_else(String::new, |&p| self.passes[p].label.clone());
                    (*target, ids.len(), first)
                }
            };
            let rt = self.targets.get(target);
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

    /// GPU milliseconds of each segment of one frame recorded through wgpu.
    fn profile_wgpu(&mut self, ctx: &GpuContext) -> Option<Vec<f64>> {
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
        self.dynbuf.flush(ctx.queue());
        self.record_segments(&mut enc, Some(&set));
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

    /// Encode and submit (convenience): one submission on either path.
    pub fn render(&mut self, ctx: &GpuContext) -> wgpu::SubmissionIndex {
        let mut encoder = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumenwall.frame"),
            });
        let frame = self.encode_frame(ctx, &mut encoder);
        ctx.queue()
            .submit(frame.into_iter().chain(std::iter::once(encoder.finish())))
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

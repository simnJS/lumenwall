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
use crate::context::MemoryToken;
use crate::dynbuf::{
    DynamicBuffer, DynamicBufferBuilder, FrameInputs, Patch, ScalarKind, Shadow, UniformBlock,
    UniformSlot, UniformType, VertexRange, frame_globals, patches_for,
};
use crate::error::{RenderError, Result};
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
        // GPU memory of the render targets, transient ones sharing textures.
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
        scene.analyse_ops();
        scene.rebuild_bindings(ctx)?;
        Ok(scene)
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
                    for t in d
                        .slots
                        .iter()
                        .flatten()
                        .map(|b| b.texture)
                        .chain(d.slot0_variants.iter().copied())
                    {
                        if let TextureRef::Target(r) = t {
                            accesses.push((i, r, Access::Read));
                        }
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
    /// The target's texture is shared with other transient targets and this pass writes it
    /// first in the frame: its render pass starts with a clear to transparent black.
    clear_first: bool,
}

enum Item {
    Bundle(usize),
    Pass(usize),
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
                        view_dimension: if volumes & (1 << n) == 0 {
                            wgpu::TextureViewDimension::D2
                        } else {
                            wgpu::TextureViewDimension::D3
                        },
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
        for input in &iface.vertex_inputs {
            if let Some(mesh) = mesh {
                let name = iface.vertex_input_name(input.location).unwrap_or("");
                if let Some(a) = mesh.layout.attributes.iter().find(|a| a.name == name) {
                    main_attrs.push(wgpu::VertexAttribute {
                        format: a.format,
                        offset: u64::from(a.offset),
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
            mesh_layout,
            front_face: desc.front_face,
            depth,
        };
        let pipeline = if let Some(p) = cache.get(&key) {
            p.clone()
        } else {
            // Passes into a target with a depth buffer clip against the near and far planes like
            // Wallpaper Engine's 3D passes; the others never discard geometry for its depth.
            let unclipped = ctx.caps().unclipped_depth && depth.is_none();
            let (_, pl) = self.texture_layout(ctx, (iface.slots, iface.volume_slots));
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
            clear_first: false,
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

    /// View and sampler of a slot. A `volume` slot (`texture_3d`) only takes a volume
    /// texture, a 2D slot only 2D textures and targets; anything else reads the default
    /// (0, 0, 0, 1) of the right dimension.
    fn view_and_sampler(
        &self,
        ctx: &GpuContext,
        b: Option<SlotBinding>,
        hazard: Option<(RtId, RtId)>,
        volume: bool,
    ) -> (&wgpu::TextureView, wgpu::Sampler) {
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
            let key = b.sampler.unwrap_or(tex.sampler_key());
            return (tex.view(), ctx.volume_sampler(key));
        }
        let (view, key) = match b.texture {
            TextureRef::Texture(t) => match self.textures.get(t.0 as usize) {
                Some(tex) if !tex.is_volume() => (tex.view(), tex.sampler_key()),
                _ => (
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
        self.bundle_runs.clear();
        self.build_segments();
        self.build_bundles(ctx);
        self.bundles_stale = false;
        self.dirty = true;
        Ok(())
    }

    /// Scratch copy of the target of pass `i` when it samples it (spec 04 §24.6).
    fn hazard_for(&mut self, ctx: &GpuContext, i: usize) {
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

    /// Texture bind groups of pass `i` (one per slot-0 variant).
    fn bind_groups_for(&mut self, ctx: &GpuContext, i: usize) {
        let iface = &self.interfaces[self.passes[i].shader.0 as usize];
        let (slots_mask, volume_mask) = (iface.slots, iface.volume_slots);
        self.texture_layout(ctx, (slots_mask, volume_mask));
        let pass = &self.passes[i];
        let hazard = pass.hazard_scratch.map(|s| (pass.desc.target, s));
        let variants: Vec<Option<TextureRef>> = if pass.desc.slot0_variants.is_empty() {
            vec![None]
        } else {
            pass.desc.slot0_variants.iter().copied().map(Some).collect()
        };
        let (bgl, _) = &self.texture_layouts[&(slots_mask, volume_mask)];
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
                let volume = volume_mask & (1 << n) != 0;
                views.push((n as u32, self.view_and_sampler(ctx, b, hazard, volume)));
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
        if self.bundles_stale {
            self.build_segments();
            self.build_bundles(ctx);
            self.bundles_stale = false;
        }
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
        r.pipeline(&pass.pipeline);
        r.bind_group(0, &self.uniform_bg, &[pass.uniform.map_or(0, |u| u.offset)]);
        r.bind_group(1, &pass.bind_groups[pass.variant], &[]);
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
            self.bundles_stale = false;
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
        self.record_segments(encoder, None);
        self.last_encode = t0.elapsed();
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
        let mut out = Vec::with_capacity(self.segments.len());
        for (si, seg) in self.segments.iter().enumerate() {
            let ms = ticks[2 * si + 1].saturating_sub(ticks[2 * si]) as f64 * period;
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

    /// Encode and submit in one command buffer (convenience).
    pub fn render(&mut self, ctx: &GpuContext) -> wgpu::SubmissionIndex {
        let mut encoder = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumenwall.frame"),
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

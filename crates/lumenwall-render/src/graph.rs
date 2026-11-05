//! Generic pass graph, backend-neutral front: a pass = shader + vertex inputs + blend + write
//! mask + culling + texture/sampler bindings + a uniform block.
//!
//! [`SceneBuilder`] collects the descriptors ([`crate::desc`]) and the CPU state; the GPU work
//! goes through a [`SceneBackend`] ([`crate::backend`]): the wgpu one
//! ([`crate::wgpu_scene::WgpuScene`], what [`SceneBuilder::new`] builds on) or another one
//! handed to [`SceneBuilder::with_backend`] (Direct3D 11 through the engine's bridge).
//!
//! Everything is created once by [`SceneBuilder::build`]. The front keeps the frame
//! operations, the render target plan (with the aliasing of transient targets and the frame
//! reordering), the uniform shadow and its patch lists, the vertices, slot-0 variants, enabled
//! flags and dirty flags; a frame is one call into the backend with a borrowed view of that
//! state, with no allocation and no GPU object creation.

use std::time::{Duration, Instant};

use crate::backend::{Frame, Gpu, MeshInfo, PassState, SceneBackend, StreamState};
pub use crate::backend::{FrameCommands, GraphCounts, SceneDesc, SegmentTime};
use crate::blend::WriteMask;
use crate::context::GpuContext;
pub use crate::desc::{
    DEPTH_FORMAT, DepthState, Extension, Geometry, IndexRange, MeshAttribute, MeshId, MeshLayout,
    Op, PassDesc, PassId, ShaderId, SkinRange, SlotBinding, StreamAttribute, StreamId,
    StreamLayout, TextureId, TextureRef,
};
use crate::dynbuf::{
    DynamicBufferBuilder, DynamicRegion, FrameInputs, Patch, Shadow, UniformBlock, UniformSlot,
    UniformType, VertexRange, frame_globals, patches_for,
};
use crate::error::{RenderError, Result};
use crate::hal::{Recording, WgpuReason};
use crate::math::{SkinVertex, Vertex};
use crate::quality::{Msaa, QualitySettings};
use crate::shader::{MAX_SLOTS, PreparedShader, ShaderDesc, ShaderInterface};
use crate::targets::{
    ALIAS_AREA_SLACK, Access, AliasPlan, AliasSize, RT_FORMAT_HDR, RenderTargetPool, RtId,
    plan_aliasing, texel_bytes,
};
use crate::texture::{GpuTexture, TextureInfo, TextureUpload, VolumeUpload};
pub use crate::wgpu_scene::DirectTimes;
use crate::wgpu_scene::WgpuScene;

/// A pass as the builder collects it.
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

/// Whose backend a builder uses: its own (a new scene) or the built scene's it extends.
enum Owner<'c> {
    New(Box<dyn SceneBackend>),
    Live(&'c mut RenderScene),
}

impl Owner<'_> {
    fn get(&mut self) -> &mut dyn SceneBackend {
        match self {
            Owner::New(b) => b.as_mut(),
            Owner::Live(scene) => scene.backend.as_mut(),
        }
    }

    fn get_ref(&self) -> &dyn SceneBackend {
        match self {
            Owner::New(b) => b.as_ref(),
            Owner::Live(scene) => scene.backend.as_ref(),
        }
    }
}

/// Load-time builder of a [`RenderScene`], also used to add passes to a built scene
/// ([`RenderScene::extend`]).
pub struct SceneBuilder<'c> {
    gpu: Gpu<'c>,
    /// Render targets: create scene, image and effect targets here.
    pub targets: RenderTargetPool,
    textures: Vec<TextureInfo>,
    shaders: Vec<ShaderInterface>,
    dynbuf: DynamicBufferBuilder,
    indices: Vec<u16>,
    skins: Vec<SkinVertex>,
    streams: Vec<StreamState>,
    meshes: Vec<MeshInfo>,
    depth_targets: Vec<RtId>,
    passes: Vec<PassRecord>,
    ops: Vec<Op>,
    scene_size: (f32, f32),
    owner: Owner<'c>,
    /// Id of the first pass added by this builder (passes of the extended scene come first).
    pass_base: u32,
    /// Quality settings (texture resolution applies to every texture added afterwards).
    quality: QualitySettings,
    /// First failed upload of a mesh or stream: returned by [`Self::build`].
    failed: Option<RenderError>,
}

impl<'c> SceneBuilder<'c> {
    /// `scene_size` = projection size `(W, H)`, used for `g_TexelSize`. The scene is built on
    /// wgpu ([`WgpuScene`]).
    #[must_use]
    pub fn new(ctx: &'c GpuContext, scene_size: (f32, f32)) -> Self {
        Self::with_backend(Gpu::Wgpu(ctx), Box::new(WgpuScene::new(ctx)), scene_size)
    }

    /// A builder on any backend. `gpu` is what the backend's calls receive and what the render
    /// target pool allocates on (`Gpu::Other` for a backend that holds its own device).
    #[must_use]
    pub fn with_backend(
        gpu: Gpu<'c>,
        backend: Box<dyn SceneBackend>,
        scene_size: (f32, f32),
    ) -> Self {
        let mut dynbuf = DynamicBufferBuilder::new(backend.uniform_alignment(gpu));
        let ts = [1.0 / scene_size.0, 1.0 / scene_size.1];
        let tsh = [0.5 / scene_size.0, 0.5 / scene_size.1];
        dynbuf
            .shadow_mut()
            .write_bytes(frame_globals::TEXEL_SIZE, bytemuck::cast_slice(&ts));
        dynbuf
            .shadow_mut()
            .write_bytes(frame_globals::TEXEL_SIZE_HALF, bytemuck::cast_slice(&tsh));
        Self {
            gpu,
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
            owner: Owner::New(backend),
            pass_base: 0,
            quality: QualitySettings::default(),
            failed: None,
        }
    }

    fn backend(&mut self) -> &mut dyn SceneBackend {
        self.owner.get()
    }

    /// The wgpu backend, when the scene is built on it.
    fn wgpu_backend(&self) -> Option<&WgpuScene> {
        self.owner.get_ref().as_any().downcast_ref()
    }

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
        // Layers created after load are 2D quads: a mesh added to a built scene is not kept.
        if matches!(self.owner, Owner::New(_)) {
            let gpu = self.gpu;
            if let Err(e) = self
                .backend()
                .add_mesh(gpu, label, &layout, vertices, indices)
            {
                tracing::error!(target: "lumenwall_render", "{label}: {e}");
                self.failed.get_or_insert(e);
            }
        }
        self.meshes.push(MeshInfo {
            layout,
            index_count: indices.len() as u32,
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
        let gpu = self.gpu;
        if let Err(e) = self
            .backend()
            .add_stream(gpu, label, &layout, max_vertices, indices)
        {
            tracing::error!(target: "lumenwall_render", "{label}: {e}");
            self.failed.get_or_insert(e);
        }
        let max_floats = max_vertices as usize * layout.stride_floats as usize;
        self.streams.push(StreamState {
            layout,
            max_floats,
            max_indices: indices.len() as u32,
            index_count: 0,
            vertices: vec![0.0; max_floats],
            pending: 0,
        });
        StreamId(self.streams.len() as u32 - 1)
    }

    /// Tells the backend the quality settings or the targets changed.
    fn quality_changed(&mut self) {
        self.owner
            .get()
            .quality_changed(self.gpu, &self.quality, &self.targets);
    }

    /// Quality settings: call before adding textures. Multisampling sends a scene on wgpu to
    /// its wgpu path ([`WgpuReason::Quality`]).
    pub fn set_quality(&mut self, quality: QualitySettings) {
        self.quality = quality;
        self.quality_changed();
    }

    /// 16-bit float render targets (scenes with `hdr`): bloom and effects keep values above 1.
    /// Call before creating any target. The direct Vulkan path has no float targets: a scene on
    /// wgpu records through wgpu ([`WgpuReason::Quality`]).
    pub fn use_hdr_targets(&mut self) {
        self.targets.set_format(RT_FORMAT_HDR);
        self.quality_changed();
    }

    /// Multisampled drawing for target `id` (the scene target): `msaa` clamped to what the
    /// backend supports for the target format. Returns the level in force. Call before the
    /// scene is built, after the target exists. Above 1 sample a scene on wgpu records through
    /// wgpu ([`WgpuReason::Quality`]); targets that are copy destinations stay single-sampled
    /// (see [`Self::build`]).
    pub fn enable_msaa(&mut self, id: RtId, msaa: Msaa) -> Msaa {
        let (gpu, format) = (self.gpu, self.targets.format());
        let backend = self.owner.get_ref();
        let level = msaa.clamp_to(|n| backend.supports_samples(gpu, format, n));
        if level != Msaa::Off {
            self.targets.set_samples(id, level.samples());
            self.quality_changed();
        }
        level
    }

    /// The quality settings in force.
    #[must_use]
    pub fn quality(&self) -> QualitySettings {
        self.quality
    }

    /// How a scene on wgpu records its frames ([`Recording::Auto`] by default, unless
    /// `LUMENWALL_DIRECT_VULKAN=0`). Call before adding shaders: [`Recording::Auto`] set later
    /// cannot make shaders already added usable by the direct path. No effect on other backends.
    pub fn set_recording(&mut self, recording: Recording) {
        if let Some(w) = self.backend().as_any_mut().downcast_mut::<WgpuScene>() {
            w.set_recording(recording);
        }
    }

    /// Frame operations appended so far ([`Self::push`] and the image helpers).
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// The uniform shadow passes write into: the extended scene's when extending.
    fn dyn_shadow(&mut self) -> &mut Shadow {
        match &mut self.owner {
            Owner::Live(scene) => scene.state.dynamic.shadow_mut(),
            Owner::New(_) => self.dynbuf.shadow_mut(),
        }
    }

    /// The wgpu context the scene is built on (the wgpu backend's convenience).
    ///
    /// # Panics
    ///
    /// On a builder of another backend ([`Self::with_backend`] with `Gpu::Other`): use
    /// [`Self::gpu`] there.
    #[must_use]
    pub fn context(&self) -> &'c GpuContext {
        self.gpu
            .wgpu()
            .expect("SceneBuilder::context on a scene that is not built on wgpu")
    }

    /// The device the scene is built on (what the render target pool's calls take).
    #[must_use]
    pub fn gpu(&self) -> Gpu<'c> {
        self.gpu
    }

    #[must_use]
    pub fn scene_size(&self) -> (f32, f32) {
        self.scene_size
    }

    /// Compile a shader (reflection, clip fix, validation). An error means the
    /// object using it must be dropped (spec 03 §10.4); the scene goes on.
    pub fn add_shader(&mut self, desc: ShaderDesc) -> Result<ShaderId> {
        self.add_prepared_shader(PreparedShader::new(desc)?)
    }

    /// [`Self::add_shader`] for a shader already prepared ([`PreparedShader::new`]).
    pub fn add_prepared_shader(&mut self, shader: PreparedShader) -> Result<ShaderId> {
        let interface = shader.interface.clone();
        let gpu = self.gpu;
        self.backend().add_shader(gpu, shader)?;
        self.shaders.push(interface);
        Ok(ShaderId(self.shaders.len() as u32 - 1))
    }

    #[must_use]
    pub fn shader_interface(&self, id: ShaderId) -> Option<&ShaderInterface> {
        self.shaders.get(id.index())
    }

    /// Upload a texture; the caller may drop its CPU copy right after.
    pub fn add_texture(&mut self, upload: &TextureUpload<'_>) -> Result<TextureId> {
        let skip = self.quality.texture_levels_to_skip(upload.levels.len());
        let gpu = self.gpu;
        let info = if skip == 0 {
            self.backend().add_texture(gpu, upload)?
        } else {
            // Texture resolution: the top levels are not uploaded. The declared content size
            // stays the original one, so texture coordinates and `g_Texture0Resolution` do not
            // change.
            let l0 = upload.levels[0];
            self.backend().add_texture(
                gpu,
                &TextureUpload {
                    levels: &upload.levels[skip..],
                    content_size: upload.content_size.or(Some((l0.width, l0.height))),
                    ..*upload
                },
            )?
        };
        self.textures.push(info);
        Ok(TextureId(self.textures.len() as u32 - 1))
    }

    /// Upload a volume (3D) texture, bound like any texture: slots the shader declares as
    /// `texture_3d` sample it, 2D slots read the default texture instead.
    pub fn add_volume_texture(&mut self, upload: &VolumeUpload<'_>) -> Result<TextureId> {
        let gpu = self.gpu;
        let info = self.backend().add_volume_texture(gpu, upload)?;
        self.textures.push(info);
        Ok(TextureId(self.textures.len() as u32 - 1))
    }

    /// The wgpu texture of `id` (the wgpu backend's convenience; `None` on other backends).
    #[must_use]
    pub fn texture(&self, id: TextureId) -> Option<&GpuTexture> {
        self.wgpu_backend()?.texture(id)
    }

    /// What the front knows of texture `id` (sizes, format, sampling).
    #[must_use]
    pub fn texture_info(&self, id: TextureId) -> Option<&TextureInfo> {
        self.textures.get(id.index())
    }

    /// Vertices in the dynamic region (rewritable per frame).
    pub fn add_vertices(&mut self, v: &[Vertex]) -> VertexRange {
        match &mut self.owner {
            Owner::Live(scene) => scene.state.dynamic.alloc_vertices(v),
            Owner::New(_) => self.dynbuf.alloc_vertices(v),
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
                .get(t.index())
                .map_or([1.0; 4], TextureInfo::resolution),
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
            .get(desc.shader.index())
            .ok_or(RenderError::BadHandle("shader"))?;
        if self.targets.try_get(desc.target).is_none() {
            return Err(RenderError::BadHandle("render target"));
        }
        if let Geometry::Stream(s) = desc.geometry
            && self.streams.get(s.index()).is_none()
        {
            return Err(RenderError::BadHandle("vertex stream"));
        }
        if let Geometry::Mesh(m) = desc.geometry
            && self.meshes.get(m.index()).is_none()
        {
            return Err(RenderError::BadHandle("mesh"));
        }
        let size = shader.uniform_size;
        let uniform = size.map(|s| match &mut self.owner {
            Owner::Live(scene) => scene.state.dynamic.alloc_uniform(s),
            Owner::New(_) => self.dynbuf.alloc_uniform(s),
        });
        if let Some(block) = uniform {
            let fields = self.shaders[desc.shader.index()].uniforms.clone();
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
            return match &self.owner {
                Owner::Live(scene) => scene.uniform(pass, name),
                Owner::New(_) => None,
            };
        };
        let rec = self.passes.get(local as usize)?;
        let block = rec.uniform?;
        let shader = self.shaders.get(rec.desc.shader.index())?;
        shader.field(name).map(|f| f.slot(block.offset))
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
        if let Some(e) = self.failed.take() {
            return Err(e);
        }
        let gpu = self.gpu;
        // Fewer render passes (chains first, scene draws together), then GPU memory of the
        // render targets, transient ones sharing textures over the new lifetimes.
        self.reorder_ops();
        // A copy into a multisampled target would leave its companion texture stale: that
        // target stays single-sampled.
        for op in &self.ops {
            if let Op::Copy { dst, .. } = *op
                && self.targets.get(dst).samples() > 1
            {
                tracing::warn!("multisampling off for a render target that is a copy destination");
                self.targets.set_samples(dst, 1);
            }
        }
        let alias = self.alias_plan();
        self.targets.materialize(gpu, &alias.shares, &alias.sizes);

        // Frame-global copies into pass blocks.
        let mut patches = Vec::new();
        for rec in &self.passes {
            let (Some(block), Some(shader)) =
                (rec.uniform, self.shaders.get(rec.desc.shader.index()))
            else {
                continue;
            };
            for f in &shader.uniforms {
                if frame_globals::FIELDS.iter().any(|g| g.0 == f.name) {
                    let slot = f.slot(block.offset);
                    if !patches_for(&f.name, &slot, &mut patches) {
                        tracing::warn!(target: "lumenwall_render", "{}: unsupported layout for {}", rec.desc.label, f.name);
                    }
                }
            }
        }
        let SceneBuilder {
            targets,
            textures,
            shaders,
            dynbuf,
            indices,
            skins,
            streams,
            meshes,
            depth_targets,
            passes,
            ops,
            scene_size,
            owner,
            quality,
            ..
        } = self;
        let Owner::New(backend) = owner else {
            return Err(RenderError::Unsupported(
                "an extension is finished by RenderScene::extend".into(),
            ));
        };
        let passes: Vec<PassState> = passes
            .into_iter()
            .map(|rec| pass_state(rec, &shaders))
            .collect();
        let mut scene = RenderScene {
            backend,
            state: SceneState {
                quality,
                targets,
                textures,
                interfaces: shaders,
                dynamic: dynbuf.finish(),
                indices,
                skins,
                streams,
                meshes,
                depth_targets,
                passes,
                ops,
                patches,
                dirty: true,
                settle_left: 0,
                settle_frames: 0,
                feedback: false,
                accumulates: false,
                last_encode: Duration::ZERO,
                scene_size,
                frame_stale: false,
                aliased: !alias.shares.is_empty(),
                unshare_pending: false,
                textures_written: false,
            },
        };
        let state = &mut scene.state;
        // The first draw into each shared texture clears it (see `AliasPlan::clear_first`).
        let mut pending = alias.clear_first;
        for op in &state.ops {
            if let Op::Draw(p) = op
                && let Some(pass) = state.passes.get_mut(p.index())
                && pending.remove(&pass.desc.target)
            {
                pass.clear_first = true;
            }
        }
        state.analyse_ops();
        for i in 0..state.passes.len() {
            state.hazard_for(gpu, i);
        }
        scene.backend.build(gpu, &scene.state.desc())?;
        let state = &mut scene.state;
        // The backend holds its copy of the region and of the static data now.
        state.dynamic.clear_growth();
        state.dynamic.shadow_mut().clear_dirty();
        state.indices = Vec::new();
        state.skins = Vec::new();
        Ok(scene)
    }

    /// Render targets a pass samples: bound in a slot its shader declares (a material may
    /// name `_rt_FullFrameBuffer` for a slot the compiled combination never samples; the
    /// graph does not bind it, so it is no dependency), or among its slot-0 variants.
    fn sampled_targets<'a>(&'a self, rec: &'a PassRecord) -> impl Iterator<Item = RtId> + 'a {
        let mask = self
            .shaders
            .get(rec.desc.shader.index())
            .map_or(0, |s| s.slots);
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
        if matches!(self.owner, Owner::Live(_))
            || !self.targets.has_transient()
            || self.ops.len() < 3
        {
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
        let format = targets.format();
        let order = crate::schedule::schedule(
            &accesses,
            |t| !targets.is_transient(RtId(t)),
            |t| {
                let id = RtId(t);
                targets.is_transient(id).then(|| {
                    let (w, h) = targets.get(id).size();
                    u64::from(w) * u64::from(h) * texel_bytes(format)
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
    fn finish_extension(self) -> Result<Extension> {
        let SceneBuilder {
            gpu,
            targets,
            textures,
            shaders,
            indices,
            skins,
            streams,
            passes,
            ops,
            owner,
            failed,
            ..
        } = self;
        let Owner::Live(scene) = owner else {
            return Err(RenderError::Unsupported(
                "a new scene is finished by SceneBuilder::build".into(),
            ));
        };
        let state = &mut scene.state;
        state.targets = targets;
        state.textures = textures;
        state.streams = streams;
        state.interfaces = shaders;
        let first = state.passes.len();
        let refused = if !indices.is_empty() || !skins.is_empty() {
            Some(RenderError::Pass {
                label: "extension".into(),
                reason: "indexed or skinned geometry cannot be added to a built scene".into(),
            })
        } else {
            failed
        };
        if refused.is_none() {
            for rec in &passes {
                let (Some(block), Some(shader)) =
                    (rec.uniform, state.interfaces.get(rec.desc.shader.index()))
                else {
                    continue;
                };
                for f in &shader.uniforms {
                    if frame_globals::FIELDS.iter().any(|g| g.0 == f.name) {
                        patches_for(&f.name, &f.slot(block.offset), &mut state.patches);
                    }
                }
            }
            for rec in passes {
                let pass = pass_state(rec, &state.interfaces);
                state.passes.push(pass);
            }
            for i in first..state.passes.len() {
                state.hazard_for(gpu, i);
            }
        }
        // Even with nothing to compile, the backend follows the shaders, textures and streams
        // added, and the dynamic region's growth.
        let rebound = state.dynamic.grew() || state.dynamic.binding_grew();
        let compiled = scene.backend.extend(gpu, &scene.state.desc(), first);
        let state = &mut scene.state;
        state.dynamic.clear_growth();
        if rebound {
            // Bundles recorded the old buffer or binding size: re-recorded before the next frame.
            state.frame_stale = true;
        }
        if let Some(e) = refused {
            return Err(e);
        }
        if compiled.is_err() {
            state.passes.truncate(first);
        }
        state.dirty = true;
        compiled?;
        Ok(Extension {
            passes: (first..state.passes.len())
                .map(|i| PassId(i as u32))
                .collect(),
            ops,
        })
    }
}

/// The front's state of a new pass.
fn pass_state(rec: PassRecord, shaders: &[ShaderInterface]) -> PassState {
    let res0 = rec.uniform.and_then(|b| {
        shaders
            .get(rec.desc.shader.index())?
            .field(RESOLUTION_NAMES[0])
            .map(|f| f.slot(b.offset))
    });
    PassState {
        variants: rec.desc.slot0_variants.len().max(1),
        desc: rec.desc,
        uniform: rec.uniform,
        variant: 0,
        enabled: true,
        clear_first: false,
        hazard_scratch: None,
        res0,
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

/// The front's CPU state of a built scene.
struct SceneState {
    /// Quality settings the scene was built with (textures added by extensions follow them).
    quality: QualitySettings,
    targets: RenderTargetPool,
    textures: Vec<TextureInfo>,
    interfaces: Vec<ShaderInterface>,
    dynamic: DynamicRegion,
    /// Static data, handed to the backend at build and dropped afterwards.
    indices: Vec<u16>,
    skins: Vec<SkinVertex>,
    streams: Vec<StreamState>,
    meshes: Vec<MeshInfo>,
    depth_targets: Vec<RtId>,
    passes: Vec<PassState>,
    ops: Vec<Op>,
    patches: Vec<Patch>,
    dirty: bool,
    settle_left: u32,
    settle_frames: u32,
    feedback: bool,
    accumulates: bool,
    last_encode: Duration,
    scene_size: (f32, f32),
    /// The frame must be planned again before the next frame (operations changed).
    frame_stale: bool,
    /// Some transient targets share a texture (planned on the frame order of the build).
    aliased: bool,
    /// The frame order changed: shared targets get their own textures before the next frame.
    unshare_pending: bool,
    /// [`RenderScene::mark_dirty`] since the last frame: textures may have been written.
    textures_written: bool,
}

impl SceneState {
    fn desc(&self) -> SceneDesc<'_> {
        SceneDesc {
            scene_size: self.scene_size,
            quality: self.quality,
            targets: &self.targets,
            depth_targets: &self.depth_targets,
            textures: &self.textures,
            shaders: &self.interfaces,
            passes: &self.passes,
            ops: &self.ops,
            dynamic: &self.dynamic,
            indices: &self.indices,
            skins: &self.skins,
            streams: &self.streams,
            meshes: &self.meshes,
            textures_written: self.textures_written,
        }
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
                    if let Some(pass) = self.passes.get(p.index()) {
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
                    let Some(pass) = self.passes.get(p.index()) else {
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

    /// Scratch copy of the target of pass `i` when it samples it (spec 04 §24.6).
    fn hazard_for(&mut self, gpu: Gpu<'_>, i: usize) {
        let desc = &self.passes[i].desc;
        let target = desc.target;
        // Only slots the shader samples (the others are not bound).
        let mask = self
            .interfaces
            .get(desc.shader.index())
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
            Some(self.targets.scratch(gpu, size))
        } else {
            None
        };
        self.passes[i].hazard_scratch = scratch;
    }
}

/// A compiled scene graph ready to render frames: the front's CPU state and its backend.
pub struct RenderScene {
    /// First field: dropped (after the GPU is done with its frames) before the render targets
    /// the frames used.
    backend: Box<dyn SceneBackend>,
    state: SceneState,
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
    /// The wgpu backend, when the scene is built on it.
    fn wgpu_backend(&self) -> Option<&WgpuScene> {
        self.backend.as_any().downcast_ref()
    }

    /// The scene's backend (owners downcast it to reach their own API).
    #[must_use]
    pub fn backend(&self) -> &dyn SceneBackend {
        self.backend.as_ref()
    }

    pub fn backend_mut(&mut self) -> &mut dyn SceneBackend {
        self.backend.as_mut()
    }

    /// The front's whole state, borrowed: what the backend compiled and renders.
    #[must_use]
    pub fn desc(&self) -> SceneDesc<'_> {
        self.state.desc()
    }

    /// Rebuild bind groups, hazard copies, segments and bundles. Call after
    /// any target reallocation or sampler change (event-driven, not per frame).
    pub fn rebuild_bindings<'g>(&mut self, gpu: impl Into<Gpu<'g>>) -> Result<()> {
        let gpu: Gpu<'g> = gpu.into();
        // Hazards: a pass sampling its own destination reads a copy (spec 04 §24.6).
        for i in 0..self.state.passes.len() {
            self.state.hazard_for(gpu, i);
        }
        let r = self.backend.rebind(gpu, &self.state.desc());
        if r.is_ok() {
            self.state.frame_stale = false;
        }
        self.state.dirty = true;
        r
    }

    /// Adds passes to the built scene (layers created after load, e.g. SceneScript
    /// `thisScene.createLayer`). `f` receives a builder whose `targets`, textures, shaders,
    /// streams and dynamic region are this scene's: already compiled shaders and pipelines (by
    /// shader, blend, write mask, cull) and layouts are reused by the backend, and only the new
    /// passes get pipelines (from the cache when possible) and bindings. Uniform blocks and
    /// vertices are appended to the dynamic region, which grows with headroom (a reallocation
    /// re-records the frame before the next one).
    ///
    /// The frame operations `f` pushes are returned, not placed: put them where the layer
    /// belongs with [`Self::set_ops`]. Indexed and skinned geometry (puppet meshes) cannot be
    /// added. On error the new passes are dropped.
    pub fn extend<'a, R>(
        &'a mut self,
        gpu: impl Into<Gpu<'a>>,
        f: impl FnOnce(&mut SceneBuilder<'a>) -> R,
    ) -> (R, Result<Extension>) {
        let gpu: Gpu<'a> = gpu.into();
        let align = self.state.dynamic.alignment();
        let state = &mut self.state;
        let mut b = SceneBuilder {
            gpu,
            targets: std::mem::take(&mut state.targets),
            textures: std::mem::take(&mut state.textures),
            shaders: std::mem::take(&mut state.interfaces),
            dynbuf: DynamicBufferBuilder::new(align),
            indices: Vec::new(),
            skins: Vec::new(),
            streams: std::mem::take(&mut state.streams),
            // Layers created after load are 2D quads: no new mesh and no new depth target.
            meshes: Vec::new(),
            depth_targets: Vec::new(),
            passes: Vec::new(),
            ops: Vec::new(),
            scene_size: state.scene_size,
            pass_base: state.passes.len() as u32,
            quality: state.quality,
            failed: None,
            owner: Owner::Live(self),
        };
        let r = f(&mut b);
        (r, b.finish_extension())
    }

    /// Frame operations in order.
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.state.ops
    }

    /// Replaces the frame operations (layers added, removed or reordered after load). Passes
    /// left out are kept, compiled, for a later reuse. Cross-frame dependencies are analysed
    /// again; the frame is planned again once, before the next frame.
    pub fn set_ops(&mut self, ops: Vec<Op>) {
        let s = &mut self.state;
        s.ops = ops;
        s.accumulates = false;
        s.feedback = false;
        s.analyse_ops();
        s.frame_stale = true;
        s.dirty = true;
        // Lifetimes were planned on the old order (lumenwall-engine marks no transient target
        // in scenes whose scripts reorder layers; this keeps any other caller correct).
        s.unshare_pending |= s.aliased;
    }

    /// Plans the frame now if the structure changed (also done by [`Self::encode`]).
    pub fn refresh<'g>(&mut self, gpu: impl Into<Gpu<'g>>) {
        let gpu: Gpu<'g> = gpu.into();
        if self.state.unshare_pending {
            let s = &mut self.state;
            s.unshare_pending = false;
            s.aliased = false;
            s.targets.unshare(gpu);
            for p in &mut s.passes {
                p.clear_first = false;
            }
            if let Err(e) = self.rebuild_bindings(gpu) {
                tracing::error!(target: "lumenwall_render", "render targets not rebound: {e}");
            }
            return;
        }
        if self.state.frame_stale {
            self.backend.replan(gpu, &self.state.desc());
            self.state.frame_stale = false;
        }
    }

    /// Feed the per-frame engine values (frame globals + pass copies).
    /// No allocation; marks the scene dirty only if a value changed.
    pub fn update_frame(&mut self, inputs: &FrameInputs<'_>) {
        let s = &mut self.state;
        s.dynamic.update_frame(inputs, &s.patches);
    }

    /// Uniform writes by slot (matrices, colours, properties).
    pub fn shadow_mut(&mut self) -> &mut Shadow {
        self.state.dynamic.shadow_mut()
    }

    /// Rewrite the vertices of a range (image rectangle moved).
    pub fn set_vertices(&mut self, range: VertexRange, v: &[Vertex]) {
        self.state.dynamic.set_vertices(range, v);
    }

    /// Set this frame's vertices of a stream and how many of its indices to draw (clamped to
    /// the capacities); they are uploaded with the frame. No allocation; an empty stream draws
    /// nothing. `gpu` is unused (kept for the callers).
    pub fn write_stream<'g>(
        &mut self,
        _gpu: impl Into<Gpu<'g>>,
        id: StreamId,
        vertices: &[f32],
        index_count: u32,
    ) {
        let Some(s) = self.state.streams.get_mut(id.index()) else {
            return;
        };
        let floats = vertices.len().min(s.max_floats);
        let count = index_count.min(s.max_indices);
        if count == 0 && s.index_count == 0 {
            return;
        }
        // Kept and uploaded with the frame: no staging buffer per call.
        if floats > 0 && count > 0 {
            s.vertices[..floats].copy_from_slice(&vertices[..floats]);
            s.pending = floats;
        }
        s.index_count = count;
        self.state.dirty = true;
    }

    /// Select the slot-0 variant of a pass (frame of an animated texture).
    /// Also updates `g_Texture0Resolution` (spec 04 §12.5).
    pub fn set_variant(&mut self, pass: PassId, variant: usize) {
        let s = &mut self.state;
        let Some(p) = s.passes.get_mut(pass.index()) else {
            return;
        };
        let v = variant.min(p.variants.saturating_sub(1));
        if p.variant == v {
            return;
        }
        p.variant = v;
        s.dirty = true;
        if let (Some(slot), Some(r)) = (p.res0.clone(), p.desc.slot0_variants.get(v).copied()) {
            let res = match r {
                TextureRef::Texture(t) => s
                    .textures
                    .get(t.index())
                    .map_or([1.0; 4], TextureInfo::resolution),
                TextureRef::Target(t) => s.targets.get(t).resolution(),
                TextureRef::Default => [1.0; 4],
            };
            s.dynamic.shadow_mut().write_floats(&slot, &res);
        }
    }

    /// Enable or disable passes (image visibility, spec 04 Q8). Disabled
    /// passes are not recorded. Plans the frame again when something changed: call it on
    /// visibility events, not every frame.
    pub fn set_enabled<'g>(&mut self, gpu: impl Into<Gpu<'g>>, passes: &[PassId], enabled: bool) {
        let mut changed = false;
        for p in passes {
            if let Some(cp) = self.state.passes.get_mut(p.index())
                && cp.enabled != enabled
            {
                cp.enabled = enabled;
                changed = true;
            }
        }
        if changed {
            self.backend.replan(gpu.into(), &self.state.desc());
            self.state.frame_stale = false;
            self.state.dirty = true;
        }
    }

    /// Something outside the uniform buffer changed (video frame written into
    /// a texture, thumbnail loaded…). Call it after every write into one of the scene's
    /// textures or targets through wgpu (`GpuTexture::write_level0`, a queue write or a copy
    /// into a target): a scene recorded directly in Vulkan then puts them back in their sampled
    /// state before the next frame. Readbacks through [`crate::headless::read_texture_rgba8`]
    /// need nothing.
    pub fn mark_dirty(&mut self) {
        self.state.dirty = true;
        self.state.textures_written = true;
    }

    /// Whether the scene records its frames directly in Vulkan ([`crate::hal`]; `false` on
    /// backends other than wgpu).
    #[must_use]
    pub fn records_direct(&self) -> bool {
        self.wgpu_backend().is_some_and(WgpuScene::records_direct)
    }

    /// Why a scene on wgpu records through wgpu (`None` when it records directly in Vulkan, and
    /// on other backends).
    #[must_use]
    pub fn wgpu_reason(&self) -> Option<WgpuReason> {
        self.wgpu_backend().and_then(WgpuScene::wgpu_reason)
    }

    /// Counters of the direct path (`None` on the wgpu path and on other backends).
    #[must_use]
    pub fn direct_stats(&self) -> Option<crate::hal::DirectStats> {
        self.wgpu_backend()?.direct_stats()
    }

    /// Whether the next frame would differ from the last one. When `false`,
    /// the caller can skip rendering and presenting entirely.
    #[must_use]
    pub fn needs_redraw(&self) -> bool {
        let s = &self.state;
        s.dirty || s.dynamic.shadow().is_dirty() || s.settle_left > 0
    }

    /// Override the number of frames rendered after the last change.
    pub fn set_settle_frames(&mut self, frames: u32) {
        self.state.settle_frames = frames;
    }

    /// Whether some pass accumulates across frames (spec 04 §15).
    #[must_use]
    pub fn accumulates(&self) -> bool {
        self.state.accumulates
    }

    /// Upload the dirty uniforms and record the frame into `encoder`. A scene recorded
    /// directly in Vulkan submits `encoder`'s commands so far (with the frame's uploads) and its
    /// frame here, and gives the caller a fresh encoder for what follows (composition): use
    /// [`Self::encode_frame`] to submit everything at once instead.
    pub fn encode(&mut self, ctx: &GpuContext, encoder: &mut wgpu::CommandEncoder) {
        let commands = self.encode_frame(ctx, encoder);
        let mut submit = Duration::ZERO;
        if !commands.is_empty() {
            let t = Instant::now();
            ctx.queue().submit(commands);
            submit = t.elapsed();
        }
        if let Some(w) = self.backend.as_any_mut().downcast_mut::<WgpuScene>() {
            w.set_submit_time(submit);
        }
    }

    /// CPU split of the last frame recorded directly in Vulkan (zero on the wgpu path and on
    /// other backends).
    #[must_use]
    pub fn last_direct_times(&self) -> DirectTimes {
        self.wgpu_backend()
            .map_or_else(DirectTimes::default, WgpuScene::last_direct_times)
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
        let mut commands = FrameCommands::default();
        self.render_frame(&mut Frame::Wgpu {
            ctx,
            encoder,
            commands: &mut commands,
        });
        commands
    }

    /// Render one frame on any backend: plans the frame again if its structure changed, then
    /// one call into the backend with the scene's state (dirty uniform range, pending stream
    /// vertices, variants, enabled flags). No allocation.
    pub fn render_frame(&mut self, frame: &mut Frame<'_>) {
        let t0 = Instant::now();
        self.refresh(frame.gpu());
        let s = &mut self.state;
        let changed =
            s.dynamic.shadow().is_dirty() || s.streams.iter().any(|st| st.pending > 0) || s.dirty;
        s.dirty = false;
        let replanned = self.backend.render(frame, &self.state.desc());
        let s = &mut self.state;
        if changed || replanned {
            s.settle_left = s.settle_frames;
        } else {
            s.settle_left = s.settle_left.saturating_sub(1);
        }
        // The backend uploaded them with the frame.
        s.dynamic.shadow_mut().clear_dirty();
        for st in &mut s.streams {
            st.pending = 0;
        }
        s.textures_written = false;
        s.last_encode = t0.elapsed();
    }

    /// Debug profile of one frame (tools, never the live loop): renders the frame alone with
    /// a timestamp around each render pass and copy, waits for the GPU and returns, in frame
    /// order, `(target label, target size, passes, first pass label, milliseconds)`. `None`
    /// when the device has no timestamp queries.
    pub fn profile_frame<'g>(&mut self, gpu: impl Into<Gpu<'g>>) -> Option<Vec<SegmentTime>> {
        let gpu: Gpu<'g> = gpu.into();
        self.refresh(gpu);
        let r = self.backend.profile(gpu, &self.state.desc());
        // The backend uploaded the dirty uniforms.
        self.state.dynamic.shadow_mut().clear_dirty();
        r
    }

    /// RGBA8 rows of a render target of the scene, tightly packed (screenshots, tests).
    pub fn read_target<'g>(&mut self, gpu: impl Into<Gpu<'g>>, target: RtId) -> Result<Vec<u8>> {
        self.backend
            .read_target(gpu.into(), &self.state.desc(), target)
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
        self.state.last_encode
    }

    #[must_use]
    pub fn targets(&self) -> &RenderTargetPool {
        &self.state.targets
    }

    /// Mutable access for resizes and sampler changes; call
    /// [`Self::rebuild_bindings`] afterwards.
    pub fn targets_mut(&mut self) -> &mut RenderTargetPool {
        &mut self.state.targets
    }

    /// The wgpu texture of `id` (the wgpu backend's convenience; `None` on other backends).
    #[must_use]
    pub fn texture(&self, id: TextureId) -> Option<&GpuTexture> {
        self.wgpu_backend()?.texture(id)
    }

    /// What the front knows of texture `id` (sizes, format, sampling).
    #[must_use]
    pub fn texture_info(&self, id: TextureId) -> Option<&TextureInfo> {
        self.state.textures.get(id.index())
    }

    /// Write handle of a uniform of a pass.
    #[must_use]
    pub fn uniform(&self, pass: PassId, name: &str) -> Option<UniformSlot> {
        let p = self.state.passes.get(pass.index())?;
        let block = p.uniform?;
        self.state.interfaces[p.desc.shader.index()]
            .field(name)
            .map(|f| f.slot(block.offset))
    }

    /// Number of passes, render passes and bundles (debug counters).
    #[must_use]
    pub fn counts(&self) -> GraphCounts {
        GraphCounts {
            passes: self.state.passes.len(),
            ..self.backend.counts()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynbuf::ScalarKind;

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

//! Render-ready descriptors of a scene: plain data, with no device behind them, that every
//! backend compiles ([`crate::backend`]). wgpu enums appear as plain data only
//! (`wgpu::FrontFace`, `wgpu::VertexFormat`, `wgpu::TextureFormat`): no wgpu object is
//! involved.

use crate::blend::{BlendMode, CullMode, FRONT_FACE, WriteMask};
use crate::dynbuf::VertexRange;
use crate::shader::MAX_SLOTS;
use crate::targets::RtId;
use crate::texture::SamplerKey;

/// Handle of a compiled shader in a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShaderId(pub(crate) u32);

/// Handle of an uploaded texture in a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureId(pub(crate) u32);

/// Handle of a pass in a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PassId(pub(crate) u32);

/// Handle of a static mesh of a scene ([`crate::graph::SceneBuilder::add_mesh`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeshId(pub(crate) u32);

/// Handle of a vertex stream of a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(pub(crate) u32);

macro_rules! handle_index {
    ($($t:ident),*) => {$(
        impl $t {
            /// Position in the scene's list (the order of the `add_*` calls, from 0): what a
            /// backend indexes its own objects with.
            #[must_use]
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    )*};
}
handle_index!(ShaderId, TextureId, PassId, MeshId, StreamId);

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
    /// 32-bit indices, draw count set every frame by
    /// [`crate::graph::RenderScene::write_stream`].
    Stream(StreamId),
    /// [`Geometry::Indexed`] plus per-vertex skinning attributes (puppets, spec 08 §9.8):
    /// shader inputs `a_BlendIndices`, `a_BlendWeights`, `a_Normal` and `a_Tangent4` read the
    /// static skin buffer, record `k` belonging to vertex `vertices.first + k`.
    Skinned {
        vertices: VertexRange,
        indices: IndexRange,
        skin: SkinRange,
    },
    /// Every triangle of a static mesh ([`crate::graph::SceneBuilder::add_mesh`]): its own
    /// interleaved vertex layout, 16-bit indices.
    Mesh(MeshId),
}

/// Format of the depth buffers (spec 09 §3.1).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Depth test and write of a pass whose target has a depth buffer
/// ([`crate::graph::SceneBuilder::add_depth`], spec 09 §3.1). Ignored for targets without one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DepthState {
    /// Test "less or equal" against the buffer; `false` = always passes.
    pub test: bool,
    /// Write the fragment depth.
    pub write: bool,
}

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

/// Records of the static skin buffer ([`crate::graph::SceneBuilder::add_skin`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkinRange {
    pub first: u32,
    pub count: u32,
}

/// Skin buffer layout of a shader input: `(name, byte offset in SkinVertex)`: the index
/// record first (integers), then three float vectors.
pub const SKIN_ATTRIBUTES: [(&str, u64); 4] = [
    ("a_BlendIndices", 0),
    ("a_BlendWeights", 16),
    ("a_Normal", 32),
    ("a_Tangent4", 48),
];

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
    /// frame with [`crate::graph::RenderScene::set_variant`]. Empty = no variants.
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

/// Passes and frame operations added by [`crate::graph::RenderScene::extend`]. The operations
/// are not placed in the frame: the caller inserts them where the layer belongs
/// ([`crate::graph::RenderScene::set_ops`]).
#[derive(Clone, Debug, Default)]
pub struct Extension {
    pub passes: Vec<PassId>,
    pub ops: Vec<Op>,
}

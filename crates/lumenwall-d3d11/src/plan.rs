//! The plan of a scene: what `lumenwall-render`'s neutral front compiles a scene to, as plain data
//! with no wgpu or Direct3D type. It is the contract between the bridge in `lumenwall-engine`
//! (task D3D11 5/n, producer) and the scene executor (`scene`, task 3/n, consumer). See the crate
//! README, "Split proposal".
//!
//! - Indices refer to what the executor's builder returned (textures, programs, meshes) and to
//!   the positions in [`ScenePlan`]'s vectors (targets, passes, streams).
//! - Uniform windows are byte ranges of the front's shadow buffer (`dynbuf::Shadow`), 256-byte
//!   aligned like wgpu's dynamic offsets: the executor mirrors the shadow into its
//!   `ConstantArena` byte for byte, and binds the same windows.
//! - Vertex attributes are matched to program inputs **by name** (`VertexInput::name`), as on the
//!   wgpu path; an input no attribute feeds reads `(0, 0, 0, 1)`.

use std::ops::Range;

use crate::error::{Error, Result};
use crate::hlsl::MAX_SLOTS;
use crate::layout::{CONSTANT_WINDOW_ALIGN, ConstantRegion, TargetFormat, VertexFormat};
use crate::state::{BlendKey, DepthKey, RasterKey, SamplerKey};

/// Bytes of one image vertex of the dynamic vertex region (`math::Vertex`: position, uv).
pub const IMAGE_VERTEX_STRIDE: u32 = 20;
/// Attributes of the image vertex region.
pub const IMAGE_VERTEX_ATTRIBUTES: [(&str, VertexFormat, u32); 2] = [
    ("a_Position", VertexFormat::Float32x3, 0),
    ("a_TexCoord", VertexFormat::Float32x2, 12),
];
/// Bytes of one skin record (`math::SkinVertex`).
pub const SKIN_STRIDE: u32 = 64;
/// Attributes of a skin record: `(name, byte offset)`; the format follows the program input
/// (integer `a_BlendIndices`, float for the others, as `lumenwall-render` does).
pub const SKIN_ATTRIBUTES: [(&str, u32); 4] = [
    ("a_BlendIndices", 0),
    ("a_BlendWeights", 16),
    ("a_Normal", 32),
    ("a_Tangent4", 48),
];

/// A render target of the scene.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetPlan {
    pub label: String,
    /// Size of the texture drawn into (for a flexible target sharing a larger texture, that
    /// texture's size).
    pub size: (u32, u32),
    pub format: TargetFormat,
    /// 1 = no MSAA (the MSAA scene target of `QualitySettings::msaa` has 2, 4 or 8).
    pub samples: u32,
    /// A depth buffer of the same size and samples (`SceneBuilder::add_depth`).
    pub depth: bool,
    /// Aliasing (`plan_aliasing`): draw into the texture of this other target, which has
    /// `shares: None`. Their frame lifetimes never overlap.
    pub shares: Option<usize>,
}

/// What a texture slot samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureSource {
    /// A texture added with the builder.
    Texture(usize),
    /// A render target of the plan (its resolved texture with MSAA).
    Target(usize),
    /// The 1×1 `(0, 0, 0, 1)` 2D texture.
    Default2D,
    /// The 1×1×1 `(0, 0, 0, 1)` volume (a 3D slot given no volume).
    Default3D,
}

/// One bound slot (`tN` + `sN`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotPlan {
    pub source: TextureSource,
    /// The sampler the front resolved (the texture's or target's own, or the pass override).
    pub sampler: SamplerKey,
}

/// Geometry of a pass. Image vertices are triangle lists of the dynamic vertex region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryPlan {
    /// `count` vertices from vertex `first`.
    Vertices { first: u32, count: u32 },
    /// 16-bit indices relative to `first_vertex`.
    Indexed {
        first_vertex: u32,
        first_index: u32,
        index_count: u32,
    },
    /// [`GeometryPlan::Indexed`] plus skin records, record `k` for vertex `first_vertex + k`.
    Skinned {
        first_vertex: u32,
        first_index: u32,
        index_count: u32,
        first_skin: u32,
    },
    /// A stream of the plan; the index count comes with each frame.
    Stream(usize),
    /// A mesh added with the builder, drawn whole.
    Mesh(usize),
}

/// One pass (one draw).
#[derive(Clone, Debug, PartialEq)]
pub struct PassPlan {
    pub label: String,
    /// A program added with the builder.
    pub program: usize,
    pub target: usize,
    pub blend: BlendKey,
    pub raster: RasterKey,
    /// Ignored when the target has no depth buffer.
    pub depth: DepthKey,
    /// Window of the pass uniform block (`b0`) in the shadow.
    pub uniform: Option<ConstantRegion>,
    pub slots: [Option<SlotPlan>; MAX_SLOTS as usize],
    /// Alternatives for slot 0 (animated textures), chosen by [`FrameInput::variants`]; empty =
    /// slot 0 as given.
    pub slot0_variants: Vec<TextureSource>,
    pub geometry: GeometryPlan,
    /// The pass samples its own target: copy the target into this scratch target before the
    /// draw and sample the copy (only for the slots the program samples).
    pub hazard_scratch: Option<usize>,
    /// The target's texture is shared and this pass writes it first in the frame: clear it to
    /// transparent black before the draw.
    pub clear_first: bool,
}

/// One frame operation, in the (already scheduled) frame order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OpPlan {
    Clear { target: usize, color: [f32; 4] },
    Copy { src: usize, dst: usize },
    Draw(usize),
}

/// One attribute of a stream or mesh layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributePlan {
    pub name: String,
    pub format: VertexFormat,
    /// Byte offset in the vertex.
    pub offset: u32,
}

/// A vertex stream rewritten from the CPU (particles), with constant 32-bit indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamPlan {
    /// Bytes per vertex.
    pub stride: u32,
    pub attributes: Vec<AttributePlan>,
    pub max_vertices: u32,
    pub indices: Vec<u32>,
}

/// Interleaved layout of a static mesh (`.mdl` bytes as stored), given to the builder with the
/// mesh's data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshLayoutPlan {
    pub stride: u32,
    pub attributes: Vec<AttributePlan>,
}

/// Everything a scene compiles to, besides the uploads made through the builder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScenePlan {
    pub targets: Vec<TargetPlan>,
    pub passes: Vec<PassPlan>,
    pub ops: Vec<OpPlan>,
    /// Bytes of the shadow (a multiple of 256).
    pub constants_len: u32,
    /// Window of the frame block (`b1`), bound once per frame.
    pub frame_block: Option<ConstantRegion>,
    /// Bytes of the dynamic image vertex region ([`IMAGE_VERTEX_STRIDE`] per vertex).
    pub vertex_bytes: u32,
    /// Static 16-bit indices (puppets).
    pub indices: Vec<u16>,
    /// Static skin records ([`SKIN_STRIDE`] bytes each).
    pub skins: Vec<u8>,
    pub streams: Vec<StreamPlan>,
}

/// What one frame reads, borrowed from `RenderScene`'s CPU state (no allocation).
#[derive(Clone, Debug)]
pub struct FrameInput<'a> {
    /// The whole shadow (`constants_len` bytes).
    pub shadow: &'a [u8],
    /// The whole vertex region (`vertex_bytes` bytes).
    pub vertices: &'a [u8],
    /// Bytes of `vertices` written since the previous frame, if any.
    pub vertices_dirty: Option<Range<u32>>,
    /// Slot-0 variant of each pass.
    pub variants: &'a [u16],
    /// Whether each pass is drawn.
    pub enabled: &'a [bool],
    /// One entry per stream.
    pub streams: &'a [StreamFrame<'a>],
}

/// One stream in a frame.
#[derive(Clone, Copy, Debug)]
pub struct StreamFrame<'a> {
    /// Vertex bytes written since the previous frame, `None` when unchanged.
    pub vertices: Option<&'a [u8]>,
    pub index_count: u32,
}

impl ScenePlan {
    /// Checks every index and window, given how many textures, programs and meshes the builder
    /// holds. The executor runs it before building; the bridge can run it in its tests.
    pub fn validate(&self, textures: usize, programs: usize, meshes: usize) -> Result<()> {
        let bad = |m: String| Err(Error::InvalidData(format!("scene plan: {m}")));
        if !self.constants_len.is_multiple_of(CONSTANT_WINDOW_ALIGN) {
            return bad(format!(
                "constants_len {} is not a multiple of 256",
                self.constants_len
            ));
        }
        let window_ok = |w: &ConstantRegion| {
            w.offset.is_multiple_of(CONSTANT_WINDOW_ALIGN)
                && w.size.is_multiple_of(CONSTANT_WINDOW_ALIGN)
                && w.offset + w.size <= self.constants_len
        };
        if let Some(w) = &self.frame_block
            && !window_ok(w)
        {
            return bad(format!("frame block {w:?}"));
        }
        if !self.vertex_bytes.is_multiple_of(IMAGE_VERTEX_STRIDE) {
            return bad(format!("vertex region of {} bytes", self.vertex_bytes));
        }
        if !self.skins.len().is_multiple_of(SKIN_STRIDE as usize) {
            return bad(format!("skin buffer of {} bytes", self.skins.len()));
        }
        let n_targets = self.targets.len();
        for (i, t) in self.targets.iter().enumerate() {
            if let Some(s) = t.shares {
                if s >= n_targets || self.targets[s].shares.is_some() || s == i {
                    return bad(format!(
                        "target {i} shares {s}, which is not a texture owner"
                    ));
                }
                let o = &self.targets[s];
                if (o.size, o.format, o.samples, o.depth) != (t.size, t.format, t.samples, t.depth)
                {
                    return bad(format!("target {i} and its owner {s} differ"));
                }
            }
            if t.samples == 0 || !t.samples.is_power_of_two() || t.samples > 8 {
                return bad(format!("target {i} has {} samples", t.samples));
            }
        }
        let source_ok = |s: &TextureSource| match *s {
            TextureSource::Texture(t) => t < textures,
            TextureSource::Target(t) => t < n_targets,
            TextureSource::Default2D | TextureSource::Default3D => true,
        };
        let vertices = self.vertex_bytes / IMAGE_VERTEX_STRIDE;
        let skins = (self.skins.len() / SKIN_STRIDE as usize) as u32;
        for (i, p) in self.passes.iter().enumerate() {
            if p.program >= programs {
                return bad(format!("pass {i} ({}): program {}", p.label, p.program));
            }
            if p.target >= n_targets || p.hazard_scratch.is_some_and(|s| s >= n_targets) {
                return bad(format!("pass {i} ({}): target", p.label));
            }
            if let Some(w) = &p.uniform
                && !window_ok(w)
            {
                return bad(format!("pass {i} ({}): uniform window {w:?}", p.label));
            }
            if !p.slots.iter().flatten().all(|s| source_ok(&s.source))
                || !p.slot0_variants.iter().all(source_ok)
            {
                return bad(format!("pass {i} ({}): texture source", p.label));
            }
            let geometry_ok = match p.geometry {
                GeometryPlan::Vertices { first, count } => first + count <= vertices,
                GeometryPlan::Indexed {
                    first_index,
                    index_count,
                    first_vertex,
                } => {
                    first_index as usize + index_count as usize <= self.indices.len()
                        && first_vertex < vertices.max(1)
                }
                GeometryPlan::Skinned {
                    first_index,
                    index_count,
                    first_vertex,
                    first_skin,
                } => {
                    first_index as usize + index_count as usize <= self.indices.len()
                        && first_vertex < vertices.max(1)
                        && first_skin < skins.max(1)
                }
                GeometryPlan::Stream(s) => s < self.streams.len(),
                GeometryPlan::Mesh(m) => m < meshes,
            };
            if !geometry_ok {
                return bad(format!("pass {i} ({}): geometry {:?}", p.label, p.geometry));
            }
        }
        for (k, op) in self.ops.iter().enumerate() {
            let ok = match *op {
                OpPlan::Clear { target, .. } => target < n_targets,
                OpPlan::Copy { src, dst } => src < n_targets && dst < n_targets && src != dst,
                OpPlan::Draw(p) => p < self.passes.len(),
            };
            if !ok {
                return bad(format!("op {k}: {op:?}"));
            }
        }
        for (i, s) in self.streams.iter().enumerate() {
            if s.stride == 0
                || s.attributes
                    .iter()
                    .any(|a| a.offset + a.format.size() > s.stride)
            {
                return bad(format!("stream {i} layout"));
            }
            if s.indices.iter().any(|&x| x >= s.max_vertices) {
                return bad(format!(
                    "stream {i} indices beyond {} vertices",
                    s.max_vertices
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(label: &str, shares: Option<usize>) -> TargetPlan {
        TargetPlan {
            label: label.into(),
            size: (64, 32),
            format: TargetFormat::Rgba8Unorm,
            samples: 1,
            depth: false,
            shares,
        }
    }

    fn pass(target: usize) -> PassPlan {
        PassPlan {
            label: "pass".into(),
            program: 0,
            target,
            blend: BlendKey::OPAQUE,
            raster: RasterKey::default(),
            depth: DepthKey::default(),
            uniform: Some(ConstantRegion {
                offset: 256,
                size: 256,
            }),
            slots: [None; MAX_SLOTS as usize],
            slot0_variants: Vec::new(),
            geometry: GeometryPlan::Vertices { first: 0, count: 6 },
            hazard_scratch: None,
            clear_first: false,
        }
    }

    fn plan() -> ScenePlan {
        let mut p = pass(1);
        p.slots[0] = Some(SlotPlan {
            source: TextureSource::Texture(0),
            sampler: SamplerKey::CLAMP_LINEAR,
        });
        ScenePlan {
            targets: vec![
                target("_rt_FullFrameBuffer", None),
                target("composite", None),
                target("fx", Some(1)),
            ],
            passes: vec![p, pass(0)],
            ops: vec![
                OpPlan::Clear {
                    target: 0,
                    color: [0.0, 0.0, 0.0, 1.0],
                },
                OpPlan::Draw(0),
                OpPlan::Draw(1),
                OpPlan::Copy { src: 0, dst: 1 },
            ],
            constants_len: 512,
            frame_block: Some(ConstantRegion {
                offset: 0,
                size: 256,
            }),
            vertex_bytes: 6 * IMAGE_VERTEX_STRIDE,
            indices: vec![0, 1, 2],
            skins: Vec::new(),
            streams: vec![StreamPlan {
                stride: 68,
                attributes: vec![AttributePlan {
                    name: "a_Position".into(),
                    format: VertexFormat::Float32x3,
                    offset: 0,
                }],
                max_vertices: 4,
                indices: vec![0, 1, 2, 2, 1, 3],
            }],
        }
    }

    #[test]
    fn a_consistent_plan_validates() {
        plan().validate(1, 1, 0).unwrap();
    }

    #[test]
    fn every_index_and_window_is_checked() {
        let p = plan();
        assert!(p.validate(0, 1, 0).is_err(), "texture index");
        assert!(p.validate(1, 0, 0).is_err(), "program index");
        let mut q = p.clone();
        q.passes[0].uniform = Some(ConstantRegion {
            offset: 512,
            size: 256,
        });
        assert!(q.validate(1, 1, 0).is_err(), "window beyond the shadow");
        let mut q = p.clone();
        q.passes[0].uniform = Some(ConstantRegion {
            offset: 128,
            size: 256,
        });
        assert!(q.validate(1, 1, 0).is_err(), "unaligned window");
        let mut q = p.clone();
        q.targets[2].shares = Some(2);
        assert!(q.validate(1, 1, 0).is_err(), "shares itself");
        let mut q = p.clone();
        q.targets[2].size = (32, 32);
        assert!(q.validate(1, 1, 0).is_err(), "alias of another size");
        let mut q = p.clone();
        q.targets.push(target("chain", Some(2)));
        assert!(q.validate(1, 1, 0).is_err(), "alias of an alias");
        let mut q = p.clone();
        q.passes[1].geometry = GeometryPlan::Vertices { first: 3, count: 6 };
        assert!(q.validate(1, 1, 0).is_err(), "vertices beyond the region");
        let mut q = p.clone();
        q.passes[1].geometry = GeometryPlan::Mesh(0);
        assert!(
            q.validate(1, 1, 0).is_err() && q.validate(1, 1, 1).is_ok(),
            "meshes"
        );
        let mut q = p.clone();
        q.ops.push(OpPlan::Copy { src: 1, dst: 1 });
        assert!(q.validate(1, 1, 0).is_err(), "copy onto itself");
        let mut q = p.clone();
        q.streams[0].indices.push(4);
        assert!(
            q.validate(1, 1, 0).is_err(),
            "stream index beyond max_vertices"
        );
        let mut q = p.clone();
        q.targets[0].samples = 3;
        assert!(q.validate(1, 1, 0).is_err(), "sample count");
        let mut q = p;
        q.constants_len = 500;
        assert!(q.validate(1, 1, 0).is_err(), "shadow size");
    }

    #[test]
    fn layouts_match_lumenwall_render() {
        // `math::Vertex` is 3 + 2 floats; `SkinVertex` four 16-byte attributes.
        let last = IMAGE_VERTEX_ATTRIBUTES[1];
        assert_eq!(last.2 + last.1.size(), IMAGE_VERTEX_STRIDE);
        assert_eq!(SKIN_ATTRIBUTES[3].1 + 16, SKIN_STRIDE);
    }
}

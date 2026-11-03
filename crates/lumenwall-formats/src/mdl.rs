//! `MDLV` puppet readers.
//!
//! - [`parse_puppet`]: the mesh only, through the heuristic of spec 01 §3.1. The reference
//!   does not parse the structure of the file: it scans for the first byte pattern that looks
//!   like a vertex block (80-byte vertices) followed by an index block (multiple of 6 bytes).
//!   This function applies the same heuristic so the same files yield the same mesh (QO-15).
//! - [`parse_model`]: the whole file (spec 08): structured mesh with bone weights, skeleton
//!   (`MDLS`), attachment points (`MDAT`), animations (`MDLA`) and editor pose (`MDLE`),
//!   falling back to the heuristic for layouts other than the observed puppet one.
//! - [`parse_static_model`]: static models of 3D model objects (spec 09 §1.10), versions
//!   `0004`, `0014`, `0017`, `0023`, several meshes and skins, raw interleaved vertices.

mod model;
mod static_model;

pub use model::{
    Animation, AnimationMode, Attachment, Bone, BoneBox, BoneSample, MdlWarning,
    PUPPET_VERTEX_FORMAT, PartRange, PuppetModel, SectionInfo, Skeleton, SkeletonEditorData,
    VertexSkin, parse_model,
};
pub use static_model::{
    StaticMesh, StaticModel, StaticModelError, VertexFormatError, VertexLayout, parse_static_model,
    vertex_format,
};

use thiserror::Error;

/// Supported versions (QO-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MdlVersion {
    /// `MDLV0021`.
    V21,
    /// `MDLV0023`.
    V23,
}

impl MdlVersion {
    /// The 8-character magic.
    pub fn magic(self) -> &'static str {
        match self {
            Self::V21 => "MDLV0021",
            Self::V23 => "MDLV0023",
        }
    }
}

/// Puppet mesh loading failures. All are non-fatal for the caller, which keeps the
/// standard quad.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MdlError {
    /// Shorter than 9 bytes or magic other than `MDLV0021` / `MDLV0023`.
    #[error("unsupported puppet version \"{0}\"")]
    UnsupportedVersion(String),
    /// No position satisfies the vertex/index block heuristic.
    #[error("no usable mesh block in puppet file")]
    NoMeshBlock,
    /// An index refers to a missing vertex; no partial mesh is kept.
    #[error("invalid puppet index {index} (only {vertex_count} vertices)")]
    InvalidIndex {
        /// Offending index.
        index: u16,
        /// Number of vertices.
        vertex_count: usize,
    },
}

/// Mesh extracted from a puppet file: a triangle list with 16-bit indices.
#[derive(Debug, Clone, PartialEq)]
pub struct PuppetMesh {
    /// File version.
    pub version: MdlVersion,
    /// Raw positions (x, y, z) as stored.
    pub positions: Vec<[f32; 3]>,
    /// Texture coordinates (u, v), passed through unchanged.
    pub uvs: Vec<[f32; 2]>,
    /// Triangle list indices.
    pub indices: Vec<u16>,
    /// Offset of the vertex block header in the file (`p`).
    pub block_offset: usize,
}

const VERTEX_SIZE: u64 = 80;
const UV_OFFSET: usize = 72;

fn u32_at(data: &[u8], pos: u64) -> Option<u32> {
    let pos = usize::try_from(pos).ok()?;
    let bytes = data.get(pos..pos.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn f32_at(data: &[u8], pos: usize) -> f32 {
    data.get(pos..pos + 4)
        .map_or(0.0, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Applies the §3.1.2 heuristic to a whole puppet file.
pub fn parse_puppet(data: &[u8]) -> Result<PuppetMesh, MdlError> {
    let len = data.len() as u64;

    // Step 1: version; the 9th byte is not checked.
    let version = match data.get(..8) {
        Some(b"MDLV0021") if data.len() >= 9 => MdlVersion::V21,
        Some(b"MDLV0023") if data.len() >= 9 => MdlVersion::V23,
        _ => {
            let shown = &data[..data.len().min(9)];
            return Err(MdlError::UnsupportedVersion(
                shown.escape_ascii().to_string(),
            ));
        }
    };

    // Step 2: limit L = first "MDLS" at p >= 9 with p + 4 < |D| (strict, QO-15).
    let limit = data
        .windows(4)
        .enumerate()
        .skip(9)
        .find(|&(p, w)| (p as u64) + 4 < len && w == b"MDLS")
        .map_or(len, |(p, _)| p as u64);

    // Step 3: first p >= 9 whose vertex and index block sizes fit before L.
    let mut found = None;
    let mut p: u64 = 9;
    while p + 12 < limit {
        let Some(vb) = u32_at(data, p + 4).map(u64::from) else {
            break;
        };
        if vb != 0
            && vb % VERTEX_SIZE == 0
            && p + 8 + vb + 4 <= limit
            && let Some(ib) = u32_at(data, p + 8 + vb).map(u64::from)
            && ib != 0
            && ib % 6 == 0
            && p + 8 + vb + 4 + ib <= limit
        {
            found = Some((p, vb, ib));
            break;
        }
        p += 1;
    }
    let (p, vb, ib) = found.ok_or(MdlError::NoMeshBlock)?;

    // Steps 4 and 5. Every offset below is inside [0, L) as checked above.
    let (p, vb, ib) = (p as usize, vb as usize, ib as usize);
    let vertex_count = vb / VERTEX_SIZE as usize;
    let mut positions = Vec::with_capacity(vertex_count);
    let mut uvs = Vec::with_capacity(vertex_count);
    for k in 0..vertex_count {
        let base = p + 8 + k * VERTEX_SIZE as usize;
        positions.push([
            f32_at(data, base),
            f32_at(data, base + 4),
            f32_at(data, base + 8),
        ]);
        uvs.push([
            f32_at(data, base + UV_OFFSET),
            f32_at(data, base + UV_OFFSET + 4),
        ]);
    }
    let index_start = p + 8 + vb + 4;
    let indices: Vec<u16> = data[index_start..index_start + ib]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| u16::from_le_bytes(b))
        .collect();
    if let Some(&index) = indices.iter().find(|&&i| usize::from(i) >= vertex_count) {
        return Err(MdlError::InvalidIndex {
            index,
            vertex_count,
        });
    }

    tracing::debug!(
        version = version.magic(),
        vertices = vertex_count,
        indices = indices.len(),
        "puppet mesh loaded"
    );
    Ok(PuppetMesh {
        version,
        positions,
        uvs,
        indices,
        block_offset: p,
    })
}

impl PuppetMesh {
    /// GPU positions for an image object of `size` pixels (§3.1.2 step 7):
    /// `(S.x / 2 + x, S.y / 2 − y, z)`.
    pub fn placed_positions(&self, size: [f32; 2]) -> impl ExactSizeIterator<Item = [f32; 3]> + '_ {
        let (hx, hy) = (size[0] / 2.0, size[1] / 2.0);
        self.positions
            .iter()
            .map(move |&[x, y, z]| [hx + x, hy - y, z])
    }
}

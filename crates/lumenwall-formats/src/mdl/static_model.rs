//! Static `.mdl` files of 3D model objects (spec 09 §1.10): `MDLV0004`, `0014`, `0017` and
//! `0023` without skeleton, one or more sub-meshes, one material per skin.
//!
//! Vertices are kept as stored (interleaved little-endian floats, [`VertexLayout`] gives the
//! offsets), so the renderer uploads them without any conversion.

use std::fmt;

use super::model::{Animation, Attachment, MdlWarning, SectionInfo, Skeleton, read_section_chain};
use crate::reader::{Reader, UnexpectedEof};

/// Vertex format bits (spec 09 §1.10.3).
pub mod vertex_format {
    /// `a_Position`, 3 floats.
    pub const POSITION: u32 = 0x0000_0001;
    /// `a_Normal`, 3 floats.
    pub const NORMAL: u32 = 0x0000_0002;
    /// `a_Tangent4`, 4 floats (w = bitangent sign).
    pub const TANGENT: u32 = 0x0000_0004;
    /// `a_TexCoord`, 2 floats.
    pub const UV: u32 = 0x0000_0008;
    /// Two UV sets `(u0, v0, u1, v1)`, 4 floats (`a_TexCoordVec4`).
    pub const UV2: u32 = 0x0000_0020;
    /// `a_BlendIndices`, 4 × `u32`.
    pub const JOINTS: u32 = 0x0080_0000;
    /// `a_BlendWeights`, 4 floats.
    pub const WEIGHTS: u32 = 0x0100_0000;
    /// Every known bit.
    pub const KNOWN: u32 = POSITION | NORMAL | TANGENT | UV | UV2 | JOINTS | WEIGHTS;
}

/// Byte offsets of the fields of one vertex (spec 09 §1.10.3: fields in table order, no
/// padding).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexLayout {
    /// The format word.
    pub format: u32,
    /// Bytes per vertex `T(vf)`.
    pub stride: u32,
    /// Position is always at offset 0.
    pub normal: Option<u32>,
    pub tangent: Option<u32>,
    pub joints: Option<u32>,
    pub weights: Option<u32>,
    /// Offset of the UV field: 2 floats, or 4 floats when [`Self::two_uv_sets`].
    pub uv: Option<u32>,
    /// Bit `0x20`: the UV field holds `(u0, v0, u1, v1)`.
    pub two_uv_sets: bool,
}

/// Why a vertex format is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VertexFormatError {
    /// A bit outside [`vertex_format::KNOWN`].
    UnknownBits(u32),
    /// `0x8` and `0x20` together (QO-3D-9).
    BothUvSets,
    /// No position.
    NoPosition,
}

impl VertexLayout {
    /// Layout of a format word, or why it is refused.
    pub fn new(format: u32) -> Result<Self, VertexFormatError> {
        use vertex_format::*;
        let unknown = format & !KNOWN;
        if unknown != 0 {
            return Err(VertexFormatError::UnknownBits(unknown));
        }
        if format & UV != 0 && format & UV2 != 0 {
            return Err(VertexFormatError::BothUvSets);
        }
        if format & POSITION == 0 {
            return Err(VertexFormatError::NoPosition);
        }
        let mut at = 12;
        let mut field = |bit: u32, size: u32| {
            (format & bit != 0).then(|| {
                let o = at;
                at += size;
                o
            })
        };
        let normal = field(NORMAL, 12);
        let tangent = field(TANGENT, 16);
        let joints = field(JOINTS, 16);
        let weights = field(WEIGHTS, 16);
        let uv = field(UV, 8).or_else(|| field(UV2, 16));
        Ok(Self {
            format,
            stride: at,
            normal,
            tangent,
            joints,
            weights,
            uv,
            two_uv_sets: format & UV2 != 0,
        })
    }
}

/// One sub-mesh (spec 09 §1.10.2).
#[derive(Clone, Debug, PartialEq)]
pub struct StaticMesh {
    /// Material path of each skin (`S` entries).
    pub materials: Vec<String>,
    /// Model-space bounding box `(min, max)` (`N ≥ 17` only).
    pub bounds: Option<([f32; 3], [f32; 3])>,
    pub layout: VertexLayout,
    /// Raw interleaved vertices, `vertex_count × layout.stride` bytes.
    pub vertices: Vec<u8>,
    /// Triangle list.
    pub indices: Vec<u16>,
}

fn f32s<const N: usize>(bytes: &[u8], at: usize) -> [f32; N] {
    std::array::from_fn(|i| {
        let o = at + 4 * i;
        bytes
            .get(o..o + 4)
            .map_or(0.0, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    })
}

impl StaticMesh {
    /// Number of vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len() / self.layout.stride as usize
    }

    fn at(&self, i: usize, offset: u32) -> usize {
        i * self.layout.stride as usize + offset as usize
    }

    /// Position of vertex `i`.
    #[must_use]
    pub fn position(&self, i: usize) -> [f32; 3] {
        f32s(&self.vertices, self.at(i, 0))
    }

    /// Normal of vertex `i`, if the format has normals.
    #[must_use]
    pub fn normal(&self, i: usize) -> Option<[f32; 3]> {
        Some(f32s(&self.vertices, self.at(i, self.layout.normal?)))
    }

    /// Tangent of vertex `i` (w = bitangent sign), if present.
    #[must_use]
    pub fn tangent(&self, i: usize) -> Option<[f32; 4]> {
        Some(f32s(&self.vertices, self.at(i, self.layout.tangent?)))
    }

    /// `a_TexCoord` of vertex `i`: `(u, v)`, or `(u0, v0)` with two UV sets.
    #[must_use]
    pub fn uv(&self, i: usize) -> Option<[f32; 2]> {
        Some(f32s(&self.vertices, self.at(i, self.layout.uv?)))
    }

    /// `a_TexCoordVec4` of vertex `i`: `(u0, v0, u1, v1)` with two UV sets, `(u, v, 0, 1)`
    /// with one (spec 09 §1.14).
    #[must_use]
    pub fn uv4(&self, i: usize) -> Option<[f32; 4]> {
        let at = self.at(i, self.layout.uv?);
        Some(if self.layout.two_uv_sets {
            f32s(&self.vertices, at)
        } else {
            let [u, v] = f32s(&self.vertices, at);
            [u, v, 0.0, 1.0]
        })
    }
}

/// A static model file (spec 09 §1.10).
#[derive(Clone, Debug, PartialEq)]
pub struct StaticModel {
    /// `N`: the version digits (4, 14, 17, 23…).
    pub version: u32,
    /// Model flags at offset 9 (the vertex format of every mesh for `N < 17`).
    pub flags: u32,
    /// `S`: number of skins.
    pub skins: u32,
    pub meshes: Vec<StaticMesh>,
    /// Offset right after the last mesh (start of the section chain or terminal byte).
    pub meshes_end: usize,
    /// Chained sections (spec 08 §3); none on the installed static models.
    pub sections: Vec<SectionInfo>,
    /// `MDLS` of a 3D model with a skeleton (spec 09 §1.10.6, not rendered yet).
    pub skeleton: Option<Skeleton>,
    pub attachments: Vec<Attachment>,
    pub animations: Vec<Animation>,
    /// Non-fatal problems in reading order.
    pub warnings: Vec<MdlWarning>,
}

impl StaticModel {
    /// Material of mesh `mesh` for skin `skin` (spec 09 §1.11.1): the skin's own path, or the
    /// path of skin 0 when `skin ≥ S` (`true` = fallback taken, worth a warning).
    #[must_use]
    pub fn material(&self, mesh: usize, skin: u32) -> Option<(&str, bool)> {
        let m = self.meshes.get(mesh)?;
        match m.materials.get(skin as usize) {
            Some(p) => Some((p.as_str(), false)),
            None => m.materials.first().map(|p| (p.as_str(), true)),
        }
    }
}

/// Why a static model cannot be read (spec 09 §1.10.5). Never fatal for the scene: the
/// object is not drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StaticModelError {
    /// Magic other than `MDLV` + 4 digits + `0x00`.
    BadMagic(String),
    /// `S = 0`.
    NoSkin,
    /// Unknown or contradictory vertex format.
    VertexFormat {
        mesh: usize,
        offset: usize,
        format: u32,
        error: VertexFormatError,
    },
    /// `vb` not a multiple of the vertex size.
    VertexBlockSize {
        mesh: usize,
        offset: usize,
        size: u32,
        stride: u32,
    },
    /// `ib` not a multiple of 6.
    IndexBlockSize {
        mesh: usize,
        offset: usize,
        size: u32,
    },
    /// An index refers to a missing vertex.
    IndexOutOfRange {
        mesh: usize,
        offset: usize,
        index: u16,
        vertices: usize,
    },
    /// Mesh tail flag neither 0 nor 1 (`N ≥ 23`, spec 08 §2.6).
    BadTail { mesh: usize, offset: usize },
    /// A read went past the end of the file.
    Eof { what: &'static str, offset: usize },
}

impl fmt::Display for StaticModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic(m) => write!(f, "bad magic \"{m}\" (MDLV + 4 digits expected)"),
            Self::NoSkin => write!(f, "skin count is 0"),
            Self::VertexFormat {
                mesh,
                offset,
                format,
                error,
            } => {
                let why = match error {
                    VertexFormatError::UnknownBits(b) => format!("unknown bits 0x{b:X}"),
                    VertexFormatError::BothUvSets => "bits 0x8 and 0x20 together".into(),
                    VertexFormatError::NoPosition => "no position".into(),
                };
                write!(
                    f,
                    "mesh {mesh} at 0x{offset:X}: unknown vertex format 0x{format:X} ({why})"
                )
            }
            Self::VertexBlockSize {
                mesh,
                offset,
                size,
                stride,
            } => write!(
                f,
                "mesh {mesh} at 0x{offset:X}: vertex size {size} is not a multiple of {stride}"
            ),
            Self::IndexBlockSize { mesh, offset, size } => write!(
                f,
                "mesh {mesh} at 0x{offset:X}: index size {size} is not a multiple of 6"
            ),
            Self::IndexOutOfRange {
                mesh,
                offset,
                index,
                vertices,
            } => write!(
                f,
                "mesh {mesh} at 0x{offset:X}: index {index} out of bounds ({vertices} vertices)"
            ),
            Self::BadTail { mesh, offset } => {
                write!(f, "mesh {mesh} at 0x{offset:X}: invalid mesh tail flag")
            }
            Self::Eof { what, offset } => {
                write!(f, "end of file at 0x{offset:X} while reading {what}")
            }
        }
    }
}

impl std::error::Error for StaticModelError {}

/// Versions observed in the installation (spec 09 §1.10.5); others are tried with the same
/// thresholds and a warning (QO-3D-10).
const OBSERVED_VERSIONS: [u32; 4] = [4, 14, 17, 23];

/// Reads a static `.mdl` (spec 09 §1.10.5).
pub fn parse_static_model(data: &[u8]) -> Result<StaticModel, StaticModelError> {
    let magic = data.get(..9).unwrap_or(data);
    let version = match magic {
        [b'M', b'D', b'L', b'V', d @ .., 0] if d.len() == 4 && d.iter().all(u8::is_ascii_digit) => {
            d.iter().fold(0u32, |n, c| n * 10 + u32::from(c - b'0'))
        }
        _ => return Err(StaticModelError::BadMagic(magic.escape_ascii().to_string())),
    };
    let mut warnings = Vec::new();
    if !OBSERVED_VERSIONS.contains(&version) {
        warnings.push(MdlWarning {
            section: "MDLV".into(),
            offset: 0,
            message: format!(
                "version {version:04} never observed, read with the 0017/0023 thresholds"
            ),
        });
    }
    let mut r = Reader::new(data);
    let eof = |what: &'static str| {
        move |e: UnexpectedEof| StaticModelError::Eof {
            what,
            offset: e.offset as usize,
        }
    };
    r.take(9).map_err(eof("header"))?;
    let flags = r.u32().map_err(eof("model flags"))?;
    let skins = r.u32().map_err(eof("skin count"))?;
    if skins == 0 {
        return Err(StaticModelError::NoSkin);
    }
    let mesh_count = r.u32().map_err(eof("mesh count"))?;
    // A mesh takes at least 13 bytes: never reserve more than the file can hold.
    let mut meshes = Vec::with_capacity((mesh_count as usize).min(r.remaining() / 13));
    for mesh in 0..mesh_count as usize {
        let mut materials = Vec::with_capacity((skins as usize).min(r.remaining()));
        for _ in 0..skins {
            let s = r.cstr().map_err(eof("material path"))?;
            materials.push(String::from_utf8_lossy(s).into_owned());
        }
        r.u32().map_err(eof("mesh reserved word"))?;
        let (bounds, format_at, format) = if version >= 17 {
            let mut b = [0.0f32; 6];
            for v in &mut b {
                *v = r.f32().map_err(eof("bounding box"))?;
            }
            let at = r.pos();
            let vf = r.u32().map_err(eof("vertex format"))?;
            (Some(([b[0], b[1], b[2]], [b[3], b[4], b[5]])), at, vf)
        } else {
            (None, 9, flags)
        };
        let layout = VertexLayout::new(format).map_err(|error| StaticModelError::VertexFormat {
            mesh,
            offset: format_at,
            format,
            error,
        })?;
        let vb_at = r.pos();
        let vb = r.u32().map_err(eof("vertex block size"))?;
        if vb % layout.stride != 0 {
            return Err(StaticModelError::VertexBlockSize {
                mesh,
                offset: vb_at,
                size: vb,
                stride: layout.stride,
            });
        }
        let vertices = r.take(vb as usize).map_err(eof("vertices"))?.to_vec();
        let ib_at = r.pos();
        let ib = r.u32().map_err(eof("index block size"))?;
        if ib % 6 != 0 {
            return Err(StaticModelError::IndexBlockSize {
                mesh,
                offset: ib_at,
                size: ib,
            });
        }
        let index_at = r.pos();
        let indices: Vec<u16> = r
            .take(ib as usize)
            .map_err(eof("indices"))?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| u16::from_le_bytes(b))
            .collect();
        let count = (vb / layout.stride) as usize;
        if let Some(k) = indices.iter().position(|&i| usize::from(i) >= count) {
            return Err(StaticModelError::IndexOutOfRange {
                mesh,
                offset: index_at + 2 * k,
                index: indices[k],
                vertices: count,
            });
        }
        if version >= 23 {
            read_tail(&mut r, mesh)?;
        }
        meshes.push(StaticMesh {
            materials,
            bounds,
            layout,
            vertices,
            indices,
        });
    }
    let meshes_end = r.pos();
    let chain = read_section_chain(data, meshes_end, &mut warnings);
    Ok(StaticModel {
        version,
        flags,
        skins,
        meshes,
        meshes_end,
        sections: chain.sections,
        skeleton: chain.skeleton,
        attachments: chain.attachments,
        animations: chain.animations,
        warnings,
    })
}

/// Mesh tail of `N ≥ 23` (spec 08 §2.6): texture-layout positions, part ranges and a reserved
/// word, skipped (editor data, six zero bytes on the static models).
fn read_tail(r: &mut Reader<'_>, mesh: usize) -> Result<(), StaticModelError> {
    let eof = |e: UnexpectedEof| StaticModelError::Eof {
        what: "mesh tail",
        offset: e.offset as usize,
    };
    for block in 0..2 {
        let at = r.pos();
        match r.take(1).map_err(eof)?[0] {
            0 => {}
            1 => {
                if block == 0 {
                    r.u32().map_err(eof)?;
                }
                let len = r.u32().map_err(eof)?;
                r.take(len as usize).map_err(eof)?;
            }
            _ => return Err(StaticModelError::BadTail { mesh, offset: at }),
        }
    }
    r.u32().map_err(eof)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_of_the_observed_formats() {
        let sizes: Vec<u32> = [0x9, 0xB, 0xF, 0x27, 0x0180_000F]
            .iter()
            .map(|&f| VertexLayout::new(f).unwrap().stride)
            .collect();
        assert_eq!(sizes, [20, 32, 48, 56, 80]);
        let l = VertexLayout::new(0x27).unwrap();
        assert_eq!(
            (l.normal, l.tangent, l.uv, l.two_uv_sets),
            (Some(12), Some(24), Some(40), true)
        );
        let puppet = VertexLayout::new(0x0180_000F).unwrap();
        assert_eq!(
            (puppet.joints, puppet.weights, puppet.uv),
            (Some(40), Some(56), Some(72))
        );
        assert_eq!(
            VertexLayout::new(0x19),
            Err(VertexFormatError::UnknownBits(0x10))
        );
        assert_eq!(VertexLayout::new(0x29), Err(VertexFormatError::BothUvSets));
        assert_eq!(VertexLayout::new(0x8), Err(VertexFormatError::NoPosition));
    }
}

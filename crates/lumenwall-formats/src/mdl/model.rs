//! Structured puppet reader (spec 08 §1–§8): mesh with bone weights, skeleton (`MDLS`),
//! attachment points (`MDAT`), animations (`MDLA`) and the editor pose (`MDLE`).
//!
//! Every section error is non-fatal: the mesh stays usable and the section is dropped with a
//! [`MdlWarning`] carrying the section and the absolute offset (spec 08 §8).

use std::fmt;

use super::{MdlError, MdlVersion, PuppetMesh, VERTEX_SIZE, f32_at, parse_puppet, u32_at};
use crate::reader::{Reader, UnexpectedEof};

/// Vertex format of every puppet observed (spec 08 §2.3): position, normal, tangent, bone
/// indices, bone weights, texture coordinates (80 bytes).
pub const PUPPET_VERTEX_FORMAT: u32 = 0x0180_000F;

/// Size of one animation sample (spec 08 §6.4).
const SAMPLE_SIZE: usize = 36;
/// Reserved block closing every animation (spec 08 §6.2).
const ANIMATION_TAIL: usize = 35;

/// Bone influences and frame of one vertex (spec 08 §2.4), in the order of
/// [`PuppetMesh::positions`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VertexSkin {
    /// Normal (`a_Normal`).
    pub normal: [f32; 3],
    /// Tangent, w = bitangent sign (`a_Tangent4`).
    pub tangent: [f32; 4],
    /// Bone indices j0…j3 (`a_BlendIndices`).
    pub joints: [u32; 4],
    /// Weights w0…w3 (`a_BlendWeights`), neither sorted nor normalised.
    pub weights: [f32; 4],
}

/// One bone of `MDLS0004` (spec 08 §4.2).
#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    /// Name, usually empty. Invalid UTF-8 bytes become U+FFFD.
    pub name: String,
    /// `type`: 0 for the vertex-less root of multi-bone skeletons, 1 otherwise.
    pub kind: u32,
    /// Parent bone, always an earlier one (invalid parents are read as `None`).
    pub parent: Option<usize>,
    /// Rest transform relative to the parent: column-major `mat4`, translation in 12–14.
    pub local: [f32; 16],
    /// Trailing string, always empty in the corpus (QO-P2).
    pub extra: String,
}

/// Editor box of a bone (spec 08 §4.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoneBox {
    /// Half extents.
    pub half_extents: [f32; 3],
    /// Box transform in the bone frame.
    pub transform: [f32; 16],
}

/// Editor data after the bones (spec 08 §4.3, §4.4). Not needed for rendering.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkeletonEditorData {
    /// One box per bone.
    pub boxes: Vec<BoneBox>,
    /// Bone indices in drawing order (already applied to the index buffer).
    pub draw_order: Vec<u32>,
    /// Sort key of each bone; `draw_order` is their ascending sort.
    pub sort_keys: Vec<u32>,
}

/// `MDLS0004` (spec 08 §4).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Skeleton {
    /// Bones in file order: parents come first.
    pub bones: Vec<Bone>,
    /// Editor data, `None` when absent or not in the observed layout.
    pub editor: Option<SkeletonEditorData>,
}

/// One attachment point of `MDAT0001` (spec 08 §5).
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    /// Bone carrying the point.
    pub bone: usize,
    /// Name matched (case-sensitive) against the `attachment` key of child objects.
    pub name: String,
    /// Transform relative to the bone frame, column-major.
    pub transform: [f32; 16],
}

/// Playback mode of an animation (spec 08 §6.2, §9.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationMode {
    /// `loop`.
    Loop,
    /// `mirror`: forward then backward at the same speed.
    Mirror,
    /// `single`: halts on the last frame.
    Single,
    /// Any other string; played as [`AnimationMode::Loop`] by the runtime.
    Unknown,
}

impl AnimationMode {
    fn parse(s: &[u8]) -> Self {
        match s {
            b"loop" => Self::Loop,
            b"mirror" => Self::Mirror,
            b"single" => Self::Single,
            _ => Self::Unknown,
        }
    }
}

/// Local pose of a bone at one frame (spec 08 §6.4): absolute values, not deltas.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BoneSample {
    /// Local position (same meaning as the rest translation).
    pub position: [f32; 3],
    /// Local Euler angles in radians (x, y, z).
    pub angles: [f32; 3],
    /// Local scale.
    pub scale: [f32; 3],
}

/// One animation of `MDLA0006` (spec 08 §6.2).
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    /// Identifier referenced by `animationlayers[].animation`.
    pub id: u32,
    /// Name (SceneScript lookup).
    pub name: String,
    /// Mode string as written.
    pub mode_name: String,
    /// Parsed mode.
    pub mode: AnimationMode,
    /// Frames per second.
    pub fps: f32,
    /// `N`: number of frames; tracks hold `N + 1` samples (frame 0 to N).
    pub frames: u32,
    /// Track `k` drives bone `k`: one sample per frame.
    pub tracks: Vec<Vec<BoneSample>>,
}

impl Animation {
    /// Duration `N / fps` in seconds (0 when `fps ≤ 0`).
    #[must_use]
    pub fn duration(&self) -> f64 {
        if self.fps > 0.0 {
            f64::from(self.frames) / f64::from(self.fps)
        } else {
            0.0
        }
    }
}

/// Entry of the per-part index ranges of the mesh tail (spec 08 §2.6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartRange {
    /// Bone of the part.
    pub bone: u32,
    /// Reserved, observed 0.
    pub reserved: u32,
    /// First index (in indices, not bytes).
    pub first: u32,
    /// Number of indices.
    pub count: u32,
}

/// One chained section as found (spec 08 §3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionInfo {
    /// 8-character magic, e.g. `MDLS0004` (non-ASCII bytes escaped).
    pub magic: String,
    /// Absolute offset of the magic.
    pub start: usize,
    /// Absolute end offset (the `u32` after the magic).
    pub end: usize,
}

/// A non-fatal problem (spec 08 §8), to be logged with the file path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdlWarning {
    /// Section concerned (`MDLV`, `MDLS0004`…).
    pub section: String,
    /// Absolute offset in the `.mdl` file.
    pub offset: usize,
    /// What happened and what Lumenwall does instead.
    pub message: String,
}

impl fmt::Display for MdlWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at 0x{:X}: {}",
            self.section, self.offset, self.message
        )
    }
}

/// Whole puppet file (spec 08).
#[derive(Clone, Debug, PartialEq)]
pub struct PuppetModel {
    /// Mesh, identical to [`parse_puppet`] on every observed file.
    pub mesh: PuppetMesh,
    /// Skinning data of every vertex (same order as `mesh.positions`).
    pub skin: Vec<VertexSkin>,
    /// `true` when the mesh came from the structured layout (spec 08 §2.8 case 1), `false`
    /// for the spec 01 heuristic.
    pub structured: bool,
    /// Texture-layout positions ("planche", spec 08 §2.6.1): editor data, not for rendering.
    pub layout_positions: Option<Vec<[f32; 3]>>,
    /// Per-part index ranges (spec 08 §2.6.2): redundant for rendering.
    pub part_ranges: Vec<PartRange>,
    /// Offset of the first chained section (`F1`), if any.
    pub sections_start: Option<usize>,
    /// Chained sections in file order.
    pub sections: Vec<SectionInfo>,
    /// Skeleton; `None` = rest pose only, attachments and animations inactive (spec 08 §8).
    pub skeleton: Option<Skeleton>,
    /// Attachment points, with valid bone indices when a skeleton exists.
    pub attachments: Vec<Attachment>,
    /// Animations in file order.
    pub animations: Vec<Animation>,
    /// Local transform of each bone in the texture layout (`MDLE`, spec 08 §7). Not a
    /// render pose.
    pub editor_pose: Option<Vec<[f32; 16]>>,
    /// Non-fatal problems, in reading order.
    pub warnings: Vec<MdlWarning>,
}

impl PuppetModel {
    /// Number of bones of the skeleton (0 without one).
    #[must_use]
    pub fn bone_count(&self) -> usize {
        self.skeleton.as_ref().map_or(0, |s| s.bones.len())
    }

    /// First attachment with this name (byte-exact comparison, first duplicate wins).
    #[must_use]
    pub fn attachment(&self, name: &str) -> Option<&Attachment> {
        self.attachments.iter().find(|a| a.name == name)
    }

    /// Animation with this `id`.
    #[must_use]
    pub fn animation(&self, id: u32) -> Option<&Animation> {
        self.animations.iter().find(|a| a.id == id)
    }
}

fn mat4(r: &mut Reader<'_>) -> Result<[f32; 16], UnexpectedEof> {
    let mut m = [0.0; 16];
    for v in &mut m {
        *v = r.f32()?;
    }
    Ok(m)
}

fn vec3(r: &mut Reader<'_>) -> Result<[f32; 3], UnexpectedEof> {
    Ok([r.f32()?, r.f32()?, r.f32()?])
}

fn u16_le(r: &mut Reader<'_>) -> Result<u16, UnexpectedEof> {
    r.array().map(u16::from_le_bytes)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Reads one 80-byte puppet vertex's skinning fields.
fn vertex_skin(v: &[u8]) -> VertexSkin {
    let f = |o: usize| f32_at(v, o);
    let u = |o: usize| u32_at(v, o as u64).unwrap_or(0);
    VertexSkin {
        normal: [f(12), f(16), f(20)],
        tangent: [f(24), f(28), f(32), f(36)],
        joints: [u(40), u(44), u(48), u(52)],
        weights: [f(56), f(60), f(64), f(68)],
    }
}

/// Structured `MDLV0023` mesh (spec 08 §2.1–§2.6).
struct StructuredMesh {
    mesh: PuppetMesh,
    skin: Vec<VertexSkin>,
    layout_positions: Option<Vec<[f32; 3]>>,
    part_ranges: Vec<PartRange>,
    end: usize,
    warnings: Vec<MdlWarning>,
}

/// Case 1 of spec 08 §2.8. `Ok(None)` = not applicable, use the heuristic.
fn read_structured_mesh(data: &[u8]) -> Result<Option<StructuredMesh>, MdlError> {
    let mut r = Reader::new(data);
    let attempt = (|| -> Result<Option<StructuredMesh>, UnexpectedEof> {
        if r.take(9)?.get(..8) != Some(b"MDLV0023".as_slice()) {
            return Ok(None);
        }
        let _flags = r.u32()?;
        let _reserved = r.u32()?;
        if r.u32()? != 1 {
            return Ok(None);
        }
        let _material = r.cstr()?;
        let _reserved = r.u32()?;
        let _bounds = r.take(24)?;
        let block_offset = r.pos();
        if r.u32()? != PUPPET_VERTEX_FORMAT {
            return Ok(None);
        }
        let vb = r.u32()? as usize;
        if vb == 0 || !vb.is_multiple_of(VERTEX_SIZE as usize) {
            return Ok(None);
        }
        let vertices = r.take(vb)?;
        let ib = r.u32()? as usize;
        if ib == 0 || !ib.is_multiple_of(6) {
            return Ok(None);
        }
        let index_bytes = r.take(ib)?;
        let mut warnings = Vec::new();
        let nv = vb / VERTEX_SIZE as usize;
        // Tail (§2.6).
        let layout_positions = match r.take(1)?[0] {
            0 => None,
            1 => {
                let _reserved = r.u32()?;
                let at = r.pos();
                let lb = r.u32()? as usize;
                let bytes = r.take(lb)?;
                if lb == 12 * nv {
                    Some(
                        bytes
                            .as_chunks::<12>()
                            .0
                            .iter()
                            .map(|c| [f32_at(c, 0), f32_at(c, 4), f32_at(c, 8)])
                            .collect(),
                    )
                } else {
                    warnings.push(MdlWarning {
                        section: "MDLV".into(),
                        offset: at,
                        message: format!(
                            "layout block of {lb} bytes for {nv} vertices, layout positions ignored"
                        ),
                    });
                    None
                }
            }
            _ => return Ok(None),
        };
        let part_ranges = match r.take(1)?[0] {
            0 => Vec::new(),
            1 => {
                let at = r.pos();
                let lp = r.u32()? as usize;
                let bytes = r.take(lp)?;
                if !lp.is_multiple_of(16) {
                    warnings.push(MdlWarning {
                        section: "MDLV".into(),
                        offset: at,
                        message: format!("part range block of {lp} bytes is not a multiple of 16"),
                    });
                }
                bytes
                    .as_chunks::<16>()
                    .0
                    .iter()
                    .map(|c| PartRange {
                        bone: u32_at(c, 0).unwrap_or(0),
                        reserved: u32_at(c, 4).unwrap_or(0),
                        first: u32_at(c, 8).unwrap_or(0),
                        count: u32_at(c, 12).unwrap_or(0),
                    })
                    .collect()
            }
            _ => return Ok(None),
        };
        let _reserved = r.u32()?;
        let end = r.pos();

        let (chunks, _) = vertices.as_chunks::<80>();
        let positions = chunks
            .iter()
            .map(|v| [f32_at(v, 0), f32_at(v, 4), f32_at(v, 8)])
            .collect();
        let uvs = chunks
            .iter()
            .map(|v| [f32_at(v, 72), f32_at(v, 76)])
            .collect();
        let skin = chunks.iter().map(|v| vertex_skin(v)).collect();
        let indices = index_bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| u16::from_le_bytes(b))
            .collect();
        Ok(Some(StructuredMesh {
            mesh: PuppetMesh {
                version: MdlVersion::V23,
                positions,
                uvs,
                indices,
                block_offset,
            },
            skin,
            layout_positions,
            part_ranges,
            end,
            warnings,
        }))
    })();
    // A read past the end of the file means "not structured": the heuristic decides (§2.8).
    let Ok(Some(s)) = attempt else {
        return Ok(None);
    };
    let nv = s.mesh.positions.len();
    if let Some(&index) = s.mesh.indices.iter().find(|&&i| usize::from(i) >= nv) {
        return Err(MdlError::InvalidIndex {
            index,
            vertex_count: nv,
        });
    }
    Ok(Some(s))
}

/// First `MDLS` at `p ≥ 9` with `p + 4 < |D|` (rule `L` of spec 01 §3.1.2).
fn first_mdls(data: &[u8]) -> Option<usize> {
    data.windows(4)
        .enumerate()
        .skip(9)
        .find(|&(p, w)| p + 4 < data.len() && w == b"MDLS")
        .map(|(p, _)| p)
}

/// Section reading context: offsets are absolute, the content is bounded to `[start, end)`.
struct Section<'a> {
    name: String,
    start: usize,
    reader: Reader<'a>,
}

impl Section<'_> {
    fn offset(&self) -> usize {
        self.start + self.reader.pos()
    }

    fn warn(&self, warnings: &mut Vec<MdlWarning>, message: impl Into<String>) {
        warnings.push(MdlWarning {
            section: self.name.clone(),
            offset: self.offset(),
            message: message.into(),
        });
    }

    fn eof_warning(&self, e: UnexpectedEof, what: &str) -> MdlWarning {
        MdlWarning {
            section: self.name.clone(),
            offset: self.start + e.offset as usize,
            message: format!("read past the end of the section ({what})"),
        }
    }
}

/// `MDLS0004` (§4). `Err` = no skeleton.
fn read_skeleton(s: &mut Section<'_>, warnings: &mut Vec<MdlWarning>) -> Result<Skeleton, ()> {
    let r = &mut s.reader;
    let n = match r.u32() {
        Ok(n) => n as usize,
        Err(e) => {
            warnings.push(s.eof_warning(e, "bone count"));
            return Err(());
        }
    };
    // Each bone needs at least 78 bytes: never reserve more than the section can hold.
    let mut bones = Vec::with_capacity(n.min(r.remaining() / 78));
    for index in 0..n {
        let at = s.offset();
        let read = |r: &mut Reader<'_>| -> Result<(Bone, i32, u32), UnexpectedEof> {
            let name = text(r.cstr()?);
            let kind = r.u32()?;
            let parent = r.i32()?;
            let tm = r.u32()?;
            if tm != 64 {
                return Ok((
                    Bone {
                        name,
                        kind,
                        parent: None,
                        local: [0.0; 16],
                        extra: String::new(),
                    },
                    parent,
                    tm,
                ));
            }
            let local = mat4(r)?;
            let extra = text(r.cstr()?);
            Ok((
                Bone {
                    name,
                    kind,
                    parent: None,
                    local,
                    extra,
                },
                parent,
                tm,
            ))
        };
        let (mut bone, parent, tm) = match read(&mut s.reader) {
            Ok(b) => b,
            Err(e) => {
                warnings.push(s.eof_warning(e, "bone record"));
                return Err(());
            }
        };
        if tm != 64 {
            s.warn(
                warnings,
                format!("bone {index}: transform of {tm} bytes (64 expected), skeleton rejected"),
            );
            return Err(());
        }
        bone.parent = match usize::try_from(parent) {
            Ok(p) if p < index => Some(p),
            _ if parent == -1 => None,
            _ => {
                warnings.push(MdlWarning {
                    section: s.name.clone(),
                    offset: at,
                    message: format!("bone {index}: invalid parent {parent}, read as a root"),
                });
                None
            }
        };
        bones.push(bone);
    }
    let editor = read_skeleton_editor(&mut s.reader, n);
    Ok(Skeleton { bones, editor })
}

/// Editor data after the bones (§4.3, §4.4): optional, `None` on any surprise.
fn read_skeleton_editor(r: &mut Reader<'_>, n: usize) -> Option<SkeletonEditorData> {
    let _reserved = r.take(11).ok()?;
    if r.take(1).ok()?[0] != 1 {
        return None;
    }
    r.ensure(76 * n as u64).ok()?;
    let mut boxes = Vec::with_capacity(n);
    for _ in 0..n {
        boxes.push(BoneBox {
            half_extents: vec3(r).ok()?,
            transform: mat4(r).ok()?,
        });
    }
    let list = |r: &mut Reader<'_>| -> Option<Vec<u32>> {
        if r.take(1).ok()?[0] != 1 {
            return None;
        }
        r.ensure(4 * n as u64).ok()?;
        (0..n).map(|_| r.u32().ok()).collect()
    };
    let draw_order = list(r)?;
    let sort_keys = list(r)?;
    Some(SkeletonEditorData {
        boxes,
        draw_order,
        sort_keys,
    })
}

/// `MDAT0001` (§5). `Err` = no attachment.
fn read_attachments(s: &mut Section<'_>) -> Result<Vec<Attachment>, UnexpectedEof> {
    let r = &mut s.reader;
    let na = u16_le(r)? as usize;
    let mut out = Vec::with_capacity(na.min(r.remaining() / 67));
    for _ in 0..na {
        let bone = usize::from(u16_le(r)?);
        let name = text(r.cstr()?);
        let transform = mat4(r)?;
        out.push(Attachment {
            bone,
            name,
            transform,
        });
    }
    Ok(out)
}

/// `MDLA0006` (§6). Animations read in full before an error are kept (§8).
fn read_animations(s: &mut Section<'_>, warnings: &mut Vec<MdlWarning>) -> Vec<Animation> {
    let mut out = Vec::new();
    let na = match s.reader.u32() {
        Ok(n) => n,
        Err(e) => {
            warnings.push(s.eof_warning(e, "animation count"));
            return out;
        }
    };
    for a in 0..na {
        let at = s.offset();
        let read = (|| -> Result<Result<(Animation, bool), String>, UnexpectedEof> {
            let r = &mut s.reader;
            let id = r.u32()?;
            let _reserved = r.u32()?;
            let name = text(r.cstr()?);
            let mode_raw = r.cstr()?;
            let fps = r.f32()?;
            let frames = r.u32()?;
            let _reserved = r.u32()?;
            let nt = r.u32()? as usize;
            let mut tracks = Vec::with_capacity(nt.min(r.remaining() / 8));
            for t in 0..nt {
                let _reserved = r.u32()?;
                let lk = r.u32()? as usize;
                let bytes = r.take(lk)?;
                if !lk.is_multiple_of(SAMPLE_SIZE) {
                    return Ok(Err(format!(
                        "animation {id} track {t}: {lk} bytes of samples (multiple of 36 expected)"
                    )));
                }
                let samples = bytes
                    .as_chunks::<SAMPLE_SIZE>()
                    .0
                    .iter()
                    .map(|c| {
                        let f = |o: usize| f32_at(c, o);
                        BoneSample {
                            position: [f(0), f(4), f(8)],
                            angles: [f(12), f(16), f(20)],
                            scale: [f(24), f(28), f(32)],
                        }
                    })
                    .collect();
                tracks.push(samples);
            }
            let tail_clean = r.take(ANIMATION_TAIL)?.iter().all(|&b| b == 0);
            Ok(Ok((
                Animation {
                    id,
                    name,
                    mode_name: text(mode_raw),
                    mode: AnimationMode::parse(mode_raw),
                    fps,
                    frames,
                    tracks,
                },
                tail_clean,
            )))
        })();
        match read {
            Ok(Ok((anim, tail_clean))) => {
                let id = anim.id;
                out.push(anim);
                if !tail_clean {
                    s.warn(
                        warnings,
                        format!(
                            "animation {id}: reserved tail not zero, following animations dropped"
                        ),
                    );
                    break;
                }
            }
            Ok(Err(msg)) => {
                warnings.push(MdlWarning {
                    section: s.name.clone(),
                    offset: at,
                    message: format!("{msg}; animation {a} and following dropped"),
                });
                break;
            }
            Err(e) => {
                warnings.push(s.eof_warning(e, &format!("animation {a} and following dropped")));
                break;
            }
        }
    }
    out
}

/// `MDLE0002` (§7).
fn read_editor_pose(s: &mut Section<'_>) -> Result<Vec<[f32; 16]>, UnexpectedEof> {
    let r = &mut s.reader;
    let le = r.u32()? as usize;
    r.ensure(le as u64)?;
    let mut out = Vec::with_capacity(le / 64);
    for _ in 0..le / 64 {
        out.push(mat4(r)?);
    }
    Ok(out)
}

/// Sections found after the mesh block (spec 08 §3), before the load-time validation of
/// [`parse_model`].
#[derive(Default)]
pub(crate) struct SectionChain {
    pub sections: Vec<SectionInfo>,
    pub skeleton: Option<Skeleton>,
    pub attachments: Vec<Attachment>,
    pub animations: Vec<Animation>,
    pub editor_pose: Option<Vec<[f32; 16]>>,
}

/// Reads the section chain starting at `p` (spec 08 §3): stops at the end of the file, at a
/// `0x00` byte or at the first invalid header (with a warning).
pub(crate) fn read_section_chain(
    data: &[u8],
    mut p: usize,
    warnings: &mut Vec<MdlWarning>,
) -> SectionChain {
    let mut chain = SectionChain::default();
    let mut seen_skeleton = false;
    loop {
        if p >= data.len() || data[p] == 0 {
            break;
        }
        let Some(header) = data.get(p..p + 13) else {
            warnings.push(MdlWarning {
                section: String::from_utf8_lossy(&data[p..data.len().min(p + 8)]).into_owned(),
                offset: p,
                message: "truncated section header, chain stopped".into(),
            });
            break;
        };
        let magic = &header[..8];
        let name = magic.escape_ascii().to_string();
        if header[8] != 0 {
            if chain.sections.is_empty() {
                warnings.push(MdlWarning {
                    section: name,
                    offset: p,
                    message: "no section magic after the mesh".into(),
                });
            }
            break;
        }
        let end = u32_at(header, 9).unwrap_or(0) as usize;
        if end <= p + 13 || end > data.len() {
            warnings.push(MdlWarning {
                section: name,
                offset: p + 9,
                message: format!("invalid section end 0x{end:X}, chain stopped"),
            });
            break;
        }
        chain.sections.push(SectionInfo {
            magic: name.clone(),
            start: p,
            end,
        });
        let mut section = Section {
            name: name.clone(),
            start: p + 13,
            reader: Reader::new(&data[p + 13..end]),
        };
        let (kind, version) = magic.split_at(4);
        match (kind, version) {
            (b"MDLS", b"0004") => {
                if seen_skeleton {
                    section.warn(warnings, "second skeleton section ignored");
                } else {
                    seen_skeleton = true;
                    chain.skeleton = read_skeleton(&mut section, warnings).ok();
                }
            }
            (b"MDAT", b"0001") => match read_attachments(&mut section) {
                Ok(a) => chain.attachments = a,
                Err(e) => {
                    warnings.push(section.eof_warning(e, "no attachment kept"));
                    chain.attachments.clear();
                }
            },
            (b"MDLA", b"0006") => {
                chain.animations = read_animations(&mut section, warnings);
            }
            (b"MDLE", b"0002") => match read_editor_pose(&mut section) {
                Ok(pose) => chain.editor_pose = Some(pose),
                Err(e) => warnings.push(section.eof_warning(e, "editor pose ignored")),
            },
            (b"MDLS" | b"MDAT" | b"MDLA" | b"MDLE", _) => {
                section.warn(
                    warnings,
                    match kind {
                        b"MDLS" => "unsupported version: no skeleton",
                        b"MDAT" => "unsupported version: no attachment",
                        b"MDLA" => "unsupported version: no animation",
                        _ => "unsupported version, ignored",
                    },
                );
            }
            _ => {}
        }
        p = end;
    }
    chain
}

/// Reads a whole puppet file: mesh (structured when possible, otherwise the spec 01
/// heuristic), bone weights and every chained section (spec 08).
///
/// Only mesh failures are errors (same cases as [`parse_puppet`]); section problems are
/// reported in [`PuppetModel::warnings`].
pub fn parse_model(data: &[u8]) -> Result<PuppetModel, MdlError> {
    let (mesh, skin, layout_positions, part_ranges, start, structured, mut warnings) =
        match read_structured_mesh(data)? {
            Some(s) => (
                s.mesh,
                s.skin,
                s.layout_positions,
                s.part_ranges,
                Some(s.end),
                true,
                s.warnings,
            ),
            None => {
                let mesh = parse_puppet(data)?;
                // Heuristic: bone indices and weights at +40 and +56 of each 80-byte vertex.
                let base = mesh.block_offset + 8;
                let skin = (0..mesh.positions.len())
                    .map(|k| {
                        let v = base + k * VERTEX_SIZE as usize;
                        data.get(v..v + VERTEX_SIZE as usize)
                            .map(vertex_skin)
                            .unwrap_or_default()
                    })
                    .collect();
                (
                    mesh,
                    skin,
                    None,
                    Vec::new(),
                    first_mdls(data),
                    false,
                    Vec::new(),
                )
            }
        };

    let chain = read_section_chain(data, start.unwrap_or(data.len()), &mut warnings);
    let mut model = PuppetModel {
        mesh,
        skin,
        structured,
        layout_positions,
        part_ranges,
        sections_start: start,
        sections: chain.sections,
        skeleton: chain.skeleton,
        attachments: chain.attachments,
        animations: chain.animations,
        editor_pose: chain.editor_pose,
        warnings: Vec::new(),
    };

    // Load-time validation (§8): weights on missing bones reject the skeleton.
    if let Some(skeleton) = &model.skeleton {
        let n = skeleton.bones.len();
        let bad = model.skin.iter().position(|s| {
            s.joints
                .iter()
                .zip(&s.weights)
                .any(|(&j, &w)| w != 0.0 && j as usize >= n)
        });
        if let Some(v) = bad {
            warnings.push(MdlWarning {
                section: "MDLS".into(),
                offset: model.mesh.block_offset + 8 + v * VERTEX_SIZE as usize,
                message: format!(
                    "vertex {v} is weighted on a bone ≥ {n}: skeleton rejected, rest pose"
                ),
            });
            model.skeleton = None;
        } else if n == 0 {
            model.skeleton = None;
        }
    }
    let n = model.bone_count();
    if n > 0 {
        let before = model.attachments.len();
        model.attachments.retain(|a| a.bone < n);
        if model.attachments.len() != before {
            warnings.push(MdlWarning {
                section: "MDAT".into(),
                offset: model
                    .sections
                    .iter()
                    .find(|s| s.magic.starts_with("MDAT"))
                    .map_or(0, |s| s.start),
                message: format!(
                    "{} attachment(s) on a missing bone ignored",
                    before - model.attachments.len()
                ),
            });
        }
    }
    if let (Some(pose), true) = (&model.editor_pose, n > 0)
        && pose.len() != n
    {
        warnings.push(MdlWarning {
            section: "MDLE".into(),
            offset: 0,
            message: format!("{} editor matrices for {n} bones, ignored", pose.len()),
        });
        model.editor_pose = None;
    }
    model.warnings = warnings;
    Ok(model)
}

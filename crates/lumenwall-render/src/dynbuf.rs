//! One GPU buffer for everything that may change at run time: the per-frame
//! globals block, every pass uniform block (bound with dynamic offsets) and
//! the image vertices. A CPU shadow copy tracks the dirty byte range, and a
//! frame uploads it with a single `write_buffer` (none when nothing changed).
//!
//! Writes go through precomputed [`UniformSlot`]s (absolute byte offset +
//! type): no name lookup and no allocation per frame.

use crate::math::{VERTEX_SIZE, Vertex};

/// Scalar kind of a uniform member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarKind {
    Float,
    Sint,
    Uint,
}

/// Type of a uniform member, with its uniform-address-space layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UniformType {
    Scalar(ScalarKind),
    /// `vecN<kind>`, N in 2..=4.
    Vector(ScalarKind, u8),
    /// `matCxR<f32>`; columns are 8 bytes apart for R = 2, 16 otherwise.
    Matrix {
        columns: u8,
        rows: u8,
    },
    /// Fixed-size array; `stride` is the byte distance between elements.
    Array {
        element: Box<UniformType>,
        len: u32,
        stride: u32,
    },
    /// Anything else (nested structs): writable only as raw bytes.
    Opaque {
        size: u32,
    },
}

impl UniformType {
    /// Number of 32-bit components of one element (scalar/vector/matrix).
    #[must_use]
    pub fn components(&self) -> u32 {
        match self {
            Self::Scalar(_) => 1,
            Self::Vector(_, n) => u32::from(*n),
            Self::Matrix { columns, rows } => u32::from(*columns) * u32::from(*rows),
            Self::Array { element, len, .. } => element.components() * len,
            Self::Opaque { size } => size / 4,
        }
    }

    /// Byte size in the uniform address space.
    #[must_use]
    pub fn size(&self) -> u32 {
        match self {
            Self::Scalar(_) => 4,
            Self::Vector(_, n) => 4 * u32::from(*n),
            Self::Matrix { columns, rows } => {
                let col = if *rows == 2 { 8 } else { 16 };
                col * u32::from(*columns)
            }
            Self::Array { len, stride, .. } => len * stride,
            Self::Opaque { size } => *size,
        }
    }
}

/// Precomputed write handle of one uniform member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniformSlot {
    /// Absolute byte offset in the dynamic buffer.
    pub offset: u32,
    pub ty: UniformType,
}

/// A pass uniform block: `offset` is the dynamic offset to bind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UniformBlock {
    pub offset: u32,
    pub size: u32,
}

/// Vertices of the dynamic buffer: `first..first + count` in vertex units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VertexRange {
    pub first: u32,
    pub count: u32,
}

/// Layout of the per-frame globals block bound at `@group(0) @binding(1)`.
///
/// A shader translator may move the engine globals into this block instead of
/// the pass block; then a frame updates them once for all passes. The WGSL
/// declaration is [`FRAME_GLOBALS_WGSL`].
pub mod frame_globals {
    /// `g_Time: f32`.
    pub const TIME: u32 = 0;
    /// `g_Daytime: f32`.
    pub const DAYTIME: u32 = 4;
    /// `g_PointerPosition: vec2<f32>`.
    pub const POINTER: u32 = 8;
    /// `g_PointerPositionLast: vec2<f32>`.
    pub const POINTER_LAST: u32 = 16;
    /// `g_TexelSize: vec2<f32>` = (1/W, 1/H).
    pub const TEXEL_SIZE: u32 = 24;
    /// `g_TexelSizeHalf: vec2<f32>` = (0.5/W, 0.5/H).
    pub const TEXEL_SIZE_HALF: u32 = 32;
    /// `g_AudioSpectrum16Left: array<vec4<f32>, 4>` (16 packed floats).
    pub const AUDIO16_LEFT: u32 = 48;
    pub const AUDIO16_RIGHT: u32 = 112;
    /// `g_AudioSpectrum32Left: array<vec4<f32>, 8>`.
    pub const AUDIO32_LEFT: u32 = 176;
    pub const AUDIO32_RIGHT: u32 = 304;
    /// `g_AudioSpectrum64Left: array<vec4<f32>, 16>`.
    pub const AUDIO64_LEFT: u32 = 432;
    pub const AUDIO64_RIGHT: u32 = 688;
    /// Block size.
    pub const SIZE: u32 = 944;

    /// `(uniform name, offset, float count)` of every global. Audio arrays are
    /// packed four floats per `vec4`.
    pub const FIELDS: &[(&str, u32, u32)] = &[
        ("g_Time", TIME, 1),
        ("g_Daytime", DAYTIME, 1),
        ("g_PointerPosition", POINTER, 2),
        ("g_PointerPositionLast", POINTER_LAST, 2),
        ("g_TexelSize", TEXEL_SIZE, 2),
        ("g_TexelSizeHalf", TEXEL_SIZE_HALF, 2),
        ("g_AudioSpectrum16Left", AUDIO16_LEFT, 16),
        ("g_AudioSpectrum16Right", AUDIO16_RIGHT, 16),
        ("g_AudioSpectrum32Left", AUDIO32_LEFT, 32),
        ("g_AudioSpectrum32Right", AUDIO32_RIGHT, 32),
        ("g_AudioSpectrum64Left", AUDIO64_LEFT, 64),
        ("g_AudioSpectrum64Right", AUDIO64_RIGHT, 64),
    ];
}

/// WGSL declaration of the frame globals block (for shader translators).
pub const FRAME_GLOBALS_WGSL: &str = "struct LumenFrame {
    g_Time: f32,
    g_Daytime: f32,
    g_PointerPosition: vec2<f32>,
    g_PointerPositionLast: vec2<f32>,
    g_TexelSize: vec2<f32>,
    g_TexelSizeHalf: vec2<f32>,
    _pad0: vec2<f32>,
    g_AudioSpectrum16Left: array<vec4<f32>, 4>,
    g_AudioSpectrum16Right: array<vec4<f32>, 4>,
    g_AudioSpectrum32Left: array<vec4<f32>, 8>,
    g_AudioSpectrum32Right: array<vec4<f32>, 8>,
    g_AudioSpectrum64Left: array<vec4<f32>, 16>,
    g_AudioSpectrum64Right: array<vec4<f32>, 16>,
}
@group(0) @binding(1) var<uniform> lumenwall_frame: LumenFrame;
";

/// Per-frame engine inputs (spec 04 §2.1, §7.1; spec 03 §13.4).
#[derive(Clone, Copy, Debug)]
pub struct FrameInputs<'a> {
    /// `g_Time`, seconds. Lumenwall keeps the clock in f64 and converts here (spec 04 Q24).
    pub time: f32,
    /// `g_Daytime` in `[0, 1)`.
    pub daytime: f32,
    pub pointer: [f32; 2],
    pub pointer_last: [f32; 2],
    /// Spectrum; the same array feeds Left and Right (mono capture).
    pub audio16: &'a [f32; 16],
    pub audio32: &'a [f32; 32],
    pub audio64: &'a [f32; 64],
}

/// A byte copy from the frame globals block into a pass block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Patch {
    pub dst: u32,
    pub src: u32,
    pub len: u32,
}

/// Build the copies feeding a pass-block global from the frame globals block.
/// Returns `false` when the declared layout is not supported.
pub(crate) fn patches_for(name: &str, slot: &UniformSlot, out: &mut Vec<Patch>) -> bool {
    let Some(&(_, src, floats)) = frame_globals::FIELDS.iter().find(|f| f.0 == name) else {
        return false;
    };
    match &slot.ty {
        UniformType::Scalar(ScalarKind::Float) if floats == 1 => {
            out.push(Patch {
                dst: slot.offset,
                src,
                len: 4,
            });
            true
        }
        UniformType::Vector(ScalarKind::Float, n) if u32::from(*n) == floats => {
            out.push(Patch {
                dst: slot.offset,
                src,
                len: 4 * floats,
            });
            true
        }
        UniformType::Array {
            element,
            len,
            stride,
        } => match element.as_ref() {
            UniformType::Scalar(ScalarKind::Float) => {
                for i in 0..(*len).min(floats) {
                    out.push(Patch {
                        dst: slot.offset + i * stride,
                        src: src + 4 * i,
                        len: 4,
                    });
                }
                true
            }
            UniformType::Vector(ScalarKind::Float, 4) if *stride == 16 => {
                out.push(Patch {
                    dst: slot.offset,
                    src,
                    len: (len * 16).min(floats * 4),
                });
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// Source of the 32-bit words written by [`Shadow::write_floats`] / [`Shadow::write_ints`].
struct Words<'a> {
    n: usize,
    next: usize,
    word: &'a dyn Fn(usize) -> u32,
}

/// CPU shadow of the dynamic buffer with dirty-range tracking.
#[derive(Debug, Default)]
pub struct Shadow {
    bytes: Vec<u8>,
    dirty: Option<(u32, u32)>,
}

impl Shadow {
    fn mark(&mut self, start: u32, end: u32) {
        self.dirty = Some(match self.dirty {
            Some((a, b)) => (a.min(start), b.max(end)),
            None => (start, end),
        });
    }

    /// Copy `data` at `offset`; marks dirty only if the bytes differ.
    /// Returns whether anything changed.
    pub fn write_bytes(&mut self, offset: u32, data: &[u8]) -> bool {
        let start = offset as usize;
        let end = start + data.len();
        let Some(dst) = self.bytes.get_mut(start..end) else {
            debug_assert!(false, "uniform write out of bounds");
            return false;
        };
        if dst == data {
            return false;
        }
        dst.copy_from_slice(data);
        self.mark(offset, end as u32);
        true
    }

    /// Write floats following the member layout (vec3 packing, matrix column
    /// stride, array stride). Extra values are ignored, missing ones untouched.
    pub fn write_floats(&mut self, slot: &UniformSlot, values: &[f32]) -> bool {
        self.write_words(slot, values.len(), |i| values[i].to_bits())
    }

    /// Integer flavour of [`Self::write_floats`] (for `int`/`uint` members).
    pub fn write_ints(&mut self, slot: &UniformSlot, values: &[i32]) -> bool {
        self.write_words(slot, values.len(), |i| values[i] as u32)
    }

    fn write_words(&mut self, slot: &UniformSlot, n: usize, word: impl Fn(usize) -> u32) -> bool {
        let mut words = Words {
            n,
            next: 0,
            word: &word,
        };
        self.write_typed(slot.offset, &slot.ty, &mut words)
    }

    /// Writes consecutive values into one member, following its layout recursively (array
    /// elements may be matrices, e.g. the `mat4x3[]` bone palette of spec 08 §9.8).
    fn write_typed(&mut self, at: u32, ty: &UniformType, w: &mut Words<'_>) -> bool {
        match ty {
            UniformType::Scalar(_) => self.put_words(at, 1, w),
            UniformType::Vector(_, c) => self.put_words(at, u32::from(*c), w),
            UniformType::Matrix { columns, rows } => {
                let stride = if *rows == 2 { 8 } else { 16 };
                let mut changed = false;
                for col in 0..u32::from(*columns) {
                    changed |= self.put_words(at + col * stride, u32::from(*rows), w);
                }
                changed
            }
            UniformType::Array {
                element,
                len,
                stride,
            } => {
                let mut changed = false;
                for e in 0..*len {
                    if w.next >= w.n {
                        break;
                    }
                    changed |= self.write_typed(at + e * stride, element, w);
                }
                changed
            }
            UniformType::Opaque { size } => self.put_words(at, size / 4, w),
        }
    }

    fn put_words(&mut self, at: u32, count: u32, w: &mut Words<'_>) -> bool {
        let mut changed = false;
        for c in 0..count {
            if w.next >= w.n {
                break;
            }
            changed |= self.write_bytes(at + 4 * c, &(w.word)(w.next).to_le_bytes());
            w.next += 1;
        }
        changed
    }

    /// Write a 4×4 matrix (column-major, as uploaded upstream).
    pub fn write_mat4(&mut self, slot: &UniformSlot, m: &glam::Mat4) -> bool {
        match slot.ty {
            UniformType::Matrix {
                columns: 4,
                rows: 4,
            } => {
                let a = m.to_cols_array();
                self.write_bytes(slot.offset, bytemuck::cast_slice(&a))
            }
            _ => self.write_floats(slot, &m.to_cols_array()),
        }
    }

    fn apply(&mut self, p: Patch) -> bool {
        let (s, d, l) = (p.src as usize, p.dst as usize, p.len as usize);
        if self.bytes[s..s + l] == self.bytes[d..d + l] {
            return false;
        }
        self.bytes.copy_within(s..s + l, d);
        self.mark(p.dst, p.dst + p.len);
        true
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty.is_some()
    }
}

/// Load-time builder of the dynamic buffer layout.
#[derive(Debug)]
pub struct DynamicBufferBuilder {
    align: u32,
    shadow: Shadow,
    max_block: u32,
    vertices: Vec<Vertex>,
}

impl DynamicBufferBuilder {
    /// `align` = `min_uniform_buffer_offset_alignment` of the device.
    #[must_use]
    pub fn new(align: u32) -> Self {
        let align = align.max(16);
        let mut shadow = Shadow::default();
        shadow
            .bytes
            .resize(frame_globals::SIZE.next_multiple_of(align) as usize, 0);
        Self {
            align,
            shadow,
            max_block: 16,
            vertices: Vec::new(),
        }
    }

    /// Reserve a pass uniform block.
    pub fn alloc_uniform(&mut self, size: u32) -> UniformBlock {
        let size = size.max(16).next_multiple_of(16);
        let offset = (self.shadow.bytes.len() as u32).next_multiple_of(self.align);
        self.shadow.bytes.resize((offset + size) as usize, 0);
        self.max_block = self.max_block.max(size);
        UniformBlock { offset, size }
    }

    /// Append vertices (they can be rewritten every frame).
    pub fn alloc_vertices(&mut self, v: &[Vertex]) -> VertexRange {
        let first = self.vertices.len() as u32;
        self.vertices.extend_from_slice(v);
        VertexRange {
            first,
            count: v.len() as u32,
        }
    }

    /// Initial values (static uniforms are written once, here).
    pub fn shadow_mut(&mut self) -> &mut Shadow {
        &mut self.shadow
    }

    /// Size of the uniform binding shared by every pass.
    #[must_use]
    pub fn binding_size(&self) -> u32 {
        self.max_block
    }

    /// Create the GPU buffer and upload the initial content.
    pub fn build(mut self, ctx: &crate::context::GpuContext) -> DynamicBuffer {
        // Any block offset + binding size must stay inside the buffer.
        let uniform_end =
            (self.shadow.bytes.len() as u32 + self.max_block).next_multiple_of(VERTEX_SIZE * 4);
        self.shadow.bytes.resize(uniform_end as usize, 0);
        let vertex_offset = uniform_end;
        self.shadow
            .bytes
            .extend_from_slice(bytemuck::cast_slice(&self.vertices));
        let total = (self.shadow.bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
        self.shadow.bytes.resize(total as usize, 0);
        let buffer = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumenwall.dynamic"),
            size: total,
            usage: wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue().write_buffer(&buffer, 0, &self.shadow.bytes);
        ctx.note_resources(1);
        self.shadow.dirty = None;
        DynamicBuffer {
            memory: ctx.memory().track(total),
            buffer,
            shadow: self.shadow,
            binding_size: self.max_block,
            vertex_offset,
        }
    }
}

/// The run-time dynamic buffer.
#[derive(Debug)]
pub struct DynamicBuffer {
    buffer: wgpu::Buffer,
    shadow: Shadow,
    binding_size: u32,
    vertex_offset: u32,
    #[allow(dead_code)]
    memory: crate::context::MemoryToken,
}

impl DynamicBuffer {
    #[must_use]
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub fn shadow_mut(&mut self) -> &mut Shadow {
        &mut self.shadow
    }

    #[must_use]
    pub fn shadow(&self) -> &Shadow {
        &self.shadow
    }

    #[must_use]
    pub fn binding_size(&self) -> u32 {
        self.binding_size
    }

    #[must_use]
    pub fn vertex_offset(&self) -> u32 {
        self.vertex_offset
    }

    /// Rewrite vertices (image moved by a script, etc.).
    pub fn set_vertices(&mut self, range: VertexRange, v: &[Vertex]) -> bool {
        let n = v.len().min(range.count as usize);
        let off = self.vertex_offset + range.first * VERTEX_SIZE;
        self.shadow.write_bytes(off, bytemuck::cast_slice(&v[..n]))
    }

    /// Write the frame globals block and feed the pass-block copies.
    pub(crate) fn update_frame(&mut self, f: &FrameInputs<'_>, patches: &[Patch]) -> bool {
        use frame_globals as g;
        let s = &mut self.shadow;
        let mut changed = s.write_bytes(g::TIME, &f.time.to_le_bytes());
        changed |= s.write_bytes(g::DAYTIME, &f.daytime.to_le_bytes());
        changed |= s.write_bytes(g::POINTER, bytemuck::cast_slice(&f.pointer));
        changed |= s.write_bytes(g::POINTER_LAST, bytemuck::cast_slice(&f.pointer_last));
        for (l, r, data) in [
            (g::AUDIO16_LEFT, g::AUDIO16_RIGHT, &f.audio16[..]),
            (g::AUDIO32_LEFT, g::AUDIO32_RIGHT, &f.audio32[..]),
            (g::AUDIO64_LEFT, g::AUDIO64_RIGHT, &f.audio64[..]),
        ] {
            changed |= s.write_bytes(l, bytemuck::cast_slice(data));
            changed |= s.write_bytes(r, bytemuck::cast_slice(data));
        }
        for p in patches {
            changed |= s.apply(*p);
        }
        changed
    }

    /// Upload the dirty range: at most one `write_buffer` per frame.
    pub fn flush(&mut self, queue: &wgpu::Queue) -> bool {
        let Some((start, end)) = self.shadow.dirty.take() else {
            return false;
        };
        let start = start & !3;
        let end = end.next_multiple_of(4).min(self.shadow.bytes.len() as u32);
        queue.write_buffer(
            &self.buffer,
            u64::from(start),
            &self.shadow.bytes[start as usize..end as usize],
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shadow(n: usize) -> Shadow {
        Shadow {
            bytes: vec![0; n],
            dirty: None,
        }
    }

    fn f(s: &Shadow, at: usize) -> f32 {
        f32::from_le_bytes(s.bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn layouts_and_sizes() {
        assert_eq!(UniformType::Vector(ScalarKind::Float, 3).size(), 12);
        assert_eq!(
            UniformType::Matrix {
                columns: 3,
                rows: 3
            }
            .size(),
            48
        );
        assert_eq!(
            UniformType::Matrix {
                columns: 4,
                rows: 4
            }
            .size(),
            64
        );
        assert_eq!(
            UniformType::Matrix {
                columns: 2,
                rows: 2
            }
            .size(),
            16
        );
        let arr = UniformType::Array {
            element: Box::new(UniformType::Scalar(ScalarKind::Float)),
            len: 16,
            stride: 16,
        };
        assert_eq!(arr.size(), 256);
        assert_eq!(arr.components(), 16);
    }

    #[test]
    fn writes_follow_layout_and_track_dirty() {
        let mut s = shadow(256);
        let m3 = UniformSlot {
            offset: 16,
            ty: UniformType::Matrix {
                columns: 3,
                rows: 3,
            },
        };
        assert!(s.write_floats(&m3, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]));
        assert_eq!(
            (f(&s, 16), f(&s, 24), f(&s, 32), f(&s, 40), f(&s, 48 + 8)),
            (1.0, 3.0, 4.0, 6.0, 9.0)
        );
        assert_eq!(s.dirty, Some((16, 60)));
        s.dirty = None;
        assert!(
            !s.write_floats(&m3, &[1.0, 2.0, 3.0]),
            "same value: not dirty"
        );
        assert!(s.dirty.is_none());
        let arr = UniformSlot {
            offset: 100,
            ty: UniformType::Array {
                element: Box::new(UniformType::Scalar(ScalarKind::Float)),
                len: 3,
                stride: 16,
            },
        };
        s.write_floats(&arr, &[1.5, 2.5, 3.5, 99.0]);
        assert_eq!((f(&s, 100), f(&s, 116), f(&s, 132)), (1.5, 2.5, 3.5));
        let i = UniformSlot {
            offset: 200,
            ty: UniformType::Scalar(ScalarKind::Sint),
        };
        s.write_ints(&i, &[-3]);
        assert_eq!(
            i32::from_le_bytes(s.bytes[200..204].try_into().unwrap()),
            -3
        );
    }

    #[test]
    fn matrix_arrays_keep_the_column_stride() {
        // `mat4x3 g_Bones[2]`: 4 columns of 3 floats, columns 16 bytes apart, 64 per bone.
        let mut s = shadow(256);
        let bones = UniformSlot {
            offset: 32,
            ty: UniformType::Array {
                element: Box::new(UniformType::Matrix {
                    columns: 4,
                    rows: 3,
                }),
                len: 2,
                stride: 64,
            },
        };
        let values: Vec<f32> = (1..=24).map(|v| v as f32).collect();
        assert!(s.write_floats(&bones, &values));
        assert_eq!((f(&s, 32), f(&s, 40), f(&s, 44)), (1.0, 3.0, 0.0));
        assert_eq!((f(&s, 48), f(&s, 80), f(&s, 88)), (4.0, 10.0, 12.0));
        assert_eq!((f(&s, 96), f(&s, 144 + 8)), (13.0, 24.0));
        assert_eq!(s.dirty, Some((32, 156)));
        s.dirty = None;
        assert!(
            !s.write_floats(&bones, &values),
            "unchanged palette: no upload"
        );
        let mut first = values[..12].to_vec();
        first[0] = -1.0;
        assert!(
            s.write_floats(&bones, &first),
            "a prefix updates the first bone only"
        );
        assert_eq!((f(&s, 32), f(&s, 96)), (-1.0, 13.0));
        assert_eq!(s.dirty, Some((32, 36)));
    }

    #[test]
    fn frame_patches() {
        let mut out = Vec::new();
        let time = UniformSlot {
            offset: 1024,
            ty: UniformType::Scalar(ScalarKind::Float),
        };
        assert!(patches_for("g_Time", &time, &mut out));
        assert_eq!(
            out,
            vec![Patch {
                dst: 1024,
                src: 0,
                len: 4
            }]
        );
        out.clear();
        let audio = UniformSlot {
            offset: 2048,
            ty: UniformType::Array {
                element: Box::new(UniformType::Scalar(ScalarKind::Float)),
                len: 16,
                stride: 16,
            },
        };
        assert!(patches_for("g_AudioSpectrum16Left", &audio, &mut out));
        assert_eq!(out.len(), 16);
        assert_eq!(
            out[1],
            Patch {
                dst: 2064,
                src: 52,
                len: 4
            }
        );
        out.clear();
        let packed = UniformSlot {
            offset: 4096,
            ty: UniformType::Array {
                element: Box::new(UniformType::Vector(ScalarKind::Float, 4)),
                len: 16,
                stride: 16,
            },
        };
        assert!(patches_for("g_AudioSpectrum64Right", &packed, &mut out));
        assert_eq!(
            out,
            vec![Patch {
                dst: 4096,
                src: frame_globals::AUDIO64_RIGHT,
                len: 256
            }]
        );
        assert!(!patches_for("g_Unknown", &time, &mut out));
        let wrong = UniformSlot {
            offset: 0,
            ty: UniformType::Vector(ScalarKind::Float, 3),
        };
        assert!(!patches_for("g_Time", &wrong, &mut out));
        // Offsets of the published WGSL block are consistent with FIELDS.
        let mut end = 0;
        for (_, off, n) in frame_globals::FIELDS {
            assert!(*off >= end);
            end = off + 4 * n;
        }
        assert_eq!(end, frame_globals::SIZE);
    }

    #[test]
    fn builder_layout() {
        let mut b = DynamicBufferBuilder::new(256);
        let a = b.alloc_uniform(100);
        let c = b.alloc_uniform(300);
        assert_eq!(a.offset % 256, 0);
        assert!(a.offset >= frame_globals::SIZE);
        assert_eq!(c.offset, a.offset + 256);
        assert_eq!(a.size, 112);
        assert_eq!(b.binding_size(), 304);
        let v = b.alloc_vertices(&crate::math::unit_quad());
        let w = b.alloc_vertices(&crate::math::unit_quad());
        assert_eq!((v.first, w.first, w.count), (0, 6, 6));
    }
}

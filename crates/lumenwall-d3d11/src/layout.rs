//! Pure layout rules of the backend, tested on every platform: constant arena windows, linear
//! buffer arenas and texture upload descriptions.

use lumenwall_formats::tex::GpuFormat;

use crate::error::{Error, Result};

/// Alignment of a constant buffer window bound with `*SetConstantBuffers1` (16 constants of
/// 16 bytes, Microsoft documentation of `VSSetConstantBuffers1`).
pub const CONSTANT_WINDOW_ALIGN: u32 = 256;
/// Largest window a shader can address: 4096 constants.
pub const MAX_CONSTANT_WINDOW: u32 = 4096 * 16;

/// One window of the constant arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConstantRegion {
    /// Byte offset in the arena, a multiple of 256.
    pub offset: u32,
    /// Byte size of the window, a multiple of 256 (the block itself may be smaller).
    pub size: u32,
}

impl ConstantRegion {
    /// `pFirstConstant` of `*SetConstantBuffers1`.
    #[must_use]
    pub fn first_constant(self) -> u32 {
        self.offset / 16
    }

    /// `pNumConstants` of `*SetConstantBuffers1`.
    #[must_use]
    pub fn num_constants(self) -> u32 {
        self.size / 16
    }

    #[must_use]
    pub fn range(self) -> std::ops::Range<usize> {
        self.offset as usize..(self.offset + self.size) as usize
    }
}

/// Allocation of windows in the constant arena (load time; the frame only rewrites bytes).
#[derive(Clone, Debug, Default)]
pub struct ConstantLayout {
    len: u32,
}

impl ConstantLayout {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A window for a block of `bytes` (at least one 256-byte unit).
    pub fn alloc(&mut self, bytes: u32) -> Result<ConstantRegion> {
        if bytes > MAX_CONSTANT_WINDOW {
            return Err(Error::InvalidData(format!(
                "constant block of {bytes} bytes exceeds the {MAX_CONSTANT_WINDOW}-byte window"
            )));
        }
        let size = bytes.max(1).next_multiple_of(CONSTANT_WINDOW_ALIGN);
        let offset = self.len;
        self.len = offset
            .checked_add(size)
            .ok_or_else(|| Error::InvalidData("constant arena overflow".into()))?;
        Ok(ConstantRegion { offset, size })
    }

    /// Bytes the arena needs (a multiple of 256, at least 256).
    #[must_use]
    pub fn len(&self) -> u32 {
        self.len.max(CONSTANT_WINDOW_ALIGN)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Bytes of a static buffer built at load time (shared vertex and index data of every pass).
#[derive(Clone, Debug, Default)]
pub struct LinearArena {
    bytes: Vec<u8>,
}

impl LinearArena {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `data` at an offset aligned to `align` bytes (a power of two) and returns it.
    pub fn push(&mut self, data: &[u8], align: usize) -> u32 {
        debug_assert!(align.is_power_of_two());
        let offset = self.bytes.len().next_multiple_of(align);
        self.bytes.resize(offset, 0);
        self.bytes.extend_from_slice(data);
        offset as u32
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Colour format of a render target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TargetFormat {
    /// Every target of the wgpu path today (`RT_FORMAT`).
    #[default]
    Rgba8Unorm,
    /// HDR targets (`QualitySettings::hdr`, bloom of HDR scenes).
    Rgba16Float,
    /// `fbos` declaring one or two channels.
    R8Unorm,
    Rg8Unorm,
}

impl TargetFormat {
    #[must_use]
    pub fn bytes_per_pixel(self) -> u64 {
        match self {
            Self::Rgba8Unorm => 4,
            Self::Rgba16Float => 8,
            Self::R8Unorm => 1,
            Self::Rg8Unorm => 2,
        }
    }
}

/// Format of a vertex attribute in a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VertexFormat {
    Float32,
    Float32x2,
    Float32x3,
    Float32x4,
    Sint32,
    Sint32x2,
    Sint32x3,
    Sint32x4,
    Uint32,
    Uint32x2,
    Uint32x3,
    Uint32x4,
    Unorm8x4,
    Uint8x4,
    Unorm16x2,
    Unorm16x4,
    Uint16x4,
}

impl VertexFormat {
    /// Bytes of one attribute.
    #[must_use]
    pub fn size(self) -> u32 {
        match self {
            Self::Float32
            | Self::Sint32
            | Self::Uint32
            | Self::Unorm8x4
            | Self::Uint8x4
            | Self::Unorm16x2 => 4,
            Self::Float32x2
            | Self::Sint32x2
            | Self::Uint32x2
            | Self::Unorm16x4
            | Self::Uint16x4 => 8,
            Self::Float32x3 | Self::Sint32x3 | Self::Uint32x3 => 12,
            Self::Float32x4 | Self::Sint32x4 | Self::Uint32x4 => 16,
        }
    }
}

/// 2D texture or volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dimension {
    D2,
    D3,
}

/// One mip level of an upload: tightly packed rows, slices one after the other (what
/// `lumenwall-formats` produces).
#[derive(Clone, Copy, Debug)]
pub struct MipLevel<'a> {
    pub width: u32,
    pub height: u32,
    /// 1 for 2D textures.
    pub depth: u32,
    pub data: &'a [u8],
}

/// Largest 2D texture at feature level 11_0.
pub const MAX_TEXTURE_2D: u32 = 16384;
/// Largest volume edge at feature level 11_0.
pub const MAX_TEXTURE_3D: u32 = 2048;

/// `(row pitch, slice pitch)` of a tightly packed level (`D3D11_SUBRESOURCE_DATA`).
#[must_use]
pub fn pitches(format: GpuFormat, width: u32, height: u32) -> (u32, u32) {
    let row = format.bytes_per_row(width);
    let slice = format.level_size(width, height);
    (row as u32, slice as u32)
}

/// Checks an upload against Direct3D 11's rules: level sizes `max(1, base >> k)`, exact byte
/// counts, BC level 0 a multiple of 4, size limits of feature level 11_0.
pub fn validate_upload(
    format: GpuFormat,
    dimension: Dimension,
    levels: &[MipLevel<'_>],
) -> Result<()> {
    let bad = |m: String| Err(Error::InvalidData(m));
    let Some(base) = levels.first() else {
        return bad("no mip level".into());
    };
    let (w, h, d) = (base.width, base.height, base.depth);
    if w == 0 || h == 0 || d == 0 {
        return bad(format!("zero-sized level 0 ({w}×{h}×{d})"));
    }
    match dimension {
        Dimension::D2 if d != 1 => return bad(format!("2D texture with depth {d}")),
        Dimension::D2 if w > MAX_TEXTURE_2D || h > MAX_TEXTURE_2D => {
            return bad(format!("{w}×{h} exceeds {MAX_TEXTURE_2D}"));
        }
        Dimension::D3 if w.max(h).max(d) > MAX_TEXTURE_3D => {
            return bad(format!("{w}×{h}×{d} exceeds {MAX_TEXTURE_3D}"));
        }
        _ => {}
    }
    if format.is_block_compressed() && (w % 4 != 0 || h % 4 != 0) {
        return bad(format!(
            "block-compressed level 0 of {w}×{h} is not a multiple of 4"
        ));
    }
    let max_levels = 32 - w.max(h).max(d).leading_zeros();
    if levels.len() as u32 > max_levels {
        return bad(format!("{} levels for a {w}×{h}×{d} texture", levels.len()));
    }
    for (k, level) in levels.iter().enumerate() {
        let expect = (
            (w >> k).max(1),
            (h >> k).max(1),
            if dimension == Dimension::D3 {
                (d >> k).max(1)
            } else {
                1
            },
        );
        if (level.width, level.height, level.depth) != expect {
            return bad(format!(
                "level {k} is {}×{}×{}, expected {}×{}×{}",
                level.width, level.height, level.depth, expect.0, expect.1, expect.2
            ));
        }
        let bytes = format.volume_size(level.width, level.height, level.depth);
        if level.data.len() as u64 != bytes {
            return bad(format!(
                "level {k} has {} bytes, expected {bytes}",
                level.data.len()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_windows_are_256_aligned() {
        let mut l = ConstantLayout::new();
        assert!(l.is_empty());
        assert_eq!(l.len(), 256, "an empty arena still binds one window");
        let a = l.alloc(4).unwrap();
        let b = l.alloc(256).unwrap();
        let c = l.alloc(257).unwrap();
        assert_eq!(
            a,
            ConstantRegion {
                offset: 0,
                size: 256
            }
        );
        assert_eq!(
            b,
            ConstantRegion {
                offset: 256,
                size: 256
            }
        );
        assert_eq!(
            c,
            ConstantRegion {
                offset: 512,
                size: 512
            }
        );
        assert_eq!((c.first_constant(), c.num_constants()), (32, 32));
        assert_eq!(l.len(), 1024);
        assert_eq!(c.range(), 512..1024);
        assert!(l.alloc(MAX_CONSTANT_WINDOW).is_ok());
        assert!(l.alloc(MAX_CONSTANT_WINDOW + 1).is_err());
    }

    #[test]
    fn linear_arena_aligns_offsets() {
        let mut a = LinearArena::new();
        assert_eq!(a.push(&[1, 2, 3], 4), 0);
        assert_eq!(a.push(&[4; 8], 16), 16);
        assert_eq!(a.push(&[5], 1), 24);
        assert_eq!(a.len(), 25);
        assert_eq!(&a.bytes()[..4], &[1, 2, 3, 0]);
    }

    #[test]
    fn upload_rules() {
        let l0 = vec![0u8; 8 * 8 * 4];
        let l1 = vec![0u8; 4 * 4 * 4];
        let l2 = vec![0u8; 2 * 2 * 4];
        let l3 = vec![0u8; 4];
        let mip = |w, h, d, data: &'static [u8]| MipLevel {
            width: w,
            height: h,
            depth: d,
            data,
        };
        let levels = [
            MipLevel {
                width: 8,
                height: 8,
                depth: 1,
                data: &l0,
            },
            MipLevel {
                width: 4,
                height: 4,
                depth: 1,
                data: &l1,
            },
            MipLevel {
                width: 2,
                height: 2,
                depth: 1,
                data: &l2,
            },
            MipLevel {
                width: 1,
                height: 1,
                depth: 1,
                data: &l3,
            },
        ];
        validate_upload(GpuFormat::Rgba8Unorm, Dimension::D2, &levels).unwrap();
        assert!(validate_upload(GpuFormat::Rgba8Unorm, Dimension::D2, &levels[1..2]).is_ok());
        // Wrong byte count, wrong level size, too many levels, nothing.
        assert!(validate_upload(GpuFormat::Rg8Unorm, Dimension::D2, &levels).is_err());
        let wrong = [levels[0], levels[2]];
        assert!(validate_upload(GpuFormat::Rgba8Unorm, Dimension::D2, &wrong).is_err());
        let mut five = levels.to_vec();
        five.push(mip(1, 1, 1, &[0; 4]));
        assert!(validate_upload(GpuFormat::Rgba8Unorm, Dimension::D2, &five).is_err());
        assert!(validate_upload(GpuFormat::Rgba8Unorm, Dimension::D2, &[]).is_err());
        // BC: 4×4 blocks, level 0 a multiple of 4, small levels still one block.
        let bc0 = [0u8; 2 * 2 * 8];
        let bc1 = [0u8; 8];
        let bc2 = [0u8; 8];
        let bc = [
            MipLevel {
                width: 8,
                height: 8,
                depth: 1,
                data: &bc0,
            },
            MipLevel {
                width: 4,
                height: 4,
                depth: 1,
                data: &bc1,
            },
            MipLevel {
                width: 2,
                height: 2,
                depth: 1,
                data: &bc2,
            },
        ];
        validate_upload(GpuFormat::Bc1RgbaUnorm, Dimension::D2, &bc).unwrap();
        let odd = [MipLevel {
            width: 6,
            height: 8,
            depth: 1,
            data: &[0u8; 2 * 2 * 8],
        }];
        assert!(validate_upload(GpuFormat::Bc1RgbaUnorm, Dimension::D2, &odd).is_err());
        // Volumes: depth halves too; 2D refuses depth.
        let v0 = vec![0u8; 4 * 4 * 4 * 4];
        let v1 = vec![0u8; 2 * 2 * 2 * 4];
        let vol = [
            MipLevel {
                width: 4,
                height: 4,
                depth: 4,
                data: &v0,
            },
            MipLevel {
                width: 2,
                height: 2,
                depth: 2,
                data: &v1,
            },
        ];
        validate_upload(GpuFormat::Rgba8Unorm, Dimension::D3, &vol).unwrap();
        assert!(validate_upload(GpuFormat::Rgba8Unorm, Dimension::D2, &vol).is_err());
        assert_eq!(pitches(GpuFormat::Bc3RgbaUnorm, 8, 8), (32, 64));
        assert_eq!(pitches(GpuFormat::R8Unorm, 3, 2), (3, 6));
    }
}

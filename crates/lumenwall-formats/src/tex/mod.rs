//! `.tex` texture parser (spec 01 §2).
//!
//! [`parse`] reads the whole block sequence (TEXV, TEXI, TEXB, optional TEXS) in one
//! pass and borrows uncompressed mip data from the input; only LZ4 levels are
//! decompressed, straight into a buffer of the final size. [`Tex::into_upload`] then
//! turns the result into a GPU-agnostic [`PreparedTexture`]: BC1/BC2/BC3 blocks are
//! passed through untouched, R8/RG8/RGBA8 pixels are borrowed, and only embedded
//! images (PNG, JPEG, PSD…) are decoded on the CPU. Textures with the `VOLUME` flag
//! (amendment A.1, colour LUTs) become 3D textures.

mod animation;
mod embedded;
mod psd;
mod texjson;
mod upload;

use std::borrow::Cow;

use thiserror::Error;

use crate::reader::{Reader, UnexpectedEof, escape_bytes};

pub use animation::{Animation, Frame, FrameState};
pub use embedded::{
    DecodedImage, EmbeddedImageError, decode_block_compressed, decode_embedded_image,
};
pub use psd::PsdError;
pub use upload::{
    AddressMode, FilterMode, GpuContent, GpuFormat, GpuImage, GpuMip, PreparedTexture, SamplerDesc,
    TextureDimension, UploadOptions,
};

/// Default cap on any single level the parser or the upload step allocates (1 GiB,
/// a 16384×16384 RGBA8 level). Lumenwall-specific protection against hostile sizes.
pub const DEFAULT_MAX_LEVEL_BYTES: u64 = 1 << 30;

/// Errors raised while parsing a `.tex` file or preparing it for the GPU. All of
/// them are in the spec's `Invalide` category (spec 01 §2.13).
#[derive(Debug, Error)]
pub enum TexError {
    /// Global magic is not `TEXV0005\0` (`E_TEX_TEXV`).
    #[error("E_TEX_TEXV: global magic \"{0}\" is not \"TEXV0005\\0\"")]
    BadTexv(String),
    /// Description magic is not `TEXI0001\0` (`E_TEX_TEXI`).
    #[error("E_TEX_TEXI: description magic \"{0}\" is not \"TEXI0001\\0\"")]
    BadTexi(String),
    /// Pixel format outside the list (`E_TEX_FORMAT`).
    #[error("E_TEX_FORMAT: unknown pixel format {0}")]
    BadFormat(u32),
    /// Flags value refused (`E_TEX_DRAPEAUX`).
    #[error("E_TEX_DRAPEAUX: flags {0:#x} ({0}) refused (must be below 524335)")]
    BadFlags(u32),
    /// Container magic is not `TEXB0001\0`..`TEXB0004\0` (`E_TEX_TEXB`).
    #[error("E_TEX_TEXB: container magic \"{0}\" is not TEXB0001..TEXB0004")]
    BadTexb(String),
    /// Embedded image type outside the list (`E_TEX_TIE`).
    #[error("E_TEX_TIE: unknown embedded image type {}", *.0 as i32)]
    BadEmbeddedType(u32),
    /// LZ4 block failed to decode (`E_TEX_LZ4`).
    #[error("E_TEX_LZ4: image {image} mip {mip}: LZ4 block failed to decode: {detail}")]
    Lz4 {
        /// Image index.
        image: u32,
        /// Mip index.
        mip: u32,
        /// Decoder message.
        detail: String,
    },
    /// A size field is negative (`E_TEX_TAILLE`, Lumenwall-specific).
    #[error("E_TEX_TAILLE: image {image} mip {mip}: negative {field} ({value})")]
    NegativeSize {
        /// Image index.
        image: u32,
        /// Mip index.
        mip: u32,
        /// `TD` or `TS`.
        field: &'static str,
        /// The value read.
        value: i32,
    },
    /// A level would need more memory than allowed (`E_TEX_TAILLE`, Lumenwall-specific).
    #[error("E_TEX_TAILLE: image {image} mip {mip}: level needs {bytes} bytes, limit is {limit}")]
    TooLarge {
        /// Image index.
        image: u32,
        /// Mip index.
        mip: u32,
        /// Bytes required.
        bytes: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Animation magic is not `TEXS0001\0`..`TEXS0003\0` (`E_TEX_TEXS`).
    #[error("E_TEX_TEXS: animation magic \"{0}\" is not TEXS0001..TEXS0003")]
    BadTexs(String),
    /// Truncated file (`FinDeFluxInattendue`).
    #[error(transparent)]
    UnexpectedEof(#[from] UnexpectedEof),
    /// Raw pixel format with no GPU mapping (`E_TEX_FORMAT_GPU`, raised at upload).
    #[error("E_TEX_FORMAT_GPU: pixel format {0:?} cannot be uploaded to the GPU")]
    UnsupportedGpuFormat(TexFormat),
    /// Video texture without any mipmap (`E_TEX_VIDEO_VIDE`, raised at upload).
    #[error("E_TEX_VIDEO_VIDE: video texture has no image or no mipmap")]
    EmptyVideo,
    /// `VOLUME` combined with `ANIMATED`, `VIDEO` or MP4 (`E_TEX_VOLUME`,
    /// Lumenwall-specific, raised at upload, amendment A.1).
    #[error(
        "E_TEX_VOLUME: volume texture combined with animation or video (flags {flags:#x}, mp4 {mp4})"
    )]
    UnsupportedVolume {
        /// Flags of the file.
        flags: u32,
        /// MP4 indicator.
        mp4: bool,
    },
}

impl TexError {
    /// Spec error code, for logs and tests.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadTexv(_) => "E_TEX_TEXV",
            Self::BadTexi(_) => "E_TEX_TEXI",
            Self::BadFormat(_) => "E_TEX_FORMAT",
            Self::BadFlags(_) => "E_TEX_DRAPEAUX",
            Self::BadTexb(_) => "E_TEX_TEXB",
            Self::BadEmbeddedType(_) => "E_TEX_TIE",
            Self::Lz4 { .. } => "E_TEX_LZ4",
            Self::NegativeSize { .. } | Self::TooLarge { .. } => "E_TEX_TAILLE",
            Self::BadTexs(_) => "E_TEX_TEXS",
            Self::UnexpectedEof(_) => "FinDeFluxInattendue",
            Self::UnsupportedGpuFormat(_) => "E_TEX_FORMAT_GPU",
            Self::EmptyVideo => "E_TEX_VIDEO_VIDE",
            Self::UnsupportedVolume { .. } => "E_TEX_VOLUME",
        }
    }
}

/// Pixel format field of TEXI (spec 01 §2.3). Names follow the file format; note that
/// `Rgba8888` (value 0, "ARGB8888" upstream) is stored R, G, B, A in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TexFormat {
    /// `0xFFFFFFFF`.
    Unknown,
    /// 0: 4 bytes per pixel, memory order R, G, B, A.
    Rgba8888,
    /// 1: 3 bytes per pixel.
    Rgb888,
    /// 2: 2 bytes per pixel.
    Rgb565,
    /// 4: BC3.
    Dxt5,
    /// 6: BC2.
    Dxt3,
    /// 7: BC1 with 1-bit alpha.
    Dxt1,
    /// 8: 2 bytes per pixel, R, G.
    Rg88,
    /// 9: 1 byte per pixel.
    R8,
    /// 10.
    Rg1616f,
    /// 11.
    R16f,
    /// 12.
    Bc7,
    /// 13.
    Rgba1010102,
    /// 14.
    Rgba16161616f,
    /// 15.
    Rgb161616f,
}

impl TexFormat {
    /// Maps the raw field; `None` for values outside the list (3, 5, 16+).
    pub fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            0xFFFF_FFFF => Self::Unknown,
            0 => Self::Rgba8888,
            1 => Self::Rgb888,
            2 => Self::Rgb565,
            4 => Self::Dxt5,
            6 => Self::Dxt3,
            7 => Self::Dxt1,
            8 => Self::Rg88,
            9 => Self::R8,
            10 => Self::Rg1616f,
            11 => Self::R16f,
            12 => Self::Bc7,
            13 => Self::Rgba1010102,
            14 => Self::Rgba16161616f,
            15 => Self::Rgb161616f,
            _ => return None,
        })
    }

    /// The raw field value.
    pub fn raw(self) -> u32 {
        match self {
            Self::Unknown => 0xFFFF_FFFF,
            Self::Rgba8888 => 0,
            Self::Rgb888 => 1,
            Self::Rgb565 => 2,
            Self::Dxt5 => 4,
            Self::Dxt3 => 6,
            Self::Dxt1 => 7,
            Self::Rg88 => 8,
            Self::R8 => 9,
            Self::Rg1616f => 10,
            Self::R16f => 11,
            Self::Bc7 => 12,
            Self::Rgba1010102 => 13,
            Self::Rgba16161616f => 14,
            Self::Rgb161616f => 15,
        }
    }

    /// Value the `TEX0FORMAT` shader combo is forced to when this is the **declared**
    /// format (offset 0x12) of an object's main texture (§2.10.5, amendment A.3): 8
    /// for RG88, 9 for R8, `None` (combo untouched) otherwise. The renderer applies it
    /// to every pass of the object, effects included.
    pub fn tex0_format_combo(self) -> Option<u32> {
        // parity: decided on the declared format only, even when an embedded image is
        // uploaded as RGBA8 (QO-36).
        match self {
            Self::Rg88 => Some(8),
            Self::R8 => Some(9),
            _ => None,
        }
    }
}

/// TEXI flags (spec 01 §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TexFlags(u32);

impl TexFlags {
    /// Nearest filtering.
    pub const NO_INTERPOLATION: u32 = 0x1;
    /// Clamp-to-edge addressing on both axes; repeat otherwise.
    pub const CLAMP_UVS: u32 = 0x2;
    /// A TEXS block follows the images.
    pub const ANIMATED: u32 = 0x4;
    /// Clamp-to-border; ignored for `.tex` textures (QO-05).
    pub const CLAMP_UVS_BORDER: u32 = 0x8;
    /// Video texture.
    pub const VIDEO: u32 = 0x20;
    /// 3D texture: a depth field in TEXI and in every mip header (amendment A.1).
    pub const VOLUME: u32 = 0x40;
    /// Alpha carried by G (RG88) or R (R8). Read, never used.
    pub const ALPHA_PRIORITY: u32 = 0x8_0000;
    /// All documented bits.
    pub const KNOWN: u32 = Self::NO_INTERPOLATION
        | Self::CLAMP_UVS
        | Self::ANIMATED
        | Self::CLAMP_UVS_BORDER
        | Self::VIDEO
        | Self::VOLUME
        | Self::ALPHA_PRIORITY;

    /// Validates a raw flags value.
    pub fn from_raw(raw: u32) -> Result<Self, TexError> {
        // parity: the value is accepted iff it is numerically below 524335 (0x8002F,
        // the sum of the documented bits other than VOLUME). This is a comparison, not
        // a mask: the combination of those bits is refused and undocumented low bits
        // are accepted (spec 01 §2.4, QO-04).
        if raw >= 524_335 {
            return Err(TexError::BadFlags(raw));
        }
        if raw & !Self::KNOWN != 0 {
            tracing::debug!(
                flags = raw,
                unknown = raw & !Self::KNOWN,
                "undocumented .tex flag bits"
            );
        }
        Ok(Self(raw))
    }

    /// Raw value.
    pub fn bits(self) -> u32 {
        self.0
    }

    /// Whether every bit of `mask` is set.
    pub fn contains(self, mask: u32) -> bool {
        self.0 & mask == mask
    }
}

/// Embedded image type `TIE` (spec 01 §2.6, FreeImage numbering). Only
/// "unknown / known" matters for decoding: the content is sniffed.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EmbeddedFormat {
    /// −1: raw pixels, no embedded image.
    Unknown,
    Bmp,
    Ico,
    Jpeg,
    Jng,
    Koala,
    Lbm,
    Mng,
    Pbm,
    PbmRaw,
    Pcd,
    Pcx,
    Pgm,
    PgmRaw,
    Png,
    Ppm,
    PpmRaw,
    Ras,
    Targa,
    Tiff,
    Wbmp,
    Psd,
    Cut,
    Xbm,
    Xpm,
    Dds,
    Gif,
    Hdr,
    FaxG3,
    Sgi,
    Exr,
    J2k,
    Jp2,
    Pfm,
    Pict,
    Raw,
    /// 35: WEBP, or MP4 video in TEXB0004 (§2.5).
    WebpOrMp4,
    Jxr,
}

impl EmbeddedFormat {
    const TABLE: [Self; 37] = [
        Self::Bmp,
        Self::Ico,
        Self::Jpeg,
        Self::Jng,
        Self::Koala,
        Self::Lbm,
        Self::Mng,
        Self::Pbm,
        Self::PbmRaw,
        Self::Pcd,
        Self::Pcx,
        Self::Pgm,
        Self::PgmRaw,
        Self::Png,
        Self::Ppm,
        Self::PpmRaw,
        Self::Ras,
        Self::Targa,
        Self::Tiff,
        Self::Wbmp,
        Self::Psd,
        Self::Cut,
        Self::Xbm,
        Self::Xpm,
        Self::Dds,
        Self::Gif,
        Self::Hdr,
        Self::FaxG3,
        Self::Sgi,
        Self::Exr,
        Self::J2k,
        Self::Jp2,
        Self::Pfm,
        Self::Pict,
        Self::Raw,
        Self::WebpOrMp4,
        Self::Jxr,
    ];

    /// Maps the raw field; `None` outside −1..=36.
    pub fn from_raw(raw: u32) -> Option<Self> {
        if raw == u32::MAX {
            return Some(Self::Unknown);
        }
        Self::TABLE.get(raw as usize).copied()
    }

    /// Raw value as a signed integer (−1 for [`Unknown`](Self::Unknown)).
    pub fn raw(self) -> i32 {
        Self::TABLE
            .iter()
            .position(|&f| f == self)
            .map_or(-1, |i| i as i32)
    }
}

/// One mipmap level as stored in the file, after decompression.
#[derive(Debug, Clone, PartialEq)]
pub struct Mip<'a> {
    /// Width from the level header.
    pub width: u32,
    /// Height from the level header.
    pub height: u32,
    /// Depth from the level header (`PM`); 1 without the `VOLUME` flag.
    pub depth: u32,
    /// Compression field (`C`): 0 raw, 1 LZ4, other values raw `TD` bytes (QO-08).
    pub compression: u32,
    /// Final bytes: borrowed from the input unless LZ4 was decoded.
    pub data: Cow<'a, [u8]>,
    /// JSON metadata of effective-version-4 levels (kept, unused).
    pub metadata: Option<&'a [u8]>,
}

/// Spritesheet values derived from TEXS or the `.tex-json` companion (spec 01 §2.9).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spritesheet {
    /// Columns.
    pub columns: u32,
    /// Rows.
    pub rows: u32,
    /// Frame count; the texture "has a spritesheet" when it is non-zero.
    pub frames: u32,
    /// Total duration in seconds.
    pub duration: f32,
}

impl Spritesheet {
    /// Whether the texture has a spritesheet (`frames > 0`).
    pub fn is_present(&self) -> bool {
        self.frames > 0
    }
}

/// Parser limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOptions {
    /// Largest LZ4 output buffer allocated for one level.
    pub max_level_bytes: u64,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            max_level_bytes: DEFAULT_MAX_LEVEL_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageRange {
    start: u32,
    count: u32,
}

/// A parsed `.tex` file.
#[derive(Debug, Clone, PartialEq)]
pub struct Tex<'a> {
    /// Pixel format field.
    pub format: TexFormat,
    /// Flags.
    pub flags: TexFlags,
    /// Allocated surface width `LT`.
    pub texture_width: u32,
    /// Allocated surface height `HT`.
    pub texture_height: u32,
    /// Useful content width `LI`.
    pub image_width: u32,
    /// Useful content height `HI`.
    pub image_height: u32,
    /// Texture depth `PT` (number of slices); 1 without the `VOLUME` flag.
    pub texture_depth: u32,
    /// Version in the container magic (1..=4).
    pub container_version: u8,
    /// Effective container version (TEXB0004 can fall back to 3).
    pub effective_version: u8,
    /// Embedded image type `TIE`.
    pub embedded_format: EmbeddedFormat,
    /// MP4 indicator of TEXB0004.
    pub is_mp4: bool,
    /// TEXS block, when the `ANIMATED` flag is set.
    pub animation: Option<Animation>,
    /// Spritesheet values.
    pub spritesheet: Spritesheet,
    /// Offset just past the last block read; bytes after it are ignored (§2.1).
    pub end_offset: usize,
    images: Vec<ImageRange>,
    mips: Vec<Mip<'a>>,
}

/// Parses a `.tex` file with default limits.
pub fn parse(data: &[u8]) -> Result<Tex<'_>, TexError> {
    parse_with(data, &ParseOptions::default())
}

/// Parses a `.tex` file.
pub fn parse_with<'a>(data: &'a [u8], options: &ParseOptions) -> Result<Tex<'a>, TexError> {
    let mut r = Reader::new(data);

    // Spec 01 §2.2: TEXV, TEXI, format, flags, in that order.
    let texv: [u8; 9] = r.array()?;
    if &texv != b"TEXV0005\0" {
        return Err(TexError::BadTexv(escape_bytes(&texv)));
    }
    let texi: [u8; 9] = r.array()?;
    if &texi != b"TEXI0001\0" {
        return Err(TexError::BadTexi(escape_bytes(&texi)));
    }
    let format_raw = r.u32()?;
    let format = TexFormat::from_raw(format_raw).ok_or(TexError::BadFormat(format_raw))?;
    let flags = TexFlags::from_raw(r.u32()?)?;
    let texture_width = r.u32()?;
    let texture_height = r.u32()?;
    let image_width = r.u32()?;
    let image_height = r.u32()?;
    // A.1: with VOLUME a depth field comes first and shifts everything after by +4.
    let volume = flags.contains(TexFlags::VOLUME);
    let texture_depth = if volume { r.u32()? } else { 1 };
    let _reserved_color = r.u32()?;

    // Spec 01 §2.5: the image count is read before the container magic is checked.
    let texb: [u8; 9] = r.array()?;
    let image_count = r.u32()?;
    let container_version = match &texb {
        b"TEXB0001\0" => 1,
        b"TEXB0002\0" => 2,
        b"TEXB0003\0" => 3,
        b"TEXB0004\0" => 4,
        _ => return Err(TexError::BadTexb(escape_bytes(&texb))),
    };
    let mut embedded_format = EmbeddedFormat::Unknown;
    let mut is_mp4 = false;
    let mut effective_version = container_version;
    if container_version >= 3 {
        let raw = r.u32()?;
        embedded_format = EmbeddedFormat::from_raw(raw).ok_or(TexError::BadEmbeddedType(raw))?;
    }
    if container_version == 4 {
        is_mp4 = r.u32()? == 1;
        if embedded_format == EmbeddedFormat::Unknown && is_mp4 {
            embedded_format = EmbeddedFormat::WebpOrMp4;
        }
        if embedded_format != EmbeddedFormat::WebpOrMp4 {
            effective_version = 3;
        }
    }

    // Spec 01 §2.7. Capacities are capped by what the input can hold.
    let mut images = Vec::with_capacity((image_count as usize).min(r.remaining() / 4));
    let mut mips = Vec::new();
    let min_mip = match effective_version {
        1 => 12,
        2 | 3 => 20,
        _ => 33,
    } + if volume { 4 } else { 0 };
    let layout = MipLayout {
        version: effective_version,
        volume,
    };
    for image in 0..image_count {
        let mip_count = r.u32()?;
        let start = mips.len() as u32;
        mips.reserve((mip_count as usize).min(r.remaining() / min_mip));
        for mip in 0..mip_count {
            mips.push(read_mip(&mut r, layout, image, mip, options)?);
        }
        images.push(ImageRange {
            start,
            count: mip_count,
        });
    }

    let mut tex = Tex {
        format,
        flags,
        texture_width,
        texture_height,
        image_width,
        image_height,
        texture_depth,
        container_version,
        effective_version,
        embedded_format,
        is_mp4,
        animation: None,
        spritesheet: Spritesheet::default(),
        end_offset: 0,
        images,
        mips,
    };

    if flags.contains(TexFlags::ANIMATED) {
        let animation = animation::read_texs(&mut r)?;
        tex.spritesheet = animation::infer_spritesheet(&animation, image_width, image_height);
        tex.animation = Some(animation);
    }
    // Anything after the last expected block is ignored (§2.1).
    tex.end_offset = r.pos();
    Ok(tex)
}

/// What decides the layout of a mip header.
#[derive(Debug, Clone, Copy)]
struct MipLayout {
    /// Effective container version.
    version: u8,
    /// `VOLUME` flag: a depth `u32` follows the height (A.1).
    volume: bool,
}

fn read_mip<'a>(
    r: &mut Reader<'a>,
    layout: MipLayout,
    image: u32,
    mip: u32,
    options: &ParseOptions,
) -> Result<Mip<'a>, TexError> {
    let version = layout.version;
    let negative = |field, value| TexError::NegativeSize {
        image,
        mip,
        field,
        value,
    };

    let mut metadata = None;
    if version == 4 {
        let _reserved_a = r.u32()?;
        let _reserved_b = r.u32()?;
        metadata = Some(r.cstr()?);
        let _reserved_c = r.u32()?;
    }
    let width = r.u32()?;
    let height = r.u32()?;
    // A.1: the depth follows the height in every version (only v3 is observed).
    let depth = if layout.volume { r.u32()? } else { 1 };

    if version == 1 {
        // No compression field: raw data, TD = TS.
        let stored = r.i32()?;
        let stored = usize::try_from(stored).map_err(|_| negative("TS", stored))?;
        return Ok(Mip {
            width,
            height,
            depth,
            compression: 0,
            data: Cow::Borrowed(r.take(stored)?),
            metadata,
        });
    }

    let compression = r.u32()?;
    let decompressed = r.i32()?;
    let stored = r.i32()?;
    let decompressed = usize::try_from(decompressed).map_err(|_| negative("TD", decompressed))?;
    let stored = usize::try_from(stored).map_err(|_| negative("TS", stored))?;

    let data = match compression {
        // TD is overwritten by TS.
        0 => Cow::Borrowed(r.take(stored)?),
        1 => {
            let src = r.take(stored)?;
            if decompressed as u64 > options.max_level_bytes {
                return Err(TexError::TooLarge {
                    image,
                    mip,
                    bytes: decompressed as u64,
                    limit: options.max_level_bytes,
                });
            }
            let mut out = vec![0u8; decompressed];
            let written =
                lz4_flex::block::decompress_into(src, &mut out).map_err(|e| TexError::Lz4 {
                    image,
                    mip,
                    detail: e.to_string(),
                })?;
            if written != decompressed {
                // QO-08: the tail stays zeroed.
                tracing::warn!(
                    image,
                    mip,
                    written,
                    expected = decompressed,
                    "LZ4 level shorter than its declared size; tail zero-filled"
                );
            }
            Cow::Owned(out)
        }
        other => {
            // parity: unknown compression reads TD raw bytes, not TS (QO-08).
            tracing::warn!(
                image,
                mip,
                compression = other,
                "unknown .tex compression; reading TD raw bytes"
            );
            Cow::Borrowed(r.take(decompressed)?)
        }
    };
    Ok(Mip {
        width,
        height,
        depth,
        compression,
        data,
        metadata,
    })
}

impl<'a> Tex<'a> {
    /// Number of images `NI`.
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Mipmaps of image `index`, level 0 first.
    pub fn image(&self, index: usize) -> Option<&[Mip<'a>]> {
        let range = self.images.get(index)?;
        let start = range.start as usize;
        self.mips.get(start..start + range.count as usize)
    }

    /// Mipmaps of every image, in file order.
    pub fn images(&self) -> impl ExactSizeIterator<Item = &[Mip<'a>]> + '_ {
        self.images.iter().map(|range| {
            let start = range.start as usize;
            &self.mips[start..start + range.count as usize]
        })
    }

    /// Whether mip levels are embedded image files (`TIE` ≠ unknown).
    pub fn has_embedded_image(&self) -> bool {
        self.embedded_format != EmbeddedFormat::Unknown
    }

    /// Whether the `ANIMATED` flag is set.
    pub fn is_animated(&self) -> bool {
        self.flags.contains(TexFlags::ANIMATED)
    }

    /// Whether the texture goes through the video path (§2.10.1 step 3).
    pub fn is_video(&self) -> bool {
        self.is_mp4 || self.flags.contains(TexFlags::VIDEO)
    }

    /// Whether the `VOLUME` flag is set: one 3D texture per image (A.1).
    pub fn is_volume(&self) -> bool {
        self.flags.contains(TexFlags::VOLUME)
    }

    /// Real size `(LR, HR)`: animation size if animated, else image size (§2.11).
    pub fn real_size(&self) -> [u32; 2] {
        match &self.animation {
            Some(a) if self.is_animated() => [a.width, a.height],
            _ => [self.image_width, self.image_height],
        }
    }

    /// Texture size of image `index` (§2.11): level-0 header size, or `(LT, HT)`
    /// past the last image. An image without mipmaps counts as 1×1 (QO-09).
    pub fn texture_size(&self, index: usize) -> [u32; 2] {
        match self.image(index) {
            Some([first, ..]) => [first.width, first.height],
            Some([]) => [1, 1],
            None => [self.texture_width, self.texture_height],
        }
    }

    /// Resolution vector for `g_TextureNResolution` (§2.11).
    pub fn resolution(&self) -> [u32; 4] {
        if self.is_animated() {
            let [w, h] = self.real_size();
            [self.texture_width, self.texture_height, w, h]
        } else if self.has_embedded_image() {
            let [w, h] = self.texture_size(0);
            [w, h, self.image_width, self.image_height]
        } else {
            [
                self.texture_width,
                self.texture_height,
                self.image_width,
                self.image_height,
            ]
        }
    }

    /// Useful UV area of an image object: `(LR / width0, HR / height0)` (§2.11).
    pub fn uv_extent(&self) -> [f32; 2] {
        uv_extent(self.real_size(), self.texture_size(0))
    }

    /// Forced value of the `TEX0FORMAT` shader combo when this texture is an object's
    /// main texture: decided on the declared format [`format`](Self::format) only
    /// (§2.10.5, A.3). See [`TexFormat::tex0_format_combo`].
    pub fn tex0_format_combo(&self) -> Option<u32> {
        self.format.tex0_format_combo()
    }

    /// Applies a `materials/<name>.tex-json` companion (§2.9.2). Any error is
    /// ignored; returns whether the spritesheet values were replaced.
    pub fn apply_tex_json(&mut self, json: &[u8]) -> bool {
        match texjson::spritesheet_from_json(json, self.image_width, self.image_height) {
            Some(sheet) => {
                self.spritesheet = sheet;
                true
            }
            None => false,
        }
    }
}

pub(crate) fn uv_extent(real: [u32; 2], texture: [u32; 2]) -> [f32; 2] {
    let ratio = |num: u32, den: u32| {
        if den == 0 {
            0.0
        } else {
            num as f32 / den as f32
        }
    };
    [ratio(real[0], texture[0]), ratio(real[1], texture[1])]
}

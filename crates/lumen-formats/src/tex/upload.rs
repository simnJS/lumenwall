//! Conversion of a parsed `.tex` into a GPU-agnostic upload description
//! (spec 01 §2.10, §2.11).

use std::borrow::Cow;

use super::embedded::{decode_block_compressed, decode_embedded_image};
use super::{
    Animation, DEFAULT_MAX_LEVEL_BYTES, FrameState, Mip, Spritesheet, Tex, TexError, TexFlags,
    TexFormat, uv_extent,
};

/// Options of [`Tex::into_upload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadOptions {
    /// Opt-in formats the reference refuses with `E_TEX_FORMAT_GPU`: RGB888 and
    /// RGB565 (expanded to RGBA8 on the CPU), RG1616f, R16f, RGBA16161616f,
    /// RGB161616f (expanded to RGBA16F), BC7, RGBA1010102, and WEBP embedded images.
    /// Off by default for parity.
    pub extended_formats: bool,
    /// Largest level (in bytes) this step allocates or accepts.
    pub max_level_bytes: u64,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            extended_formats: false,
            max_level_bytes: DEFAULT_MAX_LEVEL_BYTES,
        }
    }
}

/// GPU texture formats produced by this crate. Names match `wgpu::TextureFormat`;
/// every `.tex` texture is linear (UNORM), never sRGB (§2.10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuFormat {
    /// One 8-bit channel.
    R8Unorm,
    /// Two 8-bit channels.
    Rg8Unorm,
    /// Four 8-bit channels, R, G, B, A in memory.
    Rgba8Unorm,
    /// BC1 (DXT1) with 1-bit alpha.
    Bc1RgbaUnorm,
    /// BC2 (DXT3).
    Bc2RgbaUnorm,
    /// BC3 (DXT5).
    Bc3RgbaUnorm,
    /// BC7 (extended formats only).
    Bc7RgbaUnorm,
    /// One half-float channel (extended formats only).
    R16Float,
    /// Two half-float channels (extended formats only).
    Rg16Float,
    /// Four half-float channels (extended formats only).
    Rgba16Float,
    /// 10-10-10-2 (extended formats only).
    Rgb10a2Unorm,
}

impl GpuFormat {
    /// Whether the format stores 4×4 blocks.
    pub fn is_block_compressed(self) -> bool {
        matches!(
            self,
            Self::Bc1RgbaUnorm | Self::Bc2RgbaUnorm | Self::Bc3RgbaUnorm | Self::Bc7RgbaUnorm
        )
    }

    /// Block edge in pixels: 4 for BCn, 1 otherwise.
    pub fn block_dimension(self) -> u32 {
        if self.is_block_compressed() { 4 } else { 1 }
    }

    /// Bytes per block (per pixel for uncompressed formats).
    pub fn block_bytes(self) -> u32 {
        match self {
            Self::R8Unorm => 1,
            Self::Rg8Unorm | Self::R16Float => 2,
            Self::Rgba8Unorm | Self::Rg16Float | Self::Rgb10a2Unorm => 4,
            Self::Bc1RgbaUnorm | Self::Rgba16Float => 8,
            Self::Bc2RgbaUnorm | Self::Bc3RgbaUnorm | Self::Bc7RgbaUnorm => 16,
        }
    }

    /// Bytes of one row of blocks for a level `width` pixels wide (tightly packed).
    pub fn bytes_per_row(self, width: u32) -> u64 {
        u64::from(width.div_ceil(self.block_dimension()).max(1)) * u64::from(self.block_bytes())
    }

    /// Rows of blocks for a level `height` pixels high.
    pub fn rows_per_image(self, height: u32) -> u64 {
        u64::from(height.div_ceil(self.block_dimension()).max(1))
    }

    /// Size of a whole level: `max(1, ceil(w/4)) × max(1, ceil(h/4)) × block` for BCn.
    /// Saturates instead of overflowing on hostile sizes.
    pub fn level_size(self, width: u32, height: u32) -> u64 {
        self.bytes_per_row(width)
            .saturating_mul(self.rows_per_image(height))
    }
}

/// Texture addressing on U and V.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressMode {
    /// Clamp to edge (`CLAMP_UVS` flag).
    ClampToEdge,
    /// Repeat (default).
    Repeat,
}

/// Texture filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterMode {
    /// Nearest.
    Nearest,
    /// Linear.
    Linear,
}

/// Sampler of a `.tex` texture (§2.10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplerDesc {
    /// Addressing on U and V.
    pub address_mode: AddressMode,
    /// Magnification filter.
    pub mag_filter: FilterMode,
    /// Minification filter.
    pub min_filter: FilterMode,
    /// Filter between mip levels.
    pub mipmap_filter: FilterMode,
    /// Maximum anisotropy.
    pub anisotropy_clamp: u16,
}

impl SamplerDesc {
    /// Sampler for a texture with the given flags.
    pub fn from_flags(flags: TexFlags) -> Self {
        // parity: CLAMP_UVS_BORDER (0x8) has no effect on `.tex` textures (QO-05).
        let address_mode = if flags.contains(TexFlags::CLAMP_UVS) {
            AddressMode::ClampToEdge
        } else {
            AddressMode::Repeat
        };
        if flags.contains(TexFlags::NO_INTERPOLATION) {
            // The reference requests anisotropy 8 in every case. wgpu only allows
            // anisotropy with linear filters, and it has no effect on nearest
            // sampling, so nearest textures get 1.
            Self {
                address_mode,
                mag_filter: FilterMode::Nearest,
                min_filter: FilterMode::Nearest,
                mipmap_filter: FilterMode::Nearest,
                anisotropy_clamp: 1,
            }
        } else {
            Self {
                address_mode,
                mag_filter: FilterMode::Linear,
                min_filter: FilterMode::Linear,
                mipmap_filter: FilterMode::Linear,
                anisotropy_clamp: 8,
            }
        }
    }
}

/// One level ready for `write_texture`: tightly packed rows (alignment 1, QO-10),
/// first row of the file first (no flip).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuMip<'a> {
    /// Level width in pixels.
    pub width: u32,
    /// Level height in pixels.
    pub height: u32,
    /// Exactly `format.level_size(width, height)` bytes.
    pub data: Cow<'a, [u8]>,
}

/// One GPU texture (one `.tex` image).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuImage<'a> {
    /// Levels, 0 first. Never empty; level `k` is `max(1, base >> k)` on each axis.
    pub mips: Vec<GpuMip<'a>>,
}

impl GpuImage<'_> {
    /// Width of level 0.
    pub fn width(&self) -> u32 {
        self.mips.first().map_or(1, |m| m.width)
    }

    /// Height of level 0.
    pub fn height(&self) -> u32 {
        self.mips.first().map_or(1, |m| m.height)
    }

    /// Number of mip levels.
    pub fn mip_level_count(&self) -> u32 {
        self.mips.len() as u32
    }
}

/// What to create on the GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuContent<'a> {
    /// One texture per image (§2.10.3); image `i` is texture index `i`.
    Images {
        /// Format shared by every image.
        format: GpuFormat,
        /// The textures.
        images: Vec<GpuImage<'a>>,
    },
    /// Video texture (§2.10.2): a complete video file to play from memory.
    Video {
        /// Bytes of mip 0 of image 0.
        data: Cow<'a, [u8]>,
        /// Output surface width `LT`.
        width: u32,
        /// Output surface height `HT`.
        height: u32,
        /// Mip count of image 0, for the sampler.
        mip_count: u32,
    },
}

/// A texture ready to upload, with everything the renderer needs besides pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedTexture<'a> {
    /// Pixels or video.
    pub content: GpuContent<'a>,
    /// Sampler.
    pub sampler: SamplerDesc,
    /// `g_TextureNResolution` (§2.11).
    pub resolution: [u32; 4],
    /// Real size `(LR, HR)`.
    pub real_size: [u32; 2],
    /// Texture size of each image (§2.11, level-0 header sizes).
    pub texture_sizes: Vec<[u32; 2]>,
    /// `(LT, HT)`: texture size for indices past the last image.
    pub default_texture_size: [u32; 2],
    /// TEXS data, if animated.
    pub animation: Option<Animation>,
    /// Total animation duration `T`, computed once.
    pub animation_duration: f64,
    /// Spritesheet values (consumed by particles).
    pub spritesheet: Spritesheet,
    /// Forced `TEX0FORMAT` combo (§2.10.5).
    pub tex0_format_combo: Option<u32>,
    /// Pixel format field of the file.
    pub source_format: TexFormat,
    /// Flags of the file.
    pub flags: TexFlags,
}

impl PreparedTexture<'_> {
    /// Texture size of image `index` (§2.11).
    pub fn texture_size(&self, index: usize) -> [u32; 2] {
        self.texture_sizes
            .get(index)
            .copied()
            .unwrap_or(self.default_texture_size)
    }

    /// Useful UV area of an image object (§2.11).
    pub fn uv_extent(&self) -> [f32; 2] {
        uv_extent(self.real_size, self.texture_size(0))
    }

    /// Bound image and uniforms at `time` seconds since start (§2.12). Zeros and
    /// image 0 when the texture is not animated.
    pub fn frame_state(&self, time: f64) -> FrameState {
        match &self.animation {
            Some(animation) => animation.state_at(
                self.animation_duration,
                time,
                self.texture_sizes.len(),
                |i| self.texture_size(i),
            ),
            None => FrameState::default(),
        }
    }

    /// Whether the texture must go through [`decode_compressed`](Self::decode_compressed)
    /// before upload: BCn data on a device without BC support, or a base size that is
    /// not a multiple of the block size (which wgpu refuses).
    pub fn requires_cpu_decode(&self, bc_supported: bool) -> bool {
        match &self.content {
            GpuContent::Images { format, images } if format.is_block_compressed() => {
                !bc_supported
                    || images
                        .iter()
                        .any(|img| img.width() % 4 != 0 || img.height() % 4 != 0)
            }
            _ => false,
        }
    }

    /// Fallback for GPUs without BC support: decodes every BCn level to RGBA8 in
    /// place. No-op for other formats.
    pub fn decode_compressed(&mut self, max_level_bytes: u64) -> Result<(), TexError> {
        let GpuContent::Images { format, images } = &mut self.content else {
            return Ok(());
        };
        if !format.is_block_compressed() {
            return Ok(());
        }
        for (i, image) in images.iter_mut().enumerate() {
            for (k, mip) in image.mips.iter_mut().enumerate() {
                let bytes = GpuFormat::Rgba8Unorm.level_size(mip.width, mip.height);
                let too_large = || TexError::TooLarge {
                    image: i as u32,
                    mip: k as u32,
                    bytes,
                    limit: max_level_bytes,
                };
                if bytes > max_level_bytes {
                    return Err(too_large());
                }
                let rgba = decode_block_compressed(*format, mip.width, mip.height, &mip.data)
                    .ok_or_else(too_large)?;
                mip.data = Cow::Owned(rgba);
            }
        }
        *format = GpuFormat::Rgba8Unorm;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conversion {
    None,
    Rgb8ToRgba8,
    Rgb565ToRgba8,
    Rgb16fToRgba16f,
}

/// GPU format of the raw pixels (§2.3 table, §2.10.1 step 2).
fn gpu_format(
    format: TexFormat,
    embedded: bool,
    extended: bool,
) -> Result<(GpuFormat, Conversion), TexError> {
    use Conversion as C;
    use GpuFormat as G;
    if embedded {
        return Ok((G::Rgba8Unorm, C::None));
    }
    Ok(match format {
        TexFormat::Rgba8888 => (G::Rgba8Unorm, C::None),
        TexFormat::Dxt5 => (G::Bc3RgbaUnorm, C::None),
        TexFormat::Dxt3 => (G::Bc2RgbaUnorm, C::None),
        TexFormat::Dxt1 => (G::Bc1RgbaUnorm, C::None),
        TexFormat::Rg88 => (G::Rg8Unorm, C::None),
        TexFormat::R8 => (G::R8Unorm, C::None),
        TexFormat::Rgb888 if extended => (G::Rgba8Unorm, C::Rgb8ToRgba8),
        TexFormat::Rgb565 if extended => (G::Rgba8Unorm, C::Rgb565ToRgba8),
        TexFormat::Rg1616f if extended => (G::Rg16Float, C::None),
        TexFormat::R16f if extended => (G::R16Float, C::None),
        TexFormat::Bc7 if extended => (G::Bc7RgbaUnorm, C::None),
        TexFormat::Rgba1010102 if extended => (G::Rgb10a2Unorm, C::None),
        TexFormat::Rgba16161616f if extended => (G::Rgba16Float, C::None),
        TexFormat::Rgb161616f if extended => (G::Rgba16Float, C::Rgb16fToRgba16f),
        other => return Err(TexError::UnsupportedGpuFormat(other)),
    })
}

impl<'a> Tex<'a> {
    /// GPU format the raw pixels map to, or `E_TEX_FORMAT_GPU` (§2.10.1 step 2).
    pub fn gpu_format(&self, options: &UploadOptions) -> Result<GpuFormat, TexError> {
        gpu_format(
            self.format,
            self.has_embedded_image(),
            options.extended_formats,
        )
        .map(|(format, _)| format)
    }

    /// Consumes the texture and prepares it for the GPU (§2.10). Borrowed levels stay
    /// borrowed and LZ4 buffers are moved, so nothing is copied unless a conversion
    /// is required (embedded images, extended formats, size fix-ups).
    pub fn into_upload(self, options: &UploadOptions) -> Result<PreparedTexture<'a>, TexError> {
        // §2.10.1: resolution, then the GPU format (even for videos, QO-12), then the
        // video test.
        let resolution = self.resolution();
        let real_size = self.real_size();
        let texture_sizes: Vec<[u32; 2]> = (0..self.image_count())
            .map(|i| self.texture_size(i))
            .collect();
        let embedded = self.has_embedded_image();
        let (format, conversion) = gpu_format(self.format, embedded, options.extended_formats)?;
        let sampler = SamplerDesc::from_flags(self.flags);
        let tex0_format_combo = self.tex0_format_combo();
        let is_video = self.is_video();
        let animation_duration = self
            .animation
            .as_ref()
            .map_or(0.0, Animation::total_duration);

        let Tex {
            images,
            mips,
            animation,
            spritesheet,
            texture_width,
            texture_height,
            flags,
            format: source_format,
            ..
        } = self;

        let content = if is_video {
            let first = images
                .first()
                .filter(|range| range.count > 0)
                .ok_or(TexError::EmptyVideo)?;
            let data = mips
                .into_iter()
                .nth(first.start as usize)
                .map(|mip| mip.data)
                .ok_or(TexError::EmptyVideo)?;
            GpuContent::Video {
                data,
                width: texture_width,
                height: texture_height,
                mip_count: first.count,
            }
        } else {
            let mut levels = mips.into_iter();
            let mut out = Vec::with_capacity(images.len());
            for (i, range) in images.iter().enumerate() {
                let image_levels = levels.by_ref().take(range.count as usize);
                out.push(prepare_image(
                    i as u32,
                    image_levels,
                    format,
                    conversion,
                    embedded,
                    options,
                )?);
            }
            GpuContent::Images {
                format,
                images: out,
            }
        };

        Ok(PreparedTexture {
            content,
            sampler,
            resolution,
            real_size,
            texture_sizes,
            default_texture_size: [texture_width, texture_height],
            animation,
            animation_duration,
            spritesheet,
            tex0_format_combo,
            source_format,
            flags,
        })
    }
}

fn prepare_image<'a>(
    image: u32,
    mips: impl Iterator<Item = Mip<'a>>,
    format: GpuFormat,
    conversion: Conversion,
    embedded: bool,
    options: &UploadOptions,
) -> Result<GpuImage<'a>, TexError> {
    let mut levels: Vec<GpuMip<'a>> = Vec::new();
    let mut chain_broken = false;
    // Every level is consumed so the caller's iterator stays aligned on images.
    for (k, mip) in mips.enumerate() {
        if chain_broken {
            continue;
        }
        let k = k as u32;
        let level = if embedded {
            decode_level(image, k, &mip, options)
        } else {
            raw_level(image, k, mip, format, conversion, options)?
        };
        if let Some(base) = levels.first() {
            let expected = [level_extent(base.width, k), level_extent(base.height, k)];
            if [level.width, level.height] != expected
                || k >= max_mip_levels(base.width, base.height)
            {
                tracing::warn!(
                    image,
                    mip = k,
                    width = level.width,
                    height = level.height,
                    "mip level does not follow the mip chain; it and the following levels are dropped"
                );
                chain_broken = true;
                continue;
            }
        }
        levels.push(level);
    }
    if levels.is_empty() {
        // QO-09: an image without mipmaps becomes a 1×1 transparent texture.
        tracing::warn!(
            image,
            "image has no mipmap; using a 1x1 transparent texture"
        );
        levels.push(transparent_texel(format));
    }
    Ok(GpuImage { mips: levels })
}

fn level_extent(base: u32, level: u32) -> u32 {
    base.checked_shr(level).unwrap_or(0).max(1)
}

fn max_mip_levels(width: u32, height: u32) -> u32 {
    32 - width.max(height).max(1).leading_zeros()
}

fn transparent_texel(format: GpuFormat) -> GpuMip<'static> {
    let data: Vec<u8> = match format {
        // color0 = color1 = 0 selects 3-colour mode; index 3 is transparent black.
        GpuFormat::Bc1RgbaUnorm => vec![0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF],
        // Zero blocks decode to alpha 0 for BC2/BC3, and BC7 mode 0 bits are invalid,
        // which decodes to transparent black; uncompressed zeros are transparent.
        other => vec![0; other.block_bytes() as usize],
    };
    GpuMip {
        width: 1,
        height: 1,
        data: Cow::Owned(data),
    }
}

fn decode_level<'a>(image: u32, mip: u32, level: &Mip<'_>, options: &UploadOptions) -> GpuMip<'a> {
    match decode_embedded_image(&level.data, options.extended_formats) {
        Ok(decoded) if decoded.width > 0 && decoded.height > 0 => GpuMip {
            width: decoded.width,
            height: decoded.height,
            data: Cow::Owned(decoded.rgba),
        },
        result => {
            // Spec 01 §2.6: undefined upstream; Lumen uses transparent black at the
            // header size and logs (QO-07).
            let error = result.err().map(|e| e.to_string()).unwrap_or_default();
            tracing::warn!(image, mip, %error, "embedded image failed to decode; using transparent black");
            let (mut width, mut height) = (level.width.max(1), level.height.max(1));
            if GpuFormat::Rgba8Unorm.level_size(width, height) > options.max_level_bytes {
                (width, height) = (1, 1);
            }
            let len = GpuFormat::Rgba8Unorm.level_size(width, height) as usize;
            GpuMip {
                width,
                height,
                data: Cow::Owned(vec![0; len]),
            }
        }
    }
}

fn raw_level<'a>(
    image: u32,
    mip: u32,
    level: Mip<'a>,
    format: GpuFormat,
    conversion: Conversion,
    options: &UploadOptions,
) -> Result<GpuMip<'a>, TexError> {
    // A zero-sized level cannot exist on the GPU; it is treated as 1 pixel wide/high.
    let (width, height) = (level.width.max(1), level.height.max(1));
    let expected = format.level_size(width, height);
    if expected > options.max_level_bytes {
        return Err(TexError::TooLarge {
            image,
            mip,
            bytes: expected,
            limit: options.max_level_bytes,
        });
    }
    let data = convert(level.data, conversion, width, height);
    let data = fit_length(data, expected as usize, image, mip);
    Ok(GpuMip {
        width,
        height,
        data,
    })
}

/// CPU expansion for formats that have no GPU equivalent (extended formats only).
fn convert(data: Cow<'_, [u8]>, conversion: Conversion, width: u32, height: u32) -> Cow<'_, [u8]> {
    let pixels = width as usize * height as usize;
    match conversion {
        Conversion::None => data,
        Conversion::Rgb8ToRgba8 => {
            let mut out = vec![0u8; pixels * 4];
            for (dst, src) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(data.as_chunks::<3>().0)
            {
                *dst = [src[0], src[1], src[2], 0xFF];
            }
            Cow::Owned(out)
        }
        Conversion::Rgb565ToRgba8 => {
            // Assumed layout: little-endian u16, red in the high 5 bits (README).
            let mut out = vec![0u8; pixels * 4];
            for (dst, &src) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(data.as_chunks::<2>().0)
            {
                let v = u16::from_le_bytes(src);
                let (r, g, b) = ((v >> 11) & 0x1F, (v >> 5) & 0x3F, v & 0x1F);
                *dst = [
                    ((r << 3) | (r >> 2)) as u8,
                    ((g << 2) | (g >> 4)) as u8,
                    ((b << 3) | (b >> 2)) as u8,
                    0xFF,
                ];
            }
            Cow::Owned(out)
        }
        Conversion::Rgb16fToRgba16f => {
            const ONE_HALF: [u8; 2] = 0x3C00u16.to_le_bytes();
            let mut out = vec![0u8; pixels * 8];
            for (dst, src) in out
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .zip(data.as_chunks::<6>().0)
            {
                dst[..6].copy_from_slice(src);
                dst[6..].copy_from_slice(&ONE_HALF);
            }
            Cow::Owned(out)
        }
    }
}

/// Makes the level exactly `expected` bytes: extra bytes are dropped (they are never
/// read by the GPU), missing bytes are zeros (undefined upstream).
fn fit_length(data: Cow<'_, [u8]>, expected: usize, image: u32, mip: u32) -> Cow<'_, [u8]> {
    use std::cmp::Ordering;
    match data.len().cmp(&expected) {
        Ordering::Equal => data,
        Ordering::Greater => {
            tracing::debug!(
                image,
                mip,
                len = data.len(),
                expected,
                "level data longer than its size; truncated"
            );
            match data {
                Cow::Borrowed(bytes) => Cow::Borrowed(&bytes[..expected]),
                Cow::Owned(mut bytes) => {
                    bytes.truncate(expected);
                    Cow::Owned(bytes)
                }
            }
        }
        Ordering::Less => {
            tracing::warn!(
                image,
                mip,
                len = data.len(),
                expected,
                "level data shorter than its size; zero-padded"
            );
            let mut bytes = data.into_owned();
            bytes.resize(expected, 0);
            Cow::Owned(bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_sizes() {
        assert_eq!(GpuFormat::Bc1RgbaUnorm.level_size(1, 1), 8);
        assert_eq!(GpuFormat::Bc3RgbaUnorm.level_size(5, 4), 32);
        assert_eq!(GpuFormat::Rgba8Unorm.level_size(3, 2), 24);
        assert_eq!(GpuFormat::Rg8Unorm.bytes_per_row(3), 6);
    }

    #[test]
    fn mip_chain_helpers() {
        assert_eq!(level_extent(8, 1), 4);
        assert_eq!(level_extent(8, 5), 1);
        assert_eq!(level_extent(8, 40), 1);
        assert_eq!(max_mip_levels(8, 2), 4);
        assert_eq!(max_mip_levels(1, 1), 1);
    }

    #[test]
    fn conversions() {
        let rgb = Cow::Borrowed(&[1u8, 2, 3, 4, 5, 6][..]);
        assert_eq!(
            &*convert(rgb, Conversion::Rgb8ToRgba8, 2, 1),
            &[1, 2, 3, 255, 4, 5, 6, 255]
        );
        let white = Cow::Borrowed(&[0xFF, 0xFF][..]);
        assert_eq!(
            &*convert(white, Conversion::Rgb565ToRgba8, 1, 1),
            &[255, 255, 255, 255]
        );
        let half = Cow::Borrowed(&[1u8, 2, 3, 4, 5, 6][..]);
        assert_eq!(
            &*convert(half, Conversion::Rgb16fToRgba16f, 1, 1),
            &[1, 2, 3, 4, 5, 6, 0x00, 0x3C]
        );
    }
}

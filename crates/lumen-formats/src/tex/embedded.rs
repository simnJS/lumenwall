//! CPU decoders: embedded image files (spec 01 §2.6, amendment A.2) and the BCn
//! fallback for GPUs without block-compression support.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, ImageReader};

use super::GpuFormat;
use super::psd::{PsdError, decode_psd, is_psd};

/// An image decoded to tightly packed RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes, rows top to bottom, no padding.
    pub rgba: Vec<u8>,
}

/// Why an embedded image could not be decoded. The caller zero-fills the level and
/// logs (§2.6); none of these is fatal.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddedImageError {
    /// Error from the `image` decoders (PNG, JPEG, GIF, BMP, TGA, HDR, PNM, WEBP).
    #[error(transparent)]
    Image(#[from] image::ImageError),
    /// PSD outside the A.2 subset, or corrupt.
    #[error(transparent)]
    Psd(#[from] PsdError),
    /// Recognised but not decoded by Lumen.
    #[error("unsupported embedded image format: {0}")]
    Unsupported(&'static str),
}

/// Formats decoded through `image` (A.2: PNG and JPEG required, GIF, BMP and TGA
/// recommended, HDR and PNM optional). PSD has its own decoder; Softimage PIC is
/// not decoded.
const SUPPORTED_FORMATS: [ImageFormat; 7] = [
    ImageFormat::Png,
    ImageFormat::Jpeg,
    ImageFormat::Gif,
    ImageFormat::Bmp,
    ImageFormat::Tga,
    ImageFormat::Hdr,
    ImageFormat::Pnm,
];

/// Softimage PIC: `53 80 F6 34` at 0 and `PICT` at 88 (A.2).
fn is_softimage_pic(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x53, 0x80, 0xF6, 0x34]) && bytes.get(88..92) == Some(b"PICT")
}

/// Picks the decoder from the content, never from the `TIE` field.
fn sniff(bytes: &[u8], allow_extra_formats: bool) -> Result<ImageFormat, EmbeddedImageError> {
    match image::guess_format(bytes) {
        Ok(format) if SUPPORTED_FORMATS.contains(&format) => Ok(format),
        Ok(ImageFormat::WebP) if allow_extra_formats => Ok(ImageFormat::WebP),
        Ok(other) => Err(EmbeddedImageError::Unsupported(other.extensions_str()[0])),
        // TGA has no signature: it is the last candidate of a sniffing decoder.
        Err(_) => Ok(ImageFormat::Tga),
    }
}

/// Decodes an embedded image file to RGBA8 (§2.6, A.2): format sniffed from the
/// content, GIF reduced to its first frame, 16-bit samples reduced to their high
/// byte, HDR clamped to 8 bits, no gamma/ICC/sRGB correction, no EXIF orientation,
/// no flip, no alpha premultiplication. PSD composites follow the A.2 subset.
///
/// `allow_extra_formats` also accepts WEBP, which the reference cannot decode
/// (QO-06, QO-07). Decoding uses `image`'s default memory limits.
pub fn decode_embedded_image(
    bytes: &[u8],
    allow_extra_formats: bool,
) -> Result<DecodedImage, EmbeddedImageError> {
    if is_psd(bytes) {
        return Ok(decode_psd(bytes)?);
    }
    if is_softimage_pic(bytes) {
        return Err(EmbeddedImageError::Unsupported("Softimage PIC"));
    }
    let format = sniff(bytes, allow_extra_formats)?;
    let image = ImageReader::with_format(Cursor::new(bytes), format).decode()?;
    let (width, height) = (image.width(), image.height());
    Ok(DecodedImage {
        width,
        height,
        rgba: to_rgba8(image),
    })
}

/// RGBA8 conversion. 16-bit samples keep their high byte (A.2), where `image`'s own
/// conversion would round; grey is replicated, a missing alpha is 255.
fn to_rgba8(image: DynamicImage) -> Vec<u8> {
    fn high(v: u16) -> u8 {
        (v >> 8) as u8
    }
    match image {
        DynamicImage::ImageLuma16(img) => img
            .as_raw()
            .iter()
            .flat_map(|&l| [high(l), high(l), high(l), 0xFF])
            .collect(),
        DynamicImage::ImageLumaA16(img) => img
            .as_raw()
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[l, a]| [high(l), high(l), high(l), high(a)])
            .collect(),
        DynamicImage::ImageRgb16(img) => img
            .as_raw()
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [high(r), high(g), high(b), 0xFF])
            .collect(),
        DynamicImage::ImageRgba16(img) => img.as_raw().iter().map(|&v| high(v)).collect(),
        other => other.into_rgba8().into_raw(),
    }
}

/// Decodes one level of BC1/BC2/BC3/BC7 blocks to RGBA8 (`width × height × 4`
/// bytes). Missing blocks decode as transparent black. Returns `None` for a format
/// that is not block-compressed or a size that overflows.
pub fn decode_block_compressed(
    format: GpuFormat,
    width: u32,
    height: u32,
    blocks: &[u8],
) -> Option<Vec<u8>> {
    type BlockFn = fn(&[u8], &mut [u8], usize);
    let (block_bytes, decode): (usize, BlockFn) = match format {
        GpuFormat::Bc1RgbaUnorm => (8, bcdec_rs::bc1),
        GpuFormat::Bc2RgbaUnorm => (16, bcdec_rs::bc2),
        GpuFormat::Bc3RgbaUnorm => (16, bcdec_rs::bc3),
        GpuFormat::Bc7RgbaUnorm => (16, bcdec_rs::bc7),
        _ => return None,
    };
    let (w, h) = (width as usize, height as usize);
    let len = w.checked_mul(h)?.checked_mul(4)?;
    let mut out = vec![0u8; len];
    let blocks_x = w.div_ceil(4);
    let blocks_y = h.div_ceil(4);
    // One 4×4 RGBA block, 16 bytes per row.
    let mut tile = [0u8; 64];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let index = by * blocks_x + bx;
            let Some(block) = blocks.get(index * block_bytes..(index + 1) * block_bytes) else {
                continue;
            };
            decode(block, &mut tile, 16);
            let x0 = bx * 4;
            let cols = (w - x0).min(4);
            for row in 0..4 {
                let y = by * 4 + row;
                if y >= h {
                    break;
                }
                let dst = (y * w + x0) * 4;
                out[dst..dst + cols * 4].copy_from_slice(&tile[row * 16..row * 16 + cols * 4]);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_partial_block_and_missing_data() {
        // color0 = white (0xFFFF), color1 = black, every index 0 → white.
        let block = [0xFF, 0xFF, 0x00, 0x00, 0, 0, 0, 0];
        let out = decode_block_compressed(GpuFormat::Bc1RgbaUnorm, 6, 2, &block).unwrap();
        assert_eq!(out.len(), 6 * 2 * 4);
        assert_eq!(&out[..4], &[255, 255, 255, 255]);
        // Second block is missing: transparent black.
        assert_eq!(&out[4 * 4..4 * 5], &[0, 0, 0, 0]);
    }

    #[test]
    fn bc1_transparent_mode() {
        // color0 <= color1 and indices 3 → transparent black.
        let block = [0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF];
        let out = decode_block_compressed(GpuFormat::Bc1RgbaUnorm, 4, 4, &block).unwrap();
        assert!(out.iter().all(|&b| b == 0));
    }

    #[test]
    fn non_bc_format_is_none() {
        assert!(decode_block_compressed(GpuFormat::Rgba8Unorm, 1, 1, &[0; 4]).is_none());
    }
}

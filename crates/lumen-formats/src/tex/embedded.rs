//! CPU decoders: embedded image files (spec 01 §2.6) and the BCn fallback for GPUs
//! without block-compression support.

use std::io::Cursor;

use image::{ImageFormat, ImageReader};

use super::GpuFormat;

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

/// Formats the reference decoder recognises and `image` supports (§2.6). PSD and
/// Softimage PIC are recognised upstream but have no decoder here (see README).
const PARITY_FORMATS: [ImageFormat; 7] = [
    ImageFormat::Png,
    ImageFormat::Jpeg,
    ImageFormat::Gif,
    ImageFormat::Bmp,
    ImageFormat::Tga,
    ImageFormat::Hdr,
    ImageFormat::Pnm,
];

/// Picks the decoder from the content, never from the `TIE` field.
fn sniff(bytes: &[u8], allow_extra_formats: bool) -> Option<ImageFormat> {
    match image::guess_format(bytes) {
        Ok(format) if PARITY_FORMATS.contains(&format) => Some(format),
        Ok(ImageFormat::WebP) if allow_extra_formats => Some(ImageFormat::WebP),
        Ok(_) => None,
        // TGA has no signature: it is the last candidate of a sniffing decoder.
        Err(_) => Some(ImageFormat::Tga),
    }
}

/// Decodes an embedded image file to RGBA8 (§2.6): format sniffed from the content,
/// GIF reduced to its first frame, 16-bit and HDR data brought down to 8 bits, no
/// EXIF orientation, no flip, no alpha premultiplication.
///
/// `allow_extra_formats` also accepts WEBP, which the reference cannot decode
/// (QO-06, QO-07). Decoding uses `image`'s default memory limits.
pub fn decode_embedded_image(
    bytes: &[u8],
    allow_extra_formats: bool,
) -> Result<DecodedImage, image::ImageError> {
    let format = sniff(bytes, allow_extra_formats).ok_or_else(|| {
        image::ImageError::Unsupported(image::error::UnsupportedError::from_format_and_kind(
            image::error::ImageFormatHint::Unknown,
            image::error::UnsupportedErrorKind::Format(image::error::ImageFormatHint::Unknown),
        ))
    })?;
    let image = ImageReader::with_format(Cursor::new(bytes), format).decode()?;
    let rgba = image.into_rgba8();
    Ok(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
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

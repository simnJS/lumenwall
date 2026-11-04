//! Media artwork: decoded once per new image on the source's thread (never on the render
//! thread), reduced to at most [`MAX_SIDE`] pixels, and re-encoded as PNG for web pages.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, ImageReader, Limits};

use super::Artwork;

/// Largest side of the artwork handed to wallpapers (album covers are usually 300 to 640 px).
pub const MAX_SIDE: u32 = 512;

/// Largest artwork file read (a cover is a few hundred kB).
pub(crate) const MAX_SOURCE_BYTES: usize = 32 << 20;

/// Hash identifying artwork bytes.
pub(crate) fn artwork_key(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Decodes an image file (PNG, JPEG, WebP, GIF, BMP, TGA), keeps it within [`MAX_SIDE`]
/// pixels per side and encodes it as PNG. `None` for data that is not an image, too large
/// (more than 8192 px per side or 256 MiB decoded), or empty.
pub fn decode_artwork(bytes: &[u8]) -> Option<Artwork> {
    if bytes.is_empty() || bytes.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(i) => i,
        Err(e) => {
            tracing::debug!("media artwork not decoded: {e}");
            return None;
        }
    };
    let image = if image.width() > MAX_SIDE || image.height() > MAX_SIDE {
        image.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let mut png = Vec::with_capacity(rgba.len() / 2);
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
        .write_image(&rgba, width, height, ExtendedColorType::Rgba8)
        .ok()?;
    Some(Artwork {
        png,
        rgba: rgba.into_raw(),
        width,
        height,
        key: artwork_key(bytes),
    })
}

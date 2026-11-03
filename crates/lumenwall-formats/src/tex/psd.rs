//! Composite-image PSD decoder, limited to the subset of spec 01 amendment A.2:
//! version 1, RGB mode, 8 or 16 bits per channel, raw or PackBits RLE data, 1 to 16
//! channels of which the first four are R, G, B, A.
//!
//! File layout from Adobe's public PSD format documentation: a 26-byte big-endian
//! header, three length-prefixed sections (colour mode data, image resources,
//! layers and masks) that are skipped, then the merged image data.

use super::DecodedImage;

/// Largest decoded image accepted (same order as `image`'s default limit).
const MAX_OUTPUT_BYTES: u64 = 512 << 20;

/// Why a PSD cannot be decoded; every case is a decoding failure (§2.6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PsdError {
    /// Shorter than its header or one of its sections.
    #[error("truncated PSD ({0})")]
    Truncated(&'static str),
    /// Not version 1 (PSB is version 2).
    #[error("unsupported PSD version {0}")]
    Version(u16),
    /// Colour mode other than RGB (3).
    #[error("unsupported PSD colour mode {0} (only RGB)")]
    ColorMode(u16),
    /// Bit depth other than 8 or 16.
    #[error("unsupported PSD depth {0}")]
    Depth(u16),
    /// Channel count outside 1..=16.
    #[error("unsupported PSD channel count {0}")]
    Channels(u16),
    /// Compression other than raw (0) or RLE (1).
    #[error("unsupported PSD compression {0}")]
    Compression(u16),
    /// Empty or oversized image.
    #[error("unsupported PSD size {0}x{1}")]
    Size(u32, u32),
    /// An RLE row does not decode to exactly one row.
    #[error("corrupt PSD RLE data")]
    Rle,
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], PsdError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let end = end.ok_or(PsdError::Truncated(what))?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u16(&mut self, what: &'static str) -> Result<u16, PsdError> {
        let b = self.take(2, what)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, PsdError> {
        let b = self.take(4, what)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn skip_section(&mut self, what: &'static str) -> Result<(), PsdError> {
        let len = self.u32(what)?;
        self.take(len as usize, what).map(|_| ())
    }
}

/// Whether `bytes` starts with the PSD signature.
pub(crate) fn is_psd(bytes: &[u8]) -> bool {
    bytes.starts_with(b"8BPS")
}

/// Decodes the composite image of a PSD to RGBA8.
pub(crate) fn decode_psd(bytes: &[u8]) -> Result<DecodedImage, PsdError> {
    let mut c = Cursor {
        data: bytes,
        pos: 4,
    };
    if !is_psd(bytes) {
        return Err(PsdError::Truncated("signature"));
    }
    let version = c.u16("header")?;
    if version != 1 {
        return Err(PsdError::Version(version));
    }
    c.take(6, "header")?;
    let channels = c.u16("header")?;
    let height = c.u32("header")?;
    let width = c.u32("header")?;
    let depth = c.u16("header")?;
    let mode = c.u16("header")?;
    if mode != 3 {
        return Err(PsdError::ColorMode(mode));
    }
    if depth != 8 && depth != 16 {
        return Err(PsdError::Depth(depth));
    }
    if !(1..=16).contains(&channels) {
        return Err(PsdError::Channels(channels));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels.saturating_mul(4) > MAX_OUTPUT_BYTES {
        return Err(PsdError::Size(width, height));
    }
    c.skip_section("colour mode data")?;
    c.skip_section("image resources")?;
    c.skip_section("layer and mask information")?;
    let compression = c.u16("image data")?;

    let sample = usize::from(depth / 8);
    let row_bytes = width as usize * sample;
    let rows = height as usize;
    // Only R, G, B and A are kept; later channels are read past.
    let used = usize::from(channels).min(4);
    let mut planes: Vec<Vec<u8>> = vec![Vec::new(); used];

    match compression {
        0 => {
            for plane in planes.iter_mut() {
                let data = c.take(row_bytes.saturating_mul(rows), "raw channel data")?;
                *plane = high_bytes(data, sample);
            }
        }
        1 => {
            // One big-endian u16 byte count per row of every channel, then the rows.
            let total_rows = usize::from(channels) * rows;
            let counts = c.take(total_rows.saturating_mul(2), "RLE row counts")?;
            let mut row = vec![0u8; row_bytes];
            for (ch, plane) in planes.iter_mut().enumerate() {
                let mut out = Vec::with_capacity(width as usize * rows);
                for y in 0..rows {
                    let i = (ch * rows + y) * 2;
                    let len = usize::from(u16::from_be_bytes([counts[i], counts[i + 1]]));
                    unpack_bits(c.take(len, "RLE rows")?, &mut row)?;
                    out.extend(high_bytes(&row, sample));
                }
                *plane = out;
            }
        }
        other => return Err(PsdError::Compression(other)),
    }

    let count = pixels as usize;
    let mut rgba = vec![0u8; count * 4];
    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        // A missing colour component is 0, a missing alpha is 255.
        let get = |ch: usize, missing: u8| planes.get(ch).map_or(missing, |p| p[i]);
        *px = [get(0, 0), get(1, 0), get(2, 0), get(3, 255)];
        if used == 4 {
            remove_white_matte(px);
        }
    }
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

/// 8-bit samples as is; 16-bit big-endian samples keep their high byte.
fn high_bytes(data: &[u8], sample: usize) -> Vec<u8> {
    if sample == 1 {
        data.to_vec()
    } else {
        data.chunks_exact(sample).map(|s| s[0]).collect()
    }
}

/// PackBits: a header byte `n` copies `n + 1` literal bytes (0..=127), repeats the
/// next byte `1 − n` times (−127..=−1), or is ignored (−128). The row must be filled
/// exactly.
fn unpack_bits(src: &[u8], row: &mut [u8]) -> Result<(), PsdError> {
    let (mut i, mut o) = (0, 0);
    while o < row.len() {
        let n = *src.get(i).ok_or(PsdError::Rle)? as i8;
        i += 1;
        match n {
            0..=127 => {
                let len = n as usize + 1;
                let lit = src.get(i..i + len).ok_or(PsdError::Rle)?;
                row.get_mut(o..o + len)
                    .ok_or(PsdError::Rle)?
                    .copy_from_slice(lit);
                i += len;
                o += len;
            }
            -127..=-1 => {
                let len = (1 - i16::from(n)) as usize;
                let value = *src.get(i).ok_or(PsdError::Rle)?;
                row.get_mut(o..o + len).ok_or(PsdError::Rle)?.fill(value);
                i += 1;
                o += len;
            }
            -128 => {}
        }
    }
    Ok(())
}

/// A.2: with four or more channels, semi-transparent pixels lose the white matte:
/// `c' = (c − 255·(1 − α)) / α` with `α = a / 255`, truncated toward zero, then
/// clamped to [0, 255].
fn remove_white_matte(px: &mut [u8; 4]) {
    let a = px[3];
    if a == 0 || a == 255 {
        return;
    }
    // parity: single-precision arithmetic and truncation, as in the reference decoder.
    let alpha = f32::from(a) / 255.0;
    for c in &mut px[..3] {
        let v = (f32::from(*c) - 255.0 * (1.0 - alpha)) / alpha;
        *c = (v as i32).clamp(0, 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits() {
        let mut row = [0u8; 6];
        // 2 literals, then 3 × 0x07, then a no-op, then 1 literal.
        unpack_bits(&[0x01, 1, 2, 0xFE, 7, 0x80, 0x00, 9], &mut row).unwrap();
        assert_eq!(row, [1, 2, 7, 7, 7, 9]);
        assert_eq!(unpack_bits(&[0x05, 1], &mut [0u8; 6]), Err(PsdError::Rle));
        assert_eq!(unpack_bits(&[0xF0, 1], &mut [0u8; 4]), Err(PsdError::Rle));
    }

    #[test]
    fn matte() {
        let mut px = [200, 128, 0, 128];
        remove_white_matte(&mut px);
        // α = 128/255: (200 − 127)/α ≈ 145.4 → 145; (128 − 127)/α ≈ 1.99 → 1;
        // (0 − 127)/α < 0 → 0.
        assert_eq!(px, [145, 1, 0, 128]);
        let mut opaque = [10, 20, 30, 255];
        remove_white_matte(&mut opaque);
        assert_eq!(opaque, [10, 20, 30, 255]);
    }
}

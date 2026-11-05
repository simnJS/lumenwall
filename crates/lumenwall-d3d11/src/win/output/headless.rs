//! Headless output: an RGBA8 target, its readback and a PNG encoder (stored deflate blocks: no
//! compression, no dependency; screenshots are rare and off the hot path).

use std::path::Path;

use crate::error::{Error, Result};
use crate::layout::TargetFormat;
use crate::win::{Device, RenderTarget, TargetDesc, read_texture_rgba8};

/// Off-screen output of headless renders: an RGBA8 target and its readback.
pub struct HeadlessOutput {
    target: RenderTarget,
}

impl HeadlessOutput {
    pub fn new(device: &Device, size: (u32, u32)) -> Result<Self> {
        Ok(Self {
            target: RenderTarget::new(
                device,
                TargetDesc {
                    format: TargetFormat::Rgba8Unorm,
                    ..TargetDesc::new(size)
                },
            )?,
        })
    }

    /// The target compositions draw into (`draw_view`).
    #[must_use]
    pub fn target(&self) -> &RenderTarget {
        &self.target
    }

    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.target.size()
    }

    /// RGBA8 rows of the output (waits for the GPU).
    pub fn read_rgba8(&self, device: &Device) -> Result<Vec<u8>> {
        Ok(read_texture_rgba8(device, self.target.texture())?.2)
    }

    /// The output as a PNG file's bytes.
    pub fn png(&self, device: &Device) -> Result<Vec<u8>> {
        let (w, h) = self.size();
        encode_png_rgba8(w, h, &self.read_rgba8(device)?)
    }

    /// Writes the output as a PNG file.
    pub fn write_png(&self, device: &Device, path: &Path) -> Result<()> {
        std::fs::write(path, self.png(device)?)?;
        Ok(())
    }
}

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut table = [0u32; 256];
    for (n, e) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *e = c;
    }
    let mut c = !0u32;
    for chunk in chunks {
        for &b in *chunk {
            c = table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    !c
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// A PNG (8-bit RGBA, no filter, stored deflate blocks) of tightly packed RGBA8 rows.
pub fn encode_png_rgba8(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let row = width as usize * 4;
    if width == 0 || height == 0 || rgba.len() != row * height as usize {
        return Err(Error::InvalidData(format!(
            "PNG of {width}×{height} needs {} bytes, got {}",
            row * height as usize,
            rgba.len()
        )));
    }
    // Scanlines: filter byte 0, then the row.
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for r in rgba.chunks_exact(row) {
        raw.push(0);
        raw.extend_from_slice(r);
    }
    // zlib stream: header, stored blocks of at most 65535 bytes, Adler-32.
    let mut z = vec![0x78, 0x01];
    let blocks = raw.chunks(65535).count();
    for (i, b) in raw.chunks(65535).enumerate() {
        z.push(u8::from(i + 1 == blocks));
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut s) = (1u32, 0u32);
    for &b in &raw {
        a = (a + u32::from(b)) % 65521;
        s = (s + a) % 65521;
    }
    z.extend_from_slice(&((s << 16) | a).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_of_the_check_string() {
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xCBF4_3926);
    }

    #[test]
    fn png_structure() {
        let rgba: Vec<u8> = (0..2 * 3 * 4).map(|i| i as u8).collect();
        let png = encode_png_rgba8(2, 3, &rgba).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..24], [0, 0, 0, 2, 0, 0, 0, 3]);
        assert!(encode_png_rgba8(2, 3, &rgba[1..]).is_err());
    }
}

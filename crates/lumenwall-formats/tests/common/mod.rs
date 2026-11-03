//! Byte-exact fixtures of spec 01 §7.
#![allow(dead_code)]

/// Parses a hex dump: optional `xxxx:` offsets, whitespace-separated byte pairs.
pub fn hex(dump: &str) -> Vec<u8> {
    dump.lines()
        .map(|line| line.split_once(':').map_or(line, |(_, rest)| rest))
        .flat_map(|line| line.split_whitespace())
        .map(|b| u8::from_str_radix(b, 16).expect("hex byte"))
        .collect()
}

pub fn f1_pkg() -> Vec<u8> {
    hex("
0000: 08 00 00 00 50 4B 47 56 30 30 30 31 01 00 00 00
0010: 05 00 00 00 61 2E 74 78 74 00 00 00 00 02 00 00
0020: 00 68 69
")
}

pub fn f2_pkg() -> Vec<u8> {
    hex("
0000: 08 00 00 00 50 4B 47 56 30 30 31 39 02 00 00 00
0010: 0C 00 00 00 70 72 6F 6A 65 63 74 2E 6A 73 6F 6E
0020: 00 00 00 00 07 00 00 00 0F 00 00 00 6D 61 74 65
0030: 72 69 61 6C 73 2F 78 2E 74 65 78 07 00 00 00 03
0040: 00 00 00 7B 22 61 22 3A 31 7D 58 59 5A
")
}

pub fn f3_tex() -> Vec<u8> {
    hex("
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 00 00 00 00 02 00 00 00 02 00
0020: 00 00 02 00 00 00 02 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 33 00 01 00 00 00 FF FF FF FF 01
0040: 00 00 00 02 00 00 00 02 00 00 00 00 00 00 00 10
0050: 00 00 00 10 00 00 00 FF 00 00 FF 00 FF 00 FF 00
0060: 00 FF FF FF FF FF FF
")
}

pub fn f4_tex() -> Vec<u8> {
    hex("
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 02 00 00 00 01 00 00 00 01 00
0020: 00 00 01 00 00 00 01 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 33 00 01 00 00 00 FF FF FF FF 01
0040: 00 00 00 01 00 00 00 01 00 00 00 01 00 00 00 04
0050: 00 00 00 05 00 00 00 40 11 22 33 44
")
}

pub fn f5_tex() -> Vec<u8> {
    hex("
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 09 00 00 00 01 00 00 00 04 00 00 00 01 00
0020: 00 00 03 00 00 00 01 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 31 00 01 00 00 00 01 00 00 00 04
0040: 00 00 00 01 00 00 00 04 00 00 00 00 55 AA FF
")
}

pub const F6_PNG: &str = "
89 50 4E 47 0D 0A 1A 0A 00 00 00 0D 49 48 44 52
00 00 00 01 00 00 00 01 08 06 00 00 00 1F 15 C4
89 00 00 00 0D 49 44 41 54 78 DA 63 10 54 32 FE
0F 00 02 14 01 66 DB BE CC 6F 00 00 00 00 49 45
4E 44 AE 42 60 82
";

pub fn f6_tex() -> Vec<u8> {
    hex("
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 02 00 00 00 01 00 00 00 01 00
0020: 00 00 01 00 00 00 01 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 33 00 01 00 00 00 0D 00 00 00 01
0040: 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00 46
0050: 00 00 00 46 00 00 00 89 50 4E 47 0D 0A 1A 0A 00
0060: 00 00 0D 49 48 44 52 00 00 00 01 00 00 00 01 08
0070: 06 00 00 00 1F 15 C4 89 00 00 00 0D 49 44 41 54
0080: 78 DA 63 10 54 32 FE 0F 00 02 14 01 66 DB BE CC
0090: 6F 00 00 00 00 49 45 4E 44 AE 42 60 82
")
}

pub fn f7_tex() -> Vec<u8> {
    hex("
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 06 00 00 00 01 00 00 00 01 00
0020: 00 00 01 00 00 00 01 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 33 00 02 00 00 00 FF FF FF FF 01
0040: 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00 04
0050: 00 00 00 04 00 00 00 FF 00 00 FF 01 00 00 00 01
0060: 00 00 00 01 00 00 00 00 00 00 00 04 00 00 00 04
0070: 00 00 00 00 00 FF FF 54 45 58 53 30 30 30 33 00
0080: 02 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00
0090: 00 00 00 3F 00 00 00 00 00 00 00 00 00 00 80 3F
00A0: 00 00 00 00 00 00 00 00 00 00 80 3F 01 00 00 00
00B0: 00 00 00 3F 00 00 00 00 00 00 00 00 00 00 80 3F
00C0: 00 00 00 00 00 00 00 00 00 00 80 3F
")
}

pub fn f8_tex() -> Vec<u8> {
    hex("
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 20 00 00 00 40 00 00 00 40 00
0020: 00 00 40 00 00 00 40 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 34 00 01 00 00 00 FF FF FF FF 01
0040: 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00 7B
0050: 7D 00 00 00 00 00 40 00 00 00 40 00 00 00 00 00
0060: 00 00 04 00 00 00 04 00 00 00 4D 50 34 21
")
}

/// F11 (amendment A.5): `VOLUME` texture 2 × 1 × 2 with an embedded PNG.
pub fn f11_tex() -> Vec<u8> {
    hex("
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 42 00 00 00 02 00 00 00 01 00
0020: 00 00 04 00 00 00 01 00 00 00 02 00 00 00 80 80
0030: 80 FF 54 45 58 42 30 30 30 34 00 01 00 00 00 0D
0040: 00 00 00 00 00 00 00 01 00 00 00 02 00 00 00 01
0050: 00 00 00 02 00 00 00 00 00 00 00 00 00 00 00 4B
0060: 00 00 00 89 50 4E 47 0D 0A 1A 0A 00 00 00 0D 49
0070: 48 44 52 00 00 00 02 00 00 00 02 08 02 00 00 00
0080: FD D4 9A 73 00 00 00 12 49 44 41 54 78 DA 63 F8
0090: CF C0 C0 00 C2 0C FF 81 00 00 1F EE 05 FB F1 AB
00A0: BA 77 00 00 00 00 49 45 4E 44 AE 42 60 82
")
}

/// F9 puppet mesh, built from the spec's construction table.
pub fn f9_mdl() -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(b"MDLV0021\0");
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&240u32.to_le_bytes());
    let vertices: [([f32; 3], [f32; 2]); 3] = [
        ([-1.0, 1.0, 0.0], [0.0, 0.0]),
        ([1.0, 1.0, 0.0], [1.0, 0.0]),
        ([-1.0, -1.0, 0.0], [0.0, 1.0]),
    ];
    for (pos, uv) in vertices {
        let mut v = [0u8; 80];
        for (i, c) in pos.iter().enumerate() {
            v[i * 4..i * 4 + 4].copy_from_slice(&c.to_le_bytes());
        }
        for (i, c) in uv.iter().enumerate() {
            v[72 + i * 4..76 + i * 4].copy_from_slice(&c.to_le_bytes());
        }
        d.extend_from_slice(&v);
    }
    d.extend_from_slice(&6u32.to_le_bytes());
    d.extend_from_slice(&[0, 0, 1, 0, 2, 0]);
    d.extend_from_slice(b"MDLS0001\0");
    d
}

/// `.tex` with one embedded image (TEXB0003, format 0, `TIE` = `tie`, dims 1×1 in the
/// headers) whose single mip is `payload`.
pub fn embedded_tex(tie: u32, payload: &[u8]) -> Vec<u8> {
    let mut t = f6_tex()[..0x3B].to_vec();
    t.extend_from_slice(&tie.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes()); // NM
    for v in [1u32, 1, 0] {
        t.extend_from_slice(&v.to_le_bytes()); // width, height, C
    }
    t.extend_from_slice(&(payload.len() as i32).to_le_bytes()); // TD
    t.extend_from_slice(&(payload.len() as i32).to_le_bytes()); // TS
    t.extend_from_slice(payload);
    t
}

/// PNG built by hand from unfiltered scanlines (each prefixed with filter byte 0),
/// so every colour type, bit depth and interlacing can be produced.
pub fn png(
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlaced: bool,
    scanlines: &[u8],
    extra_chunks: &[(&[u8; 4], Vec<u8>)],
) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc.finalize().to_be_bytes());
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, u8::from(interlaced)]);
    chunk(&mut out, b"IHDR", &ihdr);
    for (kind, data) in extra_chunks {
        chunk(&mut out, kind, data);
    }
    chunk(
        &mut out,
        b"IDAT",
        &miniz_oxide::deflate::compress_to_vec_zlib(scanlines, 6),
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Adam7 scanlines (filter 0) of an 8-bit image with `bpp` bytes per pixel.
pub fn adam7_scanlines(width: usize, height: usize, bpp: usize, pixels: &[u8]) -> Vec<u8> {
    const PASSES: [(usize, usize, usize, usize); 7] = [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ];
    let mut out = Vec::new();
    for (x0, y0, dx, dy) in PASSES {
        let xs: Vec<usize> = (x0..width).step_by(dx).collect();
        if xs.is_empty() {
            continue;
        }
        for y in (y0..height).step_by(dy) {
            out.push(0);
            for &x in &xs {
                let i = (y * width + x) * bpp;
                out.extend_from_slice(&pixels[i..i + bpp]);
            }
        }
    }
    out
}

/// PackBits encoding of one row: runs of 3+ equal bytes are repeats, the rest literals.
pub fn packbits(row: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < row.len() {
        let run = row[i..]
            .iter()
            .take(128)
            .take_while(|&&b| b == row[i])
            .count();
        if run >= 3 {
            out.push((257 - run) as u8);
            out.push(row[i]);
            i += run;
        } else {
            let start = i;
            while i < row.len() && i - start < 128 {
                let ahead = row[i..]
                    .iter()
                    .take(3)
                    .take_while(|&&b| b == row[i])
                    .count();
                if ahead >= 3 {
                    break;
                }
                i += 1;
            }
            out.push((i - start - 1) as u8);
            out.extend_from_slice(&row[start..i]);
        }
    }
    out
}

/// PSD with planar `channels` (each `width × height` samples, big-endian when 16-bit)
/// stored raw or with PackBits RLE.
pub fn psd(
    width: u32,
    height: u32,
    depth: u16,
    mode: u16,
    channels: &[Vec<u8>],
    rle: bool,
) -> Vec<u8> {
    let mut out = b"8BPS".to_vec();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    out.extend_from_slice(&(channels.len() as u16).to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&depth.to_be_bytes());
    out.extend_from_slice(&mode.to_be_bytes());
    // Colour mode data, image resources (one dummy byte pair), layers: skipped.
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&[0xAB, 0xCD]);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&u16::from(rle).to_be_bytes());
    let row_bytes = width as usize * usize::from(depth / 8);
    if rle {
        let rows: Vec<Vec<u8>> = channels
            .iter()
            .flat_map(|c| c.chunks(row_bytes).map(packbits))
            .collect();
        for row in &rows {
            out.extend_from_slice(&(row.len() as u16).to_be_bytes());
        }
        for row in &rows {
            out.extend_from_slice(row);
        }
    } else {
        for c in channels {
            out.extend_from_slice(c);
        }
    }
    out
}

/// Overwrites `bytes` at `offset`.
pub fn patch(mut data: Vec<u8>, offset: usize, bytes: &[u8]) -> Vec<u8> {
    data[offset..offset + bytes.len()].copy_from_slice(bytes);
    data
}

/// F-P1 of spec 08 §10: one bone, one attachment, one two-sample animation (782 bytes).
pub fn fp1_mdl() -> Vec<u8> {
    hex("
0000: 4D 44 4C 56 30 30 32 33 00 09 00 80 01 01 00 00
0010: 00 01 00 00 00 6D 2E 6A 73 6F 6E 00 00 00 00 00
0020: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0030: 00 00 00 00 00 00 00 00 0F 00 80 01 F0 00 00 00
0040: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0050: 00 00 00 00 00 00 80 3F 00 00 80 3F 00 00 00 00
0060: 00 00 00 00 00 00 80 3F 00 00 00 00 00 00 00 00
0070: 00 00 00 00 00 00 00 00 00 00 80 3F 00 00 00 00
0080: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 80 3F
0090: 00 00 A0 41 00 00 00 00 00 00 00 00 00 00 00 00
00A0: 00 00 00 00 00 00 80 3F 00 00 80 3F 00 00 00 00
00B0: 00 00 00 00 00 00 80 3F 00 00 00 00 00 00 00 00
00C0: 00 00 00 00 00 00 00 00 00 00 80 3F 00 00 00 00
00D0: 00 00 00 00 00 00 00 00 00 00 80 3F 00 00 80 3F
00E0: 00 00 00 00 00 00 A0 41 00 00 00 00 00 00 00 00
00F0: 00 00 00 00 00 00 80 3F 00 00 80 3F 00 00 00 00
0100: 00 00 00 00 00 00 80 3F 00 00 00 00 00 00 00 00
0110: 00 00 00 00 00 00 00 00 00 00 80 3F 00 00 00 00
0120: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0130: 06 00 00 00 00 00 01 00 02 00 00 01 10 00 00 00
0140: 00 00 00 00 00 00 00 00 00 00 00 00 03 00 00 00
0150: 00 00 00 00 4D 44 4C 53 30 30 30 34 00 15 02 00
0160: 00 01 00 00 00 00 01 00 00 00 FF FF FF FF 40 00
0170: 00 00 00 00 80 3F 00 00 00 00 00 00 00 00 00 00
0180: 00 00 00 00 00 00 00 00 80 3F 00 00 00 00 00 00
0190: 00 00 00 00 00 00 00 00 00 00 00 00 80 3F 00 00
01A0: 00 00 00 00 20 41 00 00 00 00 00 00 00 00 00 00
01B0: 80 3F 00 00 00 00 00 00 00 00 00 00 00 00 01 00
01C0: 00 20 41 00 00 20 41 00 00 20 41 00 00 80 3F 00
01D0: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
01E0: 00 80 3F 00 00 00 00 00 00 00 00 00 00 00 00 00
01F0: 00 00 00 00 00 80 3F 00 00 00 00 00 00 00 00 00
0200: 00 00 00 00 00 20 41 00 00 80 3F 01 00 00 00 00
0210: 01 00 00 00 00 4D 44 41 54 30 30 30 31 00 6A 02
0220: 00 00 01 00 00 00 61 74 74 00 00 00 80 3F 00 00
0230: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0240: 80 3F 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0250: 00 00 00 00 80 3F 00 00 00 00 00 00 00 00 00 00
0260: A0 40 00 00 00 00 00 00 80 3F 4D 44 4C 41 30 30
0270: 30 36 00 0D 03 00 00 01 00 00 00 07 00 00 00 00
0280: 00 00 00 41 00 6C 6F 6F 70 00 00 00 80 3F 01 00
0290: 00 00 00 00 00 00 01 00 00 00 00 00 00 00 48 00
02A0: 00 00 00 00 20 41 00 00 00 00 00 00 00 00 00 00
02B0: 00 00 00 00 00 00 00 00 00 00 00 00 80 3F 00 00
02C0: 80 3F 00 00 80 3F 00 00 20 41 00 00 00 00 00 00
02D0: 00 00 00 00 00 00 00 00 00 00 DB 0F C9 3F 00 00
02E0: 80 3F 00 00 80 3F 00 00 80 3F 00 00 00 00 00 00
02F0: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0300: 00 00 00 00 00 00 00 00 00 00 00 00 00 00
")
}

/// F-P1 with another animation mode string at 0x285; everything after it moves and the
/// end of `MDLA` (u32 at 0x273) follows (spec 08 §10, V5/V6).
pub fn fp1_with_mode(mode: &str) -> Vec<u8> {
    let base = fp1_mdl();
    let mut d = base[..0x285].to_vec();
    d.extend_from_slice(mode.as_bytes());
    d.push(0);
    d.extend_from_slice(&base[0x28A..]);
    let shift = mode.len() as i64 - 4;
    let end = (0x30D_i64 + shift) as u32;
    d[0x273..0x277].copy_from_slice(&end.to_le_bytes());
    d
}

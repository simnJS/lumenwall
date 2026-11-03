//! Embedded image decoding (spec 01 §2.6, amendment A.2): PNG and JPEG variants the
//! spec requires, the PSD subset, and the zero-fill path for unsupported data.

mod common;

use common::*;
use lumen_formats::tex::{
    self, EmbeddedImageError, GpuContent, UploadOptions, decode_embedded_image,
};

fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let img = decode_embedded_image(bytes, false).expect("decodes");
    (img.width, img.height, img.rgba)
}

// ---------------------------------------------------------------- PNG (DOIT)

#[test]
fn png_low_bit_depth_greys_are_scaled() {
    // 1-bit: 1 → 255.
    let one = png(2, 1, 1, 0, false, &[0, 0b1000_0000], &[]);
    assert_eq!(decode(&one), (2, 1, vec![255, 255, 255, 255, 0, 0, 0, 255]));
    // 2-bit: 0, 1, 2, 3 → 0, 85, 170, 255.
    let two = png(4, 1, 2, 0, false, &[0, 0b00_01_10_11], &[]);
    let (_, _, rgba) = decode(&two);
    let greys: Vec<u8> = rgba.chunks(4).map(|p| p[0]).collect();
    assert_eq!(greys, [0, 85, 170, 255]);
    // 4-bit: 15 → 255.
    let four = png(2, 1, 4, 0, false, &[0, 0x0F], &[]);
    assert_eq!(decode(&four).2, [0, 0, 0, 255, 255, 255, 255, 255]);
}

#[test]
fn png_palette_with_trns() {
    let plte = vec![255, 0, 0, 0, 255, 0, 0, 0, 255];
    // tRNS shorter than the palette: the last entry stays opaque.
    let trns = vec![255, 128];
    let bytes = png(
        3,
        1,
        2,
        3,
        false,
        &[0, 0b00_01_10_00],
        &[(b"PLTE", plte), (b"tRNS", trns)],
    );
    assert_eq!(
        decode(&bytes).2,
        [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255]
    );
}

#[test]
fn png_colour_key_trns() {
    // RGB 8-bit, key (0, 0, 255): that pixel becomes transparent.
    let key = vec![0, 0, 0, 0, 0, 255];
    let bytes = png(
        2,
        1,
        8,
        2,
        false,
        &[0, 0, 0, 255, 1, 2, 3],
        &[(b"tRNS", key)],
    );
    assert_eq!(decode(&bytes).2, [0, 0, 255, 0, 1, 2, 3, 255]);
    // Grey 8-bit, key 128.
    let bytes = png(2, 1, 8, 0, false, &[0, 128, 7], &[(b"tRNS", vec![0, 128])]);
    assert_eq!(decode(&bytes).2, [128, 128, 128, 0, 7, 7, 7, 255]);
}

#[test]
fn png_16_bit_keeps_the_high_byte() {
    // RGBA 16: 0x12FF, 0x3480, 0x5601, 0xFFFF → 0x12, 0x34, 0x56, 0xFF (no rounding).
    let line = [0, 0x12, 0xFF, 0x34, 0x80, 0x56, 0x01, 0xFF, 0xFF];
    assert_eq!(
        decode(&png(1, 1, 16, 6, false, &line, &[])).2,
        [0x12, 0x34, 0x56, 0xFF]
    );
    // Grey 16 and grey + alpha 16.
    assert_eq!(
        decode(&png(1, 1, 16, 0, false, &[0, 0xAB, 0xCD], &[])).2,
        [0xAB, 0xAB, 0xAB, 0xFF]
    );
    assert_eq!(
        decode(&png(1, 1, 16, 4, false, &[0, 0x10, 0xF0, 0x80, 0x80], &[])).2,
        [0x10, 0x10, 0x10, 0x80]
    );
    // RGB 16.
    let line = [0, 0x01, 0xFF, 0x02, 0xFF, 0x03, 0xFF];
    assert_eq!(
        decode(&png(1, 1, 16, 2, false, &line, &[])).2,
        [1, 2, 3, 255]
    );
}

#[test]
fn png_grey_alpha_and_rgba_8() {
    assert_eq!(
        decode(&png(1, 1, 8, 4, false, &[0, 77, 200], &[])).2,
        [77, 77, 77, 200]
    );
    assert_eq!(
        decode(&png(1, 1, 8, 6, false, &[0, 1, 2, 3, 4], &[])).2,
        [1, 2, 3, 4]
    );
}

#[test]
fn png_adam7_interlacing() {
    let pixels: Vec<u8> = (0u8..27).map(|v| v * 9).collect();
    let lines = adam7_scanlines(3, 3, 3, &pixels);
    let (w, h, rgba) = decode(&png(3, 3, 8, 2, true, &lines, &[]));
    assert_eq!((w, h), (3, 3));
    let expected: Vec<u8> = pixels
        .chunks(3)
        .flat_map(|p| [p[0], p[1], p[2], 255])
        .collect();
    assert_eq!(rgba, expected);
}

#[test]
fn png_gamma_and_srgb_chunks_are_ignored() {
    // gAMA = 1.0 / 2.2 and sRGB present: values pass through unchanged.
    let gama = 45_455u32.to_be_bytes().to_vec();
    let bytes = png(
        1,
        1,
        8,
        2,
        false,
        &[0, 10, 128, 250],
        &[(b"gAMA", gama), (b"sRGB", vec![0])],
    );
    assert_eq!(decode(&bytes).2, [10, 128, 250, 255]);
}

// ---------------------------------------------------------------- JPEG (DOIT)

#[test]
fn jpeg_baseline_colour_and_grey() {
    use image::ImageEncoder;
    use image::codecs::jpeg::JpegEncoder;
    let mut colour = Vec::new();
    let rgb: Vec<u8> = [200u8, 100, 50].repeat(64);
    JpegEncoder::new_with_quality(&mut colour, 95)
        .write_image(&rgb, 8, 8, image::ExtendedColorType::Rgb8)
        .unwrap();
    let (w, h, rgba) = decode(&colour);
    assert_eq!((w, h), (8, 8));
    // A.2: ±2 per channel tolerance between JPEG decoders.
    for px in rgba.chunks(4) {
        for (got, want) in px.iter().zip([200u8, 100, 50, 255]) {
            assert!(got.abs_diff(want) <= 2, "{px:?}");
        }
    }
    let mut grey = Vec::new();
    JpegEncoder::new_with_quality(&mut grey, 95)
        .write_image(&[90u8; 64], 8, 8, image::ExtendedColorType::L8)
        .unwrap();
    for px in decode(&grey).2.chunks(4) {
        assert!(px[0].abs_diff(90) <= 2 && px[0] == px[1] && px[1] == px[2] && px[3] == 255);
    }
}

// ---------------------------------------------------------------- PSD (PEUT, subset)

#[test]
fn psd_rgb_8_bit_raw() {
    let bytes = psd(
        2,
        1,
        8,
        3,
        &[vec![10, 20], vec![30, 40], vec![50, 60]],
        false,
    );
    assert_eq!(
        decode(&bytes),
        (2, 1, vec![10, 30, 50, 255, 20, 40, 60, 255])
    );
}

#[test]
fn psd_rgba_rle_removes_the_white_matte() {
    // Pixels: opaque, half transparent, fully transparent, then a run for RLE repeats.
    let r = vec![10, 200, 99, 7, 7, 7, 7];
    let g = vec![20, 128, 99, 7, 7, 7, 7];
    let b = vec![30, 0, 99, 7, 7, 7, 7];
    let a = vec![255, 128, 0, 255, 255, 255, 255];
    let bytes = psd(7, 1, 8, 3, &[r, g, b, a], true);
    let (_, _, rgba) = decode(&bytes);
    assert_eq!(
        &rgba[..12],
        &[10, 20, 30, 255, 145, 1, 0, 128, 99, 99, 99, 0]
    );
    assert_eq!(&rgba[12..], &[7, 7, 7, 255].repeat(4)[..]);
}

#[test]
fn psd_16_bit_keeps_the_high_byte() {
    let channels = [vec![0x12, 0xFF], vec![0x34, 0x00], vec![0x56, 0xAB]];
    for rle in [false, true] {
        let bytes = psd(1, 1, 16, 3, &channels, rle);
        assert_eq!(decode(&bytes).2, [0x12, 0x34, 0x56, 0xFF], "rle {rle}");
    }
}

#[test]
fn psd_channel_counts() {
    // Two channels in RGB mode: blue is 0, alpha 255.
    assert_eq!(
        decode(&psd(1, 1, 8, 3, &[vec![1], vec![2]], false)).2,
        [1, 2, 0, 255]
    );
    // Five channels: only the first four are used.
    let five = [vec![1], vec![2], vec![3], vec![255], vec![9]];
    assert_eq!(decode(&psd(1, 1, 8, 3, &five, true)).2, [1, 2, 3, 255]);
}

#[test]
fn psd_outside_the_subset_fails() {
    let one = [vec![1]];
    for (bytes, what) in [
        (psd(1, 1, 8, 1, &one, false), "grey mode"),
        (psd(1, 1, 8, 4, &one, false), "CMYK mode"),
        (psd(1, 1, 32, 3, &one, false), "32-bit"),
        (patch(psd(1, 1, 8, 3, &one, false), 4, &[0, 2]), "PSB"),
        (psd(1, 1, 8, 3, &[], false), "no channel"),
    ] {
        let err = decode_embedded_image(&bytes, false).unwrap_err();
        assert!(matches!(err, EmbeddedImageError::Psd(_)), "{what}: {err}");
    }
    // ZIP compression (2).
    let mut zip = psd(1, 1, 8, 3, &one, false);
    let n = zip.len() - 3;
    zip[n..n + 2].copy_from_slice(&2u16.to_be_bytes());
    assert!(decode_embedded_image(&zip, false).is_err());
    // Truncated data.
    let full = psd(4, 4, 8, 3, &[vec![1; 16], vec![2; 16], vec![3; 16]], false);
    assert!(decode_embedded_image(&full[..full.len() - 1], false).is_err());
}

#[test]
fn psd_inside_a_tex() {
    let payload = psd(1, 1, 8, 3, &[vec![11], vec![22], vec![33]], false);
    // TIE = 20 (PSD); the content is sniffed anyway.
    let bytes = embedded_tex(20, &payload);
    let up = tex::parse(&bytes)
        .unwrap()
        .into_upload(&UploadOptions::default())
        .unwrap();
    let GpuContent::Images { images, .. } = &up.content else {
        panic!("expected images")
    };
    assert_eq!(&*images[0].mips[0].data, &[11, 22, 33, 255]);
}

// ---------------------------------------------------------------- unsupported

#[test]
fn softimage_pic_and_tiff_are_unsupported_and_zero_filled() {
    let mut pic = vec![0x53, 0x80, 0xF6, 0x34];
    pic.resize(88, 0);
    pic.extend_from_slice(b"PICT");
    pic.resize(120, 0);
    assert!(matches!(
        decode_embedded_image(&pic, false),
        Err(EmbeddedImageError::Unsupported("Softimage PIC"))
    ));
    let tiff = b"II*\0\x08\0\0\0\0\0".to_vec();
    assert!(matches!(
        decode_embedded_image(&tiff, false),
        Err(EmbeddedImageError::Unsupported(_))
    ));
    // Through a .tex: transparent black at the header size, no error.
    let bytes = embedded_tex(33, &pic);
    let up = tex::parse(&bytes)
        .unwrap()
        .into_upload(&UploadOptions::default())
        .unwrap();
    let GpuContent::Images { images, .. } = &up.content else {
        panic!("expected images")
    };
    assert_eq!(&*images[0].mips[0].data, &[0, 0, 0, 0]);
}

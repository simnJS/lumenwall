//! Spec 01 §7 fixtures F1 to F11, positive and negative.

mod common;

use std::borrow::Cow;

use common::*;
use lumen_formats::mdl::{self, MdlError, MdlVersion};
use lumen_formats::pkg::PkgArchive;
use lumen_formats::tex::{
    self, AddressMode, EmbeddedFormat, FilterMode, FrameState, GpuContent, GpuFormat, Spritesheet,
    TexError, TexFlags, TexFormat, TextureDimension, UploadOptions,
};

fn tex_err(data: &[u8]) -> TexError {
    tex::parse(data).expect_err("parse should fail")
}

/// Contents of an existing, in-bounds archive entry.
fn read<'a>(pkg: &'a PkgArchive, name: &str) -> &'a [u8] {
    pkg.read(name)
        .expect("entry exists")
        .expect("entry in bounds")
}

/// Per image, per level: (width, height, bytes).
type Levels = Vec<Vec<(u32, u32, Vec<u8>)>>;

fn images(content: &GpuContent<'_>) -> (GpuFormat, Levels) {
    match content {
        GpuContent::Images { format, images, .. } => (
            *format,
            images
                .iter()
                .map(|img| {
                    img.mips
                        .iter()
                        .map(|m| (m.width, m.height, m.data.to_vec()))
                        .collect()
                })
                .collect(),
        ),
        GpuContent::Video { .. } => panic!("expected images"),
    }
}

// ---------------------------------------------------------------- F1, F2: .pkg

#[test]
fn f1_minimal_pkg() {
    let bytes = f1_pkg();
    assert_eq!(bytes.len(), 35);
    let pkg = PkgArchive::from_vec(bytes).unwrap();
    assert_eq!(pkg.base(), 33);
    assert_eq!(pkg.header(), b"PKGV0001");
    assert_eq!(read(&pkg, "a.txt"), b"hi");
    assert!(!pkg.contains("A.txt"), "lookup is case-sensitive");
    assert!(!pkg.contains("b"));
    let names: Vec<_> = pkg.entries().map(|e| e.name).collect();
    assert_eq!(names, [b"a.txt"]);
}

#[test]
fn f2_two_entries_with_offset() {
    let bytes = f2_pkg();
    assert_eq!(bytes.len(), 77);
    let pkg = PkgArchive::from_vec(bytes).unwrap();
    assert_eq!(pkg.base(), 67);
    let project = pkg.get("project.json").unwrap().data().unwrap();
    assert_eq!(project, b"{\"a\":1}");
    assert_eq!(
        project.as_ptr() as usize - pkg.as_bytes().as_ptr() as usize,
        67
    );
    let tex = pkg.get("materials/x.tex").unwrap();
    assert_eq!(tex.data().unwrap(), b"XYZ");
    assert_eq!((tex.offset, tex.size), (7, 3));
    assert_eq!(
        tex.data().unwrap().as_ptr() as usize - pkg.as_bytes().as_ptr() as usize,
        74
    );
    // The archive itself does no normalization (that is the VFS's job, §1.5).
    assert!(pkg.get("./materials//x.tex").is_none());
    assert!(pkg.get("/materials/x.tex").is_none());
}

#[test]
fn f2_bad_magic_is_e_pkg_entete() {
    let err = PkgArchive::from_vec(patch(f2_pkg(), 0x04, &[0x51])).unwrap_err();
    assert_eq!(err.code(), "E_PKG_ENTETE");
    assert!(err.to_string().contains("QKGV0019"), "{err}");
}

#[test]
fn f2_bare_pkgv_header_is_accepted() {
    let mut bytes = f2_pkg();
    bytes.splice(0..12, hex("04 00 00 00 50 4B 47 56"));
    let pkg = PkgArchive::from_vec(bytes).unwrap();
    assert_eq!(pkg.header(), b"PKGV");
    assert_eq!(pkg.base(), 63);
    assert_eq!(read(&pkg, "project.json"), b"{\"a\":1}");
    assert_eq!(read(&pkg, "materials/x.tex"), b"XYZ");
}

#[test]
fn pkg_short_headers_are_e_pkg_entete() {
    for header in [&b""[..], b"PKG", b"PKGX0001"] {
        let mut bytes = (header.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(header);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let err = PkgArchive::from_vec(bytes).unwrap_err();
        assert_eq!(err.code(), "E_PKG_ENTETE", "{header:?}");
    }
}

#[test]
fn pkg_truncations_and_out_of_bounds() {
    let bytes = f1_pkg();
    // Header length past the end of the file.
    let err = PkgArchive::from_vec(patch(bytes.clone(), 0, &[0xFF, 0xFF, 0, 0])).unwrap_err();
    assert_eq!(err.code(), "FinDeFluxInattendue");
    // Every truncation inside the table is an end-of-data error.
    for len in 0..33 {
        let err = PkgArchive::from_vec(bytes[..len].to_vec()).unwrap_err();
        assert_eq!(err.code(), "FinDeFluxInattendue", "len {len}");
    }
    // A.4: a truncated data area still opens; the entry exists but fails on read.
    for truncated in [bytes[..34].to_vec(), bytes[..33].to_vec()] {
        let pkg = PkgArchive::from_vec(truncated).unwrap();
        assert!(pkg.contains("a.txt"));
        let err = pkg.read("a.txt").unwrap().unwrap_err();
        assert_eq!(err.code(), "E_PKG_ENTREE_HORS_LIMITES");
        assert!(!pkg.get("a.txt").unwrap().is_in_bounds());
    }
    // So does a huge offset (64-bit arithmetic, no overflow).
    let pkg = PkgArchive::from_vec(patch(bytes, 0x19, &[0xFF, 0xFF, 0xFF, 0xFF])).unwrap();
    let err = pkg.read("a.txt").unwrap().unwrap_err();
    assert_eq!(err.code(), "E_PKG_ENTREE_HORS_LIMITES");
    let message = err.to_string();
    for part in ["a.txt", "offset 4294967295", "size 2", "35 bytes"] {
        assert!(message.contains(part), "{message}");
    }
}

#[test]
fn a4_out_of_bounds_entry_does_not_poison_the_archive() {
    // F2 with entry 1 (project.json) pointing past the end: entry 2 stays readable,
    // before and after the failed read (QO-37 is not reproduced).
    let pkg = PkgArchive::from_vec(patch(f2_pkg(), 0x20, &[0x50, 0, 0, 0])).unwrap();
    assert_eq!(read(&pkg, "materials/x.tex"), b"XYZ");
    let err = pkg.read("project.json").unwrap().unwrap_err();
    assert_eq!(err.code(), "E_PKG_ENTREE_HORS_LIMITES");
    assert_eq!(read(&pkg, "materials/x.tex"), b"XYZ");
    let in_bounds: Vec<bool> = pkg.entries().map(|e| e.is_in_bounds()).collect();
    assert_eq!(in_bounds, [false, true]);
}

#[test]
fn pkg_open_from_disk_mapped_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scene.pkg");
    std::fs::write(&path, f2_pkg()).unwrap();
    let mut mapped = PkgArchive::open(&path).unwrap();
    assert!(mapped.is_mapped());
    assert_eq!(read(&mapped, "materials/x.tex"), b"XYZ");
    mapped.release_mapping();
    assert!(!mapped.is_mapped());
    assert_eq!(read(&mapped, "project.json"), b"{\"a\":1}");
    let read = PkgArchive::open_with(&path, lumen_formats::pkg::OpenMode::Read).unwrap();
    assert_eq!(read.len(), 2);
    std::fs::write(&path, b"").unwrap();
    assert_eq!(
        PkgArchive::open(&path).unwrap_err().code(),
        "FinDeFluxInattendue"
    );
    assert_eq!(
        PkgArchive::open(dir.path().join("missing.pkg"))
            .unwrap_err()
            .code(),
        "E_IO"
    );
}

// ---------------------------------------------------------------- F3: raw RGBA

#[test]
fn f3_rgba_2x2_uncompressed() {
    let bytes = f3_tex();
    assert_eq!(bytes.len(), 103);
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(t.format, TexFormat::Rgba8888);
    assert_eq!(t.container_version, 3);
    assert_eq!(t.effective_version, 3);
    assert_eq!(t.embedded_format, EmbeddedFormat::Unknown);
    assert_eq!(t.image_count(), 1);
    let mips = t.image(0).unwrap();
    assert_eq!(mips.len(), 1);
    assert_eq!(mips[0].data.len(), 16);
    // Zero-copy: the level borrows the input.
    assert!(matches!(mips[0].data, Cow::Borrowed(_)));
    assert_eq!(t.resolution(), [2, 2, 2, 2]);
    assert_eq!(t.uv_extent(), [1.0, 1.0]);
    assert!(!t.is_animated());
    assert!(!t.spritesheet.is_present());
    assert_eq!(t.tex0_format_combo(), None);

    let up = t.into_upload(&UploadOptions::default()).unwrap();
    assert_eq!(up.sampler.address_mode, AddressMode::Repeat);
    assert_eq!(up.sampler.min_filter, FilterMode::Linear);
    assert_eq!(up.sampler.mag_filter, FilterMode::Linear);
    assert_eq!(up.sampler.mipmap_filter, FilterMode::Linear);
    assert_eq!(up.sampler.anisotropy_clamp, 8);
    assert_eq!(up.resolution, [2, 2, 2, 2]);
    assert_eq!(up.uv_extent(), [1.0, 1.0]);
    assert_eq!(up.frame_state(0.3), FrameState::default());
    let GpuContent::Images {
        format,
        dimension,
        images,
    } = &up.content
    else {
        panic!("expected images")
    };
    assert_eq!(*format, GpuFormat::Rgba8Unorm);
    assert_eq!(*dimension, TextureDimension::D2);
    assert_eq!(images[0].depth(), 1);
    assert!(
        matches!(images[0].mips[0].data, Cow::Borrowed(_)),
        "no copy on upload"
    );
    assert_eq!(
        &*images[0].mips[0].data,
        &hex("FF 00 00 FF 00 FF 00 FF 00 00 FF FF FF FF FF FF")[..]
    );
}

// ---------------------------------------------------------------- F4: LZ4

#[test]
fn f4_lz4_1x1() {
    let bytes = f4_tex();
    assert_eq!(bytes.len(), 92);
    let t = tex::parse(&bytes).unwrap();
    let mip = &t.image(0).unwrap()[0];
    assert_eq!(mip.compression, 1);
    assert!(matches!(mip.data, Cow::Owned(_)));
    assert_eq!(&*mip.data, &[0x11, 0x22, 0x33, 0x44]);
    let up = t.into_upload(&UploadOptions::default()).unwrap();
    assert_eq!(up.sampler.address_mode, AddressMode::ClampToEdge);
    let (format, imgs) = images(&up.content);
    assert_eq!(format, GpuFormat::Rgba8Unorm);
    assert_eq!(imgs[0][0], (1, 1, vec![0x11, 0x22, 0x33, 0x44]));
}

#[test]
fn f4_truncated_lz4_literal_is_e_tex_lz4() {
    let err = tex_err(&patch(f4_tex(), 0x57, &[0x50]));
    assert_eq!(err.code(), "E_TEX_LZ4");
}

#[test]
fn f4_unknown_compression_reads_td_bytes() {
    let t_bytes = patch(f4_tex(), 0x4B, &[0x02]);
    let t = tex::parse(&t_bytes).unwrap();
    let mip = &t.image(0).unwrap()[0];
    assert_eq!(mip.compression, 2);
    assert_eq!(&*mip.data, &[0x40, 0x11, 0x22, 0x33]);
}

#[test]
fn f4_lz4_output_shorter_than_td_is_zero_filled() {
    // TD = 6 while the block only produces 4 bytes.
    let t_bytes = patch(f4_tex(), 0x4F, &[0x06]);
    let t = tex::parse(&t_bytes).unwrap();
    assert_eq!(
        &*t.image(0).unwrap()[0].data,
        &[0x11, 0x22, 0x33, 0x44, 0, 0]
    );
}

#[test]
fn lz4_recipe_for_16_literals() {
    // "F0 01" + 16 bytes (spec recipe); the level is RGBA 2×2.
    let mut bytes = f4_tex()[..0x43].to_vec();
    bytes.extend_from_slice(&2u32.to_le_bytes()); // width
    bytes.extend_from_slice(&2u32.to_le_bytes()); // height
    bytes.extend_from_slice(&1u32.to_le_bytes()); // LZ4
    bytes.extend_from_slice(&16i32.to_le_bytes()); // TD
    bytes.extend_from_slice(&18i32.to_le_bytes()); // TS
    bytes.extend_from_slice(&[0xF0, 0x01]);
    bytes.extend(0u8..16);
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(
        &*t.image(0).unwrap()[0].data,
        &(0u8..16).collect::<Vec<_>>()[..]
    );
}

// ---------------------------------------------------------------- F5: R8, v1

#[test]
fn f5_r8_texb0001() {
    let bytes = f5_tex();
    assert_eq!(bytes.len(), 79);
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(t.container_version, 1);
    assert_eq!(t.format, TexFormat::R8);
    assert_eq!(t.resolution(), [4, 1, 3, 1]);
    assert_eq!(t.uv_extent(), [0.75, 1.0]);
    assert_eq!(t.tex0_format_combo(), Some(9));
    let up = t.into_upload(&UploadOptions::default()).unwrap();
    assert_eq!(up.sampler.mag_filter, FilterMode::Nearest);
    assert_eq!(up.sampler.min_filter, FilterMode::Nearest);
    assert_eq!(up.sampler.mipmap_filter, FilterMode::Nearest);
    assert_eq!(up.tex0_format_combo, Some(9));
    let (format, imgs) = images(&up.content);
    assert_eq!(format, GpuFormat::R8Unorm);
    assert_eq!(imgs[0][0], (4, 1, vec![0, 85, 170, 255]));
}

#[test]
fn rg88_forces_combo_8() {
    let t_bytes = patch(f5_tex(), 0x12, &[0x08]);
    let t = tex::parse(&t_bytes).unwrap();
    assert_eq!(t.tex0_format_combo(), Some(8));
    // 4 bytes for a 4×1 RG88 level: zero-padded to 8 with a warning.
    let up = t.into_upload(&UploadOptions::default()).unwrap();
    let (format, imgs) = images(&up.content);
    assert_eq!(format, GpuFormat::Rg8Unorm);
    assert_eq!(imgs[0][0].2, vec![0, 0x55, 0xAA, 0xFF, 0, 0, 0, 0]);
}

// ---------------------------------------------------------------- F6: embedded PNG

#[test]
fn f6_embedded_png() {
    let bytes = f6_tex();
    assert_eq!(bytes.len(), 157);
    assert_eq!(&bytes[0x57..], &hex(F6_PNG)[..]);
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(t.embedded_format, EmbeddedFormat::Png);
    assert_eq!(t.resolution(), [1, 1, 1, 1]);
    let up = t.into_upload(&UploadOptions::default()).unwrap();
    assert_eq!(up.resolution, [1, 1, 1, 1]);
    let (format, imgs) = images(&up.content);
    assert_eq!(format, GpuFormat::Rgba8Unorm);
    assert_eq!(imgs[0][0], (1, 1, vec![0x11, 0x22, 0x33, 0xFF]));
}

#[test]
fn f6_type_is_ignored_content_is_sniffed() {
    let t_bytes = patch(f6_tex(), 0x3B, &[0x02]);
    let t = tex::parse(&t_bytes).unwrap();
    assert_eq!(t.embedded_format, EmbeddedFormat::Jpeg);
    let (_, imgs) = images(&t.into_upload(&UploadOptions::default()).unwrap().content);
    assert_eq!(imgs[0][0], (1, 1, vec![0x11, 0x22, 0x33, 0xFF]));
}

#[test]
fn f6_unknown_type_is_e_tex_tie() {
    assert_eq!(tex_err(&patch(f6_tex(), 0x3B, &[37])).code(), "E_TEX_TIE");
}

#[test]
fn f6_undecodable_image_is_transparent_black() {
    // 70 pseudo-random bytes (fixed LCG, byte 1 is not a valid TGA colour-map type).
    let mut state = 0x1234_5678u32;
    let noise: Vec<u8> = (0..70)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8 | 0x10
        })
        .collect();
    let bytes = patch(f6_tex(), 0x57, &noise);
    let t = tex::parse(&bytes).unwrap();
    let (format, imgs) = images(&t.into_upload(&UploadOptions::default()).unwrap().content);
    assert_eq!(format, GpuFormat::Rgba8Unorm);
    assert_eq!(imgs[0][0], (1, 1, vec![0, 0, 0, 0]));
}

// ---------------------------------------------------------------- F7: animation

#[test]
fn f7_animated_two_frames() {
    let bytes = f7_tex();
    assert_eq!(bytes.len(), 204);
    let t = tex::parse(&bytes).unwrap();
    assert!(t.is_animated());
    assert_eq!(t.image_count(), 2);
    let anim = t.animation.as_ref().unwrap();
    assert_eq!(anim.version, 3);
    assert_eq!((anim.width, anim.height), (1, 1));
    assert_eq!(anim.frames.len(), 2);
    assert_eq!(anim.frames[1].image, 1);
    assert_eq!(anim.total_duration(), 1.0);
    assert_eq!(t.real_size(), [1, 1]);
    assert_eq!(t.resolution(), [1, 1, 1, 1]);
    // 1 × 1 < 2 frames: no spritesheet.
    assert_eq!(t.spritesheet, Spritesheet::default());

    let up = t.into_upload(&UploadOptions::default()).unwrap();
    assert_eq!(up.sampler.address_mode, AddressMode::ClampToEdge);
    let (_, imgs) = images(&up.content);
    assert_eq!(imgs[0][0].2, vec![0xFF, 0, 0, 0xFF]);
    assert_eq!(imgs[1][0].2, vec![0, 0, 0xFF, 0xFF]);
    for (time, image) in [(0.25, 0), (0.5, 0), (0.75, 1), (1.0, 0)] {
        let state = up.frame_state(time);
        assert_eq!(state.image, image, "t = {time}");
        assert_eq!(state.translation, [0.0, 0.0]);
        assert_eq!(state.rotation, [1.0, 0.0, 0.0, 1.0]);
    }
}

#[test]
fn f7_wider_image_gives_spritesheet() {
    let t_bytes = patch(f7_tex(), 0x22, &[0x02]);
    let t = tex::parse(&t_bytes).unwrap();
    assert_eq!(
        t.spritesheet,
        Spritesheet {
            columns: 2,
            rows: 1,
            frames: 2,
            duration: 1.0
        }
    );
}

#[test]
fn f7_texs0004_is_e_tex_texs() {
    assert_eq!(tex_err(&patch(f7_tex(), 0x7E, b"4")).code(), "E_TEX_TEXS");
}

#[test]
fn f7_tex_json_overrides_spritesheet() {
    let mut t_bytes = patch(f7_tex(), 0x22, &[0x02]);
    t_bytes = patch(t_bytes, 0x26, &[0x02]);
    let mut t = tex::parse(&t_bytes).unwrap();
    let applied = t.apply_tex_json(
        br#"{"spritesheetsequences":[{"frames":4,"width":1,"height":1,"duration":2}]}"#,
    );
    assert!(applied);
    assert_eq!(
        t.spritesheet,
        Spritesheet {
            columns: 2,
            rows: 2,
            frames: 4,
            duration: 2.0
        }
    );
    // An invalid companion keeps the TEXS values.
    let before = t.spritesheet;
    assert!(!t.apply_tex_json(br#"{"spritesheetsequences":[{"frames":"3"}]}"#));
    assert_eq!(t.spritesheet, before);
}

#[test]
fn texs0001_integer_frames() {
    // F7 rewritten as TEXS0001 with integer fields; LA/HA come from frame 0.
    let mut bytes = f7_tex()[..0x77].to_vec();
    bytes.extend_from_slice(b"TEXS0001\0");
    bytes.extend_from_slice(&1u32.to_le_bytes());
    for v in [0u32, 0x3F00_0000, 3, 4, 5, 6, 7, 8] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let t = tex::parse(&bytes).unwrap();
    let anim = t.animation.as_ref().unwrap();
    let f = anim.frames[0];
    assert_eq!(
        (
            f.duration, f.x, f.y, f.width_x, f.width_y, f.height_x, f.height_y
        ),
        (0.5, 3.0, 4.0, 5.0, 0.0, 0.0, 8.0)
    );
    assert_eq!((anim.width, anim.height), (5, 8));
    assert_eq!(t.real_size(), [5, 8]);
}

#[test]
fn texs0002_without_frames_has_zero_size() {
    let mut bytes = f7_tex()[..0x77].to_vec();
    bytes.extend_from_slice(b"TEXS0002\0");
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(t.real_size(), [0, 0]);
    let up = t.into_upload(&UploadOptions::default()).unwrap();
    assert_eq!(up.frame_state(0.5), FrameState::default());
}

// ---------------------------------------------------------------- F8: MP4

#[test]
fn f8_mp4_video() {
    let bytes = f8_tex();
    assert_eq!(bytes.len(), 110);
    let t = tex::parse(&bytes).unwrap();
    assert!(t.is_mp4);
    assert_eq!(t.embedded_format, EmbeddedFormat::WebpOrMp4);
    assert_eq!(t.embedded_format.raw(), 35);
    assert_eq!(t.container_version, 4);
    assert_eq!(t.effective_version, 4);
    let mip = &t.image(0).unwrap()[0];
    assert_eq!(mip.data.len(), 4);
    assert_eq!(mip.metadata, Some(&b"{}"[..]));
    assert_eq!((mip.width, mip.height), (64, 64));
    let up = t.into_upload(&UploadOptions::default()).unwrap();
    match up.content {
        GpuContent::Video {
            data,
            width,
            height,
            mip_count,
        } => {
            assert_eq!(&*data, b"MP4!");
            assert!(matches!(data, Cow::Borrowed(_)));
            assert_eq!((width, height, mip_count), (64, 64, 1));
        }
        GpuContent::Images { .. } => panic!("expected the video path"),
    }
}

#[test]
fn f8_without_mp4_flag_falls_back_to_v3_and_runs_out_of_data() {
    let bytes = patch(f8_tex(), 0x3F, &[0x00]);
    assert_eq!(tex_err(&bytes).code(), "FinDeFluxInattendue");
}

#[test]
fn video_flag_without_mipmap_is_e_tex_video_vide() {
    // F3 with the VIDEO flag and no mipmap in its image.
    let mut bytes = patch(f3_tex(), 0x16, &[0x20]);
    bytes.truncate(0x3F);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let t = tex::parse(&bytes).unwrap();
    let err = t.into_upload(&UploadOptions::default()).unwrap_err();
    assert_eq!(err.code(), "E_TEX_VIDEO_VIDE");
}

#[test]
fn video_with_unsupported_raw_format_fails_gpu_format_first() {
    // QO-12: the GPU format is checked before the video test.
    let bytes = patch(patch(f3_tex(), 0x16, &[0x20]), 0x12, &[0x01]);
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(
        t.into_upload(&UploadOptions::default()).unwrap_err().code(),
        "E_TEX_FORMAT_GPU"
    );
}

// ---------------------------------------------------------------- F9: MDLV

#[test]
fn f9_puppet_mesh() {
    let bytes = f9_mdl();
    assert_eq!(bytes.len(), 276);
    assert_eq!(
        &bytes[..40],
        &hex("
0000: 4D 44 4C 56 30 30 32 31 00 00 00 00 00 F0 00 00
0010: 00 00 00 80 BF 00 00 80 3F 00 00 00 00 00 00 00
0020: 00 00 00 00 00 00 00 00
")[..]
    );
    let mesh = mdl::parse_puppet(&bytes).unwrap();
    assert_eq!(mesh.version, MdlVersion::V21);
    assert_eq!(mesh.block_offset, 9);
    assert_eq!(mesh.positions.len(), 3);
    assert_eq!(mesh.indices, [0, 1, 2]);
    assert_eq!(mesh.uvs, [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    let placed: Vec<_> = mesh.placed_positions([100.0, 50.0]).collect();
    assert_eq!(
        placed,
        [[49.0, 24.0, 0.0], [51.0, 24.0, 0.0], [49.0, 26.0, 0.0]]
    );
}

#[test]
fn f9_variants() {
    let err = mdl::parse_puppet(&patch(f9_mdl(), 0, b"MDLV0014")).unwrap_err();
    assert!(matches!(err, MdlError::UnsupportedVersion(_)));
    assert_eq!(
        mdl::parse_puppet(&patch(f9_mdl(), 261, &[3, 0])).unwrap_err(),
        MdlError::InvalidIndex {
            index: 3,
            vertex_count: 3
        }
    );
    let without_mdls = f9_mdl()[..267].to_vec();
    assert_eq!(
        mdl::parse_puppet(&without_mdls).unwrap(),
        mdl::parse_puppet(&f9_mdl()).unwrap()
    );
    assert_eq!(
        mdl::parse_puppet(&patch(f9_mdl(), 13, &[239])).unwrap_err(),
        MdlError::NoMeshBlock
    );
    assert_eq!(
        mdl::parse_puppet(&patch(f9_mdl(), 0, b"MDLV0023"))
            .unwrap()
            .version,
        MdlVersion::V23
    );
    assert!(mdl::parse_puppet(b"MDLV0021").is_err());
}

// ---------------------------------------------------------------- F10: .tex negatives

#[test]
fn f10_negative_cases() {
    let cases: [(usize, &[u8], &str); 6] = [
        (0x08, &[0x20], "E_TEX_TEXV"),
        (0x0C, &[0x4A], "E_TEX_TEXI"),
        (0x12, &[3, 0, 0, 0], "E_TEX_FORMAT"),
        (0x16, &[0x2F, 0x00, 0x08, 0x00], "E_TEX_DRAPEAUX"),
        (0x35, &[0x35], "E_TEX_TEXB"),
        (0x53, &[0xFF, 0xFF, 0xFF, 0xFF], "E_TEX_TAILLE"),
    ];
    for (offset, bytes, code) in cases {
        assert_eq!(
            tex_err(&patch(f3_tex(), offset, bytes)).code(),
            code,
            "offset {offset:#x}"
        );
    }
    assert_eq!(tex_err(&f3_tex()[..100]).code(), "FinDeFluxInattendue");
}

#[test]
fn f10_rgb888_parses_then_fails_on_gpu() {
    let t_bytes = patch(f3_tex(), 0x12, &[1]);
    let t = tex::parse(&t_bytes).unwrap();
    assert_eq!(t.format, TexFormat::Rgb888);
    let err = t
        .clone()
        .into_upload(&UploadOptions::default())
        .unwrap_err();
    assert_eq!(err.code(), "E_TEX_FORMAT_GPU");
    // Opt-in extension expands it on the CPU.
    let options = UploadOptions {
        extended_formats: true,
        ..UploadOptions::default()
    };
    let (format, imgs) = images(&t.into_upload(&options).unwrap().content);
    assert_eq!(format, GpuFormat::Rgba8Unorm);
    // Pixels are the first 12 bytes read as R, G, B triplets, alpha forced to 255.
    assert_eq!(
        imgs[0][0].2[..8],
        [0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF]
    );
}

#[test]
fn f10_accepted_flags() {
    let t_bytes = patch(f3_tex(), 0x16, &[0x10]);
    assert_eq!(tex::parse(&t_bytes).unwrap().flags.bits(), 16);
    let t_bytes = patch(f3_tex(), 0x16, &[0x08]);
    let t = tex::parse(&t_bytes).unwrap();
    let up = t.into_upload(&UploadOptions::default()).unwrap();
    assert_eq!(up.sampler.address_mode, AddressMode::Repeat);
}

#[test]
fn flags_rule_is_a_comparison() {
    assert!(TexFlags::from_raw(524_334).is_ok());
    assert_eq!(
        TexFlags::from_raw(524_335).unwrap_err().code(),
        "E_TEX_DRAPEAUX"
    );
    assert_eq!(
        TexFlags::from_raw(0x10_0000).unwrap_err().code(),
        "E_TEX_DRAPEAUX"
    );
    assert!(TexFlags::from_raw(64 | 128).is_ok());
}

// ---------------------------------------------------------------- F11: VOLUME (A.1, A.5)

#[test]
fn f11_volume_texture() {
    let bytes = f11_tex();
    assert_eq!(bytes.len(), 174);
    let t = tex::parse(&bytes).unwrap();
    // The analysis stops exactly at the end of the file.
    assert_eq!(t.end_offset, 174);
    assert!(t.is_volume());
    assert_eq!(t.flags.bits(), 0x42);
    assert_eq!(
        (t.texture_width, t.texture_height, t.texture_depth),
        (2, 1, 2)
    );
    assert_eq!((t.image_width, t.image_height), (4, 1));
    assert_eq!((t.container_version, t.effective_version), (4, 3));
    assert_eq!(t.embedded_format, EmbeddedFormat::Png);
    let mip = &t.image(0).unwrap()[0];
    assert_eq!((mip.width, mip.height, mip.depth), (2, 1, 2));
    assert_eq!(mip.data.len(), 75);
    assert_eq!(t.resolution(), [2, 1, 4, 1]);
    assert_eq!(t.tex0_format_combo(), None);

    let up = t.into_upload(&UploadOptions::default()).unwrap();
    assert_eq!(up.resolution, [2, 1, 4, 1]);
    assert_eq!(up.dimension(), Some(TextureDimension::D3));
    assert_eq!(up.sampler.address_mode, AddressMode::ClampToEdge);
    assert_eq!(up.sampler.mag_filter, FilterMode::Linear);
    assert_eq!(up.sampler.min_filter, FilterMode::Linear);
    assert_eq!(up.sampler.anisotropy_clamp, 1);
    let GpuContent::Images { format, images, .. } = &up.content else {
        panic!("expected images")
    };
    assert_eq!(*format, GpuFormat::Rgba8Unorm);
    let level = &images[0].mips[0];
    assert_eq!((level.width, level.height, level.depth), (2, 1, 2));
    // Texels (x, y, z): (0,0,0) red, (1,0,0) green, (0,0,1) blue, (1,0,1) white.
    assert_eq!(
        &*level.data,
        &[
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255
        ]
    );
}

#[test]
fn f11_variants() {
    // VOLUME bit removed: the reference layout reads the colour as the magic.
    let err = tex_err(&patch(f11_tex(), 0x16, &[0x02]));
    assert_eq!(err.code(), "E_TEX_TEXB");
    assert!(err.to_string().contains(r"\x80\x80\x80\xffTEXB0"), "{err}");
    // VOLUME + VIDEO: parses, refused at GPU creation.
    let bytes = patch(f11_tex(), 0x16, &[0x62]);
    let t = tex::parse(&bytes).unwrap();
    let err = t.into_upload(&UploadOptions::default()).unwrap_err();
    assert_eq!(err.code(), "E_TEX_VOLUME");
    // VOLUME + ANIMATED is refused the same way (TEXS appended so it parses).
    let mut bytes = patch(f11_tex(), 0x16, &[0x46]);
    bytes.extend_from_slice(b"TEXS0003\0");
    bytes.extend_from_slice(&[0; 12]);
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(
        t.into_upload(&UploadOptions::default()).unwrap_err().code(),
        "E_TEX_VOLUME"
    );
    // Mip depth 3: the decoded 2×2 image is not 2×(1×3): zeroed 2×1×3 level.
    let bytes = patch(f11_tex(), 0x53, &[0x03]);
    let t = tex::parse(&bytes).unwrap();
    let up = t.into_upload(&UploadOptions::default()).unwrap();
    let GpuContent::Images { images, .. } = &up.content else {
        panic!("expected images")
    };
    let level = &images[0].mips[0];
    assert_eq!((level.width, level.height, level.depth), (2, 1, 3));
    assert_eq!(&*level.data, &[0u8; 24][..]);
    // Truncation.
    assert_eq!(tex_err(&f11_tex()[..173]).code(), "FinDeFluxInattendue");
}

#[test]
fn volume_with_raw_pixels_is_a_3d_level() {
    // Extrapolated case (QO-35): TEXB0003, no embedded image, RGBA 2×1×2 raw.
    let mut bytes = f11_tex()[..0x32].to_vec();
    bytes.extend_from_slice(b"TEXB0003\0");
    bytes.extend_from_slice(&1u32.to_le_bytes()); // NI
    bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // TIE unknown
    bytes.extend_from_slice(&1u32.to_le_bytes()); // NM
    for v in [2u32, 1, 2, 0, 16, 16] {
        bytes.extend_from_slice(&v.to_le_bytes()); // w, h, depth, C, TD, TS
    }
    bytes.extend(0u8..16);
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(t.end_offset, bytes.len());
    let up = t.into_upload(&UploadOptions::default()).unwrap();
    let GpuContent::Images {
        dimension, images, ..
    } = &up.content
    else {
        panic!("expected images")
    };
    assert_eq!(*dimension, TextureDimension::D3);
    let level = &images[0].mips[0];
    assert_eq!((level.width, level.height, level.depth), (2, 1, 2));
    assert!(matches!(level.data, Cow::Borrowed(_)), "zero-copy");
    assert_eq!(&*level.data, &(0u8..16).collect::<Vec<_>>()[..]);
}

// ---------------------------------------------------------------- A.3: TEX0FORMAT

#[test]
fn a3_tex0format_follows_the_declared_format() {
    // F6 (embedded PNG, uploaded as RGBA8) declaring RG88, then R8.
    for (declared, combo) in [(8u8, Some(8)), (9, Some(9)), (0, None), (4, None)] {
        let bytes = patch(f6_tex(), 0x12, &[declared]);
        let t = tex::parse(&bytes).unwrap();
        assert_eq!(t.tex0_format_combo(), combo);
        let up = t.into_upload(&UploadOptions::default()).unwrap();
        assert_eq!(up.tex0_format_combo, combo);
        assert_eq!(up.source_format.raw(), u32::from(declared));
        assert_eq!(up.source_format.tex0_format_combo(), combo);
        let (format, _) = images(&up.content);
        assert_eq!(
            format,
            GpuFormat::Rgba8Unorm,
            "embedded image sent as RGBA8"
        );
    }
}

#[test]
fn every_truncation_of_every_fixture_fails_cleanly() {
    for fixture in [
        f3_tex(),
        f4_tex(),
        f5_tex(),
        f6_tex(),
        f7_tex(),
        f8_tex(),
        f11_tex(),
    ] {
        let full = tex::parse(&fixture).unwrap();
        let needed = fixture.len();
        for len in 0..needed {
            if let Ok(t) = tex::parse(&fixture[..len]) {
                // Only trailing ignored bytes may be cut.
                assert_eq!(t, full);
            }
        }
    }
}

// ---------------------------------------------------------------- upload edge cases

#[test]
fn image_without_mipmap_is_1x1_transparent() {
    let mut bytes = f3_tex()[..0x3F].to_vec();
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(t.texture_size(0), [1, 1]);
    let (_, imgs) = images(&t.into_upload(&UploadOptions::default()).unwrap().content);
    assert_eq!(imgs[0], vec![(1, 1, vec![0, 0, 0, 0])]);
}

#[test]
fn bc_levels_pass_through_and_decode_on_fallback() {
    // DXT1 4×4 texture with mips 4×4, 2×2, 1×1 (one block each).
    let mut bytes = patch(f3_tex(), 0x12, &[7])[..0x3F].to_vec();
    bytes.extend_from_slice(&3u32.to_le_bytes());
    for size in [4u32, 2, 1] {
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&8i32.to_le_bytes());
        bytes.extend_from_slice(&8i32.to_le_bytes());
        bytes.extend_from_slice(&[0xFF, 0xFF, 0, 0, 0, 0, 0, 0]);
    }
    let t = tex::parse(&bytes).unwrap();
    let mut up = t.into_upload(&UploadOptions::default()).unwrap();
    let (format, imgs) = images(&up.content);
    assert_eq!(format, GpuFormat::Bc1RgbaUnorm);
    assert_eq!(imgs[0].len(), 3);
    assert!(imgs[0].iter().all(|m| m.2.len() == 8));
    assert!(!up.requires_cpu_decode(true));
    assert!(up.requires_cpu_decode(false));
    up.decode_compressed(tex::DEFAULT_MAX_LEVEL_BYTES).unwrap();
    let (format, imgs) = images(&up.content);
    assert_eq!(format, GpuFormat::Rgba8Unorm);
    assert_eq!(imgs[0][0].2, vec![0xFF; 64]);
    assert_eq!(imgs[0][2].2, vec![0xFF; 4]);
}

#[test]
fn broken_mip_chain_is_truncated() {
    // Level 1 claims 2×1 for a 2×2 base: dropped.
    let mut bytes = f3_tex()[..0x3F].to_vec();
    bytes.extend_from_slice(&2u32.to_le_bytes());
    for (w, h) in [(2u32, 2u32), (2, 1)] {
        bytes.extend_from_slice(&w.to_le_bytes());
        bytes.extend_from_slice(&h.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let len = (w * h * 4) as i32;
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend(std::iter::repeat_n(7u8, len as usize));
    }
    let t = tex::parse(&bytes).unwrap();
    let (_, imgs) = images(&t.into_upload(&UploadOptions::default()).unwrap().content);
    assert_eq!(imgs[0].len(), 1);
}

#[test]
fn huge_declared_level_is_refused() {
    // 65535×65535 RGBA from 16 bytes of data: refused instead of allocating 16 GiB.
    let bytes = patch(patch(f3_tex(), 0x43, &[0xFF, 0xFF]), 0x47, &[0xFF, 0xFF]);
    let t = tex::parse(&bytes).unwrap();
    assert_eq!(
        t.into_upload(&UploadOptions::default()).unwrap_err().code(),
        "E_TEX_TAILLE"
    );
    // Same for an LZ4 TD above the parse limit.
    let options = tex::ParseOptions { max_level_bytes: 3 };
    assert_eq!(
        tex::parse_with(&f4_tex(), &options).unwrap_err().code(),
        "E_TEX_TAILLE"
    );
}

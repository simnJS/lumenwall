//! Spec 09 §4.1–§4.4 and §4.7: static `.mdl` fixtures (checked against the SHA-256 printed in
//! the spec), their negative variants, and the installed models (`--ignored`).

mod common;

use common::*;
use lumenwall_formats::mdl::{
    self, StaticModelError, VertexFormatError, parse_static_model, vertex_format,
};

/// FIPS 180-4 SHA-256, enough to check the fixtures against the spec without a dependency.
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[test]
fn sha256_known_answer() {
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// F-3D1 (spec 09 §4.1): `MDLV0014`, quad, `vf = 0x9`, 143 bytes.
fn f3d1() -> Vec<u8> {
    hex("
00000000: 4d 44 4c 56 30 30 31 34 00 09 00 00 00 01 00 00
00000010: 00 01 00 00 00 6d 61 74 65 72 69 61 6c 73 2f 6d
00000020: 2e 6a 73 6f 6e 00 00 00 00 00 50 00 00 00 00 00
00000030: 80 bf 00 00 80 bf 00 00 00 00 00 00 00 00 00 00
00000040: 80 3f 00 00 80 3f 00 00 80 bf 00 00 00 00 00 00
00000050: 80 3f 00 00 80 3f 00 00 80 3f 00 00 80 3f 00 00
00000060: 00 00 00 00 80 3f 00 00 00 00 00 00 80 bf 00 00
00000070: 80 3f 00 00 00 00 00 00 00 00 00 00 00 00 0c 00
00000080: 00 00 00 00 01 00 02 00 00 00 02 00 03 00 00
")
}

/// F-3D2 (spec 09 §4.2): `MDLV0004`, 2 skins, triangle, `vf = 0xB`, 169 bytes.
fn f3d2() -> Vec<u8> {
    hex("
00000000: 4d 44 4c 56 30 30 30 34 00 0b 00 00 00 02 00 00
00000010: 00 01 00 00 00 6d 61 74 65 72 69 61 6c 73 2f 61
00000020: 2e 6a 73 6f 6e 00 6d 61 74 65 72 69 61 6c 73 2f
00000030: 62 2e 6a 73 6f 6e 00 00 00 00 00 60 00 00 00 00
00000040: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00000050: 00 00 00 00 00 80 3f 00 00 00 00 00 00 80 3f 00
00000060: 00 80 3f 00 00 00 00 00 00 00 00 00 00 00 00 00
00000070: 00 00 00 00 00 80 3f 00 00 80 3f 00 00 80 3f 00
00000080: 00 00 00 00 00 80 3f 00 00 00 00 00 00 00 00 00
00000090: 00 00 00 00 00 80 3f 00 00 00 00 00 00 00 00 06
000000a0: 00 00 00 00 00 01 00 02 00
")
}

fn push_f32s(out: &mut Vec<u8>, v: &[f32]) {
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
}

/// F-3D3 (spec 09 §4.3): `MDLV0017`, 2 meshes, `vf = 0x27`, 484 bytes, built as described.
fn f3d3() -> Vec<u8> {
    let mut d = b"MDLV0017\0".to_vec();
    for w in [0x27u32, 1, 2] {
        d.extend_from_slice(&w.to_le_bytes());
    }
    for (k, dz) in [0.0f32, -1.0].into_iter().enumerate() {
        d.extend_from_slice(if k == 0 {
            b"materials/a.json\0"
        } else {
            b"materials/b.json\0"
        });
        d.extend_from_slice(&0u32.to_le_bytes());
        push_f32s(&mut d, &[0.0, 0.0, dz, 1.0, 1.0, dz]);
        d.extend_from_slice(&0x27u32.to_le_bytes());
        d.extend_from_slice(&168u32.to_le_bytes());
        let verts: [([f32; 3], [f32; 4], [f32; 4]); 3] = [
            ([0.0, 0.0, dz], [1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.25, 0.75]),
            ([1.0, 0.0, dz], [1.0, 0.0, 0.0, 1.0], [1.0, 1.0, 0.75, 0.75]),
            (
                [0.0, 1.0, dz],
                [1.0, 0.0, 0.0, -1.0],
                [0.0, 0.0, 0.25, 0.25],
            ),
        ];
        for (p, t, uv) in verts {
            push_f32s(&mut d, &p);
            push_f32s(&mut d, &[0.0, 0.0, 1.0]);
            push_f32s(&mut d, &t);
            push_f32s(&mut d, &uv);
        }
        d.extend_from_slice(&6u32.to_le_bytes());
        for i in [0u16, 1, 2] {
            d.extend_from_slice(&i.to_le_bytes());
        }
    }
    d.push(0);
    d
}

#[test]
fn fixtures_match_the_spec_hashes() {
    let cases = [
        (
            f3d1(),
            143,
            "32ba848b54f94b3af6f6dd56d9ce1a898df1502fd1cea2a9bc390cd04c4c3076",
        ),
        (
            f3d2(),
            169,
            "5731638a30d8028e7c4d3e930b7114c5a4709f88281702af8a72db6dc4ae9cfc",
        ),
        (
            f3d3(),
            484,
            "6797460743e342b336857f771401d87a24bf3ad5861f6abd4783029f51b396a3",
        ),
    ];
    for (bytes, len, hash) in cases {
        assert_eq!(bytes.len(), len);
        assert_eq!(sha256(&bytes), hash);
    }
}

#[test]
fn f3d1_quad() {
    let m = parse_static_model(&f3d1()).unwrap();
    assert_eq!((m.version, m.flags, m.skins), (14, 0x9, 1));
    assert_eq!(m.meshes.len(), 1);
    let mesh = &m.meshes[0];
    assert_eq!(mesh.materials, ["materials/m.json"]);
    assert_eq!(mesh.bounds, None);
    assert_eq!((mesh.layout.stride, mesh.vertex_count()), (20, 4));
    let got: Vec<([f32; 3], [f32; 2])> = (0..4)
        .map(|i| (mesh.position(i), mesh.uv(i).unwrap()))
        .collect();
    assert_eq!(
        got,
        [
            ([-1.0, -1.0, 0.0], [0.0, 1.0]),
            ([1.0, -1.0, 0.0], [1.0, 1.0]),
            ([1.0, 1.0, 0.0], [1.0, 0.0]),
            ([-1.0, 1.0, 0.0], [0.0, 0.0]),
        ]
    );
    assert_eq!(mesh.normal(0), None);
    assert_eq!(mesh.uv4(1), Some([1.0, 1.0, 0.0, 1.0]));
    assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3]);
    // Read until 142: the terminal 0x00 ends the chain.
    assert_eq!(m.meshes_end, 142);
    assert!(m.sections.is_empty() && m.warnings.is_empty());
    // Counter-clockwise seen from +Z: front faces towards the default camera.
    for t in mesh.indices.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|k| mesh.position(usize::from(t[k])));
        let z = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        assert!(z > 0.0);
    }
}

#[test]
fn f3d2_two_skins_without_terminal_byte() {
    let m = parse_static_model(&f3d2()).unwrap();
    assert_eq!((m.version, m.skins), (4, 2));
    let mesh = &m.meshes[0];
    assert_eq!(mesh.materials, ["materials/a.json", "materials/b.json"]);
    assert_eq!(mesh.layout.stride, 32);
    assert_eq!(mesh.normal(2), Some([0.0, 0.0, 1.0]));
    assert_eq!(mesh.position(2), [0.0, 1.0, 0.0]);
    assert_eq!(mesh.uv(0), Some([0.0, 1.0]));
    assert_eq!(mesh.indices, [0, 1, 2]);
    assert_eq!(m.meshes_end, 169);
    assert_eq!(m.material(0, 0), Some(("materials/a.json", false)));
    assert_eq!(m.material(0, 1), Some(("materials/b.json", false)));
    // `skin: 5` falls back to skin 0 (the caller warns).
    assert_eq!(m.material(0, 5), Some(("materials/a.json", true)));
}

#[test]
fn f3d3_two_meshes_two_uv_sets() {
    let m = parse_static_model(&f3d3()).unwrap();
    assert_eq!((m.version, m.meshes.len()), (17, 2));
    for (k, mesh) in m.meshes.iter().enumerate() {
        let dz = -(k as f32);
        assert_eq!(mesh.bounds, Some(([0.0, 0.0, dz], [1.0, 1.0, dz])));
        assert_eq!(mesh.layout.format, 0x27);
        assert_eq!(mesh.layout.stride, 56);
        assert_eq!(mesh.uv(1), Some([1.0, 1.0]));
        assert_eq!(mesh.uv4(1), Some([1.0, 1.0, 0.75, 0.75]));
        // Bitangent (n × t)·w of vertex 2.
        let n = mesh.normal(2).unwrap();
        let t = mesh.tangent(2).unwrap();
        let b = [
            (n[1] * t[2] - n[2] * t[1]) * t[3],
            (n[2] * t[0] - n[0] * t[2]) * t[3],
            (n[0] * t[1] - n[1] * t[0]) * t[3],
        ];
        assert_eq!(b, [0.0, -1.0, 0.0]);
    }
    assert_eq!(m.meshes[1].materials, ["materials/b.json"]);
    assert_eq!(m.meshes_end, 483);
}

#[test]
fn f3d1_negative_variants() {
    let base = f3d1();
    let err = |d: Vec<u8>| parse_static_model(&d).unwrap_err();
    assert!(matches!(
        err(patch(base.clone(), 0x09, &[0x19])),
        StaticModelError::VertexFormat {
            error: VertexFormatError::UnknownBits(0x10),
            ..
        }
    ));
    assert!(matches!(
        err(patch(base.clone(), 0x09, &[0x29])),
        StaticModelError::VertexFormat {
            error: VertexFormatError::BothUvSets,
            ..
        }
    ));
    assert!(matches!(
        err(patch(base.clone(), 0x2A, &[81])),
        StaticModelError::VertexBlockSize { size: 81, .. }
    ));
    assert!(matches!(
        err(patch(base.clone(), 0x8C, &[4])),
        StaticModelError::IndexOutOfRange { index: 4, .. }
    ));
    assert_eq!(
        err(patch(base.clone(), 0x0D, &[0])),
        StaticModelError::NoSkin
    );
    assert!(matches!(
        err(base[..100].to_vec()),
        StaticModelError::Eof { .. }
    ));
    // Last byte 'M' followed by nothing: chain stopped with a warning, model accepted.
    let m = parse_static_model(&patch(base.clone(), 0x8E, &[0x4D])).unwrap();
    assert_eq!(m.meshes.len(), 1);
    assert!(m.sections.is_empty());
    assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
    assert_eq!(m.warnings[0].offset, 0x8E);
    // Bad magics.
    assert!(matches!(
        err(patch(base.clone(), 0x08, &[0x41])),
        StaticModelError::BadMagic(_)
    ));
    assert!(matches!(
        err(b"MDLV00".to_vec()),
        StaticModelError::BadMagic(_)
    ));
}

#[test]
fn unobserved_versions_are_tried_with_a_warning() {
    // 0016 reads like 0014 (no box, no vf per mesh).
    let m = parse_static_model(&patch(f3d1(), 6, b"16")).unwrap();
    assert_eq!(m.version, 16);
    assert_eq!(m.meshes[0].vertex_count(), 4);
    assert_eq!(m.warnings.len(), 1);
}

#[test]
fn mdlv0023_static_tail_is_skipped() {
    // F-3D3 as 0023: each mesh gains the 6-byte static tail.
    let base = f3d3();
    let mut d = b"MDLV0023\0".to_vec();
    d.extend_from_slice(&base[9..21]);
    let mesh_len = (base.len() - 22) / 2;
    for k in 0..2 {
        d.extend_from_slice(&base[21 + k * mesh_len..21 + (k + 1) * mesh_len]);
        d.extend_from_slice(&[0; 6]);
    }
    d.push(0);
    let m = parse_static_model(&d).unwrap();
    assert_eq!(m.meshes.len(), 2);
    assert_eq!(m.meshes_end, d.len() - 1);
    assert!(m.warnings.is_empty());
    // A tail flag other than 0/1 is refused.
    let bad = patch(d.clone(), 21 + mesh_len, &[7]);
    assert!(matches!(
        parse_static_model(&bad),
        Err(StaticModelError::BadTail { mesh: 0, .. })
    ));
}

#[test]
fn format_bits_constants() {
    assert_eq!(vertex_format::UV2, 0x20);
    assert_eq!(
        mdl::PUPPET_VERTEX_FORMAT & vertex_format::KNOWN,
        mdl::PUPPET_VERTEX_FORMAT
    );
}

/// Expected structure of the installed models (spec 09 §4.7): path, version, S, vf, vertices
/// and indices per mesh, terminal byte.
type Expected = (
    &'static str,
    u32,
    u32,
    u32,
    &'static [usize],
    &'static [usize],
    bool,
);

const INSTALLED: &[Expected] = &[
    (
        "assets/models/editor/camera/camera.mdl",
        17,
        1,
        0xF,
        &[692],
        &[1671],
        true,
    ),
    (
        "assets/scenes/particleelementpreviews/collisionmodel/models/sphere/sphere.mdl",
        23,
        1,
        0xF,
        &[559],
        &[2880],
        true,
    ),
    (
        "projects/defaultprojects/arsenal/models/pistols/pistols.mdl",
        14,
        1,
        0x27,
        &[1423, 8, 4698, 382, 260, 2566],
        &[4041, 12, 11292, 900, 540, 6720],
        true,
    ),
    (
        "projects/defaultprojects/audiophile/models/audiophile/bars.mdl",
        4,
        1,
        0xB,
        &[384],
        &[576],
        false,
    ),
    (
        "projects/defaultprojects/audiophile/models/audiophile/flow.mdl",
        4,
        1,
        0x9,
        &[163],
        &[600],
        false,
    ),
    (
        "projects/defaultprojects/audiophile/models/audiophile/glow.mdl",
        4,
        1,
        0x9,
        &[4],
        &[6],
        false,
    ),
    (
        "projects/defaultprojects/audiophile/models/grid/grid.mdl",
        4,
        2,
        0x9,
        &[4],
        &[6],
        false,
    ),
    (
        "projects/defaultprojects/demon_core/models/backgroundsphere/backgroundsphere.mdl",
        14,
        1,
        0xB,
        &[1087],
        &[5952],
        true,
    ),
    (
        "projects/defaultprojects/demon_core/models/core/core.mdl",
        14,
        1,
        0xB,
        &[970],
        &[1440],
        true,
    ),
    (
        "projects/defaultprojects/dna_fragment/models/bg/bg.mdl",
        4,
        1,
        0xB,
        &[4],
        &[6],
        false,
    ),
    (
        "projects/defaultprojects/dna_fragment/models/bg/bgfade.mdl",
        4,
        1,
        0x9,
        &[4],
        &[6],
        false,
    ),
    (
        "projects/defaultprojects/dna_fragment/models/curve/curve.mdl",
        4,
        1,
        0xB,
        &[4],
        &[6],
        false,
    ),
    (
        "projects/defaultprojects/dna_fragment/models/dna/dna.mdl",
        4,
        1,
        0xB,
        &[2604],
        &[11472],
        false,
    ),
    (
        "projects/defaultprojects/fantasticcar/models/car/body.mdl",
        14,
        1,
        0xF,
        &[1782, 103, 1824, 2181, 42, 1104],
        &[4908, 318, 4182, 6315, 120, 2880],
        true,
    ),
    (
        "projects/defaultprojects/fantasticcar/models/dome/dome.mdl",
        14,
        1,
        0x9,
        &[236],
        &[600],
        true,
    ),
    (
        "projects/defaultprojects/fantasticcar/models/grid/grid.mdl",
        14,
        1,
        0x9,
        &[4],
        &[6],
        true,
    ),
    (
        "projects/defaultprojects/fantasticcar/models/util/shadow.mdl",
        14,
        1,
        0x9,
        &[90],
        &[210],
        true,
    ),
    (
        "projects/defaultprojects/neon_sunset/models/neongrid/neongrid.mdl",
        14,
        1,
        0xF,
        &[2601],
        &[15000],
        true,
    ),
    (
        "projects/defaultprojects/neon_sunset/models/neonsun/neonsun.mdl",
        14,
        1,
        0xF,
        &[4],
        &[6],
        true,
    ),
    (
        "projects/defaultprojects/retro/models/bgfade/bgfade.mdl",
        14,
        1,
        0x9,
        &[4],
        &[6],
        true,
    ),
    (
        "projects/defaultprojects/ricepod/models/ricepod/jet.mdl",
        14,
        1,
        0x9,
        &[60],
        &[168],
        true,
    ),
    (
        "projects/defaultprojects/ricepod/models/ricepod/orbitaleffects.mdl",
        14,
        1,
        0x9,
        &[62, 55, 102],
        &[180, 174, 360],
        true,
    ),
    (
        "projects/defaultprojects/ricepod/models/ricepod/ricepod.mdl",
        14,
        1,
        0xB,
        &[682, 159, 10995],
        &[1725, 414, 24999],
        true,
    ),
    (
        "projects/defaultprojects/ricepod/models/ricepod/skybox.mdl",
        14,
        1,
        0x9,
        &[24],
        &[36],
        true,
    ),
    (
        "projects/defaultprojects/techno/models/techno/glow.mdl",
        23,
        1,
        0x9,
        &[4],
        &[6],
        true,
    ),
    (
        "projects/defaultprojects/techno/models/techno/orbitsmall.mdl",
        23,
        1,
        0x9,
        &[66, 66, 66, 99],
        &[192, 192, 192, 384],
        true,
    ),
    (
        "projects/defaultprojects/techno/models/techno/rays.mdl",
        23,
        1,
        0x9,
        &[66],
        &[192],
        true,
    ),
    (
        "projects/defaultprojects/techno/models/techno/sphere.mdl",
        14,
        1,
        0xB,
        &[417],
        &[2400],
        true,
    ),
];

/// Spec 09 §4.7 and F-3D-R1 on the installed models (`LUMENWALL_WE_ASSETS`; its parent folder
/// is the Wallpaper Engine installation).
#[test]
#[ignore = "needs LUMENWALL_WE_ASSETS"]
fn installed_static_models() {
    let Some(assets) = std::env::var_os("LUMENWALL_WE_ASSETS") else {
        eprintln!("LUMENWALL_WE_ASSETS not set");
        return;
    };
    let root = std::path::Path::new(&assets)
        .parent()
        .unwrap()
        .to_path_buf();
    for &(path, version, skins, vf, vertices, indices, terminal) in INSTALLED {
        let bytes = std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        let m = parse_static_model(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(m.warnings.is_empty(), "{path}: {:?}", m.warnings);
        assert!(m.sections.is_empty(), "{path}");
        assert_eq!((m.version, m.skins), (version, skins), "{path}");
        let got_v: Vec<usize> = m.meshes.iter().map(|x| x.vertex_count()).collect();
        let got_i: Vec<usize> = m.meshes.iter().map(|x| x.indices.len()).collect();
        assert_eq!(got_v, vertices, "{path}");
        assert_eq!(got_i, indices, "{path}");
        assert!(m.meshes.iter().all(|x| x.layout.format == vf), "{path}");
        // End of reading: size − 1 with a terminal 0x00, or the size itself for 0004.
        if terminal {
            assert_eq!(m.meshes_end, bytes.len() - 1, "{path}");
            assert_eq!(bytes[m.meshes_end], 0, "{path}");
        } else {
            assert_eq!(m.meshes_end, bytes.len(), "{path}");
        }
        // F-3D-R1: faces counter-clockwise around their normals for > 99 % of triangles.
        for mesh in &m.meshes {
            if mesh.layout.normal.is_none() {
                continue;
            }
            let (mut ok, mut total) = (0usize, 0usize);
            for t in mesh.indices.as_chunks::<3>().0 {
                let p = t.map(|i| mesh.position(usize::from(i)));
                let n = t.map(|i| mesh.normal(usize::from(i)).unwrap());
                let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
                let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
                let c = [
                    e1[1] * e2[2] - e1[2] * e2[1],
                    e1[2] * e2[0] - e1[0] * e2[2],
                    e1[0] * e2[1] - e1[1] * e2[0],
                ];
                let s: [f32; 3] = std::array::from_fn(|k| n[0][k] + n[1][k] + n[2][k]);
                total += 1;
                if c[0] * s[0] + c[1] * s[1] + c[2] * s[2] > 0.0 {
                    ok += 1;
                }
            }
            assert!(
                ok * 100 > total * 99,
                "{path}: {ok}/{total} counter-clockwise"
            );
        }
    }
    let read = |p: &str| parse_static_model(&std::fs::read(root.join(p)).unwrap()).unwrap();
    // F-3D-R1 (sphere): every normal of backgroundsphere points inwards.
    let sphere = read(INSTALLED[7].0);
    let mesh = &sphere.meshes[0];
    assert!((0..mesh.vertex_count()).all(|i| {
        let (p, n) = (mesh.position(i), mesh.normal(i).unwrap());
        p[0] * n[0] + p[1] * n[1] + p[2] * n[2] < 0.0
    }));
    // F-3D-R1 (UV): top-left origin on neonsun.
    let sun = read(INSTALLED[18].0);
    let mesh = &sun.meshes[0];
    let uv_of = |pos: [f32; 3]| {
        (0..mesh.vertex_count())
            .find(|&i| {
                let p = mesh.position(i);
                (p[0] - pos[0]).abs() < 1e-4 && (p[1] - pos[1]).abs() < 1e-4
            })
            .and_then(|i| mesh.uv(i))
    };
    assert_eq!(uv_of([-0.5, 0.5, 0.0]), Some([0.0, 0.0]));
    assert_eq!(uv_of([-0.5, -0.5, 0.0]), Some([0.0, 1.0]));
    // audiophile/grid: skin 1 is grid2.
    let grid = read(INSTALLED[6].0);
    assert!(grid.material(0, 1).unwrap().0.ends_with("grid2.json"));
}

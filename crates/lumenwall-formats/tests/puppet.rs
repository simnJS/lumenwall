//! Spec 08 §10: fixture F-P1 and its negative variants (structured puppet reader).

mod common;

use common::*;
use lumenwall_formats::mdl::{self, AnimationMode, MdlVersion, PuppetModel, VertexSkin};

fn patch(mut data: Vec<u8>, at: usize, bytes: &[u8]) -> Vec<u8> {
    data[at..at + bytes.len()].copy_from_slice(bytes);
    data
}

fn translation(m: &[f32; 16]) -> [f32; 3] {
    [m[12], m[13], m[14]]
}

fn magics(m: &PuppetModel) -> Vec<(String, usize, usize)> {
    m.sections
        .iter()
        .map(|s| (s.magic.clone(), s.start, s.end))
        .collect()
}

#[test]
fn fp1_reads_every_section() {
    let bytes = fp1_mdl();
    assert_eq!(bytes.len(), 782);
    let m = mdl::parse_model(&bytes).unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);

    // MDLV, structured: 3 vertices, 3 indices, vf at 0x038, F1 = 0x154.
    assert!(m.structured);
    assert_eq!(m.mesh.version, MdlVersion::V23);
    assert_eq!(m.mesh.block_offset, 0x38);
    assert_eq!(m.sections_start, Some(0x154));
    assert_eq!(
        m.mesh.positions,
        [[0.0, 0.0, 0.0], [20.0, 0.0, 0.0], [0.0, 20.0, 0.0]]
    );
    assert_eq!(m.mesh.uvs, [[0.0, 1.0], [1.0, 1.0], [0.0, 0.0]]);
    assert_eq!(m.mesh.indices, [0, 1, 2]);
    assert_eq!(m.skin.len(), 3);
    assert_eq!(
        m.skin[0],
        VertexSkin {
            normal: [0.0, 0.0, 1.0],
            tangent: [1.0, 0.0, 0.0, 1.0],
            joints: [0; 4],
            weights: [1.0, 0.0, 0.0, 0.0],
        }
    );
    assert_eq!(m.layout_positions, None);
    assert_eq!(m.part_ranges.len(), 1);
    assert_eq!(
        (
            m.part_ranges[0].bone,
            m.part_ranges[0].first,
            m.part_ranges[0].count
        ),
        (0, 0, 3)
    );
    // The spec 01 heuristic finds the same block.
    let heuristic = mdl::parse_puppet(&bytes).unwrap();
    assert_eq!(heuristic, m.mesh);

    // Chain: MDLS, MDAT, MDLA, then the terminal 00.
    assert_eq!(
        magics(&m),
        [
            ("MDLS0004".to_string(), 0x154, 0x215),
            ("MDAT0001".to_string(), 0x215, 0x26A),
            ("MDLA0006".to_string(), 0x26A, 0x30D),
        ]
    );

    // One root bone, rest T(10, 0, 0).
    let sk = m.skeleton.as_ref().unwrap();
    assert_eq!(sk.bones.len(), 1);
    assert_eq!(sk.bones[0].parent, None);
    assert_eq!(sk.bones[0].kind, 1);
    assert_eq!(translation(&sk.bones[0].local), [10.0, 0.0, 0.0]);
    assert_eq!(sk.bones[0].local[0], 1.0);
    let editor = sk.editor.as_ref().unwrap();
    assert_eq!(editor.boxes[0].half_extents, [10.0; 3]);
    assert_eq!(translation(&editor.boxes[0].transform), [0.0, 0.0, 10.0]);
    assert_eq!(
        (editor.draw_order.as_slice(), editor.sort_keys.as_slice()),
        (&[0][..], &[0][..])
    );

    // Attachment `att` on bone 0, T(0, 5, 0).
    assert_eq!(m.attachments.len(), 1);
    let att = m.attachment("att").unwrap();
    assert_eq!(att.bone, 0);
    assert_eq!(translation(&att.transform), [0.0, 5.0, 0.0]);
    assert!(m.attachment("Att").is_none(), "case-sensitive");

    // Animation 7 "A", loop, 1 fps, N = 1, D = 1 s, one track of two samples.
    let a = m.animation(7).unwrap();
    assert_eq!(a.name, "A");
    assert_eq!(a.mode, AnimationMode::Loop);
    assert_eq!(a.mode_name, "loop");
    assert_eq!((a.fps, a.frames), (1.0, 1));
    assert_eq!(a.duration(), 1.0);
    assert_eq!(a.tracks.len(), 1);
    assert_eq!(a.tracks[0].len(), 2);
    assert_eq!(a.tracks[0][0].position, [10.0, 0.0, 0.0]);
    assert_eq!(a.tracks[0][0].angles, [0.0; 3]);
    assert_eq!(a.tracks[0][1].angles[2], std::f32::consts::FRAC_PI_2);
    assert_eq!(a.tracks[0][1].scale, [1.0; 3]);
    assert_eq!(m.editor_pose, None);
}

#[test]
fn fp1_mode_variants() {
    for (mode, expected) in [
        ("mirror", AnimationMode::Mirror),
        ("single", AnimationMode::Single),
    ] {
        let bytes = fp1_with_mode(mode);
        assert_eq!(bytes.len(), 784);
        let m = mdl::parse_model(&bytes).unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert_eq!(m.sections.last().unwrap().end, 0x30F);
        let a = m.animation(7).unwrap();
        assert_eq!(a.mode, expected);
        assert_eq!(a.tracks[0].len(), 2);
    }
    let m = mdl::parse_model(&fp1_with_mode("pingpong")).unwrap();
    assert_eq!(m.animation(7).unwrap().mode, AnimationMode::Unknown);
}

#[test]
fn fp1_skeleton_end_out_of_file() {
    let m = mdl::parse_model(&patch(fp1_mdl(), 0x15D, &[0xFF, 0xFF, 0, 0])).unwrap();
    assert!(m.skeleton.is_none());
    assert!(m.sections.is_empty());
    assert!(m.attachments.is_empty() && m.animations.is_empty());
    assert_eq!(m.mesh.positions.len(), 3, "mesh drawn in its rest pose");
    assert_eq!(m.warnings.len(), 1);
    assert_eq!(m.warnings[0].offset, 0x15D);
}

#[test]
fn fp1_unknown_animation_version() {
    let m = mdl::parse_model(&patch(fp1_mdl(), 0x271, b"7")).unwrap();
    assert!(m.animations.is_empty());
    assert_eq!(m.skeleton.as_ref().unwrap().bones.len(), 1);
    assert_eq!(m.attachments.len(), 1);
    assert_eq!(m.warnings.len(), 1);
    assert_eq!(m.warnings[0].section, "MDLA0007");
}

#[test]
fn fp1_bone_parent_of_itself() {
    let reference = mdl::parse_model(&fp1_mdl()).unwrap();
    let m = mdl::parse_model(&patch(fp1_mdl(), 0x16A, &[0, 0, 0, 0])).unwrap();
    assert_eq!(m.warnings.len(), 1);
    assert!(m.warnings[0].message.contains("parent"));
    assert_eq!(m.skeleton, reference.skeleton);
    assert_eq!(m.attachments, reference.attachments);
    assert_eq!(m.animations, reference.animations);
}

#[test]
fn fp1_weight_on_missing_bone_rejects_skeleton() {
    let m = mdl::parse_model(&patch(fp1_mdl(), 0x068, &[1])).unwrap();
    assert_eq!(m.skin[0].joints, [1, 0, 0, 0]);
    assert!(m.skeleton.is_none());
    assert_eq!(m.bone_count(), 0);
    assert_eq!(m.warnings.len(), 1);
    // A zero weight on a missing bone is harmless.
    let ok = mdl::parse_model(&patch(fp1_mdl(), 0x06C, &[9])).unwrap();
    assert_eq!(ok.skin[0].joints, [0, 9, 0, 0]);
    assert!(ok.skeleton.is_some());
}

#[test]
fn fp1_dirty_animation_tail_keeps_the_animation() {
    let m = mdl::parse_model(&patch(fp1_mdl(), 0x2EA, &[1])).unwrap();
    assert_eq!(m.animations.len(), 1);
    assert_eq!(m.animations[0].id, 7);
    assert_eq!(m.warnings.len(), 1);
}

#[test]
fn fp1_animation_without_tracks() {
    let mut d = fp1_mdl();
    d[0x296..0x29A].copy_from_slice(&0u32.to_le_bytes());
    d.drain(0x29A..0x2EA);
    d[0x273..0x277].copy_from_slice(&0x2BDu32.to_le_bytes());
    assert_eq!(d[0x2BD], 0);
    let m = mdl::parse_model(&d).unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let a = m.animation(7).unwrap();
    assert!(a.tracks.is_empty());
}

#[test]
fn fp1_as_mdlv0021_uses_the_heuristic() {
    let reference = mdl::parse_model(&fp1_mdl()).unwrap();
    let m = mdl::parse_model(&patch(fp1_mdl(), 0x006, b"21")).unwrap();
    assert!(!m.structured);
    assert_eq!(m.mesh.version, MdlVersion::V21);
    assert_eq!(m.mesh.positions, reference.mesh.positions);
    assert_eq!(m.mesh.indices, reference.mesh.indices);
    assert_eq!(m.mesh.block_offset, 0x38);
    assert_eq!(m.skin, reference.skin);
    assert_eq!(m.sections_start, Some(0x154));
    assert_eq!(m.skeleton, reference.skeleton);
    assert_eq!(m.attachments, reference.attachments);
    assert_eq!(m.animations, reference.animations);
}

#[test]
fn mesh_errors_match_the_heuristic_reader() {
    // An index past the vertices invalidates the mesh (spec 08 §2.5).
    let err = mdl::parse_model(&patch(fp1_mdl(), 0x138, &[3, 0])).unwrap_err();
    assert_eq!(
        err,
        mdl::MdlError::InvalidIndex {
            index: 3,
            vertex_count: 3
        }
    );
    assert!(mdl::parse_model(b"MDLV0014\0").is_err());
    // F9 (spec 01): heuristic mesh, then an `MDLS0001` magic with no end: no skeleton.
    let m = mdl::parse_model(&f9_mdl()).unwrap();
    assert_eq!(m.mesh, mdl::parse_puppet(&f9_mdl()).unwrap());
    assert!(m.skeleton.is_none());
}

#[test]
fn other_vertex_formats_fall_back_to_the_heuristic() {
    // vf = 0x0180000B: not the puppet layout, so the structured read is not applicable.
    let m = mdl::parse_model(&patch(fp1_mdl(), 0x038, &[0x0B])).unwrap();
    assert!(!m.structured);
    assert_eq!(m.mesh.block_offset, 0x38);
    assert_eq!(m.sections_start, Some(0x154));
    assert!(m.skeleton.is_some());
}

/// `(file, vertices, indices, bones, attachments (name, bone), animations (id, fps))`.
type Expected = (
    &'static str,
    usize,
    usize,
    usize,
    &'static [(&'static str, usize)],
    &'static [(u32, f32)],
);

/// F-P2 (spec 08 §10): the puppets of 3463520581, `LUMENWALL_WE_WORKSHOP` (docs/TESTING.md).
#[test]
#[ignore = "needs LUMENWALL_WE_WORKSHOP"]
fn real_puppets_of_3463520581() {
    use lumenwall_formats::pkg::PkgArchive;
    let Some(dir) = std::env::var_os("LUMENWALL_WE_WORKSHOP") else {
        eprintln!("LUMENWALL_WE_WORKSHOP not set");
        return;
    };
    let pkg = PkgArchive::open(std::path::Path::new(&dir).join("3463520581/scene.pkg")).unwrap();
    // (file, vertices, indices, bones, attachments, animations (id, fps))
    let expected: &[Expected] = &[
        (
            "puppet",
            163,
            882,
            1,
            &[("Attachment", 0), ("hair back", 0)],
            &[(1953, 10.0)],
        ),
        (
            "puppet - Copy",
            163,
            882,
            1,
            &[("Attachment", 0)],
            &[(1949, 12.0)],
        ),
        (
            "asuna body bottom",
            872,
            4503,
            7,
            &[("Attachment bottom", 2)],
            &[],
        ),
        (
            "asuna body",
            985,
            5481,
            15,
            &[("head", 2)],
            &[(264, 20.0), (2385, 15.0)],
        ),
        (
            "hair back big chunk",
            227,
            1197,
            2,
            &[("hair back", 1)],
            &[],
        ),
        (
            "main hair back c2",
            266,
            1455,
            2,
            &[("hair back 2", 1)],
            &[],
        ),
        ("kirito arm", 317, 1650, 4, &[], &[(3330, 7.5)]),
        ("kirito face", 487, 2541, 7, &[], &[(374, 15.0)]),
    ];
    for &(file, nv, ni, nb, atts, anims) in expected {
        let name = format!("models/{file}_puppet.mdl");
        let bytes = pkg.read(&name).unwrap().unwrap();
        let m = mdl::parse_model(bytes).unwrap();
        assert!(m.warnings.is_empty(), "{file}: {:?}", m.warnings);
        assert!(m.structured, "{file}");
        assert_eq!(
            m.mesh,
            mdl::parse_puppet(bytes).unwrap(),
            "{file}: same mesh both ways"
        );
        assert_eq!(
            (m.mesh.positions.len(), m.mesh.indices.len()),
            (nv, ni),
            "{file}"
        );
        let sk = m.skeleton.as_ref().unwrap();
        assert_eq!(sk.bones.len(), nb, "{file}");
        let got: Vec<(&str, usize)> = m
            .attachments
            .iter()
            .map(|a| (a.name.as_str(), a.bone))
            .collect();
        assert_eq!(got, atts, "{file}");
        let got: Vec<(u32, f32)> = m.animations.iter().map(|a| (a.id, a.fps)).collect();
        assert_eq!(got, anims, "{file}");
        for a in &m.animations {
            assert_eq!((a.mode, a.frames), (AnimationMode::Loop, 60), "{file}");
            assert_eq!(a.tracks.len(), nb, "{file}");
            for (b, t) in a.tracks.iter().enumerate() {
                assert_eq!(t.len(), 61, "{file}");
                // Weights sum to 1; non-animated bones start at their rest translation.
                if t.iter().all(|s| *s == t[0]) {
                    let rest = &sk.bones[b].local;
                    assert_eq!(
                        t[0].position,
                        [rest[12], rest[13], rest[14]],
                        "{file} bone {b}"
                    );
                }
            }
        }
        for s in &m.skin {
            let sum: f32 = s.weights.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "{file}: weight sum {sum}");
        }
    }
}

/// F-P2 negatives: the static models of the installation have no chained section
/// (`LUMENWALL_WE_ASSETS`, its parent folder holds `projects/defaultprojects`).
#[test]
#[ignore = "needs LUMENWALL_WE_ASSETS"]
fn static_models_have_no_skeleton() {
    let Some(assets) = std::env::var_os("LUMENWALL_WE_ASSETS") else {
        eprintln!("LUMENWALL_WE_ASSETS not set");
        return;
    };
    let root = std::path::Path::new(&assets)
        .parent()
        .unwrap()
        .to_path_buf();
    let mut stack = vec![root];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "mdl") {
                let bytes = std::fs::read(&path).unwrap();
                let model = mdl::parse_model(&bytes);
                assert_eq!(
                    model.as_ref().map(|m| &m.mesh).ok(),
                    mdl::parse_puppet(&bytes).as_ref().ok(),
                    "{}",
                    path.display()
                );
                if let Ok(m) = model {
                    assert!(m.skeleton.is_none(), "{}", path.display());
                    assert!(m.sections.is_empty(), "{}", path.display());
                    assert!(
                        m.warnings.is_empty(),
                        "{}: {:?}",
                        path.display(),
                        m.warnings
                    );
                }
                seen += 1;
            }
        }
    }
    assert_eq!(seen, 28);
}

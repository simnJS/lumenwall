//! Light fuzzing: corrupted and random inputs must produce errors, never panics,
//! overflows or huge allocations.

mod common;

use common::*;
use lumen_formats::mdl;
use lumen_formats::pkg::PkgArchive;
use lumen_formats::tex::{self, ParseOptions, UploadOptions};
use proptest::prelude::*;

const LIMIT: u64 = 1 << 20;

fn exercise_tex(data: &[u8], extended: bool) {
    let parse_options = ParseOptions {
        max_level_bytes: LIMIT,
    };
    let Ok(mut parsed) = tex::parse_with(data, &parse_options) else {
        return;
    };
    let _ = parsed.resolution();
    let _ = parsed.uv_extent();
    let _ = parsed.texture_size(3);
    parsed.apply_tex_json(br#"{"spritesheetsequences":[{"frames":3,"width":0.5,"height":2}]}"#);
    let upload_options = UploadOptions {
        extended_formats: extended,
        max_level_bytes: LIMIT,
    };
    if let Ok(mut prepared) = parsed.into_upload(&upload_options) {
        for t in [0.0, 0.3, 1.7, 1e9, f64::NAN, -2.0] {
            let _ = prepared.frame_state(t);
        }
        let _ = prepared.uv_extent();
        let _ = prepared.requires_cpu_decode(false);
        let _ = prepared.decode_compressed(LIMIT);
    }
}

fn exercise_pkg(data: Vec<u8>) {
    if let Ok(pkg) = PkgArchive::from_vec(data) {
        for entry in pkg.entries() {
            let found = pkg.get_bytes(entry.name).expect("every name resolves");
            assert!(found.index <= entry.index, "first occurrence wins");
            let _ = entry.data.len();
        }
        let _ = pkg.get("project.json");
    }
}

fn mutate(mut data: Vec<u8>, edits: &[(usize, u8)], cut: Option<usize>) -> Vec<u8> {
    for &(pos, byte) in edits {
        if !data.is_empty() {
            let i = pos % data.len();
            data[i] = byte;
        }
    }
    if let Some(cut) = cut {
        let len = data.len();
        data.truncate(cut % (len + 1));
    }
    data
}

fn fixture(index: usize) -> Vec<u8> {
    match index % 6 {
        0 => f3_tex(),
        1 => f4_tex(),
        2 => f5_tex(),
        3 => f6_tex(),
        4 => f7_tex(),
        _ => f8_tex(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn random_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        exercise_tex(&data, true);
        exercise_pkg(data.clone());
        let _ = mdl::parse_puppet(&data);
        let mut prefixed = b"TEXV0005\0TEXI0001\0".to_vec();
        prefixed.extend_from_slice(&data);
        exercise_tex(&prefixed, false);
    }

    #[test]
    fn mutated_textures_never_panic(
        which in 0usize..6,
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
        cut in proptest::option::of(any::<usize>()),
        extended in any::<bool>(),
    ) {
        let data = mutate(fixture(which), &edits, cut);
        exercise_tex(&data, extended);
    }

    #[test]
    fn mutated_archives_never_panic(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..6),
        cut in proptest::option::of(any::<usize>()),
        second in any::<bool>(),
    ) {
        let base = if second { f2_pkg() } else { f1_pkg() };
        exercise_pkg(mutate(base, &edits, cut));
    }

    #[test]
    fn mutated_puppets_never_panic(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..6),
        cut in proptest::option::of(any::<usize>()),
    ) {
        let data = mutate(f9_mdl(), &edits, cut);
        if let Ok(mesh) = mdl::parse_puppet(&data) {
            prop_assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.positions.len()));
        }
    }

    #[test]
    fn tex_json_never_panics(json in proptest::collection::vec(any::<u8>(), 0..128)) {
        let bytes = f7_tex();
        let mut parsed = tex::parse(&bytes).unwrap();
        let _ = parsed.apply_tex_json(&json);
    }
}

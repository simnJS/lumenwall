//! Blending contract between `lumen-scene` and `lumen-render` (spec 04 §9.8, spec 02 §12.6):
//! when an image has more than one pass, the last pass takes the first pass's blending and the
//! first pass becomes `normal`. This transfer must happen **exactly once**: `lumen-scene` hands
//! over each material's own blending (`PassDesc::blending`) and `lumen-render`'s chain planner
//! applies the transfer to the chain it draws. The engine feeds one into the other unchanged.
//!
//! The model material is `translucent` and the effect material `additive`, so the three
//! possible outcomes differ: transfer applied once → last pass `translucent`; never → `additive`;
//! twice (by both crates) → `normal`.

use std::collections::HashMap;

use lumen_render::BlendMode;
use lumen_render::chain::{ChainOptions, ChainPassInput, plan_image_chain};
use lumen_scene::{Blending, BuildOptions, ImagePasses, PropertyOverrides, load_project};

fn files() -> HashMap<String, Vec<u8>> {
    [
        (
            "project.json",
            r#"{ "type": "scene", "title": "blend contract", "file": "scene.json" }"#,
        ),
        (
            "scene.json",
            r#"{
              "camera": { "center": "0 0 -1", "eye": "0 0 0", "up": "0 1 0" },
              "general": { "orthogonalprojection": { "width": 1920, "height": 1080 } },
              "objects": [
                { "id": 1, "name": "with effect", "image": "models/a.json",
                  "effects": [ { "file": "effects/e/effect.json" } ] },
                { "id": 2, "name": "alone", "image": "models/a.json" }
              ]
            }"#,
        ),
        ("models/a.json", r#"{ "material": "materials/a.json" }"#),
        (
            "materials/a.json",
            r#"{ "passes": [ { "shader": "genericimage2", "blending": "translucent", "textures": [ "a" ] } ] }"#,
        ),
        (
            "effects/e/effect.json",
            r#"{ "passes": [ { "material": "materials/e.json" } ] }"#,
        ),
        (
            "materials/e.json",
            r#"{ "passes": [ { "shader": "e", "blending": "additive" } ] }"#,
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.as_bytes().to_vec()))
    .collect()
}

/// The engine's mapping (`scene.rs`): `ImagePass::blend` = the scene pass blending as is.
fn blend(b: Blending) -> BlendMode {
    match b {
        Blending::Normal => BlendMode::Normal,
        Blending::Translucent => BlendMode::Translucent,
        Blending::Additive => BlendMode::Additive,
    }
}

fn planned_blends(passes: &ImagePasses) -> Vec<BlendMode> {
    let inputs: Vec<ChainPassInput<u32>> = passes
        .passes
        .iter()
        .map(|d| ChainPassInput {
            target: None,
            blend: blend(d.blending),
        })
        .collect();
    plan_image_chain(&inputs, ChainOptions::default())
        .iter()
        .map(|p| p.blend)
        .collect()
}

#[test]
fn blending_transfer_is_applied_exactly_once() {
    let f = files();
    let mut project = load_project(&f, PropertyOverrides::new()).expect("project loads");
    let build = project
        .build(&f, &BuildOptions::new(1920, 1080))
        .expect("scene builds");

    let with_effect = build.image_passes[0].as_ref().expect("image 1 is set up");
    assert_eq!(with_effect.passes.len(), 2);
    let blends = planned_blends(with_effect);
    assert_eq!(blends[0], BlendMode::Normal, "first pass becomes normal");
    assert_eq!(
        blends[1],
        BlendMode::Translucent,
        "last pass takes the model's blending once (additive = never applied, normal = applied twice)"
    );

    // A single pass keeps its own blending.
    let alone = build.image_passes[1].as_ref().expect("image 2 is set up");
    assert_eq!(planned_blends(alone), [BlendMode::Translucent]);
}

//! A load reads each referenced file once, while every reference still gets its own copy of
//! the model, material or effect (spec 02 §8–§10) and its own warnings.

mod common;

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

use common::files;
use lumenwall_scene::*;

const PROJECT: &str = r#"{ "type": "scene", "title": "t", "file": "scene.json" }"#;
const CAMERA: &str = r#""camera": { "center": "0 0 -1", "eye": "0 0 0", "up": "0 1 0" }"#;
const MODEL: &str = r#"{ "material": "materials/fond.json" }"#;
const PASS: &str = r#"{ "passes": [ { "shader": "genericimage2", "textures": [ "fond" ] } ] }"#;
const EFFECT: &str = r#"{
  "name": "Flou",
  "passes": [
    { "material": "materials/effects/blur_h.json" },
    { "material": "materials/effects/blur_v.json" }
  ]
}"#;

/// In-memory files that count the reads of each path.
struct Counting {
    files: HashMap<String, Vec<u8>>,
    reads: RefCell<HashMap<String, usize>>,
}

impl AssetSource for Counting {
    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>, AssetError> {
        *self.reads.borrow_mut().entry(path.to_owned()).or_default() += 1;
        self.files.read(path)
    }
}

fn image(id: u32, effects: &str) -> String {
    format!(
        r#"{{ "id": {id}, "name": "i{id}", "image": "models/fond.json", "effects": [ {effects} ] }}"#
    )
}

#[test]
fn shared_files_are_read_once_and_copied_per_reference() {
    let blur = r#"{ "file": "effects/blur/effect.json" }"#;
    let missing = r#"{ "file": "effects/none/effect.json" }"#;
    let objects = [
        image(1, blur),
        image(2, &format!("{blur}, {missing}")),
        image(3, &format!("{blur}, {missing}")),
    ]
    .join(", ");
    let scene = format!(
        r#"{{ {CAMERA}, "general": {{ "orthogonalprojection": {{ "width": 64, "height": 64 }} }}, "objects": [ {objects} ] }}"#
    );
    let src = Counting {
        files: files(&[
            ("project.json", PROJECT),
            ("scene.json", &scene),
            ("models/fond.json", MODEL),
            ("materials/fond.json", PASS),
            ("effects/blur/effect.json", EFFECT),
            ("materials/effects/blur_h.json", PASS),
            ("materials/effects/blur_v.json", PASS),
        ]),
        reads: RefCell::default(),
    };
    let p = load_project(&src, PropertyOverrides::new()).expect("loads");
    let reads = src.reads.borrow();
    for path in [
        "scene.json",
        "models/fond.json",
        "materials/fond.json",
        "effects/blur/effect.json",
        "materials/effects/blur_h.json",
        "materials/effects/blur_v.json",
        "effects/none/effect.json",
    ] {
        assert_eq!(reads.get(path), Some(&1), "{path}");
    }
    let s = p.scene.as_ref().unwrap();
    // A fresh copy per reference: 3 models, 3 model materials + 3 × 2 effect materials,
    // 3 effects.
    assert_eq!(s.models().len(), 3);
    assert_eq!(s.materials().len(), 9);
    assert_eq!(s.effects().len(), 3);
    // The missing effect is reported for each of its two references.
    let count = |what: &str| p.warnings().iter().filter(|w| w.message == what).count();
    assert_eq!(
        count("referenced file `effects/none/effect.json` not found"),
        2
    );
    assert_eq!(count("effect `effects/none/effect.json` skipped"), 2);
}

//! Spec 02 examples and edge cases of its open questions.

mod common;

use std::collections::HashMap;

use common::files;
use glam::{UVec2, Vec2, Vec3, Vec4};
use lumenwall_scene::*;

type Files = HashMap<String, Vec<u8>>;

const PROJECT_24: &str = r#"{
  "type": "Scene",
  "title": "Mon fond",
  "file": "scene.json",
  "workshopid": "123456789",
  "general": {
    "supportsaudioprocessing": true,
    "properties": {
      "schemecolor": { "type": "color", "text": "ui_browse_properties_scheme_color", "value": "0.2 0.4 0.8" },
      "showclock":   { "type": "bool", "text": "Horloge", "value": true },
      "speed":       { "type": "slider", "text": "Vitesse", "value": 1.5, "min": 0, "max": 10, "step": 0.1 },
      "mode":        { "type": "combo", "text": "Mode", "value": 1,
                       "options": [ { "label": "Calme", "value": 0 }, { "label": "Agité", "value": 1 } ] },
      "titre":       { "type": "group" }
    }
  }
}"#;

const CAMERA: &str = r#""camera": { "center": "0.00000 0.00000 -1.00000", "eye": "0.00000 0.00000 0.00000", "up": "0.00000 1.00000 0.00000" }"#;

const MODEL: &str =
    r#"{ "material": "materials/fond.json", "fullscreen": false, "passthrough": false }"#;

const MATERIAL_9: &str = r#"{
  "passes": [{
    "shader": "genericimage2",
    "blending": "translucent", "cullmode": "nocull",
    "depthtest": "disabled", "depthwrite": "disabled",
    "textures": [ "fond", null, "masks/m1" ],
    "combos": { "TRANSPARENCY": 1 },
    "constantshadervalues": { "alpha": 1.0, "color": "1 1 1" }
  }]
}"#;

const EFFECT_10: &str = r#"{
  "name": "Flou", "group": "blur", "dependencies": [ "shaders/effects/blur.frag" ],
  "fbos": [ { "name": "_rt_HalfCompoBuffer1", "scale": 2, "format": "rgba8888" } ],
  "passes": [
    { "material": "materials/effects/blur_h.json", "target": "_rt_HalfCompoBuffer1" },
    { "material": "materials/effects/blur_v.json",
      "bind": [ { "name": "_rt_HalfCompoBuffer1", "index": 0 } ] },
    { "command": "copy", "source": "previous", "target": "_rt_copie" }
  ]
}"#;

const SIMPLE_PASS: &str = r#"{ "passes": [ { "shader": "s" } ] }"#;

fn scene_with(general: &str, objects: &str) -> String {
    format!(r#"{{ {CAMERA}, "general": {{ {general} }}, "objects": [ {objects} ] }}"#)
}

fn ortho() -> &'static str {
    r#""orthogonalprojection": { "width": 1920, "height": 1080 }"#
}

fn base_files(scene: &str) -> Files {
    files(&[
        ("project.json", PROJECT_24),
        ("scene.json", scene),
        ("models/fond.json", MODEL),
        ("materials/fond.json", MATERIAL_9),
        ("effects/blur/effect.json", EFFECT_10),
        ("materials/effects/blur_h.json", SIMPLE_PASS),
        (
            "materials/effects/blur_v.json",
            r#"{ "passes": [ { "shader": "v1" }, { "shader": "v2" } ] }"#,
        ),
    ])
}

fn load(f: &Files) -> Project {
    load_project(f, PropertyOverrides::new()).expect("loads")
}

fn load_with(f: &Files, overrides: &[&str]) -> Project {
    load_project(f, overrides.iter().copied().collect()).expect("loads")
}

fn load_err(f: &Files) -> LoadError {
    load_project(f, PropertyOverrides::new()).expect_err("must fail")
}

fn data_err(e: &LoadError) -> (String, String, DataError) {
    match e {
        LoadError::Data { file, path, error } => (file.clone(), path.to_string(), error.clone()),
        other => panic!("expected a data error, got {other}"),
    }
}

fn image(p: &Project, i: usize) -> &ImageObject {
    p.scene.as_ref().unwrap().objects[i]
        .as_image()
        .expect("image")
}

fn obj(p: &Project, i: usize) -> &Object {
    &p.scene.as_ref().unwrap().objects[i]
}

fn img_obj(extra: &str) -> String {
    format!(r#"{{ "id": 1, "name": "a", "image": "models/fond.json" {extra} }}"#)
}

// ---------------------------------------------------------------------------------------------
// §2, §3: project and properties

#[test]
fn project_example_2_4() {
    let f = base_files(&scene_with(ortho(), ""));
    let p = load(&f);
    assert_eq!(p.kind, ProjectKind::Scene);
    assert_eq!(p.title, "Mon fond");
    assert_eq!(p.workshop_id, "123456789");
    assert!(p.supports_audio_processing);
    let names: Vec<&str> = p.properties.iter().map(|(_, p)| p.name()).collect();
    assert_eq!(names, ["mode", "schemecolor", "showclock", "speed"]); // sorted, group ignored
    let mode = p.properties.get(p.property("mode").unwrap());
    assert_eq!(mode.value().tag(), ValueTag::Text);
    assert_eq!(mode.value().text(), "1");
    let scheme = p.properties.get(p.property("schemecolor").unwrap());
    assert_eq!(scheme.value().vec4(), Vec4::new(0.2, 0.4, 0.8, 1.0));
    let speed = p.properties.get(p.property("speed").unwrap());
    assert_eq!(
        *speed.kind(),
        PropertyKind::Slider {
            min: 0.0,
            max: 10.0,
            step: 0.1
        }
    );
    assert_eq!(speed.value().float(), 1.5);
}

#[test]
fn property_listing_15_1() {
    let p = load(&base_files(&scene_with(ortho(), "")));
    let expected = "mode - combo\n\tText: Mode\n\tValue: 1\nValues: \n\t\t0 = Calme\n\t\t1 = Agité\n\
schemecolor - color\n\tText: ui_browse_properties_scheme_color\n\tValue: 0.200000, 0.400000, 0.800000, 1.000000\n\
showclock - boolean\n\tText: Horloge\n\tValue: 1\n\
speed - slider\n\tText: Vitesse\n\tMin: 0\n\tMax: 10\n\tStep: 0.1\n\tValue: 1.500000\n";
    assert_eq!(p.list_properties(), expected);
}

#[test]
fn all_property_kinds() {
    let project = r#"{ "type": "web", "title": "t", "file": "index.html", "general": { "properties": {
        "c": { "type": "color", "value": "255 128 0" },
        "b": { "type": "bool" },
        "s": { "type": "slider", "value": true },
        "k": { "type": "combo", "value": "x", "options": [ {"label": "A", "value": "y"}, 5, {"label": "B", "value": 2.9}, {"label": "dup", "value": "y"} ] },
        "t": { "type": "text", "text": "Label" },
        "st": { "type": "scenetexture", "value": "tex" },
        "f": { "type": "file" },
        "d": { "type": "directory", "value": "/x" },
        "ti": { "type": "textinput", "value": "abc" },
        "to": { "type": "textinput", "value": {"k": 1} },
        "us": { "type": "usershortcut", "value": 3 },
        "z": { "type": "mystery" },
        "n": 4,
        "g": { "text": "no type" }
    } } }"#;
    let f = files(&[("project.json", project)]);
    let p = load(&f);
    let v = |n: &str| p.properties.get(p.property(n).unwrap()).value().clone();
    assert!((v("c").vec4().y - 128.0 / 255.0).abs() < 1e-6);
    assert!(!v("b").boolean());
    assert_eq!((v("s").tag(), v("s").float()), (ValueTag::Float, 1.0));
    // "x" is not an option: Null with a warning.
    assert_eq!(v("k").tag(), ValueTag::Null);
    let PropertyKind::Combo { options } = p.properties.get(p.property("k").unwrap()).kind().clone()
    else {
        panic!()
    };
    assert_eq!(
        options
            .iter()
            .map(|o| (&*o.value, &*o.label))
            .collect::<Vec<_>>(),
        [("2", "B"), ("y", "A")]
    );
    assert_eq!(v("t").tag(), ValueTag::Null);
    assert_eq!(v("st").text(), "tex");
    assert_eq!((v("f").tag(), v("f").text()), (ValueTag::Text, ""));
    assert_eq!(v("d").text(), "/x");
    // Q9: a string is stored raw, other JSON compact.
    assert_eq!(v("ti").text(), "abc");
    assert_eq!(v("to").text(), r#"{"k":1}"#);
    assert_eq!(v("us").text(), "3");
    assert!(p.property("z").is_none() && p.property("n").is_none() && p.property("g").is_none());
    assert!(p.warnings().iter().any(|w| w.message.contains("mystery")));
    assert!(
        p.warnings()
            .iter()
            .any(|w| w.message.contains("not one of its options"))
    );
    let listing = p.list_properties();
    assert!(listing.contains("t - text\n\tText: Label\n\tValue: Label\n"));
    assert!(listing.contains("ti - textinput\n\tText: \n\tValue: abc\n")); // Q10
    assert!(listing.contains("k - combo\n\tText: \n\tValue: unknown conversion"));
}

#[test]
fn property_type_errors_are_typed() {
    let bad = |props: &str| {
        let project = format!(
            r#"{{ "type": "web", "title": "t", "file": "i", "general": {{ "properties": {props} }} }}"#
        );
        data_err(&load_err(&files(&[("project.json", &project)])))
    };
    // Q12: `"value": 1` on a bool is a typed error, not a crash.
    let (file, path, err) = bad(r#"{ "b": { "type": "bool", "value": 1 } }"#);
    assert_eq!(file, "project.json");
    assert_eq!(path, "$.general.properties.b.value");
    assert_eq!(
        err,
        DataError::Type {
            expected: Expected::Bool,
            found: lumenwall_scene::raw::JsonKind::Number
        }
    );
    let (_, path, err) = bad(r##"{ "c": { "type": "color", "value": "#12345" } }"##);
    assert_eq!(path, "$.general.properties.c.value");
    assert!(matches!(err, DataError::Color { .. }));
    let (_, path, _) = bad(r#"{ "k": { "type": "combo", "value": 1, "options": {} } }"#);
    assert_eq!(path, "$.general.properties.k.options");
    let (_, path, _) = bad(
        r#"{ "k": { "type": "combo", "value": 1, "options": [ { "label": "a", "value": true } ] } }"#,
    );
    assert_eq!(path, "$.general.properties.k.options[0].value");
    let (_, path, err) = bad(r#"{ "s": { "type": "slider" } }"#);
    assert_eq!(
        (path.as_str(), err),
        ("$.general.properties.s.value", DataError::Missing)
    );
}

#[test]
fn project_header_rules() {
    let p = |s: &str| load_project(&files(&[("project.json", s)]), PropertyOverrides::new());
    assert_eq!(
        p(r#"{"type": "VIDEO", "title": "t", "file": "v.mp4"}"#)
            .unwrap()
            .kind,
        ProjectKind::Video
    );
    // Q7: numeric ids keep all their digits.
    assert_eq!(
        p(r#"{"type": "web", "title": "t", "file": "i", "workshopid": 3018340632}"#)
            .unwrap()
            .workshop_id,
        "3018340632"
    );
    let e = p(r#"{"type": "gif", "title": "t", "file": "x"}"#).unwrap_err();
    assert_eq!(data_err(&e).2, DataError::ProjectType("gif".into()));
    let e = p(r#"{"type": "web", "title": 3, "file": "x"}"#).unwrap_err();
    assert_eq!(data_err(&e).1, "$.title");
    let e = p(r#"{"type": "web", "title": "t", "file": "x",}"#).unwrap_err();
    assert!(matches!(e, LoadError::Syntax { line: 1, .. }), "{e}");
    assert!(matches!(
        load_project(&Files::new(), PropertyOverrides::new()),
        Err(LoadError::Asset { .. })
    ));
    // A wrong workshopid type is only a warning; the synthetic id is negative.
    let w = p(r#"{"type": "web", "title": "t", "file": "x", "workshopid": [1]}"#).unwrap();
    assert!(w.workshop_id.starts_with('-'));
    // BOM tolerated.
    assert!(p("\u{feff}{\"type\": \"web\", \"title\": \"t\", \"file\": \"x\"}").is_ok());
}

#[test]
fn synthetic_ids_count_every_project() {
    let f = files(&[(
        "project.json",
        r#"{"type": "web", "title": "t", "file": "x", "workshopid": "7"}"#,
    )]);
    let g = files(&[(
        "project.json",
        r#"{"type": "web", "title": "t", "file": "x"}"#,
    )]);
    let mut cx = LoadContext::default();
    assert_eq!(cx.load(&f).unwrap().workshop_id, "7");
    assert_eq!(cx.load(&g).unwrap().workshop_id, "-2");
    assert_eq!(cx.load(&g).unwrap().workshop_id, "-3");
}

#[test]
fn overrides_3_3_and_3_5() {
    let mut o = PropertyOverrides::new();
    o.push_arg("a=b=c");
    o.push_arg("flag");
    o.push_arg("a=last");
    assert_eq!(o.get("a"), Some("last"));
    assert_eq!(o.get("flag"), Some("1"));

    let objects = img_obj(
        r#", "visible": { "value": true, "user": { "name": "mode", "condition": "1" } }, "alpha": { "value": 0.5, "user": "speed" }"#,
    );
    let f = base_files(&scene_with(ortho(), &objects));
    let p = load_with(
        &f,
        &[
            "mode=0",
            "speed=0.25abc",
            "showclock=TRUE",
            "schemecolor=1 0 0",
            "unknown=3",
        ],
    );
    let img = image(&p, 0);
    assert!(!p.value(img.visible).boolean());
    assert_eq!(p.value(img.alpha).float(), 0.25);
    assert!(
        !p.properties
            .get(p.property("showclock").unwrap())
            .value()
            .boolean()
    );
    assert_eq!(
        p.properties
            .get(p.property("schemecolor").unwrap())
            .value()
            .vec4(),
        Vec4::new(1.0 / 255.0, 0.0, 0.0, 1.0)
    );

    // Q13: invalid slider / colour / combo texts are refused with a warning.
    let p = load_with(&f, &["speed=abc", "schemecolor=1 2", "mode=7"]);
    assert_eq!(
        p.properties
            .get(p.property("speed").unwrap())
            .value()
            .float(),
        1.5
    );
    assert_eq!(
        p.properties.get(p.property("mode").unwrap()).value().text(),
        "1"
    );
    assert_eq!(
        p.warnings()
            .iter()
            .filter(|w| w.message.contains("override ignored"))
            .count(),
        3
    );
}

#[test]
fn runtime_property_update_touches_only_subscribers() {
    let objects = format!(
        "{}, {}",
        img_obj(r#", "visible": { "value": true, "user": { "name": "mode", "condition": "1" } }"#),
        r#"{ "id": 2, "name": "b", "image": "models/fond.json", "alpha": { "value": 1, "user": "mode" } }"#
    );
    let mut p = load(&base_files(&scene_with(ortho(), &objects)));
    let vis = image(&p, 0).visible;
    let alpha = image(&p, 1).alpha;
    // Q8: a numeric field bound to a combo without condition sees 0.
    assert_eq!(
        (p.value(alpha).tag(), p.value(alpha).float()),
        (ValueTag::Text, 0.0)
    );
    assert!(p.value(vis).boolean());
    let updated: Vec<SettingId> = p
        .set_property_text("mode", "0")
        .unwrap()
        .iter()
        .map(|s| s.setting)
        .collect();
    // The group `visible` of object 0 is a distinct setting bound to the same property (§7.2).
    assert_eq!(updated, [obj(&p, 0).visible, vis, alpha]);
    assert!(!p.value(vis).boolean());
    // A script write is overwritten by the next property update (§4.4-3).
    p.settings.set(vis, Value::from_bool(true));
    p.set_property_text("mode", "0").unwrap();
    assert!(!p.value(vis).boolean());
    assert_eq!(
        p.set_property_text("nope", "1").unwrap_err(),
        PropertyUpdateError::Unknown("nope".into())
    );
    // Overrides applied after load propagate too.
    let mut o = PropertyOverrides::new();
    o.push_arg("mode=1");
    p.apply_overrides(&o);
    assert!(p.value(vis).boolean());
}

// ---------------------------------------------------------------------------------------------
// §5: settings

#[test]
fn setting_forms_5_1() {
    let objects = img_obj(
        r#", "alpha": 0.5, "origin": "960 540 0", "visible": { "value": true },
        "color": { "value": "1 0 0", "user": "schemecolor" },
        "brightness": { "value": "00:00", "script": "export function update(v) { return v; }",
                        "scriptproperties": { "format": { "value": 24, "user": "fmt" }, "a": null } }"#,
    );
    let p = load(&base_files(&scene_with(ortho(), &objects)));
    let img = image(&p, 0);
    assert_eq!(p.value(img.alpha).float(), 0.5);
    assert_eq!(
        p.value(obj(&p, 0).origin).vec3(),
        Vec3::new(960.0, 540.0, 0.0)
    );
    assert!(p.value(img.visible).boolean());
    // Bound colour: replaced by the property value.
    assert_eq!(p.value(img.color).vec4(), Vec4::new(0.2, 0.4, 0.8, 1.0));
    assert!(p.settings.meta(img.color).property.is_some());
    let meta = p.settings.meta(img.brightness);
    let script = p.settings.script(meta.script.expect("script"));
    assert!(script.source.starts_with("export function update"));
    let props: Vec<(&str, ValueTag)> = script
        .properties
        .iter()
        .map(|(n, id)| (p.strings.resolve(*n), p.value(*id).tag()))
        .collect();
    // Key order; `fmt` is not a property so the value stays; null gives Null.
    assert_eq!(props, [("a", ValueTag::Null), ("format", ValueTag::Int)]);
    assert_eq!(p.value(img.brightness).text(), "00:00");
    assert_eq!(p.settings.scripted(), [img.brightness]);
}

#[test]
fn setting_value_analysis_5_2() {
    let constants = r#"{ "f": "0.5", "t": "5px", "i": "inf", "e": "", "v2": "1 2", "v3": "1 2 3", "v4": "1 2 3 4 5",
        "n": 3, "x": 3.0, "b": true, "nu": null, "a": [1], "o": { "value": { "k": 1 } }, "w": "\t5", "big": 4294967297 }"#;
    let material =
        format!(r#"{{ "passes": [ {{ "shader": "s", "constantshadervalues": {constants} }} ] }}"#);
    let mut f = base_files(&scene_with(ortho(), &img_obj("")));
    f.insert("materials/fond.json".into(), material.into_bytes());
    let p = load(&f);
    let scene = p.scene.as_ref().unwrap();
    let mat = scene.material(scene.model(image(&p, 0).model.unwrap()).material);
    let get = |n: &str| {
        let c = mat.passes[0]
            .constants
            .iter()
            .find(|c| p.strings.resolve(c.name) == n)
            .unwrap();
        p.value(c.setting).clone()
    };
    assert_eq!((get("f").tag(), get("f").float()), (ValueTag::Float, 0.5));
    assert_eq!((get("t").tag(), get("t").text()), (ValueTag::Text, "5px"));
    assert_eq!(get("i").float(), f32::INFINITY);
    assert_eq!(get("e").tag(), ValueTag::Text);
    assert_eq!(
        (get("v2").tag(), get("v2").vec2()),
        (ValueTag::Vec2, Vec2::new(1.0, 2.0))
    );
    assert_eq!(get("v3").tag(), ValueTag::Vec3);
    assert_eq!(
        (get("v4").tag(), get("v4").vec4()),
        (ValueTag::Vec4, Vec4::new(1.0, 2.0, 3.0, 4.0))
    );
    assert_eq!(get("n").tag(), ValueTag::Int);
    assert_eq!(get("x").tag(), ValueTag::Float);
    assert_eq!(get("b").tag(), ValueTag::Bool);
    assert_eq!(get("nu").tag(), ValueTag::Null);
    assert_eq!(get("a").tag(), ValueTag::Null);
    assert_eq!(get("o").tag(), ValueTag::Null);
    assert_eq!(get("w").float(), 5.0);
    assert_eq!(get("big").int(), 1);
}

#[test]
fn setting_errors_5_2() {
    let check = |extra: &str, expected_path: &str| {
        let f = base_files(&scene_with(ortho(), &img_obj(extra)));
        let (file, path, _) = data_err(&load_err(&f));
        assert_eq!(file, "scene.json");
        assert_eq!(path, expected_path);
    };
    check(
        r#", "alpha": { "user": "speed" }"#,
        "$.objects[0].alpha.value",
    );
    check(
        r#", "alpha": { "value": 1, "script": 3 }"#,
        "$.objects[0].alpha.script",
    );
    // Q30: `user` of the wrong type stays an error.
    check(
        r#", "alpha": { "value": 1, "user": 3 }"#,
        "$.objects[0].alpha.user",
    );
    check(
        r#", "alpha": { "value": 1, "user": { "condition": "1" } }"#,
        "$.objects[0].alpha.user.name",
    );
    check(r#", "color": "1 2""#, "$.objects[0].color");
    check(
        r#", "effects": [ { "file": "effects/blur/effect.json", "passes": [ { "combos": { "A": "x" } } ] } ]"#,
        "$.objects[0].effects[0].passes[0].combos.A",
    );
}

#[test]
fn conditions_5_3_and_q30() {
    // Condition with a slider: ignored, value copied. Numeric condition accepted (Q30).
    // Missing condition: no condition (Q30).
    let objects = img_obj(
        r#", "visible": { "value": true, "user": { "name": "mode", "condition": 1 } },
        "alpha": { "value": 1, "user": { "name": "speed", "condition": "x" } },
        "brightness": { "value": 1, "user": { "name": "speed" } },
        "parallaxDepth": { "value": "1 1", "user": "missing" }"#,
    );
    let p = load(&base_files(&scene_with(ortho(), &objects)));
    let img = image(&p, 0);
    assert!(p.value(img.visible).boolean());
    assert_eq!(p.value(img.alpha).float(), 1.5);
    assert_eq!(p.value(img.brightness).float(), 1.5);
    // Unknown property: initial value kept, no binding, no condition.
    assert_eq!(p.value(img.parallax_depth).vec2(), Vec2::ONE);
    assert_eq!(*p.settings.meta(img.parallax_depth), SettingMeta::default());
}

#[test]
fn bound_to_decorative_text_property_is_null_q11() {
    let project = r#"{ "type": "scene", "title": "t", "file": "scene.json", "general": { "properties": {
        "label": { "type": "text", "text": "hello" } } } }"#;
    let mut f = base_files(&scene_with(
        ortho(),
        &img_obj(r#", "alpha": { "value": 0.7, "user": "label" }"#),
    ));
    f.insert("project.json".into(), project.as_bytes().to_vec());
    let p = load(&f);
    assert_eq!(p.value(image(&p, 0).alpha).tag(), ValueTag::Null);
}

// ---------------------------------------------------------------------------------------------
// §6: scene

#[test]
fn scene_example_6_4() {
    let general = r#""clearcolor": { "value": "0 0 0", "user": "schemecolor" },
        "bloom": true, "bloomstrength": 2.0, "bloomthreshold": 0.65,
        "cameraparallax": true, "cameraparallaxamount": 0.5,
        "orthogonalprojection": { "width": 1920, "height": 1080 }"#;
    let p = load(&base_files(&scene_with(general, "")));
    let s = p.scene.as_ref().unwrap();
    assert_eq!(s.camera.center, Vec3::new(0.0, 0.0, -1.0));
    assert_eq!(s.camera.up, Vec3::Y);
    assert_eq!(p.value(s.camera.farz).float(), 1000.0);
    assert_eq!(p.value(s.camera.fov).float(), 50.0);
    assert_eq!(
        p.value(s.general.clear_color).vec3(),
        Vec3::new(0.2, 0.4, 0.8)
    );
    assert!(p.value(s.general.bloom).boolean());
    assert_eq!(p.value(s.general.bloom_strength).float(), 2.0);
    assert!(p.value(s.general.camera_parallax).boolean());
    assert_eq!(p.value(s.general.camera_parallax_amount).float(), 0.5);
    assert_eq!(
        p.value(s.general.camera_parallax_mouse_influence).float(),
        1.0
    );
    assert_eq!(
        s.general.orthogonal_projection,
        Some(OrthogonalProjection {
            auto: false,
            width: 1920,
            height: 1080
        })
    );
}

#[test]
fn camera_planes_from_general_q5_and_perspective_q6() {
    let p = load(&base_files(&scene_with(
        r#""nearz": 0.01, "farz": 10000.0, "fov": 40"#,
        "",
    )));
    let s = p.scene.as_ref().unwrap();
    assert_eq!(p.value(s.camera.nearz).float(), 0.01);
    assert_eq!(p.value(s.camera.farz).float(), 10000.0);
    assert_eq!(p.value(s.camera.fov).int(), 40);
    assert_eq!(s.general.orthogonal_projection, None);
    let p = load(&base_files(&scene_with(
        r#""orthogonalprojection": null"#,
        "",
    )));
    assert_eq!(
        p.scene.as_ref().unwrap().general.orthogonal_projection,
        None
    );
    let (_, path, _) = data_err(&load_err(&base_files(&scene_with(
        r#""orthogonalprojection": {}"#,
        "",
    ))));
    assert_eq!(path, "$.general.orthogonalprojection.width");
    let p = load(&base_files(&scene_with(
        r#""orthogonalprojection": { "auto": true }"#,
        "",
    )));
    assert_eq!(
        p.scene.as_ref().unwrap().general.orthogonal_projection,
        Some(OrthogonalProjection {
            auto: true,
            width: 0,
            height: 0
        })
    );
}

#[test]
fn scene_root_errors() {
    let f = |s: &str| data_err(&load_err(&base_files(s))).1;
    assert_eq!(f(r#"{ "general": {}, "objects": [] }"#), "$.camera");
    assert_eq!(
        f(&format!(r#"{{ {CAMERA}, "general": 1, "objects": [] }}"#)),
        "$.general"
    );
    assert_eq!(
        f(&format!(r#"{{ {CAMERA}, "general": {{}}, "objects": 1 }}"#)),
        "$.objects"
    );
    assert_eq!(
        f(
            r#"{ "camera": { "center": "0 0", "eye": "0 0 0", "up": "0 1 0" }, "general": {}, "objects": [] }"#
        ),
        "$.camera.center"
    );
    // Objects as a map: values in key order.
    let s = format!(
        r#"{{ {CAMERA}, "general": {{}}, "objects": {{ "b": {{ "id": 2, "name": "b" }}, "a": {{ "id": 1, "name": "a" }} }} }}"#
    );
    let p = load(&base_files(&s));
    assert_eq!(
        p.scene
            .as_ref()
            .unwrap()
            .objects
            .iter()
            .map(|o| o.id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

// ---------------------------------------------------------------------------------------------
// §7: objects

#[test]
fn object_classification_7_1() {
    let objects = r#"
        { "id": 1, "name": "img", "image": "models/fond.json", "sound": ["a.mp3"] },
        { "id": 2, "name": "snd", "sound": ["a.mp3", "b.mp3"], "playbackmode": "loop", "particle": "p.json" },
        { "id": 3, "name": "par", "particle": "particles/x.json", "text": "t", "instanceoverride": { "rate": 2 } },
        { "id": 4, "name": "inline", "particle": { "maxcount": 3 } },
        { "id": 5, "name": "none", "particle": "None", "text": null },
        { "id": 6, "name": "light", "light": "point", "particle": "" },
        { "id": 7, "name": "shape", "shape": "cone" },
        { "id": 8, "name": "group", "solid": true },
        { "id": 9, "name": "model3d", "model": "x.mdl" },
        { "id": 10, "name": "img2", "image": 5 }"#;
    let p = load(&base_files(&scene_with(ortho(), objects)));
    let kinds: Vec<&str> = p
        .scene
        .as_ref()
        .unwrap()
        .objects
        .iter()
        .map(|o| match &o.kind {
            ObjectKind::Image(_) => "image",
            ObjectKind::Sound(s) if s.looping && s.files.len() == 2 => "sound-loop",
            ObjectKind::Sound(_) => "sound",
            ObjectKind::Particle(p) => match &p.source {
                ParticleSource::File(_) if p.instance_override.is_some() => {
                    "particle-file-override"
                }
                ParticleSource::File(_) => "particle-file",
                ParticleSource::Inline(r) if r.get() == r#"{ "maxcount": 3 }"# => "particle-inline",
                ParticleSource::Inline(_) => "?",
            },
            ObjectKind::Text(_) => "text",
            ObjectKind::Model3d(_) => "model",
            ObjectKind::Light(_) => "light",
            ObjectKind::Sprite(_) => "sprite",
            ObjectKind::Generic(GenericKind::Light) => "light-unknown",
            ObjectKind::Generic(GenericKind::Shape) => "shape",
            ObjectKind::Generic(GenericKind::Group) => "group",
            ObjectKind::Generic(GenericKind::Unknown) => "unknown",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "image",
            "sound-loop",
            "particle-file-override",
            "particle-inline",
            "text",
            "light",
            "shape",
            "group",
            "model",
            "unknown"
        ]
    );
    // Q20: unsupported types are warned about (lights and models are supported, spec 09).
    assert_eq!(
        p.warnings()
            .iter()
            .filter(|w| w.message.contains("not supported"))
            .count(),
        1
    );
    assert_eq!(
        p.warnings()
            .iter()
            .filter(|w| w.message.contains("unknown object type"))
            .count(),
        1
    );
}

#[test]
fn common_fields_and_degraded_mode_7_2() {
    let objects = r#"
        { "id": 1, "name": "a", "dependencies": [2, 3.7, true], "parent": 4, "origin": "1 2 3", "scale": 2, "visible": false },
        { "id": "x", "name": 12.9 },
        { "id": 7.9, "name": [1] },
        { "name": "only" },
        { "id": 9, "name": "d", "dependencies": "ignored" }"#;
    let p = load(&base_files(&scene_with(ortho(), objects)));
    let o = obj(&p, 0);
    assert_eq!(
        (o.id, &*o.dependencies, o.parent),
        (1, &[2, 3, 1][..], Some(4))
    );
    assert_eq!(p.value(o.origin).vec3(), Vec3::new(1.0, 2.0, 3.0));
    assert_eq!(p.value(o.scale).vec3(), Vec3::splat(2.0));
    assert!(!p.value(o.visible).boolean());
    let names: Vec<(i32, &str)> = p.scene.as_ref().unwrap().objects[1..4]
        .iter()
        .map(|o| (o.id, p.strings.resolve(o.name)))
        .collect();
    // Spec 09 §1.8: an object without any `id` gets a synthetic id `-1000 - rank`.
    assert_eq!(names, [(-1, "12"), (7, "unknown"), (-1003, "only")]);
    assert!(obj(&p, 4).dependencies.is_empty());
    // Second failure in degraded mode is fatal.
    let (_, path, _) = data_err(&load_err(&base_files(&scene_with(
        ortho(),
        r#"{ "name": 1, "parent": "x" }"#,
    ))));
    assert_eq!(path, "$.objects[0].parent");
    let (_, path, _) = data_err(&load_err(&base_files(&scene_with(
        ortho(),
        r#"{ "id": 1, "name": "a", "dependencies": ["x"] }"#,
    ))));
    assert_eq!(path, "$.objects[0].dependencies[0]");
}

#[test]
fn dependencies_accept_id_objects_spec05_a7_8() {
    let objects = [
        generic(1, ""),
        generic(2, ""),
        r#"{ "id": 3, "name": "p", "particle": "x.json",
             "dependencies": [ { "id": 2, "index": 0, "type": "emitterimage" }, 1, { "id": 1.9, "type": "collisionmodel" } ] }"#
            .to_owned(),
    ]
    .join(",");
    let f = base_files(&scene_with(ortho(), &objects));
    let mut p = load(&f);
    assert_eq!(&*obj(&p, 2).dependencies, &[2, 1, 1]);
    let b = p.build(&f, &BuildOptions::new(1, 1)).unwrap();
    assert_eq!(ids(&p, &b.creation_order), [1, 2, 3]);
    // An object without a usable `id` stays a typed error.
    let bad = r#"{ "id": 3, "name": "p", "dependencies": [ 1, { "index": 0 } ] }"#;
    let (_, path, err) = data_err(&load_err(&base_files(&scene_with(ortho(), bad))));
    assert_eq!(
        (path.as_str(), err),
        ("$.objects[0].dependencies[1].id", DataError::Missing)
    );
    let bad = r#"{ "id": 3, "name": "p", "dependencies": [ { "id": "2" } ] }"#;
    assert_eq!(
        data_err(&load_err(&base_files(&scene_with(ortho(), bad)))).1,
        "$.objects[0].dependencies[0].id"
    );
}

#[test]
fn image_example_7_4_and_instance_textures_q14() {
    let objects = r#"{
      "id": 12, "name": "Fond", "image": "models/fond.json",
      "origin": "960 540 0", "scale": "1 1 1", "angles": "0 0 0",
      "size": "1920 1080", "alpha": 1, "color": "1.00000 1.00000 1.00000",
      "visible": { "value": true, "user": "showbg" },
      "alignment": "topleft",
      "effects": [{
        "file": "effects/blur/effect.json", "id": 3, "visible": true,
        "passes": [{
          "combos": { "MODE": 1 },
          "constantshadervalues": { "speed": { "value": 1.0, "user": "speed" }, "ripplestrength": 0.3 },
          "textures": [ null, "masks/masque_ripple" ]
        }]
      }],
      "animationlayers": [ { "id": 4, "rate": 2 } ],
      "instance": { "textures": [ "monimage", "inst1", "", "inst3" ] }
    }"#;
    let p = load(&base_files(&scene_with(ortho(), objects)));
    let img = image(&p, 0);
    assert_eq!(img.size, Vec2::new(1920.0, 1080.0));
    assert_eq!(p.value(img.color).vec4(), Vec4::ONE);
    assert_eq!(p.value(img.alpha).tag(), ValueTag::Int);
    assert_eq!(
        img.alignment,
        Alignment {
            top: true,
            left: true,
            ..Default::default()
        }
    );
    assert_eq!(p.value(img.color_blend_mode).tag(), ValueTag::Int);
    assert_eq!(img.animation_layers.len(), 1);
    assert_eq!(p.value(img.animation_layers[0].rate).int(), 2);
    assert!(!p.value(img.animation_layers[0].visible).boolean());
    let scene = p.scene.as_ref().unwrap();
    let pass = &scene
        .material(scene.model(img.model.unwrap()).material)
        .passes[0];
    let tex: Vec<(u32, &str)> = pass
        .textures
        .entries()
        .iter()
        .map(|&(s, n)| (s, p.strings.resolve(n)))
        .collect();
    // Slot 0 keeps the material texture; slots 1 and 3 are filled.
    assert_eq!(
        tex,
        [(0, "fond"), (1, "inst1"), (2, "masks/m1"), (3, "inst3")]
    );
    assert_eq!(img.effects.len(), 1);
    let e = &img.effects[0];
    assert_eq!(e.id, 3);
    let ov = &e.overrides[0];
    assert_eq!(ov.id, -1);
    assert_eq!(
        (p.strings.resolve(ov.combos[0].key), ov.combos[0].value),
        ("MODE", 1)
    );
    let consts: Vec<(&str, f32)> = ov
        .constants
        .iter()
        .map(|c| (p.strings.resolve(c.name), p.value(c.setting).float()))
        .collect();
    assert_eq!(consts, [("ripplestrength", 0.3), ("speed", 1.5)]);
    assert_eq!(ov.textures.entries().len(), 1);
    assert_eq!(
        p.main_texture(ObjectIndex::new(0)),
        Some(TextureRef::File(p.strings.get("fond").unwrap()))
    );
}

#[test]
fn texture_maps_11_1() {
    let material = r#"{ "passes": [ { "shader": "s",
        "textures": [ null, "", "a", { "name": "b" }, { "x": 1 }, 3, true, ["c"], { "name": 2 }, "_rt_X" ] } ] }"#;
    let mut f = base_files(&scene_with(ortho(), &img_obj("")));
    f.insert("materials/fond.json".into(), material.as_bytes().to_vec());
    let p = load(&f);
    let s = p.scene.as_ref().unwrap();
    let pass = &s
        .material(s.model(image(&p, 0).model.unwrap()).material)
        .passes[0];
    let tex: Vec<(u32, &str)> = pass
        .textures
        .entries()
        .iter()
        .map(|&(s, n)| (s, p.strings.resolve(n)))
        .collect();
    assert_eq!(tex, [(2, "a"), (3, "b"), (9, "_rt_X")]);
    assert_eq!(
        p.main_texture(ObjectIndex::new(0)),
        Some(TextureRef::File(p.strings.get("a").unwrap()))
    );
}

#[test]
fn text_object_7_7_and_q19() {
    let objects = r#"{ "id": 1, "name": "t", "text": { "value": "12:00", "script": "x" }, "font": "fonts/a.ttf",
        "pointsize": 20, "size": { "value": "100 50" }, "padding": 4, "horizontalalign": "right", "verticalalign": "top",
        "color": "255 0 0" },
        { "id": 2, "name": "u", "text": null, "alignment": "left" }"#;
    let p = load(&base_files(&scene_with(ortho(), objects)));
    let ObjectKind::Text(t) = &obj(&p, 0).kind else {
        panic!()
    };
    assert_eq!(p.value(t.text).text(), "12:00");
    assert!(p.settings.meta(t.text).script.is_some());
    assert_eq!(p.strings.resolve(t.font), "fonts/a.ttf");
    assert_eq!(p.value(t.point_size).float(), 20.0);
    assert_eq!(t.size, Vec2::new(100.0, 50.0));
    assert_eq!(t.padding, Vec2::splat(4.0));
    assert!(t.horizontal_align.right && t.vertical_align.top);
    assert_eq!(p.value(t.color).vec4(), Vec4::new(1.0, 0.0, 0.0, 1.0));
    let ObjectKind::Text(u) = &obj(&p, 1).kind else {
        panic!()
    };
    assert_eq!(p.value(u.text).tag(), ValueTag::Null);
    assert!(u.horizontal_align.left);
    assert_eq!(p.value(u.point_size).float(), 32.0);
    let (_, path, _) = data_err(&load_err(&base_files(&scene_with(
        ortho(),
        r#"{ "id": 1, "name": "t", "text": "x", "size": "1 2 3" }"#,
    ))));
    assert_eq!(path, "$.objects[0].size");
}

/// Lumenwall: a text is drawn as an image layer (font material, glyph texture in slot 0) with the
/// object's effects and colour blend pass, and its content stays text even when it looks like
/// a number or a vector.
#[test]
fn text_layer_and_content() {
    let objects = r#"{ "id": 1, "name": "t", "text": { "value": "26 NOV 2024", "script": "x" },
        "horizontalalign": "left", "verticalalign": "bottom", "brightness": 2, "colorBlendMode": 11,
        "parallaxDepth": "0.5 0.5",
        "effects": [ { "file": "effects/blur/effect.json", "passes": [ { "combos": { "A": 1 } } ] } ] },
        { "id": 2, "name": "u", "text": "2024" }"#;
    let f = base_files(&scene_with(ortho(), objects));
    let mut p = load(&f);
    let ObjectKind::Text(t) = &obj(&p, 0).kind else {
        panic!()
    };
    assert_eq!(p.value(t.text).text(), "26 NOV 2024");
    assert_eq!(p.value(t.brightness).float(), 2.0);
    let layer = obj(&p, 0).as_layer().expect("text layer");
    assert!(obj(&p, 0).as_image().is_none());
    assert!(layer.alignment.left && layer.alignment.bottom);
    assert_eq!(layer.effects.len(), 1);
    assert_eq!(p.value(layer.color_blend_mode).int(), 11);
    assert_eq!(p.value(layer.parallax_depth).vec2(), Vec2::splat(0.5));
    // Neutral layer values: the glyph pass applies the text's own.
    assert_eq!(p.value(layer.alpha).float(), 1.0);
    assert_eq!(p.value(layer.brightness).float(), 1.0);
    let ObjectKind::Text(u) = &obj(&p, 1).kind else {
        panic!()
    };
    assert_eq!(p.value(u.text).text(), "2024");
    let Some(TextureRef::File(main)) = p.main_texture(ObjectIndex::new(0)) else {
        panic!("glyph texture slot")
    };
    assert!(p.strings.resolve(main).starts_with(TEXT_TEXTURE_PREFIX));
    let b = p.build(&f, &BuildOptions::new(1920, 1080)).unwrap();
    let passes = b.image_passes[0].as_ref().expect("text passes");
    // Font pass, blur_h, blur_v (2 passes), colour blend pass.
    assert_eq!(passes.passes.len(), 5);
    let s = p.scene.as_ref().unwrap();
    let PassKind::Material { material, .. } = passes.passes[0].kind else {
        panic!()
    };
    assert_eq!(
        p.strings.resolve(s.material(material).passes[0].shader),
        "font"
    );
    // lumenwall-scene reports each pass's own material mode; lumenwall-render applies the first/last
    // transfer of spec 04 §9.8 (blend contract, see the README).
    assert_eq!(passes.passes[0].blending, Blending::Translucent);
    assert_eq!(passes.passes[4].blending, Blending::Normal);
}

#[test]
fn sound_errors_7_6() {
    let (_, path, _) = data_err(&load_err(&base_files(&scene_with(
        ortho(),
        r#"{ "id": 1, "name": "s", "sound": ["a", 2] }"#,
    ))));
    assert_eq!(path, "$.objects[0].sound[1]");
}

// ---------------------------------------------------------------------------------------------
// §8–§11: models, materials, effects

#[test]
fn model_material_effect_8_9_10() {
    let objects = img_obj(r#", "effects": [ { "file": "effects/blur/effect.json" } ]"#);
    let mut f = base_files(&scene_with(ortho(), &objects));
    f.insert("models/fond.json".into(), br#"{ "material": "materials/fond.json", "width": 64, "puppet": "p.mdl", "passthrough": false }"#.to_vec());
    let p = load(&f);
    let s = p.scene.as_ref().unwrap();
    let img = image(&p, 0);
    let model = s.model(img.model.unwrap());
    assert_eq!((model.width, model.height), (Some(64), None));
    assert_eq!(model.puppet.map(|x| p.strings.resolve(x)), Some("p.mdl"));
    let pass = &s.material(model.material).passes[0];
    assert_eq!(p.strings.resolve(pass.shader), "genericimage2");
    assert_eq!(
        (
            pass.blending,
            pass.cull_mode,
            pass.depth_test,
            pass.depth_write
        ),
        (
            Blending::Translucent,
            CullMode::NoCull,
            DepthMode::Disabled,
            DepthMode::Disabled
        )
    );
    assert_eq!(pass.combos[0].value, 1);
    let consts: Vec<(&str, ValueTag)> = pass
        .constants
        .iter()
        .map(|c| (p.strings.resolve(c.name), p.value(c.setting).tag()))
        .collect();
    assert_eq!(
        consts,
        [("alpha", ValueTag::Float), ("color", ValueTag::Vec3)]
    );

    let e = s.effect(img.effects[0].effect);
    assert_eq!(
        (&*e.name, &*e.group, e.dependencies.len()),
        ("Flou", "blur", 1)
    );
    assert_eq!(e.fbos.len(), 1);
    assert_eq!(
        (
            p.strings.resolve(e.fbos[0].name),
            e.fbos[0].scale,
            p.strings.resolve(e.fbos[0].format)
        ),
        ("_rt_HalfCompoBuffer1", 2.0, "rgba8888")
    );
    assert_eq!(e.passes.len(), 3);
    assert_eq!(
        e.passes[0].target.map(|t| p.strings.resolve(t)),
        Some("_rt_HalfCompoBuffer1")
    );
    assert_eq!(e.passes[1].binds.len(), 1);
    assert_eq!(e.passes[2].command, Some(Command::Copy));
    assert_eq!(e.passes[2].source, Some(BindSource::Previous));
    // Each reference loads its own copy (§1.1-4).
    let two = format!(
        "{}, {}",
        img_obj(""),
        r#"{ "id": 2, "name": "b", "image": "models/fond.json" }"#
    );
    let p = load(&base_files(&scene_with(ortho(), &two)));
    assert_ne!(image(&p, 0).model, image(&p, 1).model);
    assert_eq!(p.scene.as_ref().unwrap().materials().len(), 2);
}

#[test]
fn material_and_effect_details() {
    let material = r#"{ "passes": [ { "shader": "s", "blending": "weird", "cullmode": "normal", "depthtest": "enabled",
        "combos": { "b": true, "A": 2.9, "N": "3" } } ] }"#;
    let effect = r#"{ "passes": [
        { "material": "materials/fond.json", "bind": [ { "index": 1, "name": "x" }, { "index": 0, "name": "previous" }, { "index": 1, "name": "y" } ] },
        { "command": "swap", "source": "a", "target": "b" } ] }"#;
    let mut f = base_files(&scene_with(
        ortho(),
        &img_obj(r#", "effects": [ { "file": "e.json" } ]"#),
    ));
    f.insert("materials/fond.json".into(), material.as_bytes().to_vec());
    f.insert("e.json".into(), effect.as_bytes().to_vec());
    let p = load(&f);
    let s = p.scene.as_ref().unwrap();
    let pass = &s
        .material(s.model(image(&p, 0).model.unwrap()).material)
        .passes[0];
    assert_eq!(
        (pass.blending, pass.cull_mode, pass.depth_test),
        (Blending::Normal, CullMode::Normal, DepthMode::Enabled)
    );
    assert!(
        p.warnings()
            .iter()
            .any(|w| w.file == "materials/fond.json" && w.message.contains("weird"))
    );
    // Q29: integer strings are accepted; booleans and floats convert.
    let combos: Vec<(&str, &str, i32)> = pass
        .combos
        .iter()
        .map(|c| (p.strings.resolve(c.name), p.strings.resolve(c.key), c.value))
        .collect();
    assert_eq!(combos, [("A", "A", 2), ("N", "N", 3), ("b", "B", 1)]);
    let e = s.effect(image(&p, 0).effects[0].effect);
    let binds: Vec<(i32, BindSource)> = e.passes[0]
        .binds
        .iter()
        .map(|b| (b.index, b.source))
        .collect();
    assert_eq!(
        binds,
        [
            (0, BindSource::Previous),
            (1, BindSource::Named(p.strings.get("x").unwrap()))
        ]
    );
    // Q15: any other command is a swap.
    assert_eq!(e.passes[1].command, Some(Command::Swap));
    // Command requires source and target.
    f.insert(
        "e.json".into(),
        br#"{ "passes": [ { "command": "copy", "target": "b" } ] }"#.to_vec(),
    );
    let (file, path, _) = data_err(&load_err(&f));
    assert_eq!(
        (file.as_str(), path.as_str()),
        ("e.json", "$.passes[0].source")
    );
    f.insert("e.json".into(), br#"{ "name": "no passes" }"#.to_vec());
    assert_eq!(data_err(&load_err(&f)).1, "$.passes");
    f.insert("e.json".into(), br#"{ "passes": [ ] "#.to_vec());
    assert!(matches!(load_err(&f), LoadError::Syntax { ref file, .. } if file == "e.json"));
}

#[test]
fn missing_files_degrade_q31_and_empty_material_q27() {
    let objects = format!(
        "{}, {}, {}",
        img_obj(
            r#", "effects": [ { "file": "effects/missing.json" }, { "file": "effects/blur/effect.json" } ]"#
        ),
        r#"{ "id": 2, "name": "b", "image": "models/missing.json" }"#,
        r#"{ "id": 3, "name": "c", "image": "models/empty.json" }"#
    );
    let mut f = base_files(&scene_with(ortho(), &objects));
    f.insert(
        "models/empty.json".into(),
        br#"{ "material": "materials/empty.json" }"#.to_vec(),
    );
    f.insert(
        "materials/empty.json".into(),
        br#"{ "passes": {} }"#.to_vec(),
    );
    let mut p = load(&f);
    assert_eq!(image(&p, 0).effects.len(), 1);
    assert!(image(&p, 1).model.is_none());
    assert!(
        p.warnings()
            .iter()
            .any(|w| w.message.contains("effects/missing.json"))
    );
    let b = p.build(&f, &BuildOptions::new(1920, 1080)).unwrap();
    assert_eq!(b.created, [true, false, false]);
    // A missing material inside an effect drops the effect.
    f.remove("materials/effects/blur_v.json");
    let p = load(&f);
    assert!(image(&p, 0).effects.is_empty());
}

// ---------------------------------------------------------------------------------------------
// §12: passes and precedence

fn effect_files() -> Files {
    let objects = img_obj(
        r#", "effects": [
            { "file": "effects/blur/effect.json", "passes": [
                { "id": 10, "combos": { "MODE": 1, "tex0format": 5 }, "constantshadervalues": { "k": 2 }, "textures": [ null, "ov1" ], "usertextures": [ null, null, "ou2" ] },
                { "id": 11, "constantshadervalues": { "k": 3 } },
                { "id": 12 } ] },
            { "file": "effects/blur/effect.json", "visible": { "value": true, "user": { "name": "mode", "condition": "1" } } }
        ]"#,
    );
    let mut f = base_files(&scene_with(ortho(), &objects));
    f.insert(
        "materials/effects/blur_h.json".into(),
        br#"{ "passes": [ { "shader": "h", "blending": "additive", "textures": [ "m0", "m1" ], "usertextures": [ "u0" ],
              "combos": { "MODE": 7, "OTHER": 2 }, "constantshadervalues": { "k": 1, "j": 5 } } ] }"#
            .to_vec(),
    );
    f
}

#[test]
fn override_matching_12_2() {
    let mut p = load(&effect_files());
    let f = effect_files();
    let b = p.build(&f, &BuildOptions::new(1920, 1080)).unwrap();
    let passes = b.image_passes[0].as_ref().unwrap();
    type Row = (String, OverrideRef, Option<(u32, u32)>);
    let summary: Vec<Row> = passes
        .passes
        .iter()
        .map(|d| {
            let k = match d.kind {
                PassKind::Material { material, pass } => {
                    let s = p.scene.as_ref().unwrap();
                    p.strings
                        .resolve(s.material(material).passes[pass as usize].shader)
                        .to_owned()
                }
                PassKind::Copy { .. } => "copy".into(),
            };
            (k, d.over, d.effect)
        })
        .collect();
    let ov = |effect, index| OverrideRef::Effect { effect, index };
    assert_eq!(
        summary,
        [
            ("genericimage2".into(), OverrideRef::None, None),
            ("h".into(), ov(0, 0), Some((0, 0))),
            // Both passes of blur_v share the second override.
            ("v1".into(), ov(0, 1), Some((0, 1))),
            ("v2".into(), ov(0, 1), Some((0, 1))),
            // The copy pass consumes nothing; the third override is extra and ignored.
            ("copy".into(), OverrideRef::None, Some((0, 2))),
            ("h".into(), OverrideRef::None, Some((1, 0))),
            ("v1".into(), OverrideRef::None, Some((1, 1))),
            ("v2".into(), OverrideRef::None, Some((1, 1))),
            ("copy".into(), OverrideRef::None, Some((1, 2))),
        ]
    );
    // Every pass keeps its material's own blending: the first/last swap of §12.6 is the
    // renderer's (spec 04 §9.8), applied exactly once to the final chain.
    assert_eq!(passes.passes[0].blending, Blending::Translucent);
    assert_eq!(passes.passes[1].blending, Blending::Additive);
    assert_eq!(passes.passes.last().unwrap().blending, Blending::Normal);
    assert!(passes.draws);
    assert_eq!(
        p.pass_target(ObjectIndex::new(0), passes, 1)
            .map(|t| p.strings.resolve(t)),
        Some("_rt_HalfCompoBuffer1")
    );
    assert_eq!(
        p.pass_target(ObjectIndex::new(0), passes, 4)
            .map(|t| p.strings.resolve(t)),
        Some("_rt_copie")
    );

    // Q16: effect visibility is live: rebuild after the property changes.
    p.set_property_text("mode", "0").unwrap();
    let passes = p.image_passes(ObjectIndex::new(0)).unwrap();
    assert_eq!(passes.passes.len(), 5);
}

#[test]
fn texture_precedence_12_3() {
    let f = effect_files();
    let mut p = load(&f);
    let b = p.build(&f, &BuildOptions::new(1920, 1080)).unwrap();
    let passes = b.image_passes[0].clone().unwrap();
    let o = ObjectIndex::new(0);
    let mut exists = |n: &str| n != "missing";
    let names = |p: &Project, v: Vec<(u32, TextureRef)>| -> Vec<(u32, String)> {
        v.into_iter()
            .map(|(s, r)| {
                (
                    s,
                    match r {
                        TextureRef::File(n) => format!("file:{}", p.strings.resolve(n)),
                        TextureRef::RenderTarget(n) => format!("rt:{}", p.strings.resolve(n)),
                        TextureRef::Previous => "previous".into(),
                        TextureRef::OwnComposite => "own".into(),
                    },
                )
            })
            .collect()
    };
    // Effect pass 0 (blur_h, override 0).
    let t = p.pass_textures(
        o,
        &passes,
        1,
        &[(0, "d0"), (3, "d3"), (4, "missing")],
        &mut exists,
    );
    assert_eq!(
        names(&p, t),
        [
            (0, "file:u0".into()),
            (1, "file:ov1".into()),
            (2, "file:ou2".into()),
            (3, "file:d3".into())
        ]
    );
    // Effect pass 1 (blur_v): bind slot 0 to the FBO.
    let t = p.pass_textures(o, &passes, 2, &[], &mut exists);
    assert_eq!(names(&p, t), [(0, "rt:_rt_HalfCompoBuffer1".into())]);
    // Copy pass: slot 0 is the source.
    let t = p.pass_textures(o, &passes, 4, &[], &mut exists);
    assert_eq!(names(&p, t), [(0, "previous".into())]);
    let ct = p.pass_compile_textures(o, &passes, 1);
    let ct: Vec<(u32, &str)> = ct
        .entries()
        .iter()
        .map(|&(s, n)| (s, p.strings.resolve(n)))
        .collect();
    assert_eq!(ct, [(0, "u0"), (1, "ov1")]);
}

#[test]
fn combo_and_constant_precedence_12_4_12_5() {
    let f = effect_files();
    let mut p = load(&f);
    let b = p.build(&f, &BuildOptions::new(1920, 1080)).unwrap();
    let passes = b.image_passes[0].clone().unwrap();
    let o = ObjectIndex::new(0);
    let combos = p.pass_combos(
        o,
        &passes,
        1,
        MainTextureFormat::Rg88,
        &[("other", 9), ("Def", 4)],
    );
    let combos: Vec<(&str, i32)> = combos
        .iter()
        .map(|&(k, v)| (p.strings.resolve(k), v))
        .collect();
    // Override wins over material; TEX0FORMAT forced at material level loses to the override.
    assert_eq!(
        combos,
        [("DEF", 4), ("MODE", 1), ("OTHER", 2), ("TEX0FORMAT", 5)]
    );
    let combos = p.pass_combos(o, &passes, 0, MainTextureFormat::R8, &[]);
    let combos: Vec<(&str, i32)> = combos
        .iter()
        .map(|&(k, v)| (p.strings.resolve(k), v))
        .collect();
    assert_eq!(combos, [("TEX0FORMAT", 9), ("TRANSPARENCY", 1)]);

    let c = p.pass_constants(o, &passes, 1, &["k", "j", "absent"]);
    let ov = &image(&p, 0).effects[0].overrides[0];
    assert_eq!(c[0], ConstantSource::Setting(ov.constants[0].setting));
    assert!(matches!(c[1], ConstantSource::Setting(id) if p.value(id).int() == 5));
    assert_eq!(c[2], ConstantSource::Default);
}

#[test]
fn compatibility_passes_12_6_q28() {
    let objects = img_obj(
        r#", "colorBlendMode": 4, "effects": [
            { "file": "effects/one.json", "passes": [ { "combos": { "COMPOSITE": 2 }, "constantshadervalues": { "compositecolor": "0.1 0.1 0.9" } } ] },
            { "file": "effects/one.json", "visible": false, "passes": [ { "combos": { "COMPOSITE": 2 }, "constantshadervalues": { "compositecolor": "0.9 0.1 0.9" } } ] },
            { "file": "effects/one.json", "passes": [ { "combos": { "composite": 2 }, "constantshadervalues": { "compositecolor": "0.8 0.2 0.5" } } ] }
        ]"#,
    );
    let mut f = base_files(&scene_with(ortho(), &objects));
    f.insert(
        "effects/one.json".into(),
        br#"{ "passes": [ { "material": "materials/effects/blur_h.json" } ] }"#.to_vec(),
    );
    f.insert(
        "materials/effects/tint.json".into(),
        br#"{ "passes": [ { "shader": "tint" } ] }"#.to_vec(),
    );
    f.insert(
        "materials/util/effectpassthrough.json".into(),
        br#"{ "passes": [ { "shader": "pass" } ] }"#.to_vec(),
    );
    let mut p = load(&f);
    let b = p.build(&f, &BuildOptions::new(1920, 1080)).unwrap();
    let passes = b.image_passes[0].as_ref().unwrap();
    let n = passes.passes.len();
    assert_eq!(n, 5);
    assert_eq!(passes.passes[n - 2].over, OverrideRef::Synthetic(0));
    assert_eq!(passes.passes[n - 1].over, OverrideRef::Synthetic(1));
    let tint = &passes.synthetic[0];
    assert_eq!(tint.combos, [(p.strings.get("BLENDMODE").unwrap(), 30)]);
    let color = tint
        .constants
        .iter()
        .find(|(k, _)| p.strings.resolve(*k) == "color")
        .unwrap();
    assert_eq!(color.1.vec3(), Vec3::new(0.8, 0.2, 0.5));
    assert_eq!(passes.synthetic[1].combos[0].1, 4);
    let c = p.pass_constants(ObjectIndex::new(0), passes, n - 2, &["alpha"]);
    assert!(
        matches!(&c[0], ConstantSource::Fixed(v) if v.float() == 1.0 && v.tag() == ValueTag::Float)
    );
}

#[test]
fn passthrough_images() {
    let objects = format!(
        "{}, {}",
        img_obj(
            r#", "effects": [ { "file": "effects/blur/effect.json", "visible": { "value": false, "user": "showclock" } } ]"#
        ),
        r#"{ "id": 2, "name": "b", "image": "models/fond.json" }"#
    );
    let mut f = base_files(&scene_with(ortho(), &objects));
    f.insert(
        "models/fond.json".into(),
        br#"{ "material": "materials/fond.json", "passthrough": true }"#.to_vec(),
    );
    let mut p = load_with(&f, &["showclock=0"]);
    let b = p.build(&f, &BuildOptions::new(1920, 1080)).unwrap();
    // No effect at all: not set up. Effects all hidden: set up but draws nothing.
    assert_eq!(b.created, [true, false]);
    assert!(!b.image_passes[0].as_ref().unwrap().draws);
    p.set_property_text("showclock", "1").unwrap();
    assert!(p.image_passes(ObjectIndex::new(0)).unwrap().draws);
}

// ---------------------------------------------------------------------------------------------
// §13: construction

fn generic(id: i32, extra: &str) -> String {
    format!(r#"{{ "id": {id}, "name": "o{id}", "solid": true {extra} }}"#)
}

fn ids(p: &Project, v: &[ObjectIndex]) -> Vec<i32> {
    v.iter()
        .map(|i| p.scene.as_ref().unwrap().object(*i).id)
        .collect()
}

#[test]
fn creation_and_render_order_13() {
    let objects = [
        generic(1, r#", "dependencies": [3, 1, 99]"#),
        generic(2, r#", "parent": 4"#),
        generic(3, ""),
        generic(4, r#", "dependencies": [2]"#),
        generic(1, ""), // duplicate id: never created
        generic(5, r#", "parent": 42"#),
    ]
    .join(",");
    let f = base_files(&scene_with(ortho(), &objects));
    let mut p = load(&f);
    let b = p.build(&f, &BuildOptions::new(1920, 1080)).unwrap();
    // 2 → parent 4 → dependency 2 is a cycle (cut).
    assert_eq!(ids(&p, &b.creation_order), [3, 1, 4, 2, 5]);
    assert_eq!(b.created, [true, true, true, true, false, true]);
    // Parents do not influence the render order; dependencies do.
    assert_eq!(ids(&p, &b.render_order), [3, 1, 2, 4, 5]);
    let w: Vec<&str> = p.warnings().iter().map(|w| w.message.as_str()).collect();
    assert!(w.iter().any(|m| m.contains("cycle")));
    assert!(w.iter().any(|m| m.contains("parent 42 not found")));
    assert!(w.iter().any(|m| m.contains("dependency 99")));
}

#[test]
fn deep_dependency_chain_has_no_recursion() {
    let n = 20_000;
    let objects: Vec<String> = (0..n)
        .map(|i| generic(i, &format!(r#", "dependencies": [{}]"#, i + 1)))
        .collect();
    let f = base_files(&scene_with(ortho(), &objects.join(",")));
    let mut p = load(&f);
    let b = p.build(&f, &BuildOptions::new(1, 1)).unwrap();
    assert_eq!(b.creation_order.len(), n as usize);
    assert_eq!(
        p.scene.as_ref().unwrap().object(b.render_order[0]).id,
        n - 1
    );
}

#[test]
fn transforms_13_3() {
    let objects = [
        generic(1, r#", "origin": "100 50 10", "scale": "2 3 4", "angles": "0 0 1.5707964""#),
        r#"{ "id": 2, "name": "child", "image": "models/fond.json", "parent": 1, "origin": "10 0 1", "scale": "0.5 1 1", "angles": "0 0 0.25" }"#.to_string(),
        r#"{ "id": 3, "name": "text", "text": "x", "parent": 2, "origin": "1 1 0", "angles": "0 0 9" }"#.to_owned(),
    ]
    .join(",");
    let f = base_files(&scene_with(ortho(), &objects));
    let mut p = load(&f);
    let b = p.build(&f, &BuildOptions::new(1920, 1080)).unwrap();
    let t = b.transforms[1];
    // (10·2, 0·3) rotated by 90° = (0, 20).
    assert!(
        (t.origin - Vec3::new(100.0, 70.0, 14.0)).length() < 1e-4,
        "{t:?}"
    );
    assert_eq!(t.scale, Vec3::new(1.0, 3.0, 4.0));
    assert!((t.angle - (1.5707964 + 0.25)).abs() < 1e-6);
    // Text: angle 0 locally.
    assert!((b.transforms[2].angle - t.angle).abs() < 1e-6);
    assert_eq!(p.world_transform(ObjectIndex::new(1), &b), t);
}

#[test]
fn particles_disabled_and_renderer_veto() {
    let objects = format!(
        "{}, {}",
        r#"{ "id": 1, "name": "p", "particle": "x.json" }"#,
        generic(2, "")
    );
    let f = base_files(&scene_with(ortho(), &objects));
    let mut p = load(&f);
    let opts = BuildOptions {
        output_size: UVec2::new(10, 10),
        particles_enabled: false,
    };
    assert_eq!(p.build(&f, &opts).unwrap().created, [false, true]);
    let b = p
        .build_with(&f, &BuildOptions::new(10, 10), |_, o| o.id != 2)
        .unwrap();
    assert_eq!(b.created, [true, false]);
}

#[test]
fn scene_size_and_auto_projection() {
    let objects = format!(
        "{}, {}",
        img_obj(r#", "origin": "-100 20 0", "size": "50 40""#),
        r#"{ "id": 2, "name": "b", "image": "models/fond.json", "origin": "10 300 0", "size": "10 10" }"#
    );
    let p = load(&base_files(&scene_with(
        r#""orthogonalprojection": { "auto": true }"#,
        &objects,
    )));
    assert_eq!(p.scene_size(UVec2::new(1, 1)), Vec2::new(250.0, 610.0));
    let p = load(&base_files(&scene_with(
        r#""orthogonalprojection": { "auto": true }"#,
        "",
    )));
    assert_eq!(p.scene_size(UVec2::new(800, 600)), Vec2::new(800.0, 600.0));
    let f = base_files(&scene_with(r#""clearcolor": "0.5 0.25 1""#, ""));
    let mut p = load(&f);
    let b = p.build(&f, &BuildOptions::new(640, 480)).unwrap();
    assert_eq!(b.size, Vec2::new(640.0, 480.0));
    assert_eq!(b.clear_color, Vec4::new(0.5, 0.25, 1.0, 1.0));
}

// ---------------------------------------------------------------------------------------------
// §14: bloom

fn bloom_files(general: &str) -> Files {
    let mut f = base_files(&scene_with(general, &generic(-1, "")));
    for m in BLOOM_MATERIALS {
        f.insert(m.into(), br#"{ "passes": [ { "shader": "b" } ] }"#.to_vec());
    }
    f
}

#[test]
fn bloom_object_14() {
    let f = bloom_files(
        r#""bloom": { "value": false, "user": "showclock" }, "bloomstrength": 2.0, "bloomthreshold": 0.65, "orthogonalprojection": { "width": 1921, "height": 1080 }"#,
    );
    let mut p = load(&f);
    let b = p.build(&f, &BuildOptions::new(1, 1)).unwrap();
    let bloom = b.bloom.expect("bloom");
    assert_eq!((bloom.strength, bloom.threshold), (2.0, 0.65));
    // Not reachable by id: no collision with the real object −1 (Q17).
    assert_eq!(b.created_object(-1), Some(ObjectIndex::new(0)));
    assert_eq!(*b.render_order.last().unwrap(), bloom.object);
    let s = p.scene.as_ref().unwrap();
    let o = s.object(bloom.object);
    assert_eq!(p.value(o.origin).vec3(), Vec3::new(960.5, 540.0, 0.0));
    let img = o.as_image().unwrap();
    assert_eq!(img.size, Vec2::new(1921.0, 1080.0));
    let effect = s.effect(img.effects[0].effect);
    assert_eq!(effect.passes.len(), 4);
    assert_eq!(effect.passes[3].binds[0].source, BindSource::OwnComposite);
    let rts: Vec<(&str, u32, u32)> = b
        .render_targets
        .iter()
        .map(|r| (p.strings.resolve(r.name), r.width, r.height))
        .collect();
    assert_eq!(
        rts,
        [
            ("_rt_4FrameBuffer", 480, 270),
            ("_rt_8FrameBuffer", 240, 135),
            ("_rt_Bloom", 240, 135)
        ]
    );
    let passes = b.image_passes[bloom.object.index()].as_ref().unwrap();
    // Model pass + 4 effect passes; the fourth gets no override (Q21).
    let overs: Vec<OverrideRef> = passes.passes.iter().map(|d| d.over).collect();
    assert_eq!(
        overs[1..],
        [
            OverrideRef::Effect {
                effect: 0,
                index: 0
            },
            OverrideRef::Effect {
                effect: 0,
                index: 1
            },
            OverrideRef::Effect {
                effect: 0,
                index: 2
            },
            OverrideRef::None
        ]
    );
    let model_tex = &s.material(s.model(img.model.unwrap()).material).passes[0].textures;
    assert_eq!(
        p.strings.resolve(model_tex.entries()[0].1),
        "_rt_FullFrameBuffer"
    );
    // Rebuilding reuses the object.
    let objects_before = s.objects.len();
    let b2 = p.build(&f, &BuildOptions::new(1, 1)).unwrap();
    assert_eq!(b2.bloom.unwrap().object, bloom.object);
    assert_eq!(p.scene.as_ref().unwrap().objects.len(), objects_before);
    print!("{}", p.dump());
}

#[test]
fn bloom_off_or_missing_materials() {
    let f = bloom_files(r#""bloom": false"#);
    let mut p = load(&f);
    assert!(
        p.build(&f, &BuildOptions::new(1, 1))
            .unwrap()
            .bloom
            .is_none()
    );
    let mut f = bloom_files(r#""bloom": true"#);
    f.remove(BLOOM_MATERIALS[2]);
    let mut p = load(&f);
    assert!(
        p.build(&f, &BuildOptions::new(1, 1))
            .unwrap()
            .bloom
            .is_none()
    );
    assert!(
        p.warnings()
            .iter()
            .any(|w| w.message.contains("bloom disabled"))
    );
}

// ---------------------------------------------------------------------------------------------
// misc

#[test]
fn snapshot_sees_overrides_q18() {
    let project = r#"{ "type": "scene", "title": "t", "file": "scene.json", "general": { "properties": {
        "w": { "type": "slider", "value": 10 } } } }"#;
    let mut f = base_files(&scene_with(
        ortho(),
        &img_obj(r#", "size": { "value": "1 1", "user": "w" }"#),
    ));
    f.insert("project.json".into(), project.as_bytes().to_vec());
    let p = load_with(&f, &["w=64"]);
    assert_eq!(image(&p, 0).size, Vec2::splat(64.0));
}

#[test]
fn colors_q1_q2_q3() {
    let objects = img_obj(r#", "color": "1 1 1", "effects": [] "#);
    let p = load(&base_files(&scene_with(ortho(), &objects)));
    assert_eq!(
        p.value(image(&p, 0).color).vec4(),
        Vec4::new(1.0 / 255.0, 1.0 / 255.0, 1.0 / 255.0, 1.0)
    );
    let objects = img_obj(r#", "color": "1, 0, 0" "#);
    let p = load(&base_files(&scene_with(ortho(), &objects)));
    assert_eq!(
        p.value(image(&p, 0).color).vec4(),
        Vec4::new(1.0 / 255.0, 0.0, 0.0, 0.0)
    );
    let objects = img_obj(r##", "color": "#ffffffff" "##);
    let p = load(&base_files(&scene_with(ortho(), &objects)));
    assert_eq!(p.value(image(&p, 0).color).vec4(), Vec4::ONE);
}

#[test]
fn particle_overrides_are_registered_by_consumers() {
    let objects = r#"{ "id": 1, "name": "p", "particle": "x.json", "instanceoverride": { "rate": { "value": 2, "user": "speed" }, "colorn": "1 0 0" } }"#;
    let mut p = load(&base_files(&scene_with(ortho(), objects)));
    let ObjectKind::Particle(po) = &obj(&p, 0).kind else {
        panic!()
    };
    let io = po.instance_override.clone().unwrap();
    let rate = p
        .add_setting_json(
            "scene.json",
            &io["rate"],
            Some(Value::from_float(1.0)),
            false,
        )
        .unwrap();
    assert_eq!(p.value(rate).float(), 1.5);
    let color = p
        .add_setting_json(
            "scene.json",
            &io["colorn"],
            Some(Value::from_vec3(Vec3::ONE)),
            false,
        )
        .unwrap();
    assert_eq!(p.value(color).vec3(), Vec3::X);
    p.set_property_text("speed", "3").unwrap();
    assert_eq!(p.value(rate).float(), 3.0);
    let err = p
        .add_setting_json(
            "x.json",
            &serde_json::json!({ "user": "speed" }),
            None,
            false,
        )
        .unwrap_err();
    assert_eq!(data_err(&err).1, "$.value");
}

#[test]
fn video_and_web_projects() {
    let f = files(&[(
        "project.json",
        r#"{"type": "video", "title": "t", "file": "v.mp4"}"#,
    )]);
    let mut p = load(&f);
    assert!(p.scene.is_none());
    assert_eq!(p.dump(), "Video wallpaper: v.mp4\n");
    assert!(matches!(
        p.build(&f, &BuildOptions::new(1, 1)),
        Err(LoadError::NotAScene)
    ));
}

#[test]
fn hostile_inputs_do_not_panic() {
    let cases = [
        "",
        "null",
        "[]",
        "{",
        r#"{"type": 1}"#,
        r#"{"type": "scene", "title": "t", "file": "scene.json"}"#,
        r#"{"type": "scene", "title": "t", "file": "scene.json", "general": {"properties": {"a": {"type": "combo"}}}}"#,
    ];
    for c in cases {
        let _ = load_project(&files(&[("project.json", c)]), PropertyOverrides::new());
    }
    let scenes = [
        "{}",
        "[]",
        r#"{"camera": [], "general": [], "objects": []}"#,
        &scene_with(
            "",
            r#"1, null, "x", [], {"id": 1e300, "name": "x", "image": "models/fond.json", "effects": [1]}"#,
        ),
        &scene_with(
            "",
            r#"{"id": 1, "name": "x", "image": "models/fond.json", "effects": [{"file": "effects/blur/effect.json", "passes": [1]}]}"#,
        ),
        &scene_with(
            "",
            r#"{"id": 1, "name": "x", "text": {"value": "a", "scriptproperties": {"a": {"value": {"value": 1}}}, "script": "s"}}"#,
        ),
        &scene_with(
            r##""clearcolor": {"value": "#zz"}, "bloom": {"value": [], "user": []}"##,
            "",
        ),
    ];
    for s in scenes {
        if let Ok(mut p) = load_project(&base_files(s), PropertyOverrides::new()) {
            let _ = p.build(&base_files(s), &BuildOptions::new(0, 0));
            let _ = p.dump();
        }
    }
    // Deeply nested JSON is rejected by the parser's recursion limit, not by a stack overflow.
    let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    let _ = load_project(
        &base_files(&scene_with("", &deep)),
        PropertyOverrides::new(),
    );
}

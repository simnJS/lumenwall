//! Spec 09 (3D scenes) on the scene model: classification of model, light and sprite objects,
//! synthetic ids, perspective defaults, model material states, `usershadervalues`, camera paths
//! and projects without `type`. The real-content test (`--ignored`) is F-3D-R3.

mod common;

use common::*;
use glam::Vec3;
use lumenwall_scene::{
    BuildOptions, CullMode, DepthMode, LightKind, ObjectKind, Project, PropertyOverrides,
    load_project,
};

const PROJECT: &str = r#"{ "type": "scene", "title": "t", "file": "scene.json",
    "general": { "properties": {
        "carbodycolor": { "type": "color", "value": "1.0 0.0 0.0", "text": "c" } } } }"#;

fn scene(general: &str, objects: &str) -> String {
    format!(
        r#"{{ "camera": {{ "center": "0 0 -1", "eye": "0 0 0", "up": "0 1 0",
              "paths": ["scripts/camera_00.json"] }},
             "general": {{ {general} }}, "objects": [ {objects} ] }}"#
    )
}

fn load(scene_json: &str, extra: &[(&str, &str)]) -> Project {
    let mut entries = vec![("project.json", PROJECT), ("scene.json", scene_json)];
    entries.extend_from_slice(extra);
    load_project(&files(&entries), PropertyOverrides::new()).expect("loads")
}

const MATERIAL_PLAIN: &str = r#"{ "passes": [ { "shader": "generic" } ] }"#;
const MATERIAL_EXPLICIT: &str = r#"{ "passes": [ { "shader": "generic",
    "cullmode": "nocull", "depthtest": "disabled", "depthwriting": "disabled",
    "usershadervalues": { "carbodycolor": "paintcolor", "missing": "x" },
    "constantshadervalues": { "paintcolor": "0 1 0" } } ] }"#;

#[test]
fn classification_1_4() {
    let objects = r#"
        { "id": 1, "name": "m", "model": "models/a.mdl", "skin": 1, "reflected": true },
        { "id": 2, "name": "", "light": "point", "model": null, "particle": null, "sprite": null,
          "color": "0.890 0.690 0.086", "intensity": 0.18, "radius": 22.85 },
        { "id": 3, "name": "l2", "light": "lspot" },
        { "id": 4, "name": "s", "sprite": "materials/sun.json" },
        { "id": 5, "name": "e", "model": "", "light": null },
        { "id": 6, "name": "w", "light": "weird" },
        { "id": 7, "name": "p", "image": null, "model": null, "particle": "particles/x.json" }"#;
    let p = load(
        &scene("", objects),
        &[("materials/sun.json", MATERIAL_PLAIN)],
    );
    let s = p.scene.as_ref().unwrap();
    let ObjectKind::Model3d(m) = &s.objects[0].kind else {
        panic!("model expected");
    };
    assert_eq!(p.strings.resolve(m.file), "models/a.mdl");
    assert_eq!((m.skin, m.reflected, m.cast_shadow), (1, true, true));
    let ObjectKind::Light(l) = &s.objects[1].kind else {
        panic!("light expected");
    };
    assert_eq!(l.kind, LightKind::Point);
    assert_eq!(p.value(l.color).vec3(), Vec3::new(0.890, 0.690, 0.086));
    assert!((p.value(l.intensity).float() - 0.18).abs() < 1e-6);
    assert!((p.value(l.radius).float() - 22.85).abs() < 1e-5);
    assert_eq!(p.value(l.exponent).float(), 2.0);
    assert!(!l.cast_shadow);
    assert!(matches!(&s.objects[2].kind, ObjectKind::Light(l) if l.kind == LightKind::Spot));
    let ObjectKind::Sprite(sp) = &s.objects[3].kind else {
        panic!("sprite expected");
    };
    // Sprite materials get the model defaults (§1.11.2).
    let mp = &s.material(sp.material.unwrap()).passes[0];
    assert_eq!(
        (mp.cull_mode, mp.depth_test, mp.depth_write),
        (CullMode::Normal, DepthMode::Enabled, DepthMode::Enabled)
    );
    // Empty strings and nulls count as absent.
    assert!(matches!(s.objects[4].kind, ObjectKind::Generic(_)));
    assert!(matches!(s.objects[5].kind, ObjectKind::Generic(_)));
    assert!(matches!(s.objects[6].kind, ObjectKind::Particle(_)));
    assert_eq!(
        s.camera_paths
            .iter()
            .map(|p_| p.strings.resolve(*p_))
            .collect::<Vec<_>>(),
        ["scripts/camera_00.json"]
    );
}

#[test]
fn objects_without_id_get_synthetic_ids_1_8() {
    let objects = r#"
        { "model": "models/a.mdl" }, { "model": "models/b.mdl" },
        { "id": -1001, "name": "taken", "model": "models/c.mdl" },
        { "model": "models/d.mdl" }"#;
    let mut p = load(&scene("", objects), &[]);
    let ids: Vec<i32> = p
        .scene
        .as_ref()
        .unwrap()
        .objects
        .iter()
        .map(|o| o.id)
        .collect();
    // -1001 is taken by the file: the next free id is used.
    assert_eq!(ids, [-1000, -1002, -1001, -1003]);
    assert!(
        p.warnings()
            .iter()
            .all(|w| !w.message.contains("unreadable"))
    );
    let b = p
        .build(&files(&[]), &BuildOptions::new(1920, 1080))
        .unwrap();
    assert_eq!(b.creation_order.len(), 4, "every object is created");
}

#[test]
fn perspective_defaults_1_2() {
    let p = load(
        &scene(
            r#""bloom": true, "orthogonalprojection": null, "clearenabled": null"#,
            "",
        ),
        &[],
    );
    let s = p.scene.as_ref().unwrap();
    assert!(s.general.orthogonal_projection.is_none());
    assert_eq!(p.value(s.camera.fov).float(), 50.0);
    assert_eq!(p.value(s.camera.nearz).float(), 0.01);
    assert_eq!(p.value(s.camera.farz).float(), 10000.0);
    assert!(p.value(s.general.clear_enabled).boolean());
    // §3.10: bloom defaults of Wallpaper Engine.
    assert_eq!(p.value(s.general.bloom_strength).float(), 2.0);
    assert_eq!(p.value(s.general.bloom_threshold).float(), 0.65);
    // `general` planes win over the defaults; 2D scenes keep their own defaults.
    let p = load(
        &scene(
            r#""fov": 60, "nearz": 0.1, "farz": 100, "orthogonalprojection": { "width": 10, "height": 10 }, "bloom": true"#,
            "",
        ),
        &[],
    );
    let s = p.scene.as_ref().unwrap();
    assert_eq!(
        (
            p.value(s.camera.fov).float(),
            p.value(s.camera.nearz).float(),
            p.value(s.camera.farz).float()
        ),
        (60.0, 0.1, 100.0)
    );
    assert_eq!(p.value(s.general.bloom_strength).float(), 0.0);
    let p = load(
        &scene(
            r#""orthogonalprojection": { "width": 10, "height": 10 }"#,
            "",
        ),
        &[],
    );
    let s = p.scene.as_ref().unwrap();
    assert_eq!(p.value(s.camera.farz).float(), 1000.0);
}

#[test]
fn model_material_states_and_user_values_1_11() {
    let mut p = load(&scene("", ""), &[]);
    let src = files(&[
        ("materials/plain.json", MATERIAL_PLAIN),
        ("materials/explicit.json", MATERIAL_EXPLICIT),
    ]);
    let plain = p
        .load_model_material(&src, "materials/plain.json")
        .unwrap()
        .unwrap();
    let explicit = p
        .load_model_material(&src, "materials/explicit.json")
        .unwrap()
        .unwrap();
    assert_eq!(
        p.load_model_material(&src, "materials/none.json").unwrap(),
        None
    );
    let s = p.scene.as_ref().unwrap();
    let mp = &s.material(plain).passes[0];
    assert_eq!(
        (mp.cull_mode, mp.depth_test, mp.depth_write),
        (CullMode::Normal, DepthMode::Enabled, DepthMode::Enabled)
    );
    let mp = &s.material(explicit).passes[0];
    // Explicit values and the misspelt `depthwriting` alias (QO-3D-13).
    assert_eq!(
        (mp.cull_mode, mp.depth_test, mp.depth_write),
        (CullMode::NoCull, DepthMode::Disabled, DepthMode::Disabled)
    );
    assert_eq!(mp.user_constants.len(), 1, "missing property skipped");
    let c = mp.user_constants[0];
    assert_eq!(p.strings.resolve(c.name), "paintcolor");
    assert_eq!(p.value(c.setting).vec3(), Vec3::new(1.0, 0.0, 0.0));
    // Live: a property change reaches the setting.
    p.set_property_text("carbodycolor", "0.0 0.0 1.0").unwrap();
    assert_eq!(p.value(c.setting).vec3(), Vec3::new(0.0, 0.0, 1.0));
    assert!(
        p.warnings()
            .iter()
            .any(|w| w.message.contains("\"missing\" not found"))
    );
    // Image materials keep the spec 02 defaults.
    let image_src = files(&[
        ("project.json", PROJECT),
        (
            "scene.json",
            &scene("", r#"{ "id": 1, "name": "i", "image": "models/i.json" }"#),
        ),
        ("models/i.json", r#"{ "material": "materials/plain.json" }"#),
        ("materials/plain.json", MATERIAL_PLAIN),
    ]);
    let p = load_project(&image_src, PropertyOverrides::new()).unwrap();
    let s = p.scene.as_ref().unwrap();
    let mp = &s
        .material(
            s.model(s.objects[0].as_image().unwrap().model.unwrap())
                .material,
        )
        .passes[0];
    assert_eq!(
        (mp.cull_mode, mp.depth_test, mp.depth_write),
        (CullMode::NoCull, DepthMode::Disabled, DepthMode::Disabled)
    );
}

#[test]
fn project_without_type_is_a_scene_qo_3d_1() {
    let src = files(&[
        ("project.json", r#"{ "title": "t", "file": "techno.json" }"#),
        ("techno.json", &scene("", r#"{ "model": "models/a.mdl" }"#)),
    ]);
    let p = load_project(&src, PropertyOverrides::new()).unwrap();
    assert!(p.scene.is_some());
    // The type follows the file extension like Wallpaper Engine: a video file is a video.
    let src = files(&[("project.json", r#"{ "title": "t", "file": "a.mp4" }"#)]);
    let p = load_project(&src, PropertyOverrides::new()).unwrap();
    assert_eq!(p.kind, lumenwall_scene::ProjectKind::Video);
}

/// F-3D-R3: each installed 3D scene creates the expected number of model objects, without a
/// scene failure, `audiophile` and `techno` included.
#[test]
#[ignore = "needs LUMENWALL_WE_ASSETS (docs/TESTING.md)"]
fn installed_3d_scenes_f_3d_r3() {
    let assets = assets_dir();
    let projects = assets.parent().unwrap().join("projects/defaultprojects");
    let expected = [
        ("arsenal", 1, 2, 0),
        ("audiophile", 4, 0, 0),
        ("demon_core", 2, 1, 0),
        ("dna_fragment", 5, 0, 0),
        ("fantasticcar", 4, 0, 0),
        ("neon_sunset", 2, 0, 0),
        ("ricepod", 6, 0, 1),
        ("techno", 4, 0, 0),
        ("retro", 1, 0, 0),
    ];
    for (name, models, lights, sprites) in expected {
        let src = Layers(vec![
            Layer::Dir(projects.join(name)),
            Layer::Dir(assets.clone()),
        ]);
        let mut p =
            load_project(&src, PropertyOverrides::new()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let b = p
            .build(&src, &BuildOptions::new(1920, 1080))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let s = p.scene.as_ref().unwrap();
        let count = |f: fn(&ObjectKind) -> bool| {
            s.file_objects()
                .iter()
                .enumerate()
                .filter(|(i, o)| f(&o.kind) && b.created[*i])
                .count()
        };
        assert_eq!(
            count(|k| matches!(k, ObjectKind::Model3d(_))),
            models,
            "{name}: models"
        );
        assert_eq!(
            count(|k| matches!(k, ObjectKind::Light(_))),
            lights,
            "{name}: lights"
        );
        assert_eq!(
            count(|k| matches!(k, ObjectKind::Sprite(_))),
            sprites,
            "{name}: sprites"
        );
        eprintln!("{name}: {} objects created", b.creation_order.len());
    }
}

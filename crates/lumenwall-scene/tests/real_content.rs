//! Tests on real Wallpaper Engine content (docs/TESTING.md). Ignored by default; run with
//! `cargo test -p lumenwall-scene -- --ignored`.

mod common;

use std::path::PathBuf;

use common::{Layer, Layers, assets_dir, env_path, read_pkg, workshop_dir};
use lumenwall_scene::{
    BuildOptions, LoadContext, ObjectKind, ParticleSource, ProjectKind, PropertyKind,
    PropertyOverrides,
};

fn testdata_wallpaper() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/we/3018340632")
}

#[test]
#[ignore = "needs the reference wallpaper and Wallpaper Engine assets (docs/TESTING.md)"]
fn reference_wallpaper_3018340632() {
    let unpacked = testdata_wallpaper();
    let installed = env_path(
        "LUMENWALL_TEST_WALLPAPER",
        &workshop_dir().join("3018340632").to_string_lossy(),
    );
    let src = Layers(vec![
        Layer::Dir(unpacked),
        Layer::Dir(installed),
        Layer::Dir(assets_dir()),
    ]);

    let mut cx = LoadContext::new(PropertyOverrides::new());
    let mut project = cx.load(&src).expect("project loads");
    for w in project.warnings() {
        eprintln!("load warning: {w}");
    }
    assert_eq!(project.kind, ProjectKind::Scene);
    assert_eq!(project.workshop_id, "3018340632");

    let ash = project.property("ash").expect("ash");
    let rain = project.property("rain").expect("rain");
    let scheme = project.property("schemecolor").expect("schemecolor");
    assert_eq!(*project.properties.get(ash).kind(), PropertyKind::Bool);
    assert!(!project.properties.get(ash).value().boolean());
    assert!(project.properties.get(rain).value().boolean());
    assert_eq!(*project.properties.get(scheme).kind(), PropertyKind::Color);
    assert!((project.properties.get(scheme).value().vec4().x - 0.160_784_3).abs() < 1e-5);
    print!("{}", project.list_properties());

    let scene = project.scene.as_ref().expect("scene");
    // nearz / farz are written in `general` (Q5).
    assert!((project.value(scene.camera.nearz).float() - 0.01).abs() < 1e-6);
    assert_eq!(project.value(scene.camera.farz).float(), 10000.0);
    let ortho = scene.general.orthogonal_projection.expect("orthographic");
    assert_eq!((ortho.width, ortho.height), (3440, 1440));
    assert!(project.value(scene.general.bloom).boolean());

    let mut images = 0;
    let mut particles = Vec::new();
    let mut sounds = 0;
    for o in scene.file_objects() {
        match &o.kind {
            ObjectKind::Image(img) => {
                images += 1;
                assert!(img.model.is_some(), "model loaded");
                assert_eq!(img.effects.len(), 11, "all 11 effects loaded");
                let names: Vec<&str> = img
                    .effects
                    .iter()
                    .map(|e| project.strings.resolve(e.file))
                    .collect();
                assert_eq!(names.iter().filter(|n| n.contains("shake")).count(), 3);
                assert_eq!(
                    names.iter().filter(|n| n.contains("foliagesway")).count(),
                    2
                );
                // depthparallax is hidden.
                assert!(!project.value(img.effects[0].visible).boolean());
                assert_eq!(img.size.x, 3440.0);
            }
            ObjectKind::Particle(p) => {
                let ParticleSource::File(path) = p.source else {
                    panic!("particle file expected")
                };
                particles.push(project.strings.resolve(path).to_owned());
                assert!(p.instance_override.is_some());
                let bound = project
                    .settings
                    .meta(p.visible)
                    .property
                    .expect("visibility bound");
                assert!(bound == ash || bound == rain);
                assert_eq!(project.value(p.visible).boolean(), bound == rain);
            }
            ObjectKind::Sound(s) => {
                sounds += 1;
                assert_eq!(s.files.len(), 5);
                assert!(!s.looping);
            }
            _ => {}
        }
    }
    assert_eq!(images, 1);
    particles.sort();
    assert_eq!(
        particles,
        [
            "particles/presets/ash.json",
            "particles/presets/ash.json",
            "particles/presets/rainperspective.json",
            "particles/presets/rainperspective.json"
        ]
    );
    assert_eq!(sounds, 1);

    let build = project
        .build(&src, &BuildOptions::new(3440, 1440))
        .expect("build");
    for w in project.warnings() {
        eprintln!("warning: {w}");
    }
    assert_eq!(build.size, glam::Vec2::new(3440.0, 1440.0));
    assert_eq!(
        build.creation_order.len(),
        6 + usize::from(build.bloom.is_some())
    );
    let image = build.render_order[0];
    let passes = build.image_passes[image.index()]
        .as_ref()
        .expect("image passes");
    // Model pass + passes of the 10 visible effects.
    assert!(passes.passes.len() > 10, "{} passes", passes.passes.len());
    assert!(passes.draws);
    if let Some(bloom) = build.bloom {
        assert!((bloom.strength - 4.58).abs() < 1e-5);
        assert_eq!(build.render_order.last(), Some(&bloom.object));
        assert_eq!(build.render_targets.len(), 3);
    }

    // Toggling a property reaches the bound visibilities: group and particle settings of both
    // ash systems (§7.2: re-read settings are distinct but bound to the same property).
    let updated = project
        .set_property_text("ash", "true")
        .expect("ash update")
        .len();
    assert_eq!(updated, 4);
    print!("{}", project.dump());
}

#[test]
#[ignore = "needs the Wallpaper Engine workshop folder (docs/TESTING.md)"]
fn all_workshop_scenes_load_and_build() {
    let assets = assets_dir();
    let Ok(dirs) = std::fs::read_dir(workshop_dir()) else {
        panic!("workshop folder not found: {}", workshop_dir().display());
    };
    let mut dirs: Vec<PathBuf> = dirs.filter_map(|d| d.ok().map(|d| d.path())).collect();
    dirs.sort();
    let mut loaded = 0;
    let mut failures = Vec::new();
    let mut cx = LoadContext::new(PropertyOverrides::new());
    for dir in dirs {
        let Ok(bytes) = std::fs::read(dir.join("project.json")) else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let is_scene = v
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.eq_ignore_ascii_case("scene"));
        if !is_scene {
            continue;
        }
        let mut layers = vec![Layer::Dir(dir.clone())];
        for pkg in ["scene.pkg", "gifscene.pkg"] {
            if let Some(m) = read_pkg(&dir.join(pkg)) {
                layers.push(Layer::Pkg(m));
            }
        }
        layers.push(Layer::Dir(assets.clone()));
        let src = Layers(layers);
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match cx.load(&src) {
            Ok(mut p) => match p.build(&src, &BuildOptions::new(1920, 1080)) {
                Ok(b) => {
                    loaded += 1;
                    let mut kinds = std::collections::BTreeMap::<&str, usize>::new();
                    let mut effects = 0;
                    for o in p.scene.as_ref().map_or(&[][..], |s| s.file_objects()) {
                        let k = match &o.kind {
                            ObjectKind::Image(i) => {
                                effects += i.effects.len();
                                "image"
                            }
                            ObjectKind::Sound(_) => "sound",
                            ObjectKind::Particle(_) => "particle",
                            ObjectKind::Text(_) => "text",
                            ObjectKind::Generic(_) => "generic",
                        };
                        *kinds.entry(k).or_default() += 1;
                    }
                    eprintln!(
                        "{name}: {kinds:?}, {effects} effects, {} created, bloom {}, {} settings, {} scripts, {} warnings",
                        b.creation_order.len(),
                        b.bloom.is_some(),
                        p.settings.len(),
                        p.settings.scripts().len(),
                        p.warnings().len()
                    );
                    for w in p.warnings() {
                        eprintln!("  {w}");
                    }
                }
                Err(e) => failures.push(format!("{name}: build: {e}")),
            },
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    eprintln!("{loaded} scene wallpapers loaded");
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
    assert!(loaded > 0, "no scene wallpaper found");
}

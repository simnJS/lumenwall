//! Tests against real Wallpaper Engine content (see `docs/TESTING.md`).
//!
//! The content is copyrighted: it is only read from its local location at run time, never copied
//! into the repository or into fixtures. Run with `cargo test -p lumenwall-particles -- --ignored`.

use glam::Vec2;
use lumenwall_particles::{
    ChildAssets, EmitterKind, FrameInput, InitializerDef, ParticleDefinition, ParticleObject,
    ParticleSystem, SystemOptions, UniformInput, Value,
};
use serde_json::Value as Json;
use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

const FPS: f32 = 30.0;
const SECONDS: f32 = 10.0;

/// Unpacked copy of the reference wallpaper 3018340632 (gitignored).
fn reference_wallpaper() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/we/3018340632")
}

fn read_json(path: &Path) -> Json {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Child definitions resolve relative to the folder that holds `particles/` (preset or scene root).
fn child_loader(definition: &Path) -> impl FnMut(&str) -> Option<ChildAssets> + use<> {
    let s = definition.to_string_lossy();
    let root = s.find("/particles/").map_or_else(
        || definition.parent().unwrap_or(Path::new(".")).to_path_buf(),
        |i| PathBuf::from(&s[..i]),
    );
    move |name: &str| {
        let text = std::fs::read_to_string(root.join(name)).ok()?;
        let json: Json = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
        Some(ChildAssets {
            definition: ParticleDefinition::from_json(&json),
            ..ChildAssets::default()
        })
    }
}

#[derive(Debug, Default)]
struct Stats {
    capacity: usize,
    peak: usize,
    last: usize,
    last_quads: usize,
    peak_children: usize,
    child_particles: usize,
}

/// Checks the invariants of one system (and recursively of its children).
fn check(sys: &ParticleSystem, k: usize) -> Result<usize, String> {
    let n = sys.len();
    if n > sys.capacity() {
        return Err(format!(
            "frame {k}: {n} particles > capacity {}",
            sys.capacity()
        ));
    }
    let p = sys.particles();
    if let Some(i) = p.positions.iter().position(|v| !v.is_finite()) {
        return Err(format!(
            "frame {k}: particle {i} has a non-finite position {}",
            p.positions[i]
        ));
    }
    if let Some(i) = p.ages.iter().zip(p.lifetimes).position(|(a, l)| a >= l) {
        return Err(format!("frame {k}: expired particle {i} still alive"));
    }
    let mut total = 0;
    for c in sys.children() {
        if !c.is_child() {
            return Err("child without attachment".into());
        }
        total += c.len() + check(c, k)?;
    }
    Ok(total)
}

/// Simulates `SECONDS` at `FPS` (children loaded through `loader`), writing the vertices and
/// uniforms of the system and of its children every frame, and checks the invariants.
fn simulate(
    def: &ParticleDefinition,
    object: &ParticleObject,
    width: f32,
    height: f32,
    loader: &mut dyn FnMut(&str) -> Option<ChildAssets>,
) -> Result<Stats, String> {
    let opts = SystemOptions {
        width,
        height,
        seed: Some(42),
        ..SystemOptions::default()
    };
    let mut sys = ParticleSystem::new_with_children(def, object, &opts, loader);
    let mut vertices = Vec::new();
    let mut stats = Stats {
        capacity: sys.capacity(),
        ..Stats::default()
    };
    let uniform_input = UniformInput {
        width,
        height,
        ..UniformInput::default()
    };
    let frames = (SECONDS * FPS) as usize;
    for k in 1..=frames {
        let t = k as f32 / FPS;
        let mouse = Vec2::new(0.5 + 0.3 * (t * 0.7).sin(), 0.5 + 0.3 * (t * 0.4).cos());
        let input = FrameInput {
            time: t,
            width,
            height,
            mouse,
        };
        let draw = sys.update(&input);
        stats.peak = stats.peak.max(sys.len());
        if draw {
            stats.last_quads = sys.write_vertices_vec(&mut vertices);
            if stats.last_quads > sys.max_quads() {
                return Err(format!(
                    "frame {k}: {} quads > max {}",
                    stats.last_quads,
                    sys.max_quads()
                ));
            }
            let parent = sys.uniforms(&uniform_input);
            let mut bad = None;
            sys.for_each_child_mut(&mut |c| {
                let q = c.write_vertices_vec(&mut vertices);
                let u = c.child_uniforms(&uniform_input, &parent.model);
                if q > c.max_quads() || !u.model.is_finite() {
                    bad = Some(format!(
                        "frame {k}: child {q} quads / max {}",
                        c.max_quads()
                    ));
                }
            });
            if let Some(e) = bad {
                return Err(e);
            }
        }
        stats.child_particles = check(&sys, k)?;
        stats.peak_children = stats.peak_children.max(sys.child_count());
    }
    stats.last = sys.len();
    Ok(stats)
}

/// The particle presets of the reference wallpaper, with their scene objects (instance
/// overrides, transforms) and the scene's 3440×1440 projection.
#[test]
#[ignore = "needs the local copy of wallpaper 3018340632 (docs/TESTING.md)"]
fn reference_wallpaper_presets() {
    let root = reference_wallpaper();
    let scene = read_json(&root.join("scene.json"));
    let proj = &scene["general"]["orthogonalprojection"];
    let (w, h) = (
        proj["width"].as_f64().unwrap() as f32,
        proj["height"].as_f64().unwrap() as f32,
    );
    let mut seen = 0;
    for obj_json in scene["objects"].as_array().unwrap() {
        let Some(path) = obj_json.get("particle").and_then(Json::as_str) else {
            continue;
        };
        seen += 1;
        let def_path = root.join(path);
        let def: ParticleDefinition = serde_json::from_value(read_json(&def_path)).unwrap();
        let mut object = ParticleObject::from_json(obj_json);
        let name = format!("{path} (object {})", obj_json["id"]);
        let mut loader = child_loader(&def_path);
        let as_authored =
            simulate(&def, &object, w, h, &mut loader).unwrap_or_else(|e| panic!("{name}: {e}"));
        if !object.visible.bool() {
            // Hidden by its user property: no simulation at all (and no pre-simulation, A7.1).
            assert_eq!(as_authored.peak, 0, "{name}");
        }
        object.visible.set(Value::Bool(true));
        let s =
            simulate(&def, &object, w, h, &mut loader).unwrap_or_else(|e| panic!("{name}: {e}"));
        let over = &object.instance_override;
        let rate: f32 = def.emitters.iter().map(|e| e.rate * over.rate.f32()).sum();
        let (lo, hi) = last_lifetime_range(&def);
        let mean_life = (lo + hi) / 2.0 * over.lifetime.f32();
        // A1: the pre-simulation adds `starttime` seconds of history.
        let elapsed = SECONDS + def.start_time_secs;
        let expected = (rate * mean_life.min(elapsed)).min(s.capacity as f32);
        eprintln!(
            "{name}: starttime {} capacity {} peak {} last {} quads {} (expected ≈ {expected:.0})",
            def.start_time_secs, s.capacity, s.peak, s.last, s.last_quads
        );
        assert!(
            (s.last as f32) > expected * 0.75 && (s.last as f32) < expected * 1.25 + 2.0,
            "{name}: {} particles, expected ≈ {expected}",
            s.last
        );
        assert_eq!(s.last_quads, s.last, "{name}: every particle is drawable");
        if def.start_time_secs > 0.0 {
            // Pre-simulated: particles already on screen at the first drawn frame.
            let opts = SystemOptions {
                width: w,
                height: h,
                seed: Some(1),
                ..SystemOptions::default()
            };
            let mut sys = ParticleSystem::new(&def, &object, &opts);
            sys.update(&FrameInput {
                time: 1.0,
                width: w,
                height: h,
                mouse: Vec2::splat(0.5),
            });
            let expected_now = rate * def.start_time_secs;
            assert!(
                (sys.len() as f32 - expected_now).abs() <= expected_now * 0.25 + 2.0,
                "{name}: {} particles at the first frame, expected ≈ {expected_now}",
                sys.len()
            );
        }
    }
    assert_eq!(seen, 4, "the reference scene has 4 particle systems");
}

/// `(min, max)` of the last `lifetimerandom` (it decides the lifetime), `(1, 1)` without one.
fn last_lifetime_range(def: &ParticleDefinition) -> (f32, f32) {
    def.initializers
        .iter()
        .rev()
        .find_map(|i| match i {
            InitializerDef::LifetimeRandom { min, max, .. } => Some((min.f32(), max.f32())),
            _ => None,
        })
        .unwrap_or((1.0, 1.0))
}

/// True when the definition is expected to spawn at least one particle within the run.
fn expects_particles(def: &ParticleDefinition) -> bool {
    let (lo, hi) = last_lifetime_range(def);
    if lo.max(hi) <= 0.0 {
        return false;
    }
    let frame = 1.0 / FPS;
    def.emitters.iter().any(|e| {
        let window = SECONDS + def.start_time_secs - 2.0 * frame - e.delay.max(0.0);
        let periodic_start = e.flags & 4 == 0 || e.max_periodic_delay < window;
        e.kind != EmitterKind::LayerImage
            && window > 0.0
            && periodic_start
            && (e.instantaneous > 0
                || e.rate
                    * window.min(if e.duration > 0.0 {
                        e.duration
                    } else {
                        f32::MAX
                    })
                    >= 1.0)
    })
}

fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

/// Every particle definition shipped in the Wallpaper Engine `assets` folder (`particles/`,
/// `presets/*/particles/`, `scenes/particleelementpreviews/*/particles/`, …): a JSON object with an
/// `emitter` array and a `material`. Children are loaded from their preset folder.
#[test]
#[ignore = "needs LUMENWALL_WE_ASSETS (docs/TESTING.md)"]
fn assets_particle_definitions() {
    let assets = PathBuf::from(
        std::env::var_os("LUMENWALL_WE_ASSETS").expect("LUMENWALL_WE_ASSETS is not set"),
    );
    let mut files = Vec::new();
    collect_json(&assets, &mut files);
    files.sort();
    let (mut checked, mut in_presets, mut with_children, mut with_prewarm) = (0, 0, 0, 0);
    let mut unique = HashSet::new();
    let mut failures = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        // Some non-particle files of the assets folder are not strict JSON: skip them.
        let Ok(json) = serde_json::from_str::<Json>(text.trim_start_matches('\u{feff}')) else {
            continue;
        };
        if !(json.get("emitter").is_some_and(Json::is_array) && json.get("material").is_some()) {
            continue;
        }
        checked += 1;
        unique.insert(json.to_string());
        let rel = path
            .strip_prefix(&assets)
            .unwrap_or(path)
            .display()
            .to_string();
        in_presets += usize::from(rel.starts_with("presets/"));
        let result = catch_unwind(AssertUnwindSafe(|| -> Result<(usize, bool), String> {
            let def: ParticleDefinition = serde_json::from_str(text.trim_start_matches('\u{feff}'))
                .map_err(|e| e.to_string())?;
            let centred = serde_json::json!({"origin": "960 540 0"});
            let overridden = serde_json::json!({
                "origin": "960 540 0",
                "instanceoverride": {"count": 0.5, "rate": 2, "speed": 1.5, "lifetime": 0.8, "size": 1.2,
                    "alpha": 0.7, "brightness": 2, "controlpoint1": "100 50 0"}
            });
            let mut children = 0;
            for obj in [centred, overridden] {
                let mut loader = child_loader(path);
                let s = simulate(
                    &def,
                    &ParticleObject::from_json(&obj),
                    1920.0,
                    1080.0,
                    &mut loader,
                )?;
                if expects_particles(&def) && s.peak == 0 {
                    return Err(format!("no particle spawned (capacity {})", s.capacity));
                }
                children = children.max(s.peak_children);
            }
            Ok((children, def.start_time_secs > 0.0))
        }));
        match result {
            Ok(Ok((children, prewarm))) => {
                with_children += usize::from(children > 0);
                with_prewarm += usize::from(prewarm);
            }
            Ok(Err(e)) => failures.push(format!("{rel}: {e}")),
            Err(_) => failures.push(format!("{rel}: panicked")),
        }
    }
    eprintln!(
        "{checked} particle definitions simulated ({} unique, {in_presets} under presets/, \
         {with_children} with live children, {with_prewarm} pre-simulated), {} failures",
        unique.len(),
        failures.len()
    );
    assert!(
        checked >= 6,
        "expected the particle presets of the assets folder, found {checked}"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

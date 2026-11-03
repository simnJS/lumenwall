//! Frame cost of a typical 10 000-particle system: update (emit, age, operators, compaction) plus
//! sprite vertex generation, in steady state (pool full, continuous deaths and respawns).

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use glam::Vec2;
use lumenwall_particles::{
    FrameInput, ParticleDefinition, ParticleObject, ParticleSystem, SystemOptions,
};
use serde_json::json;
use std::hint::black_box;

const COUNT: usize = 10_000;
const DT: f32 = 1.0 / 60.0;

fn typical_definition(extra_operators: &[serde_json::Value]) -> ParticleDefinition {
    let mut operators = vec![
        json!({"name": "movement", "gravity": "0 -40 0", "drag": 0.2}),
        json!({"name": "angularmovement", "drag": 0.1}),
        json!({"name": "alphafade", "fadeintime": 0.1, "fadeouttime": 0.8}),
        json!({"name": "sizechange", "startvalue": 1, "endvalue": 0.2}),
        json!({"name": "colorchange", "startvalue": "1 1 1", "endvalue": "1 0.4 0.2"}),
    ];
    operators.extend_from_slice(extra_operators);
    ParticleDefinition::from_json(&json!({
        "material": "materials/particle/spark.json",
        "maxcount": COUNT,
        "emitter": [{"name": "boxrandom", "rate": 6000, "distancemax": "960 540 0"}],
        "initializer": [
            {"name": "colorrandom", "min": "200 120 40", "max": "255 255 255"},
            {"name": "sizerandom", "min": 4, "max": 24},
            {"name": "alpharandom", "min": 0.4, "max": 1},
            {"name": "lifetimerandom", "min": 2, "max": 3},
            {"name": "velocityrandom", "min": "-40 -40 0", "max": "40 40 0"},
            {"name": "rotationrandom"},
            {"name": "angularvelocityrandom"}
        ],
        "operator": operators,
        "renderer": [{"name": "sprite"}]
    }))
}

/// Builds the system and runs it until the pool is full and respawning (steady state).
fn warmed(def: &ParticleDefinition) -> (ParticleSystem, f32) {
    let opts = SystemOptions {
        width: 1920.0,
        height: 1080.0,
        seed: Some(7),
        ..SystemOptions::default()
    };
    let mut sys = ParticleSystem::new(def, &ParticleObject::default(), &opts);
    let mut t = 1.0;
    for _ in 0..300 {
        t += DT;
        sys.update(&frame(t));
    }
    assert!(
        sys.len() > COUNT * 9 / 10,
        "warm-up should fill the pool, got {}",
        sys.len()
    );
    (sys, t)
}

fn frame(t: f32) -> FrameInput {
    FrameInput {
        time: t,
        width: 1920.0,
        height: 1080.0,
        mouse: Vec2::new(0.5, 0.5),
    }
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("particles_10k");
    g.throughput(Throughput::Elements(COUNT as u64));

    let cases = [
        ("typical", typical_definition(&[])),
        (
            "typical+turbulence",
            typical_definition(&[json!({"name": "turbulence"})]),
        ),
        (
            "typical+boids50",
            typical_definition(&[json!({"name": "boids", "neighborthreshold": 50,
                "separationthreshold": 20, "cohesionfactor": 4, "alignmentfactor": 5})]),
        ),
        (
            "typical+collisions",
            typical_definition(&[
                json!({"name": "collisionplane", "distance": -400, "bouncefactor": 0.7}),
                json!({"name": "collisionsphere", "collisionbehavior": "slide", "radius": 150}),
                json!({"name": "collisionbounds"}),
            ]),
        ),
        (
            "typical+blended_cap",
            typical_definition(&[json!({"name": "capvelocity", "maxspeed": 30,
                "blendinstart": 0.2, "blendinend": 0.6})]),
        ),
    ];
    for (name, def) in &cases {
        let (mut sys, mut t) = warmed(def);
        let mut vertices = Vec::with_capacity(sys.max_quads() * sys.quad_floats());
        g.bench_function(BenchmarkId::new("update+vertices", name), |b| {
            b.iter(|| {
                t += DT;
                sys.update(black_box(&frame(t)));
                black_box(sys.write_vertices_vec(&mut vertices));
            });
        });
    }

    real_rain(&mut g);

    let (mut sys, mut t) = warmed(&cases[0].1);
    g.bench_function("update_only/typical", |b| {
        b.iter(|| {
            t += DT;
            black_box(sys.update(black_box(&frame(t))));
        });
    });
    let mut vertices = Vec::with_capacity(sys.max_quads() * sys.quad_floats());
    g.bench_function("vertices_only/typical", |b| {
        b.iter(|| black_box(sys.write_vertices_vec(&mut vertices)));
    });
    g.finish();
}

/// `rainperspective.json` of the reference wallpaper 3018340632 with its scene object (instance
/// override, 3440×1440 projection), read from the local, gitignored copy (docs/TESTING.md). The
/// content is never copied into the repository; the case is skipped when it is absent.
fn real_rain(g: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/we/3018340632");
    let read = |p: &std::path::Path| -> Option<serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    };
    let (Some(scene), Some(preset)) = (
        read(&root.join("scene.json")),
        read(&root.join("particles/presets/rainperspective.json")),
    ) else {
        eprintln!(
            "skipping rainperspective: reference wallpaper not found under {}",
            root.display()
        );
        return;
    };
    let Some(object) = scene["objects"].as_array().and_then(|objs| {
        objs.iter()
            .find(|o| o["particle"] == "particles/presets/rainperspective.json")
    }) else {
        eprintln!("skipping rainperspective: no scene object uses it");
        return;
    };
    let (w, h) = (3440.0, 1440.0);
    let def = ParticleDefinition::from_json(&preset);
    let opts = SystemOptions {
        width: w,
        height: h,
        seed: Some(7),
        ..SystemOptions::default()
    };
    let mut sys = ParticleSystem::new(&def, &ParticleObject::from_json(object), &opts);
    let frame = |t: f32| FrameInput {
        time: t,
        width: w,
        height: h,
        mouse: Vec2::new(0.5, 0.5),
    };
    let mut t = 1.0;
    for _ in 0..300 {
        t += DT;
        sys.update(&frame(t));
    }
    let mut vertices = Vec::with_capacity(sys.max_quads() * sys.quad_floats());
    g.throughput(Throughput::Elements(sys.len() as u64));
    g.bench_function("update+vertices/rainperspective_real", |b| {
        b.iter(|| {
            t += DT;
            sys.update(black_box(&frame(t)));
            black_box(sys.write_vertices_vec(&mut vertices));
        });
    });
    g.throughput(Throughput::Elements(COUNT as u64));
}

/// Boids at the size WE presets use it (the boids preview scene: 250 particles).
fn bench_boids_small(c: &mut Criterion) {
    let def = ParticleDefinition::from_json(&json!({
        "maxcount": 250,
        "emitter": [{"name": "sphererandom", "rate": 100, "distancemax": 400}],
        "initializer": [{"name": "lifetimerandom", "min": 3, "max": 4},
                        {"name": "velocityrandom", "min": "-40 -40 0", "max": "40 40 0"}],
        "operator": [{"name": "movement"}, {"name": "boids", "neighborthreshold": 150,
            "separationfactor": 25, "cohesionfactor": 4, "alignmentfactor": 5}]
    }));
    let opts = SystemOptions {
        width: 1920.0,
        height: 1080.0,
        seed: Some(7),
        ..SystemOptions::default()
    };
    let mut sys = ParticleSystem::new(&def, &ParticleObject::default(), &opts);
    let mut t = 1.0;
    for _ in 0..300 {
        t += DT;
        sys.update(&frame(t));
    }
    let mut g = c.benchmark_group("particles_250");
    g.throughput(Throughput::Elements(sys.len() as u64));
    let mut vertices = Vec::with_capacity(sys.max_quads() * sys.quad_floats());
    g.bench_function("update+vertices/boids150", |b| {
        b.iter(|| {
            t += DT;
            sys.update(black_box(&frame(t)));
            black_box(sys.write_vertices_vec(&mut vertices));
        });
    });
    g.finish();
}

criterion_group!(benches, bench, bench_boids_small);
criterion_main!(benches);

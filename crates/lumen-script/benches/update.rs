//! Per-frame cost of property scripts: `ScriptEngine::tick` with 100 `update()` scripts.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lumen_script::{
    Clock, LayerId, ObjectRef, PropertyScript, SceneHost, ScriptConfig, ScriptEngine, Value,
};

const SCRIPTS: usize = 100;

/// Allocation-free host: one `alpha` (Float) and one `origin` (Vec3) per layer.
struct BenchHost {
    clock: Clock,
    alpha: Vec<f32>,
    origin: Vec<[f32; 3]>,
}

impl BenchHost {
    fn new() -> Self {
        BenchHost {
            clock: Clock {
                runtime: 0.0,
                frametime: 1.0 / 60.0,
                time_of_day: 0.5,
            },
            alpha: vec![1.0; SCRIPTS],
            origin: vec![[0.0, 0.0, 0.0]; SCRIPTS],
        }
    }
}

impl SceneHost for BenchHost {
    fn clock(&self) -> Clock {
        self.clock
    }

    fn get(&self, obj: ObjectRef, name: &str) -> Option<Value> {
        let ObjectRef::Layer(id) = obj else {
            return None;
        };
        let i = id as usize;
        match name {
            "alpha" => self.alpha.get(i).map(|a| Value::Float(*a)),
            "origin" => self.origin.get(i).map(|o| Value::Vec3(*o)),
            _ => None,
        }
    }

    fn set(&mut self, obj: ObjectRef, name: &str, value: &Value) -> bool {
        let ObjectRef::Layer(id) = obj else {
            return false;
        };
        let i = id as usize;
        match name {
            "alpha" => self.alpha[i] = value.float(),
            "origin" => self.origin[i] = value.vec3(),
            _ => return false,
        }
        true
    }

    fn layer_exists(&self, id: LayerId) -> bool {
        (id as usize) < SCRIPTS
    }

    fn find_layer(&self, _name: &str) -> Option<LayerId> {
        None
    }

    fn layer_count(&self) -> usize {
        SCRIPTS
    }

    fn layer_at(&self, index: usize) -> Option<LayerId> {
        (index < SCRIPTS).then_some(index as LayerId)
    }
}

fn engine_with(property: &str, source: &str) -> (ScriptEngine, BenchHost) {
    let mut host = BenchHost::new();
    let mut engine = ScriptEngine::new(ScriptConfig::default()).expect("engine");
    for layer in 0..SCRIPTS as LayerId {
        engine
            .load_property_script(
                &mut host,
                &PropertyScript {
                    layer,
                    property,
                    object: None,
                    source,
                    binding: 0,
                    script_properties: &[],
                },
            )
            .expect("script loads");
    }
    (engine, host)
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("tick");

    let (mut engine, mut host) =
        engine_with("alpha", "export function update(value) { return value; }");
    group.bench_function("100_trivial_scalar_updates", |b| {
        b.iter(|| {
            host.clock.runtime += 1.0 / 60.0;
            engine.tick(black_box(&mut host));
        })
    });
    drop(engine);

    let (mut engine, mut host) = engine_with(
        "origin",
        "export function update(value) { value.y = Math.sin(engine.runtime) * 10; return value; }",
    );
    group.bench_function("100_vec3_updates", |b| {
        b.iter(|| {
            host.clock.runtime += 1.0 / 60.0;
            engine.tick(black_box(&mut host));
        })
    });
    drop(engine);

    let mut host = BenchHost::new();
    let mut empty = ScriptEngine::new(ScriptConfig::default()).expect("engine");
    group.bench_function("no_scripts", |b| {
        b.iter(|| empty.tick(black_box(&mut host)))
    });
    group.finish();

    c.bench_function("engine_new", |b| {
        b.iter(|| black_box(ScriptEngine::new(ScriptConfig::default()).expect("engine")))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);

//! The steady-state frame (update + vertex generation) must not touch the heap.

use glam::{Mat4, Vec2};
use lumen_particles::{
    ChildAssets, DepthRange, FrameInput, ParticleDefinition, ParticleObject, ParticleSystem,
    SystemOptions, UniformInput,
};
use serde_json::json;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: forwards to the system allocator; only adds a thread-local counter (const-initialised,
// so reading it never allocates).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.with(|c| c.set(c.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.with(|c| c.set(c.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations() -> usize {
    ALLOCS.with(Cell::get)
}

fn frame(t: f32) -> FrameInput {
    FrameInput {
        time: t,
        width: 1280.0,
        height: 720.0,
        mouse: Vec2::new(0.3, 0.6),
    }
}

fn run_steady_state(def: serde_json::Value, object: serde_json::Value) {
    run_with_children(def, object, None);
}

fn run_with_children(
    def: serde_json::Value,
    object: serde_json::Value,
    child: Option<serde_json::Value>,
) {
    let def = ParticleDefinition::from_json(&def);
    let object = ParticleObject::from_json(&object);
    let mut loader = |_: &str| {
        child.as_ref().map(|c| ChildAssets {
            definition: ParticleDefinition::from_json(c),
            ..ChildAssets::default()
        })
    };
    let opts = SystemOptions {
        width: 1280.0,
        height: 720.0,
        seed: Some(3),
        ..SystemOptions::default()
    };
    let mut sys = ParticleSystem::new_with_children(&def, &object, &opts, &mut loader);
    let mut vertices = Vec::new();
    let uniforms_in = UniformInput {
        width: 1280.0,
        height: 720.0,
        scene_view_projection: Mat4::IDENTITY,
        fov_degrees: 50.0,
        near: 0.01,
        far: 10_000.0,
        depth: DepthRange::ZeroToOne,
        parallax: None,
        brightness: 1.0,
    };
    let mut t = 1.0;
    // Warm-up: fill the pool and let the vertex Vec reach its size.
    for _ in 0..240 {
        t += 1.0 / 60.0;
        if sys.update(&frame(t)) {
            sys.write_vertices_vec(&mut vertices);
            sys.for_each_child_mut(&mut |c| {
                c.write_vertices_vec(&mut vertices);
            });
        }
    }
    assert!(!sys.is_empty());
    if child.is_some() {
        assert!(
            sys.child_count() > 1,
            "event children must be active during the measure"
        );
    }
    let before = allocations();
    assert!(
        before > 0,
        "the counting allocator must see the warm-up allocations"
    );
    for _ in 0..240 {
        t += 1.0 / 60.0;
        if sys.update(&frame(t)) {
            std::hint::black_box(sys.write_vertices_vec(&mut vertices));
            let parent = sys.uniforms(&uniforms_in);
            sys.for_each_child_mut(&mut |c| {
                std::hint::black_box(c.write_vertices_vec(&mut vertices));
                std::hint::black_box(c.child_uniforms(&uniforms_in, &parent.model));
            });
        }
    }
    assert_eq!(allocations() - before, 0, "steady-state frames allocated");
}

#[test]
fn steady_state_frames_do_not_allocate() {
    // Sprites with every initializer and operator.
    run_steady_state(
        json!({
            "maxcount": 2000,
            "animationmode": "randomframe",
            "emitter": [
                {"name": "boxrandom", "rate": 2000, "flags": 4, "minperiodicdelay": 0.1, "maxperiodicdelay": 0.2},
                {"name": "sphererandom", "rate": 300, "distancemax": 100, "speedmin": 5, "speedmax": 50}
            ],
            "controlpoint": [{"id": 0, "flags": 1}, {"id": 1, "flags": 2, "offset": "10 10 0"}],
            "initializer": [
                {"name": "colorrandom"}, {"name": "sizerandom"}, {"name": "alpharandom"},
                {"name": "lifetimerandom", "min": 0.5, "max": 1.5}, {"name": "velocityrandom"},
                {"name": "rotationrandom"}, {"name": "angularvelocityrandom"},
                {"name": "turbulentvelocityrandom"},
                {"name": "mapsequencearoundcontrolpoint", "count": 7}
            ],
            "operator": [
                {"name": "movement", "gravity": "0 10 0", "drag": 0.1}, {"name": "angularmovement"},
                {"name": "alphafade"}, {"name": "sizechange"}, {"name": "alphachange"}, {"name": "colorchange"},
                {"name": "turbulence"}, {"name": "vortex", "flags": 7}, {"name": "controlpointattract"},
                {"name": "oscillatealpha"}, {"name": "oscillatesize"}, {"name": "oscillateposition"}
            ]
        }),
        json!({"instanceoverride": {"speed": 1.2}}),
    );
    // Rope with UV smoothing.
    run_steady_state(
        json!({
            "maxcount": 300,
            "emitter": [{"name": "boxrandom", "rate": 200}],
            "initializer": [{"name": "lifetimerandom", "min": 1, "max": 1}],
            "operator": [{"name": "movement"}],
            "renderer": [{"name": "rope", "subdivision": 3}]
        }),
        json!({}),
    );
    // Amendment types: pre-simulation, blending, sequences, remaps, boids, collisions and
    // children created and recycled on spawn/death events.
    run_with_children(
        json!({
            "maxcount": 800,
            "starttime": 2,
            "emitter": [{"name": "sphererandom", "rate": 400, "distancemax": 300, "speedmin": 20, "speedmax": 60,
                "flags": 4, "maxtoemitperperiod": 50, "minperiodicdelay": 0.1, "maxperiodicdelay": 0.2,
                "minperiodicduration": 0.3, "maxperiodicduration": 0.5}],
            "controlpoint": [{"id": 0}, {"id": 1, "offset": "200 100 0"}],
            "initializer": [
                {"name": "lifetimerandom", "min": 0.3, "max": 0.9, "exponent": 2},
                {"name": "mapsequencebetweencontrolpoints", "count": 16, "flags": 31, "limitbehavior": "mirror"},
                {"name": "positionoffsetrandom", "distance": 30},
                {"name": "hsvcolorrandom", "huesteps": 6},
                {"name": "colorlist", "colors": ["1 0 0", "0 1 0"]},
                {"name": "inheritcontrolpointvelocity", "controlpoint": 1},
                {"name": "remapinitialvalue", "input": "distancetocontrolpoint", "output": "opacity"},
                {"name": "mapsequencearoundcontrolpoint", "count": 5, "limitbehavior": "mirror", "axis": "0 1 1"}
            ],
            "operator": [
                {"name": "movement", "gravity": "0 50 0"},
                {"name": "remapvalue", "output": "velocity", "transformfunction": "fbmnoise",
                 "outputrangemin": "-10 -10 0", "outputrangemax": "10 10 0", "operation": "add"},
                {"name": "remapvalue", "input": "particlesystemtime", "output": "size", "transformfunction": "sine"},
                {"name": "reducemovementnearcontrolpoint", "controlpoint": 1},
                {"name": "maintaindistancebetweencontrolpoints"},
                {"name": "capvelocity", "maxspeed": 80, "blendinstart": 0.2, "blendinend": 0.6},
                {"name": "boids", "neighborthreshold": 60},
                {"name": "maintaindistancetocontrolpoint", "variablestrength": 0.1},
                {"name": "collisionplane", "distance": -250},
                {"name": "collisionsphere", "collisionbehavior": "slide"},
                {"name": "collisionbounds", "collisionbehavior": "delete"},
                {"name": "collisionquad", "origin": "0 -100 0"},
                {"name": "turbulence", "blendoutstart": 0.5}
            ],
            "children": [
                {"type": "eventspawn", "name": "child.json", "maxcount": 4, "probability": 0.5, "flags": 1},
                {"type": "eventdeath", "name": "child.json", "maxcount": 6},
                {"type": "eventfollow", "name": "child.json", "maxcount": 3, "flags": 1},
                {"name": "child.json"}
            ]
        }),
        json!({"instanceoverride": {"brightness": 2, "controlpoint1": "150 80 0"}}),
        Some(json!({
            "maxcount": 40,
            "emitter": [{"name": "boxrandom", "rate": 0, "instantaneous": 10}],
            "initializer": [{"name": "lifetimerandom", "min": 0.2, "max": 0.4},
                            {"name": "inheritinitialvaluefromevent", "input": "setcoloropacity"}],
            "operator": [{"name": "movement"}, {"name": "inheritvaluefromevent", "input": "addvelocity"}]
        })),
    );
}

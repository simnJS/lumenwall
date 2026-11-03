//! The steady-state frame (update + vertex generation) must not touch the heap.

use glam::{Mat4, Vec2};
use lumen_particles::{
    DepthRange, FrameInput, ParticleDefinition, ParticleObject, ParticleSystem, SystemOptions,
    UniformInput,
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
    let def = ParticleDefinition::from_json(&def);
    let object = ParticleObject::from_json(&object);
    let opts = SystemOptions {
        width: 1280.0,
        height: 720.0,
        seed: Some(3),
        ..SystemOptions::default()
    };
    let mut sys = ParticleSystem::new(&def, &object, &opts);
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
        }
    }
    assert!(!sys.is_empty());
    let before = allocations();
    assert!(
        before > 0,
        "the counting allocator must see the warm-up allocations"
    );
    for _ in 0..240 {
        t += 1.0 / 60.0;
        if sys.update(&frame(t)) {
            std::hint::black_box(sys.write_vertices_vec(&mut vertices));
            std::hint::black_box(sys.uniforms(&uniforms_in));
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
}

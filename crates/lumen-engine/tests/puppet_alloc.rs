//! The per-frame puppet path (spec 08 §9.11) allocates nothing (docs/PERFORMANCE.md).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use glam::Vec2;
use lumen_engine::puppet::{Layer, Puppet, Rig, decompose_attachment};
use lumen_formats::mdl::{Animation, AnimationMode, BoneSample};
use lumen_render::{SkinVertex, Vertex};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn translation(x: f32, y: f32) -> [f32; 16] {
    glam::Mat4::from_translation(glam::Vec3::new(x, y, 0.0)).to_cols_array()
}

/// A 15-bone chain (the size of the largest puppet of 3463520581) with two layers.
fn puppet() -> Puppet {
    let bones = 15;
    let frames = 60;
    let track = |b: usize| -> Vec<BoneSample> {
        (0..=frames)
            .map(|f| BoneSample {
                position: [10.0 * b as f32, 5.0, 0.0],
                angles: [0.0, 0.0, 0.01 * (f as f32) * (b as f32 + 1.0)],
                scale: [1.0, 1.0 + 0.001 * f as f32, 1.0],
            })
            .collect()
    };
    let animation = |id: u32, mode: AnimationMode| Animation {
        id,
        name: String::new(),
        mode_name: String::new(),
        mode,
        fps: 15.0,
        frames,
        tracks: (0..bones).map(track).collect(),
    };
    let rig = Rig::from_parts(
        (0..bones)
            .map(|b| ((b > 0).then(|| b - 1), translation(10.0 * b as f32, 5.0)))
            .collect(),
        vec![("head".into(), bones - 1, translation(-30.0, 100.0))],
        vec![
            animation(1, AnimationMode::Loop),
            animation(2, AnimationMode::Mirror),
        ],
    );
    let mut first = Layer::new(Some(0));
    first.additive = true;
    let mut second = Layer::new(Some(1));
    second.blend = 0.5;
    second.blend_in = true;
    Puppet::new(rig, vec![first, second])
}

#[test]
fn per_frame_puppet_work_does_not_allocate() {
    let mut p = puppet();
    let n = p.rig().bone_count();
    let vertices = 1000;
    let rest: Vec<Vertex> = (0..vertices)
        .map(|i| Vertex {
            position: [i as f32, 2.0 * i as f32, 0.0],
            uv: [0.0, 0.0],
        })
        .collect();
    let skin: Vec<SkinVertex> = (0..vertices)
        .map(|i| {
            SkinVertex::new(
                [(i % n) as u32, ((i + 1) % n) as u32, 0, 0],
                [0.75, 0.25, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 0.0, 1.0],
            )
        })
        .collect();
    let mut out = rest.clone();
    let mut palette = vec![0.0f32; 12 * n];
    let half = Vec2::new(360.0, 578.0);

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let mut changed = 0;
    for _ in 0..300 {
        p.advance(1.0 / 30.0);
        if p.evaluate() {
            changed += 1;
        }
        p.write_palette(half, &mut palette);
        p.skin_vertices(half, &rest, &skin, &mut out);
        let node = decompose_attachment(&p.attachment_frame(0));
        assert!(node.origin.is_finite());
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(
        after - before,
        0,
        "allocations in the per-frame puppet path"
    );
    assert!(changed > 250, "the pose moves every frame");
}

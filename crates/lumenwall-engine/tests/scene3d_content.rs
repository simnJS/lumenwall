//! Spec 09 on the installed 3D projects (docs/TESTING.md): ignored by default, needs a GPU and
//! `LUMENWALL_WE_ASSETS` (its parent folder holds `projects/defaultprojects`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use lumenwall_engine::{
    FrameState, HeadlessOptions, SceneOptions, ScreenState, Wallpaper, WallpaperResolver,
    build_scene, default_provider, render_headless,
};
use lumenwall_render::math::{UvRange, pointer_disabled};
use lumenwall_render::{ContextOptions, GpuContext};

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

fn assets() -> Option<PathBuf> {
    std::env::var_os("LUMENWALL_WE_ASSETS")
        .map(PathBuf::from)
        .or_else(|| WallpaperResolver::new().assets_dir(None).ok())
}

/// F-3D-R3 through the whole engine: every installed 3D project draws its model and sprite
/// objects with real shaders, `audiophile` and `techno` included (no `type`, no ids).
#[test]
#[ignore = "needs Wallpaper Engine content and a GPU"]
fn installed_3d_projects_render() {
    let Some(assets) = assets() else {
        eprintln!("skipped: assets not found");
        return;
    };
    let projects = assets.parent().unwrap().join("projects/defaultprojects");
    let expected = [
        ("arsenal", 1, 6),
        ("audiophile", 4, 4),
        ("demon_core", 2, 2),
        ("dna_fragment", 5, 5),
        ("fantasticcar", 4, 9),
        ("neon_sunset", 2, 2),
        // 6 models and the sun sprite.
        ("ricepod", 7, 10),
        ("techno", 4, 7),
        ("retro", 1, 1),
    ];
    for (name, objects, meshes) in expected {
        let opts = HeadlessOptions {
            width: 320,
            height: 180,
            frames: 3,
            save: vec![2],
            out_dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lumenwall-engine-3d"),
            prefix: name.into(),
            ..HeadlessOptions::default()
        };
        let r = render_headless(&projects.join(name), &assets, &opts)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(r.stats.models, objects, "{name}: 3D objects");
        assert_eq!(r.stats.meshes, meshes, "{name}: meshes");
        assert_eq!(r.stats.skipped_objects, 0, "{name}");
        #[cfg(feature = "we-shaders")]
        assert_eq!(
            r.stats.stand_in_shaders, 0,
            "{name}: every shader translates"
        );
    }
}

/// The per-frame update of a 3D scene (camera path, lights, model uniforms) allocates nothing
/// (docs/PERFORMANCE.md) and only dirties the scene when the camera moves.
#[test]
#[ignore = "needs Wallpaper Engine content and a GPU"]
fn steady_state_3d_update_does_not_allocate() {
    let Some(assets) = assets() else {
        eprintln!("skipped: assets not found");
        return;
    };
    let path = assets
        .parent()
        .unwrap()
        .join("projects/defaultprojects/arsenal");
    let Ok(ctx) = GpuContext::new_headless(&ContextOptions::default()) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut wp = Wallpaper::load(&path, &assets, &BTreeMap::new()).expect("load");
    let mut shaders = default_provider();
    let mut rt = build_scene(
        &ctx,
        &mut wp.project,
        &wp.vfs,
        shaders.as_mut(),
        &SceneOptions {
            output_size: (640, 360),
            ..SceneOptions::default()
        },
    )
    .expect("build");
    assert!(rt.animated, "the camera path animates the scene");
    let (a16, a32, a64) = ([0.0f32; 16], [0.0f32; 32], [0.0f32; 64]);
    let pointer = pointer_disabled(UvRange::FULL);
    let mut frame = |rt: &mut lumenwall_engine::SceneRuntime, time: f64| {
        rt.update(
            &ctx,
            &mut wp.project,
            &FrameState {
                time,
                dt: 1.0 / 30.0,
                daytime: 0.5,
                pointer: lumenwall_render::math::Pointer {
                    position: pointer.position,
                    normalized: pointer.normalized,
                },
                audio16: &a16,
                audio32: &a32,
                audio64: &a64,
                screen: ScreenState::default(),
            },
        );
    };
    // Warm-up (first frame may toggle passes).
    frame(&mut rt, 1.0);
    rt.scene.render(&ctx);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let mut dirty = 0;
    for i in 0..120 {
        frame(&mut rt, 1.0 + f64::from(i) / 30.0);
        dirty += u32::from(rt.scene.needs_redraw());
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "allocations in the 3D update path");
    assert_eq!(dirty, 120, "the moving camera rewrites the uniforms");
}

//! Text objects on real Wallpaper Engine content (docs/TESTING.md): ignored by default, needs a
//! GPU and the Workshop items 3044659344 (one scripted clock, Atami from the WE assets, colour
//! blend pass) and 3372348904 (seven texts, fonts inside the `.pkg`, blur effects).
//!
//! Checks that every text is drawn, that no glyph texture is regenerated in the steady state,
//! and measures one regeneration through `SceneRuntime::set_text`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use lumen_engine::{
    FrameState, SceneOptions, SceneRuntime, Wallpaper, WallpaperResolver, build_scene,
    default_provider,
};
use lumen_render::math::{UvRange, pointer_disabled};
use lumen_render::{ContextOptions, GpuContext};

fn frame(ctx: &GpuContext, rt: &mut SceneRuntime, wp: &Wallpaper, time: f64) -> Duration {
    let (a16, a32, a64) = ([0.0f32; 16], [0.0f32; 32], [0.0f32; 64]);
    let t0 = Instant::now();
    rt.update(
        ctx,
        &wp.project,
        &FrameState {
            time,
            dt: 1.0 / 30.0,
            daytime: 0.5,
            pointer: pointer_disabled(UvRange::FULL),
            audio16: &a16,
            audio32: &a32,
            audio64: &a64,
        },
    );
    let dt = t0.elapsed();
    let mut enc = ctx.device().create_command_encoder(&Default::default());
    rt.scene.encode(ctx, &mut enc);
    ctx.queue().submit([enc.finish()]);
    dt
}

fn load(id: &str) -> Option<(GpuContext, Wallpaper, SceneRuntime)> {
    let resolver = WallpaperResolver::new();
    let (Ok(dir), Ok(assets)) = (resolver.workshop_dir(id), resolver.assets_dir(None)) else {
        eprintln!("skipped: wallpaper {id} or the WE assets not found");
        return None;
    };
    let ctx = GpuContext::new_headless(&ContextOptions::default()).ok()?;
    let mut wp = Wallpaper::load(&dir, &assets, &BTreeMap::new()).expect("wallpaper");
    let mut shaders = default_provider();
    let rt = build_scene(
        &ctx,
        &mut wp.project,
        &wp.vfs,
        shaders.as_mut(),
        &SceneOptions {
            output_size: (1920, 1080),
            particles: false,
            ..SceneOptions::default()
        },
    )
    .expect("scene");
    Some((ctx, wp, rt))
}

/// Steady state: no regeneration; `set_text`: exactly one, only when the string changes.
fn steady_state_and_set_text(id: &str, texts: usize, object: i32, initial: &str) {
    let Some((ctx, wp, mut rt)) = load(id) else {
        return;
    };
    assert_eq!(rt.stats.texts, texts, "every text object is drawn");
    assert_eq!(rt.stats.skipped_objects, 0);
    #[cfg(feature = "we-shaders")]
    assert_eq!(
        rt.stats.stand_in_shaders, 0,
        "the WE font shader translates"
    );
    assert_eq!(rt.text(object), Some(initial));

    let mut cpu = Duration::ZERO;
    for i in 0..120 {
        cpu += frame(&ctx, &mut rt, &wp, 1.0 + f64::from(i) / 30.0);
    }
    assert_eq!(
        rt.text_regenerations().0,
        0,
        "nothing regenerated in the steady state"
    );
    if !rt.animated {
        assert!(
            !rt.scene.needs_redraw(),
            "a static scene with text stops redrawing"
        );
    }
    eprintln!(
        "{id}: update with {texts} text(s): {:?} per frame",
        cpu / 120
    );

    // Same string: no work.
    assert!(rt.set_text(object, initial));
    frame(&ctx, &mut rt, &wp, 5.0);
    assert_eq!(rt.text_regenerations().0, 0);

    for (k, s) in ["23:59", "-03:42-", "00:00"].iter().enumerate() {
        assert!(rt.set_text(object, s));
        frame(&ctx, &mut rt, &wp, 6.0 + k as f64);
        assert_eq!(rt.text(object), Some(*s));
        for i in 0..30 {
            frame(&ctx, &mut rt, &wp, 7.0 + k as f64 + f64::from(i) / 30.0);
        }
        assert_eq!(
            rt.text_regenerations().0,
            k as u64 + 1,
            "one regeneration per change"
        );
    }
    let (n, total) = rt.text_regenerations();
    eprintln!(
        "{id}: glyph texture regeneration (layout + rasterisation + upload): {:?} mean over {n}",
        total / n as u32
    );
    assert!(!rt.set_text(-12345, "x"), "unknown object id");
}

#[test]
#[ignore = "needs Wallpaper Engine content and a GPU"]
fn porsche_clock() {
    steady_state_and_set_text("3044659344", 1, 90, "12:34");
}

#[test]
#[ignore = "needs Wallpaper Engine content and a GPU"]
fn soren_seven_texts() {
    steady_state_and_set_text("3372348904", 7, 39, "12:34");
}

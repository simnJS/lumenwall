//! Real Wallpaper Engine content (docs/TESTING.md): ignored by default, needs a GPU.
//!
//! `LUMEN_TEST_WALLPAPER` (wallpaper folder, default: Workshop item 3018340632 found through
//! Steam) and `LUMEN_WE_ASSETS` (assets folder, default: the Steam installation's).

use std::path::PathBuf;

use lumen_engine::{HeadlessOptions, WallpaperResolver, render_headless};

fn inputs() -> Option<(PathBuf, PathBuf)> {
    let resolver = WallpaperResolver::new();
    let wallpaper = std::env::var_os("LUMEN_TEST_WALLPAPER")
        .map(PathBuf::from)
        .or_else(|| resolver.workshop_dir("3018340632").ok())?;
    let assets = std::env::var_os("LUMEN_WE_ASSETS")
        .map(PathBuf::from)
        .or_else(|| resolver.assets_dir(None).ok())?;
    Some((wallpaper, assets))
}

#[test]
#[ignore = "needs Wallpaper Engine content and a GPU"]
fn reference_wallpaper_renders_with_real_shaders_and_particles() {
    let Some((wallpaper, assets)) = inputs() else {
        eprintln!("skipped: reference wallpaper or assets not found");
        return;
    };
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lumen-engine-real");
    let opts = HeadlessOptions {
        width: 860,
        height: 360,
        frames: 45,
        save: vec![44],
        out_dir: out,
        ..HeadlessOptions::default()
    };
    let r = render_headless(&wallpaper, &assets, &opts).expect("render");
    assert_eq!(r.scene_size, (3440, 1440));
    assert_eq!(r.stats.images, 1);
    assert_eq!(r.stats.particle_systems, 4);
    assert_eq!(r.stats.skipped_objects, 0);
    #[cfg(feature = "we-shaders")]
    assert_eq!(r.stats.stand_in_shaders, 0, "every WE shader translates");
    assert!(r.live_particles > 0, "rain is visible by default");
    let png = std::fs::read(&r.pngs[0]).unwrap();
    assert!(png.len() > 10_000, "a non-trivial image was written");
}

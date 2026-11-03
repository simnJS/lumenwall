//! Web wallpapers end to end (docs/TESTING.md): ignored by default, needs a GPU, Wallpaper Engine
//! content and the CEF host built with `cargo build --release -p lumenwall-web --features cef`
//! (found next to the test binary, one folder up, or through `LUMENWALL_WEB_HOST`).
//!
//! `LUMENWALL_TEST_WEB_WALLPAPER` (default: Workshop item 3044696375 found through Steam).

use std::path::PathBuf;

use lumenwall_engine::{HeadlessOptions, Wallpaper, WallpaperResolver, render_headless};

fn host_available() -> bool {
    let found = std::env::var_os("LUMENWALL_WEB_HOST").is_some_and(|p| PathBuf::from(p).is_file())
        || std::env::current_exe().is_ok_and(|exe| {
            let name = format!("lumenwall-web-host{}", std::env::consts::EXE_SUFFIX);
            exe.with_file_name(&name).is_file()
                || exe
                    .parent()
                    .and_then(|d| d.parent())
                    .is_some_and(|d| d.join(&name).is_file())
        });
    if !found {
        eprintln!("skipped: lumenwall-web-host not built (set LUMENWALL_WEB_HOST)");
    }
    found
}

#[test]
#[ignore = "needs the CEF host, Wallpaper Engine content and a GPU"]
fn web_page_frames_reach_the_output() {
    let resolver = WallpaperResolver::new();
    let Some(wallpaper) = std::env::var_os("LUMENWALL_TEST_WEB_WALLPAPER")
        .map(PathBuf::from)
        .or_else(|| resolver.workshop_dir("3044696375").ok())
    else {
        eprintln!("skipped: web wallpaper not found");
        return;
    };
    let Ok(assets) = resolver.assets_dir(None) else {
        eprintln!("skipped: assets not found");
        return;
    };
    if !host_available() {
        return;
    }
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lumenwall-engine-web");
    let opts = HeadlessOptions {
        width: 1280,
        height: 720,
        frames: 15,
        start_time: 0.5,
        save: vec![14],
        out_dir: out,
        ..HeadlessOptions::default()
    };
    let r = render_headless(&wallpaper, &assets, &opts).expect("render");
    // The page is painted at the output size.
    assert_eq!(r.scene_size, (1280, 720));
    let png = std::fs::read(&r.pngs[0]).unwrap();
    assert!(png.len() > 10_000, "the page's pixels were composed");
}

/// Shipped wallpapers quote booleans and numbers in `project.json` (corsair_o_tron:
/// `"value": "true"`, `"value": "150"`): they load like Wallpaper Engine loads them.
#[test]
#[ignore = "needs the Wallpaper Engine installation"]
fn quoted_property_values_load() {
    let resolver = WallpaperResolver::new();
    let (Ok(assets), Ok(install)) = (resolver.assets_dir(None), resolver.install_dir()) else {
        eprintln!("skipped: Wallpaper Engine not found");
        return;
    };
    let folder = install.join("projects/defaultprojects/corsair_o_tron");
    if !folder.is_dir() {
        eprintln!("skipped: corsair_o_tron not installed");
        return;
    }
    let wp = Wallpaper::load(&folder, &assets, &Default::default()).expect("load");
    let values = wp.property_values();
    let get = |k: &str| values.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    use lumenwall_engine::monitor::PropertyValue;
    assert_eq!(get("logorings"), Some(PropertyValue::Bool(true)));
    assert_eq!(get("showbottom"), Some(PropertyValue::Number(150.0)));
}

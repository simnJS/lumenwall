//! Layers created by scripts after load (SceneScript `createLayer`, `destroyLayer`,
//! `sortLayer`, `registerAsset`) on real Wallpaper Engine content (docs/TESTING.md): ignored by
//! default, needs a GPU.
//!
//! - Dino Run (`projects/defaultprojects/dino_run` next to `LUMENWALL_WE_ASSETS`): coins
//!   created every 5 s from a registered model and destroyed off screen, pickup particles
//!   created on a catch (a click makes the dinosaur jump into the first coin);
//! - Workshop 3372348904 (`LUMENWALL_WE_WORKSHOP`): 63 visualizer bars created in `init` from
//!   a Workshop-relative path, sorted under the script's layer, driven by the spectrum.

use std::path::PathBuf;

use glam::Vec2;
use lumenwall_engine::{HeadlessOptions, WallpaperResolver, render_headless};

fn assets() -> Option<PathBuf> {
    std::env::var_os("LUMENWALL_WE_ASSETS")
        .map(PathBuf::from)
        .or_else(|| WallpaperResolver::new().assets_dir(None).ok())
}

fn out(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

#[test]
#[ignore = "needs Wallpaper Engine content and a GPU"]
fn dino_run_coins_and_pickup_particles() {
    let Some(assets) = assets() else {
        eprintln!("skipped: assets not found");
        return;
    };
    let dino = assets.join("../projects/defaultprojects/dino_run");
    if !dino.join("scene.json").exists() {
        eprintln!("skipped: {} not found", dino.display());
        return;
    }
    // 686×386 = the 343×193 scene ×2; the pointer rests on the dinosaur, which jumps at
    // frame 230 and meets the first coin (created after 3 s of frames, 50 px/s from x = 360).
    let opts = HeadlessOptions {
        width: 686,
        height: 386,
        // The pickup happens around frame 242; its particles live 0.5 to 1 s.
        frames: 255,
        save: vec![160, 246],
        out_dir: out("lumenwall-dino-run"),
        pointer: Some(Vec2::new(214.0, 340.0)),
        clicks: vec![230],
        ..HeadlessOptions::default()
    };
    let r = render_headless(&dino, &assets, &opts).expect("render");
    let l = r.layers.expect("created layers");
    // Coin and pickup particles registered (one object each, set up with the scene), one
    // more coin while two are on screen; the rest comes from the pools.
    assert!(l.objects_added <= 3, "{l:?}");
    assert!(l.reused >= 2, "{l:?}");
    assert_eq!(l.gpu_created, l.objects_added, "{l:?}");
    assert!(r.live_particles > 0, "pickup particles alive at the end");
}

#[test]
#[ignore = "needs Wallpaper Engine content and a GPU"]
fn visualizer_bars_follow_the_spectrum() {
    let Some(assets) = assets() else {
        eprintln!("skipped: assets not found");
        return;
    };
    let Some(wallpaper) = std::env::var_os("LUMENWALL_WE_WORKSHOP")
        .map(|w| PathBuf::from(w).join("3372348904"))
        .or_else(|| WallpaperResolver::new().workshop_dir("3372348904").ok())
        .filter(|p| p.exists())
    else {
        eprintln!("skipped: 3372348904 not found");
        return;
    };
    let render = |audio: bool| {
        let opts = HeadlessOptions {
            width: 768,
            height: 1024,
            frames: 20,
            save: vec![19],
            out_dir: out(if audio {
                "lumenwall-visualizer-audio"
            } else {
                "lumenwall-visualizer-silent"
            }),
            particles: false,
            synthetic_audio: audio,
            ..HeadlessOptions::default()
        };
        render_headless(&wallpaper, &assets, &opts).expect("render")
    };
    let silent = render(false);
    let loud = render(true);
    let l = loud.layers.expect("created layers");
    assert_eq!(l.objects_added, 63);
    assert_eq!(l.gpu_created, 63);
    assert_eq!(
        l.reorders, 1,
        "init's 63 creations and sorts: one order rebuild"
    );
    // The bars stand on their baseline only with sound: compare the band above it.
    let img = |p: &PathBuf| image_rgba(p);
    let (a, w) = img(&silent.pngs[0]);
    let (b, _) = img(&loud.pngs[0]);
    let dark = |d: &[u8], x: usize, y: usize| {
        let i = (y * w + x) * 4;
        d[i] < 30 && d[i + 1] < 30 && d[i + 2] < 30
    };
    let (mut silent_dark, mut loud_dark) = (0, 0);
    // 768×1024 "fit" output of the 3024×4032 scene (0.254 px per scene pixel): the bars
    // start at x = 800, y = 1235 (from the bottom) and are 22.8 scene pixels apart.
    for k in 0..64 {
        let x = ((800.0 + 22.8 * k as f32 + 3.0) * 0.254) as usize;
        let y = 1024 - ((1235.0 + 6.0) * 0.254) as usize;
        silent_dark += usize::from(dark(&a, x, y));
        loud_dark += usize::from(dark(&b, x, y));
    }
    assert!(
        loud_dark > silent_dark + 20,
        "black bars above the baseline: {loud_dark} with sound, {silent_dark} without"
    );
}

/// RGBA pixels and width of a PNG written by the headless renderer.
fn image_rgba(path: &PathBuf) -> (Vec<u8>, usize) {
    let decoder = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(path).expect("png"),
    ));
    let mut reader = decoder.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size().expect("png size")];
    let info = reader.next_frame(&mut buf).expect("png frame");
    assert_eq!(info.color_type, png::ColorType::Rgba);
    (buf, info.width as usize)
}

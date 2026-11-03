//! Scene time on real Wallpaper Engine content (docs/TESTING.md): ignored by default, needs a
//! GPU and Dino Run (`projects/defaultprojects/dino_run` next to `LUMENWALL_WE_ASSETS`).
//!
//! Dino Run's script moves the dinosaur by `speed × engine.frametime` pixels. A first frame
//! whose `frametime` was the whole running time (`--start 5000`, or a wallpaper applied to a
//! daemon running for hours) threw it thousands of pixels off screen.

use std::path::{Path, PathBuf};

use lumenwall_engine::{DebugPlan, HeadlessOptions, WallpaperResolver, render_headless};

/// The dinosaur's layer (`mario_walk_1`, the layer that carries the game script).
const DINO: u32 = 28;

fn dino_run() -> Option<(PathBuf, PathBuf)> {
    let assets = std::env::var_os("LUMENWALL_WE_ASSETS")
        .map(PathBuf::from)
        .or_else(|| WallpaperResolver::new().assets_dir(None).ok())?;
    let dino = assets.join("../projects/defaultprojects/dino_run");
    dino.join("scene.json").exists().then_some((dino, assets))
}

/// Box of the pixels that differ from the clear colour (top-left pixel), in output pixels.
fn drawn_box(path: &Path) -> Option<[usize; 4]> {
    let decoder = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(path).expect("png"),
    ));
    let mut reader = decoder.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size().expect("png size")];
    let info = reader.next_frame(&mut buf).expect("png frame");
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let (w, h) = (info.width as usize, info.height as usize);
    let bg = [buf[0], buf[1], buf[2]];
    let mut b: Option<[usize; 4]> = None;
    for y in 0..h {
        for x in 0..w {
            let p = &buf[(y * w + x) * 4..][..3];
            if p.iter().zip(bg).any(|(&a, b)| a.abs_diff(b) > 16) {
                let r = b.get_or_insert([x, y, x, y]);
                *r = [r[0].min(x), r[1].min(y), r[2].max(x), r[3].max(y)];
            }
        }
    }
    b
}

/// One simulated second of the dinosaur alone, at `fps`, starting at scene time `start`.
fn dino_box(dino: &Path, assets: &Path, fps: f32, start: f64) -> [usize; 4] {
    let frames = fps.round() as u32;
    let opts = HeadlessOptions {
        width: 686,
        height: 386,
        fps,
        frames,
        start_time: start,
        save: vec![frames - 1],
        out_dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lumenwall-scene-clock"),
        prefix: format!("dino-{fps}-{start}"),
        debug: DebugPlan {
            only_object: Some(DINO),
            ..DebugPlan::default()
        },
        ..HeadlessOptions::default()
    };
    let r = render_headless(dino, assets, &opts).expect("render");
    drawn_box(&r.pngs[0])
        .unwrap_or_else(|| panic!("no dinosaur on screen at {fps} fps from t = {start}"))
}

#[test]
#[ignore = "needs Wallpaper Engine content and a GPU"]
fn dino_stays_on_screen_after_long_uptimes_and_at_any_frame_rate() {
    let Some((dino, assets)) = dino_run() else {
        eprintln!("skipped: Dino Run or assets not found");
        return;
    };
    let reference = dino_box(&dino, &assets, 30.0, 1.0);
    // On the ground, in the left half (686×386 = the 343×193 scene ×2).
    assert!(reference[0] > 150 && reference[2] < 343, "{reference:?}");
    assert!(reference[3] > 300, "{reference:?}");
    for (fps, start) in [
        (144.0, 1.0),
        (144.0, 5000.0),
        (144.0, 100_000.0),
        (30.0, 100_000.0),
    ] {
        let b = dino_box(&dino, &assets, fps, start);
        // Same place after the same simulated second (the walk cycle may change the
        // silhouette by a pixel or two).
        for (a, r) in b.iter().zip(reference) {
            assert!(
                a.abs_diff(r) <= 4,
                "{fps} fps from t = {start}: {b:?}, 30 fps from t = 1: {reference:?}"
            );
        }
    }
}

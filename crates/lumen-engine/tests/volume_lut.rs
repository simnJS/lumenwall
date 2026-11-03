//! Colour LUTs of the Wallpaper Engine assets (`materials/lut/*.tex`, flag 0x40) load as
//! volume textures, ready for the `sampler3D` slot of `ccsimple` (docs/TESTING.md: ignored by
//! default, needs the assets and a GPU; `LUMEN_WE_ASSETS` overrides the Steam lookup).

use std::path::PathBuf;

use lumen_engine::{TextureCache, WallpaperResolver, open_vfs};
use lumen_render::{ContextOptions, GpuContext, SceneBuilder};

#[test]
#[ignore = "needs Wallpaper Engine assets and a GPU"]
fn colour_luts_upload_as_volume_textures() {
    let assets = std::env::var_os("LUMEN_WE_ASSETS")
        .map(PathBuf::from)
        .or_else(|| WallpaperResolver::new().assets_dir(None).ok());
    let Some(assets) = assets.filter(|a| a.join("materials/lut").is_dir()) else {
        eprintln!("skipped: assets folder not found");
        return;
    };
    let ctx = match GpuContext::new_headless(&ContextOptions::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: no GPU adapter ({e})");
            return;
        }
    };
    // An empty wallpaper folder: every name resolves in the assets.
    let wallpaper = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lumen-engine-lut");
    std::fs::create_dir_all(&wallpaper).unwrap();
    let vfs = open_vfs(&wallpaper, &assets).expect("vfs");
    let mut b = SceneBuilder::new(&ctx, (64.0, 64.0));
    let mut cache = TextureCache::default();
    for name in ["lut/neutral", "lut/lutx32_amber"] {
        let t = cache.get(&mut b, &vfs, name).expect("the LUT loads");
        let gpu = b.texture(t.id()).unwrap();
        assert!(gpu.is_volume(), "{name} is a 3D texture");
        assert_eq!((gpu.storage_size(), gpu.depth()), ((32, 32), 32), "{name}");
    }
    // An ordinary texture stays 2D.
    let t = cache.get(&mut b, &vfs, "util/noise").expect("2D texture");
    assert!(!b.texture(t.id()).unwrap().is_volume());
}

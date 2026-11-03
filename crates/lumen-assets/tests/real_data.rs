//! Tests against real Wallpaper Engine content (docs/TESTING.md). Ignored by default;
//! run with `cargo test -p lumen-assets -- --ignored --nocapture` after setting
//! `LUMEN_TEST_WALLPAPER` and `LUMEN_WE_ASSETS`. Nothing is copied into the repository.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use lumen_assets::path::ShaderStage;
use lumen_assets::steam::{AssetsDirOrigin, SteamLocator, resolve_assets_dir};
use lumen_assets::{Adapter, Vfs, WallpaperVfsOptions};
use lumen_formats::tex::{GpuContent, UploadOptions};

fn env_path(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {var} (see docs/TESTING.md)"))
}

fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    fs::canonicalize(a).unwrap() == fs::canonicalize(b).unwrap()
}

#[test]
#[ignore = "needs real Wallpaper Engine content and a Steam install (docs/TESTING.md)"]
fn steam_discovery_finds_reference_wallpaper_and_assets() {
    let wallpaper = env_path("LUMEN_TEST_WALLPAPER");
    let assets = env_path("LUMEN_WE_ASSETS");
    let id = wallpaper.file_name().unwrap().to_str().unwrap().to_owned();

    let locator = SteamLocator::from_env();
    let found = locator.resolve_wallpaper(&id).unwrap();
    println!("workshop {id} -> {}", found.display());
    assert!(same_dir(&found, &wallpaper));

    let chosen = resolve_assets_dir(None, &locator).unwrap();
    println!("assets -> {} ({:?})", chosen.path.display(), chosen.origin);
    assert_eq!(chosen.origin, AssetsDirOrigin::Installation);
    assert!(same_dir(&chosen.path, &assets));

    let install = locator.install_dir("").unwrap();
    assert!(install.join("assets").is_dir());
}

#[test]
#[ignore = "needs real Wallpaper Engine content (docs/TESTING.md)"]
fn reference_wallpaper_resolves_through_pkg_and_assets() {
    let wallpaper = env_path("LUMEN_TEST_WALLPAPER");
    let assets = env_path("LUMEN_WE_ASSETS");
    let options = WallpaperVfsOptions {
        mount_current_dir: false,
        ..WallpaperVfsOptions::default()
    };
    let start = Instant::now();
    let vfs = Vfs::for_wallpaper(&wallpaper, &assets, &options).unwrap();
    println!("VFS mounted in {:?}", start.elapsed());
    let archives = vfs
        .mounts()
        .iter()
        .filter(|m| matches!(m.adapter(), Adapter::Archive(_)))
        .count();
    assert_eq!(archives, 1, "scene.pkg mounted, no gifscene.pkg");

    // From the folder.
    let project = vfs.read("project.json").unwrap();
    assert!(project.starts_with(b"{") || project.starts_with(b"\xEF\xBB\xBF"));
    // From scene.pkg (borrowed from the mapping).
    let scene = vfs.read("scene.json").unwrap();
    assert!(matches!(scene, std::borrow::Cow::Borrowed(_)));
    assert!(vfs.physical_path("scene.json").unwrap_err().is_not_found());
    let shader = vfs
        .read_shader("effects/pulse", ShaderStage::Fragment)
        .unwrap();
    assert!(!shader.is_empty());
    // From the assets folder only.
    let common = vfs.read_shader("common", ShaderStage::Include).unwrap();
    assert!(!common.is_empty());
    assert!(
        vfs.physical_path("shaders/common.h")
            .unwrap()
            .starts_with(fs::canonicalize(&assets).unwrap())
    );
    // Built-in memory files still come first.
    assert!(vfs.exists("effects/wpenginelinux/bloomeffect.json"));

    // A texture from the archive and one from the assets folder, end to end.
    for name in ["edit1688319042946_depth", "util/black"] {
        let files = vfs.read_texture(name).unwrap();
        let parsed = files.parse().unwrap();
        let prepared = parsed.into_upload(&UploadOptions::default()).unwrap();
        let GpuContent::Images { format, images, .. } = &prepared.content else {
            panic!("{name}: expected images");
        };
        println!(
            "{name}: {format:?}, {} image(s), level 0 {}x{}, {} mips, resolution {:?}, tex-json: {}",
            images.len(),
            images[0].width(),
            images[0].height(),
            images[0].mip_level_count(),
            prepared.resolution,
            files.tex_json.is_some()
        );
    }
}

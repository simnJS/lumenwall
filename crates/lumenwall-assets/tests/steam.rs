//! Steam discovery (spec 01 §5) on fake home folders.
#![cfg(not(windows))]

use std::fs;
use std::path::{Path, PathBuf};

use lumenwall_assets::steam::{
    AssetsDirOrigin, SteamError, SteamLocator, WallpaperArg, resolve_assets_dir,
};

const NATIVE: &str = ".local/share/Steam";
const DOT_STEAM: &str = ".steam/steam";
const FLATPAK: &str = ".var/app/com.valvesoftware.Steam/.local/share/Steam";
const SNAP: &str = "snap/steam/common/.local/share/Steam";

fn workshop(home: &Path, root: &str, id: &str) -> PathBuf {
    let dir = home
        .join(root)
        .join("steamapps/workshop/content/431960")
        .join(id);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn install(home: &Path, root: &str, sub: &str) -> PathBuf {
    let dir = home
        .join(root)
        .join("steamapps/common/wallpaper_engine")
        .join(sub);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn wallpaper_argument_classification() {
    assert_eq!(
        WallpaperArg::parse("3018340632"),
        WallpaperArg::WorkshopId("3018340632".into())
    );
    // No slash: workshop id, even when not numeric.
    assert_eq!(
        WallpaperArg::parse("myfolder"),
        WallpaperArg::WorkshopId("myfolder".into())
    );
    assert_eq!(
        WallpaperArg::parse("./myfolder"),
        WallpaperArg::Path("./myfolder".into())
    );
    assert_eq!(
        WallpaperArg::parse("/abs/x.pkg"),
        WallpaperArg::Path("/abs/x.pkg".into())
    );
}

#[test]
fn workshop_and_install_orders_differ() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let native = workshop(h, NATIVE, "42");
    workshop(h, DOT_STEAM, "42");
    install(h, NATIVE, "assets");
    let dot_assets = install(h, DOT_STEAM, "assets");
    let locator = SteamLocator::with_home(h);
    // Workshop: ~/.local/share/Steam first; installation: ~/.steam/steam first.
    assert_eq!(locator.workshop_dir("42").unwrap(), native);
    assert_eq!(locator.install_dir("assets").unwrap(), dot_assets);
    assert_eq!(locator.resolve_wallpaper("42").unwrap(), native);
    assert_eq!(
        locator.resolve_wallpaper("./42").unwrap(),
        PathBuf::from("./42")
    );
}

#[test]
fn flatpak_and_snap_candidates() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let flatpak = workshop(h, FLATPAK, "1");
    let snap = workshop(h, SNAP, "2");
    let snap_assets = install(h, SNAP, "assets");
    let locator = SteamLocator::with_home(h);
    assert_eq!(locator.workshop_dir("1").unwrap(), flatpak);
    assert_eq!(locator.workshop_dir("2").unwrap(), snap);
    assert_eq!(locator.install_dir("assets").unwrap(), snap_assets);
    // The installation folder itself (for config.json).
    assert_eq!(
        locator.install_dir("").unwrap(),
        h.join(SNAP).join("steamapps/common/wallpaper_engine")
    );
}

#[test]
fn additional_libraries_from_libraryfolders_vdf() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let external = h.join("mnt/games/SteamLibrary");
    let item = external.join("steamapps/workshop/content/431960/77");
    fs::create_dir_all(&item).unwrap();
    let steamapps = h.join(NATIVE).join("steamapps");
    fs::create_dir_all(&steamapps).unwrap();
    let vdf = format!(
        "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"apps\"\n\t\t{{\n\t\t\t\"431960\"\t\t\"123\"\n\t\t}}\n\t}}\n}}\n",
        h.join(NATIVE).display(),
        external.display()
    );
    fs::write(steamapps.join("libraryfolders.vdf"), vdf).unwrap();
    let locator = SteamLocator::with_home(h);
    assert_eq!(locator.workshop_dir("77").unwrap(), item);
    let roots = locator
        .steam_roots(lumenwall_assets::steam::SearchKind::Workshop)
        .unwrap();
    assert_eq!(locator.additional_libraries(&roots), [external]);
}

#[test]
fn missing_folders_and_bad_ids() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    workshop(h, NATIVE, "5");
    let locator = SteamLocator::with_home(h);
    assert!(matches!(
        locator.workshop_dir("6"),
        Err(SteamError::WorkshopNotFound { .. })
    ));
    for bad in ["", ".", ".."] {
        assert!(
            matches!(
                locator.workshop_dir(bad),
                Err(SteamError::WorkshopNotFound { .. })
            ),
            "{bad:?}"
        );
    }
    let err = locator.install_dir("assets").unwrap_err();
    assert_eq!(
        err.to_string(),
        "folder not found for application wallpaper_engine: \"assets\""
    );
    let not_dir = SteamLocator::with_home(h.join("nope"));
    assert!(matches!(
        not_dir.workshop_dir("5"),
        Err(SteamError::HomeNotDir(_))
    ));
}

#[test]
fn assets_dir_priority() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let locator = SteamLocator::with_home(h);
    // 1. --assets-dir, unchecked.
    let cli = resolve_assets_dir(Some(Path::new("/does/not/exist")), &locator).unwrap();
    assert_eq!(cli.origin, AssetsDirOrigin::CommandLine);
    assert_eq!(cli.path, PathBuf::from("/does/not/exist"));
    // 3. Next to the executable when there is no installation (empty option ignored).
    let exe = resolve_assets_dir(Some(Path::new("")), &locator).unwrap();
    assert_eq!(exe.origin, AssetsDirOrigin::ExecutableDir);
    let exe_dir = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    assert_eq!(exe.path, exe_dir.parent().unwrap().join("assets"));
    // 2. The installation's assets folder.
    let assets = install(h, DOT_STEAM, "assets");
    let found = resolve_assets_dir(None, &locator).unwrap();
    assert_eq!(found.origin, AssetsDirOrigin::Installation);
    assert_eq!(found.path, assets);
}

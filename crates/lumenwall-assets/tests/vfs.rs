//! Virtual file system behaviour (spec 01 §4) and fixtures F1/F2 through the VFS.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use lumenwall_assets::path::ShaderStage;
use lumenwall_assets::{
    Adapter, BUILTIN_FILES, ErrorCategory, MEDIA_THUMBNAIL, MediaArtSource, Vfs,
    WallpaperVfsOptions, find_texture, is_reserved_texture_name,
};
use lumenwall_formats::pkg::build_pkg;

fn hex(dump: &str) -> Vec<u8> {
    dump.lines()
        .map(|line| line.split_once(':').map_or(line, |(_, rest)| rest))
        .flat_map(str::split_whitespace)
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

fn f1_pkg() -> Vec<u8> {
    hex("
0000: 08 00 00 00 50 4B 47 56 30 30 30 31 01 00 00 00
0010: 05 00 00 00 61 2E 74 78 74 00 00 00 00 02 00 00
0020: 00 68 69
")
}

fn f2_pkg() -> Vec<u8> {
    hex("
0000: 08 00 00 00 50 4B 47 56 30 30 31 39 02 00 00 00
0010: 0C 00 00 00 70 72 6F 6A 65 63 74 2E 6A 73 6F 6E
0020: 00 00 00 00 07 00 00 00 0F 00 00 00 6D 61 74 65
0030: 72 69 61 6C 73 2F 78 2E 74 65 78 07 00 00 00 03
0040: 00 00 00 7B 22 61 22 3A 31 7D 58 59 5A
")
}

/// Spec fixture F7 (animated, 2 images, TEXS0003) with LI = HI = 2.
fn animated_tex() -> Vec<u8> {
    let mut t = hex("
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 06 00 00 00 01 00 00 00 01 00
0020: 00 00 02 00 00 00 02 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 33 00 02 00 00 00 FF FF FF FF 01
0040: 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00 04
0050: 00 00 00 04 00 00 00 FF 00 00 FF 01 00 00 00 01
0060: 00 00 00 01 00 00 00 00 00 00 00 04 00 00 00 04
0070: 00 00 00 00 00 FF FF 54 45 58 53 30 30 30 33 00
0080: 02 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00
0090: 00 00 00 3F 00 00 00 00 00 00 00 00 00 00 80 3F
00A0: 00 00 00 00 00 00 00 00 00 00 80 3F 01 00 00 00
00B0: 00 00 00 3F 00 00 00 00 00 00 00 00 00 00 80 3F
00C0: 00 00 00 00 00 00 00 00 00 00 80 3F
");
    t.truncate(204);
    t
}

fn write(path: &Path, content: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn options() -> WallpaperVfsOptions {
    WallpaperVfsOptions {
        mount_current_dir: false,
        ..WallpaperVfsOptions::default()
    }
}

/// Wallpaper folder `w/` (with `scene.pkg`) and assets folder `a/`.
fn wallpaper_fixture() -> (tempfile::TempDir, Vfs) {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("w");
    let a = dir.path().join("a");
    write(&w.join("project.json"), b"{\"from\":\"folder\"}");
    write(&w.join("only_folder.txt"), b"folder");
    write(
        &w.join("scene.pkg"),
        &build_pkg(
            "PKGV0019",
            [
                ("project.json", &b"{\"from\":\"pkg\"}"[..]),
                ("scene.json", b"{\"scene\":1}"),
                ("shaders/commands/copy.frag", b"overridden?"),
            ],
        ),
    );
    write(
        &w.join("gifscene.pkg"),
        &build_pkg(
            "PKGV0001",
            [("gif.json", &b"gif"[..]), ("scene.json", b"second")],
        ),
    );
    write(&a.join("materials/util/combine.json"), b"{}");
    write(&a.join("scene.json"), b"assets");
    let vfs = Vfs::for_wallpaper(&w, &a, &options()).unwrap();
    (dir, vfs)
}

#[test]
fn f1_through_vfs() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("w.pkg");
    fs::write(&pkg, f1_pkg()).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount(&pkg, "/").unwrap();
    assert_eq!(&*vfs.read("a.txt").unwrap(), b"hi");
    assert!(!vfs.exists("A.txt"));
    assert!(!vfs.exists("b"));
    assert_eq!(
        vfs.read("b").unwrap_err().category(),
        ErrorCategory::NotFound
    );
}

#[test]
fn f2_through_vfs() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("scene.pkg");
    fs::write(&pkg, f2_pkg()).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount(&pkg, "/").unwrap();
    assert_eq!(&*vfs.read("./materials//x.tex").unwrap(), b"XYZ");
    assert_eq!(&*vfs.read("project.json").unwrap(), b"{\"a\":1}");
    // Lookup succeeds for the absolute form. The reference then fails to open it
    // (QO-26); Lumenwall opens the path that was found.
    assert_eq!(
        vfs.resolve("/materials/x.tex"),
        Some((0, "materials/x.tex".into()))
    );
    assert_eq!(&*vfs.read("/materials/x.tex").unwrap(), b"XYZ");
    assert_eq!(&*vfs.read("../materials/x.tex").unwrap(), b"XYZ");
    // Archive contents are borrowed from the mapping.
    assert!(matches!(
        vfs.read("materials/x.tex").unwrap(),
        std::borrow::Cow::Borrowed(_)
    ));
    assert!(
        vfs.physical_path("project.json")
            .unwrap_err()
            .is_not_found()
    );
}

#[test]
fn mount_order_and_priority() {
    let (dir, vfs) = wallpaper_fixture();
    let kinds: Vec<(&str, &str)> = vfs
        .mounts()
        .iter()
        .map(|m| {
            let kind = match m.adapter() {
                Adapter::Memory(_) => "memory",
                Adapter::MediaThumbnail(_) => "thumbnail",
                Adapter::Directory(_) => "dir",
                Adapter::Archive(_) => "pkg",
            };
            (m.root(), kind)
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("/", "memory"),
            (MEDIA_THUMBNAIL, "thumbnail"),
            ("/", "dir"),
            ("/", "pkg"),
            ("/", "pkg"),
            ("/", "dir"),
        ]
    );
    // Folder beats scene.pkg, scene.pkg beats gifscene.pkg and the assets.
    assert_eq!(
        &*vfs.read("project.json").unwrap(),
        b"{\"from\":\"folder\"}"
    );
    assert_eq!(&*vfs.read("scene.json").unwrap(), b"{\"scene\":1}");
    assert_eq!(&*vfs.read("gif.json").unwrap(), b"gif");
    assert_eq!(&*vfs.read("materials/util/combine.json").unwrap(), b"{}");
    // Built-in files come first of all.
    assert_eq!(
        &*vfs.read("shaders/commands/copy.frag").unwrap(),
        BUILTIN_FILES[4].1.as_bytes()
    );
    // Physical paths only exist for folder mounts.
    let physical = vfs.physical_path("only_folder.txt").unwrap();
    assert_eq!(
        physical,
        fs::canonicalize(dir.path().join("w/only_folder.txt")).unwrap()
    );
    assert!(vfs.physical_path("gif.json").unwrap_err().is_not_found());
    assert!(
        vfs.physical_path("shaders/commands/copy.vert")
            .unwrap_err()
            .is_not_found()
    );
}

#[test]
fn builtin_files_are_valid_and_reread() {
    let (_dir, vfs) = wallpaper_fixture();
    for (path, content) in BUILTIN_FILES {
        // QO-22 is not reproduced: every read returns the full content.
        assert_eq!(&*vfs.read(path).unwrap(), content.as_bytes());
        assert_eq!(&*vfs.read(path).unwrap(), content.as_bytes());
        if path.ends_with(".json") {
            serde_json::from_str::<serde_json::Value>(content).expect(path);
        }
    }
    let bloom: serde_json::Value = serde_json::from_str(BUILTIN_FILES[0].1).unwrap();
    assert_eq!(bloom["name"], "camerabloom_wpengine_linux");
    assert_eq!(bloom["passes"].as_array().unwrap().len(), 4);
    assert_eq!(bloom["passes"][3]["bind"][1]["name"], "_rt_Bloom");
    assert_eq!(bloom["passes"][3]["bind"][1]["index"], 1);
    let material: serde_json::Value = serde_json::from_str(BUILTIN_FILES[2].1).unwrap();
    assert_eq!(material["passes"][0]["shader"], "genericimage2");
}

#[test]
fn wallpaper_mount_errors() {
    let dir = tempfile::tempdir().unwrap();
    let assets = dir.path().join("a");
    fs::create_dir_all(&assets).unwrap();
    // Missing wallpaper: propagated, NotFound.
    let err = Vfs::for_wallpaper(&dir.path().join("missing"), &assets, &options()).unwrap_err();
    assert!(err.is_not_found());
    // Wallpaper given as an invalid .pkg: propagated, Invalid.
    let bad = dir.path().join("bad.pkg");
    fs::write(&bad, b"\x04\x00\x00\x00QKGV\x00\x00\x00\x00").unwrap();
    let err = Vfs::for_wallpaper(&bad, &assets, &options()).unwrap_err();
    assert_eq!(err.category(), ErrorCategory::Invalid);
    assert!(err.to_string().contains("E_PKG_ENTETE"), "{err}");
    // Invalid scene.pkg: skipped.
    let w = dir.path().join("w");
    write(&w.join("scene.pkg"), b"garbage");
    write(&w.join("project.json"), b"{}");
    let vfs = Vfs::for_wallpaper(&w, &assets, &options()).unwrap();
    assert_eq!(vfs.mounts().len(), 4);
    // Missing assets folder: fatal.
    let err = Vfs::for_wallpaper(&w, &dir.path().join("no-assets"), &options()).unwrap_err();
    assert!(matches!(
        err,
        lumenwall_assets::AssetError::NoAssetsDir { .. }
    ));
    // A .pkg given directly works as the wallpaper.
    let pkg = dir.path().join("direct.pkg");
    fs::write(&pkg, f2_pkg()).unwrap();
    let vfs = Vfs::for_wallpaper(&pkg, &assets, &options()).unwrap();
    assert_eq!(&*vfs.read("project.json").unwrap(), b"{\"a\":1}");
}

#[test]
fn current_dir_is_mounted_last_by_default() {
    let (dir, _) = wallpaper_fixture();
    let vfs = Vfs::for_wallpaper(
        &dir.path().join("w"),
        &dir.path().join("a"),
        &WallpaperVfsOptions::default(),
    )
    .unwrap();
    assert_eq!(vfs.mounts().len(), 7);
    let Adapter::Directory(last) = vfs.mounts()[6].adapter() else {
        panic!("current directory should be a folder mount");
    };
    assert_eq!(
        last.root(),
        fs::canonicalize(std::env::current_dir().unwrap()).unwrap()
    );
}

#[test]
fn factories() {
    let dir = tempfile::tempdir().unwrap();
    let mut vfs = Vfs::new();
    // `virtual` is an empty memory adapter even if a folder has that name.
    vfs.mount("virtual", "/").unwrap();
    assert!(matches!(vfs.mounts()[0].adapter(), Adapter::Memory(m) if m.is_empty()));
    // The extension check is case-sensitive: `.PKG` is neither archive nor folder.
    let upper = dir.path().join("x.PKG");
    fs::write(&upper, f1_pkg()).unwrap();
    assert!(vfs.mount(&upper, "/").unwrap_err().is_not_found());
    // A symlink to a .pkg is resolved before the extension test.
    #[cfg(unix)]
    {
        let real = dir.path().join("real.pkg");
        fs::write(&real, f1_pkg()).unwrap();
        let link = dir.path().join("link.bin");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        vfs.mount(&link, "/").unwrap();
        assert_eq!(&*vfs.read("a.txt").unwrap(), b"hi");
    }
}

#[test]
fn directory_adapter_rules() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    write(&root.join("sub/file.txt"), b"x");
    write(&dir.path().join("rootsibling/secret.txt"), b"secret");
    let mut vfs = Vfs::new();
    vfs.mount(&root, "/").unwrap();
    // A folder does not "exist" as a file and cannot be opened, but has a path.
    assert!(!vfs.exists("sub"));
    assert!(vfs.read("sub").unwrap_err().is_not_found());
    // Escaping the root through `..` is normalized away first.
    assert!(!vfs.exists("../rootsibling/secret.txt"));
    #[cfg(unix)]
    {
        // Links leading outside the root are invisible, including to a sibling
        // whose name shares the root's prefix (QO-20, compared by component).
        std::os::unix::fs::symlink(
            dir.path().join("rootsibling/secret.txt"),
            root.join("leak.txt"),
        )
        .unwrap();
        assert!(!vfs.exists("leak.txt"));
        assert!(vfs.read("leak.txt").unwrap_err().is_not_found());
        assert!(vfs.physical_path("leak.txt").unwrap_err().is_not_found());
        // Links inside the root are fine.
        std::os::unix::fs::symlink(root.join("sub/file.txt"), root.join("alias.txt")).unwrap();
        assert_eq!(&*vfs.read("alias.txt").unwrap(), b"x");
    }
}

#[cfg(unix)]
#[test]
fn unreadable_file_is_invalid_not_missing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("materials/locked.tex");
    write(&file, b"x");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&file).is_ok() {
        return; // running as root
    }
    let mut vfs = Vfs::new();
    vfs.mount(dir.path(), "/").unwrap();
    let err = vfs.read("materials/locked.tex").unwrap_err();
    assert_eq!(err.category(), ErrorCategory::Invalid);
    // An Invalid error stops the multi-wallpaper texture search.
    let other = tempfile::tempdir().unwrap();
    write(&other.path().join("materials/locked.tex"), b"y");
    let mut second = Vfs::new();
    second.mount(other.path(), "/").unwrap();
    let err = find_texture(&[("b", &second), ("a", &vfs)], "locked").unwrap_err();
    assert_eq!(err.category(), ErrorCategory::Invalid);
}

struct FixedArt(Option<String>);

impl MediaArtSource for FixedArt {
    fn art_url(&self) -> Option<String> {
        self.0.clone()
    }
}

#[test]
fn media_thumbnail() {
    let dir = tempfile::tempdir().unwrap();
    let art = dir.path().join("my covers/art.png");
    write(&art, b"PNGDATA");
    let w = dir.path().join("w");
    fs::create_dir_all(&w).unwrap();
    let url = format!("file://{}", art.to_string_lossy().replace(' ', "%20"));
    let with_art = |url: Option<String>| {
        Vfs::for_wallpaper(
            &w,
            &w,
            &WallpaperVfsOptions {
                media: Some(Arc::new(FixedArt(url))),
                mount_current_dir: false,
                ..WallpaperVfsOptions::default()
            },
        )
        .unwrap()
    };
    let vfs = with_art(Some(url));
    assert!(vfs.exists(MEDIA_THUMBNAIL));
    assert!(!vfs.exists("$mediaThumbnail/x"));
    // Percent-escapes are decoded (QO-23 not reproduced).
    #[cfg(unix)]
    assert_eq!(&*vfs.read(MEDIA_THUMBNAIL).unwrap(), b"PNGDATA");
    assert!(
        vfs.physical_path(MEDIA_THUMBNAIL)
            .unwrap_err()
            .is_not_found()
    );
    let vfs = with_art(Some("https://example.invalid/a.png".into()));
    assert!(vfs.read(MEDIA_THUMBNAIL).unwrap_err().is_not_found());
    let vfs = with_art(None);
    assert!(vfs.read(MEDIA_THUMBNAIL).unwrap_err().is_not_found());
    let vfs = with_art(Some("file:///definitely/missing.png".into()));
    assert!(vfs.read(MEDIA_THUMBNAIL).unwrap_err().is_not_found());
}

#[test]
fn shaders_and_workshop_compat() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("shaders/effects/blur.frag"), b"blur");
    write(
        &dir.path().join("shaders/workshop/123/effects/foo.frag"),
        b"original",
    );
    write(
        &dir.path().join("shaders/workshop/456/effects/bar.vert"),
        b"original-bar",
    );
    write(
        &dir.path().join("zcompat/scene/shaders/123/foo.frag"),
        b"compat",
    );
    write(&dir.path().join("shaders/a/b.frag"), b"dotted");
    let mut vfs = Vfs::new();
    vfs.mount(dir.path(), "/").unwrap();
    assert_eq!(
        &*vfs
            .read_shader("effects/blur", ShaderStage::Fragment)
            .unwrap(),
        b"blur"
    );
    assert_eq!(
        &*vfs.read_shader("a/b.c", ShaderStage::Fragment).unwrap(),
        b"dotted"
    );
    assert_eq!(
        &*vfs
            .read_shader("workshop/123/effects/foo", ShaderStage::Fragment)
            .unwrap(),
        b"compat"
    );
    assert_eq!(
        &*vfs
            .read_shader("workshop/456/effects/bar", ShaderStage::Vertex)
            .unwrap(),
        b"original-bar"
    );
    assert!(
        vfs.read_shader("missing", ShaderStage::Include)
            .unwrap_err()
            .is_not_found()
    );
}

#[test]
fn textures_and_multi_wallpaper_search() {
    let dir = tempfile::tempdir().unwrap();
    let make = |name: &str, files: &[(&str, &[u8])]| {
        let root = dir.path().join(name);
        fs::create_dir_all(&root).unwrap();
        for (path, content) in files {
            write(&root.join(path), content);
        }
        let mut vfs = Vfs::new();
        vfs.mount(&root, "/").unwrap();
        vfs
    };
    let tex = animated_tex();
    let json = br#"{"spritesheetsequences":[{"frames":4,"width":1,"height":1,"duration":3}]}"#;
    let upper = make(
        "upper",
        &[
            ("materials/sprite.tex", &tex),
            ("materials/sprite.tex-json", json),
        ],
    );
    let lower = make("lower", &[("materials/sprite.tex", b"not a texture")]);
    let empty = make("empty", &[]);

    // "DP-1" < "default" < "span:HDMI-1" in byte order.
    let wallpapers = [
        ("span:HDMI-1", &lower),
        ("default", &upper),
        ("DP-1", &empty),
    ];
    let (index, files) = find_texture(&wallpapers, "sprite").unwrap();
    assert_eq!(index, 1);
    let parsed = files.parse().unwrap();
    assert_eq!(parsed.spritesheet.frames, 4);
    assert_eq!(parsed.spritesheet.columns, 2);
    assert_eq!(parsed.spritesheet.duration, 3.0);

    // Without the companion, TEXS inference applies (2 × 2 >= 2 frames).
    let plain = make("plain", &[("materials/sprite.tex", &tex)]);
    let parsed_files = plain.read_texture("sprite").unwrap();
    assert!(parsed_files.tex_json.is_none());
    assert_eq!(parsed_files.parse().unwrap().spritesheet.frames, 2);

    let (index, files) = find_texture(&[("z", &upper), ("a", &lower)], "sprite").unwrap();
    assert_eq!(index, 1);
    assert_eq!(files.parse().unwrap_err().code(), "E_TEX_TEXV");
    assert!(
        find_texture(&[("a", &empty)], "sprite")
            .unwrap_err()
            .is_not_found()
    );

    assert!(is_reserved_texture_name("$mediaThumbnail"));
    assert!(is_reserved_texture_name("$mediaPreviousThumbnail"));
    assert!(is_reserved_texture_name("_rt_FullFrameBuffer"));
    assert!(is_reserved_texture_name("_alias_x"));
    assert!(!is_reserved_texture_name("rt_x"));
}

#[test]
fn a4_out_of_bounds_pkg_entry() {
    // scene.pkg = F2 with project.json pointing past the end of the file; the assets
    // folder also has a project.json.
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("w");
    let a = dir.path().join("a");
    let mut pkg = f2_pkg();
    pkg[0x20] = 0x50;
    write(&w.join("scene.pkg"), &pkg);
    write(&a.join("project.json"), b"{\"from\":\"assets\"}");
    // The archive mounts (it used to be refused as a whole).
    let vfs = Vfs::for_wallpaper(&w, &a, &options()).unwrap();
    assert_eq!(
        vfs.mounts()
            .iter()
            .filter(|m| matches!(m.adapter(), Adapter::Archive(_)))
            .count(),
        1
    );
    // The entry exists, so lookup stops on the archive: no fallback to the assets.
    assert!(vfs.exists("project.json"));
    let err = vfs.read("project.json").unwrap_err();
    assert_eq!(err.category(), ErrorCategory::Invalid);
    assert!(
        err.to_string().contains("E_PKG_ENTREE_HORS_LIMITES"),
        "{err}"
    );
    assert!(matches!(err, lumenwall_assets::AssetError::PkgEntry { .. }));
    // Other entries stay readable, before and after the failed read.
    assert_eq!(&*vfs.read("materials/x.tex").unwrap(), b"XYZ");
}

#[test]
fn release_mappings_keeps_archives_usable() {
    let (_dir, mut vfs) = wallpaper_fixture();
    vfs.release_mappings();
    for mount in vfs.mounts() {
        if let Adapter::Archive(pkg) = mount.adapter() {
            assert!(!pkg.is_mapped());
        }
    }
    assert_eq!(&*vfs.read("gif.json").unwrap(), b"gif");
}

#[test]
fn generation_is_unique_and_follows_the_mounts() {
    let (_dir, mut a) = wallpaper_fixture();
    let (_dir2, b) = wallpaper_fixture();
    assert_ne!(a.generation(), b.generation(), "two VFS never share one");
    assert_ne!(Vfs::new().generation(), Vfs::new().generation());
    // Reads leave it alone: readers can keep what they read under it.
    let g = a.generation();
    a.read("project.json").unwrap();
    assert!(a.exists("scene.json"));
    assert_eq!(a.generation(), g);
    // Any change of what the mounts serve renews it.
    a.mount_adapter("/", Adapter::Memory(Default::default()));
    let g2 = a.generation();
    assert_ne!(g2, g);
    a.set_media_source(None);
    assert_ne!(a.generation(), g2);
    let g3 = a.generation();
    a.release_mappings();
    assert_ne!(a.generation(), g3);
    assert_ne!(a.generation(), b.generation());
}

//! Tests against real Wallpaper Engine content (docs/TESTING.md). Ignored by default;
//! run with `cargo test -p lumenwall-formats -- --ignored --nocapture` after setting
//! `LUMENWALL_TEST_WALLPAPER` and `LUMENWALL_WE_ASSETS`. Nothing from these files is
//! copied into the repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use lumenwall_formats::pkg::PkgArchive;
use lumenwall_formats::tex::{
    self, AddressMode, FilterMode, GpuContent, GpuFormat, Tex, TextureDimension, UploadOptions,
};

fn env_path(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {var} (see docs/TESTING.md)"))
}

fn collect_files(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_files(&path, extension, out);
        } else if path.extension().is_some_and(|e| e == extension) {
            out.push(path);
        }
    }
}

#[derive(Default)]
struct Report {
    total: usize,
    ok: usize,
    by_kind: BTreeMap<String, usize>,
    failures: Vec<String>,
    /// Files with ignored bytes after the last block (§2.1), informative.
    trailing: Vec<String>,
}

impl Report {
    fn add(&mut self, name: &str, data: &[u8], tex_json: Option<&[u8]>) {
        self.total += 1;
        let mut parsed = match tex::parse(data) {
            Ok(t) => t,
            Err(e) => {
                self.failures.push(format!("{name}: parse: {e}"));
                return;
            }
        };
        let kind = describe(&parsed);
        if parsed.end_offset != data.len() {
            self.trailing
                .push(format!("{name}: {} bytes", data.len() - parsed.end_offset));
        }
        if let Some(json) = tex_json {
            parsed.apply_tex_json(json);
        }
        let file_mips: Vec<usize> = parsed.images().map(<[_]>::len).collect();
        match parsed.into_upload(&UploadOptions::default()) {
            Ok(prepared) => {
                if let GpuContent::Images { images, format, .. } = &prepared.content {
                    for (image, &count) in images.iter().zip(&file_mips) {
                        if image.mips.len() != count {
                            self.failures.push(format!(
                                "{name}: mip chain cut from {count} to {}",
                                image.mips.len()
                            ));
                        }
                        for mip in &image.mips {
                            assert_eq!(
                                mip.data.len() as u64,
                                format.volume_size(mip.width, mip.height, mip.depth),
                                "{name}"
                            );
                        }
                    }
                }
                self.ok += 1;
                *self.by_kind.entry(kind).or_default() += 1;
            }
            Err(e) => self.failures.push(format!("{name}: upload ({kind}): {e}")),
        }
    }

    fn print(&self, title: &str) {
        println!(
            "== {title}: {} textures, {} ok, {} failures, {} with trailing bytes",
            self.total,
            self.ok,
            self.failures.len(),
            self.trailing.len()
        );
        for (kind, count) in &self.by_kind {
            println!("  {count:4}  {kind}");
        }
        for failure in &self.failures {
            println!("  FAIL {failure}");
        }
        for trailing in &self.trailing {
            println!("  trailing {trailing}");
        }
    }
}

fn describe(t: &Tex<'_>) -> String {
    let compression: Vec<u32> = {
        let mut c: Vec<u32> = t.images().flatten().map(|m| m.compression).collect();
        c.sort_unstable();
        c.dedup();
        c
    };
    format!(
        "{:?} TEXB{:04}(eff {}) TIE={:?}{}{}{} flags={:#x} C={:?}",
        t.format,
        t.container_version,
        t.effective_version,
        t.embedded_format,
        if t.is_mp4 { " mp4" } else { "" },
        if t.is_volume() { " VOLUME" } else { "" },
        t.animation
            .as_ref()
            .map_or(String::new(), |a| format!(" TEXS{:04}", a.version)),
        t.flags.bits(),
        compression,
    )
}

#[test]
#[ignore = "needs real Wallpaper Engine content (docs/TESTING.md)"]
fn reference_scene_pkg_lists_70_entries() {
    let path = env_path("LUMENWALL_TEST_WALLPAPER").join("scene.pkg");
    let start = Instant::now();
    let pkg = PkgArchive::open(&path).unwrap();
    let elapsed = start.elapsed();
    println!(
        "opened {} in {elapsed:?}: header {:?}, base {}",
        path.display(),
        String::from_utf8_lossy(pkg.header()),
        pkg.base()
    );
    for entry in pkg.entries() {
        println!(
            "  {:>10}  {}",
            entry.size,
            String::from_utf8_lossy(entry.name)
        );
    }
    assert_eq!(pkg.header(), b"PKGV0018");
    assert_eq!(pkg.len(), 70);
    for entry in pkg.entries() {
        let name = entry.name_str().expect("UTF-8 names");
        assert_eq!(pkg.get(name).unwrap().index, entry.index, "unique names");
    }
    let textures = pkg.entries().filter(|e| e.name.ends_with(b".tex")).count();
    assert_eq!(textures, 9);
}

#[test]
#[ignore = "needs real Wallpaper Engine content (docs/TESTING.md)"]
fn real_textures_parse_and_prepare() {
    // Show the parser's warnings (zero-padding, undecodable images…).
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();
    let mut pkg_report = Report::default();
    let pkg = PkgArchive::open(env_path("LUMENWALL_TEST_WALLPAPER").join("scene.pkg")).unwrap();
    let start = Instant::now();
    for entry in pkg.entries().filter(|e| e.name.ends_with(b".tex")) {
        let name = entry.name_str().unwrap();
        let json = pkg.read(&format!("{name}-json"));
        pkg_report.add(name, entry.data().unwrap(), json.map(Result::unwrap));
    }
    println!("scene.pkg textures prepared in {:?}", start.elapsed());
    pkg_report.print("scene.pkg");

    let assets = env_path("LUMENWALL_WE_ASSETS");
    let mut files = Vec::new();
    collect_files(&assets, "tex", &mut files);
    let mut assets_report = Report::default();
    let start = Instant::now();
    for path in &files {
        let data = std::fs::read(path).unwrap();
        let json = std::fs::read(path.with_extension("tex-json")).ok();
        let name = path
            .strip_prefix(&assets)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assets_report.add(&name, &data, json.as_deref());
    }
    println!("assets textures prepared in {:?}", start.elapsed());
    assets_report.print("assets");

    assert_eq!((pkg_report.total, pkg_report.ok), (9, 9));
    assert_eq!(
        assets_report.ok, assets_report.total,
        "every assets texture"
    );
    assert!(pkg_report.failures.is_empty() && assets_report.failures.is_empty());
}

#[test]
#[ignore = "needs real Wallpaper Engine content (docs/TESTING.md)"]
fn colour_luts_are_32_cubed_volumes() {
    let dir = env_path("LUMENWALL_WE_ASSETS").join("materials/lut");
    let mut files = Vec::new();
    collect_files(&dir, "tex", &mut files);
    assert_eq!(files.len(), 28);
    for path in &files {
        let data = std::fs::read(path).unwrap();
        let t = tex::parse(&data).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // A.1: every file is read to its last byte.
        assert_eq!(t.end_offset, data.len(), "{name}");
        assert!(t.is_volume(), "{name}");
        assert_eq!(t.resolution(), [32, 32, 1024, 32], "{name}");
        let up = t.into_upload(&UploadOptions::default()).unwrap();
        assert_eq!(up.dimension(), Some(TextureDimension::D3), "{name}");
        assert_eq!(up.sampler.address_mode, AddressMode::ClampToEdge);
        assert_eq!(up.sampler.min_filter, FilterMode::Linear);
        assert_eq!(up.tex0_format_combo, None);
        let GpuContent::Images { format, images, .. } = &up.content else {
            panic!("{name}: expected images")
        };
        assert_eq!(*format, GpuFormat::Rgba8Unorm);
        let level = &images[0].mips[0];
        assert_eq!(
            (level.width, level.height, level.depth),
            (32, 32, 32),
            "{name}"
        );
        assert_eq!(level.data.len(), 32 * 32 * 32 * 4);
        assert!(
            level.data.chunks(4).all(|p| p[3] == 255),
            "{name}: RGB PNG, alpha 255"
        );

        if name == "neutral.tex" {
            // Identity LUT: texel (x, y, z) ≈ (x, y, z) · 255 / 31.
            for z in 0..32 {
                for y in 0..32 {
                    for x in 0..32 {
                        let i = ((z * 32 + y) * 32 + x) * 4;
                        let px = &level.data[i..i + 3];
                        let want = [x, y, z].map(|v| (v * 255 / 31) as u8);
                        for (got, want) in px.iter().zip(want) {
                            assert!(got.abs_diff(want) <= 2, "({x},{y},{z}): {px:?}");
                        }
                    }
                }
            }
        }
    }
}

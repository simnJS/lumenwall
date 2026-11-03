//! Tests against real Wallpaper Engine content (docs/TESTING.md). Ignored by default;
//! run with `cargo test -p lumen-formats -- --ignored --nocapture` after setting
//! `LUMEN_TEST_WALLPAPER` and `LUMEN_WE_ASSETS`. Nothing from these files is
//! copied into the repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use lumen_formats::pkg::PkgArchive;
use lumen_formats::tex::{self, GpuContent, Tex, UploadOptions};

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
    by_kind: BTreeMap<String, usize>,
    failures: Vec<String>,
    /// Files outside spec 01: undocumented flag 0x40, which comes with extra header
    /// fields the spec does not describe (reported as an open question).
    spec_gaps: Vec<String>,
}

/// Flags field at its fixed offset (spec 01 §2.2), read without parsing.
fn raw_flags(data: &[u8]) -> Option<u32> {
    data.get(0x16..0x1A)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

impl Report {
    fn add(&mut self, name: &str, data: &[u8], tex_json: Option<&[u8]>) {
        self.total += 1;
        let parsed = match tex::parse(data) {
            Ok(t) => t,
            Err(e)
                if e.code() == "E_TEX_TEXB" && raw_flags(data).is_some_and(|f| f & 0x40 != 0) =>
            {
                self.spec_gaps.push(format!("{name}: {e}"));
                return;
            }
            Err(e) => {
                self.failures.push(format!("{name}: parse: {e}"));
                return;
            }
        };
        let kind = describe(&parsed);
        let mut parsed = parsed;
        if let Some(json) = tex_json {
            parsed.apply_tex_json(json);
        }
        let file_mips: Vec<usize> = parsed.images().map(<[_]>::len).collect();
        match parsed.into_upload(&UploadOptions::default()) {
            Ok(prepared) => {
                if let GpuContent::Images { images, format } = &prepared.content {
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
                                format.level_size(mip.width, mip.height),
                                "{name}"
                            );
                        }
                    }
                }
                *self.by_kind.entry(kind).or_default() += 1;
            }
            Err(e) => self.failures.push(format!("{name}: upload ({kind}): {e}")),
        }
    }

    fn print(&self, title: &str) {
        println!(
            "== {title}: {} textures, {} ok, {} outside spec (flag 0x40), {} failures",
            self.total,
            self.by_kind.values().sum::<usize>(),
            self.spec_gaps.len(),
            self.failures.len()
        );
        for (kind, count) in &self.by_kind {
            println!("  {count:4}  {kind}");
        }
        for gap in &self.spec_gaps {
            println!("  SPEC-GAP {gap}");
        }
        for failure in &self.failures {
            println!("  FAIL {failure}");
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
        "{:?} TEXB{:04}(eff {}) TIE={:?}{}{} flags={:#x} C={:?}",
        t.format,
        t.container_version,
        t.effective_version,
        t.embedded_format,
        if t.is_mp4 { " mp4" } else { "" },
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
    let path = env_path("LUMEN_TEST_WALLPAPER").join("scene.pkg");
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
            entry.data.len(),
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
    let pkg = PkgArchive::open(env_path("LUMEN_TEST_WALLPAPER").join("scene.pkg")).unwrap();
    let start = Instant::now();
    for entry in pkg.entries().filter(|e| e.name.ends_with(b".tex")) {
        let name = entry.name_str().unwrap();
        let json = pkg.read(&format!("{name}-json"));
        pkg_report.add(name, entry.data, json);
    }
    println!("scene.pkg textures prepared in {:?}", start.elapsed());
    pkg_report.print("scene.pkg");

    let assets = env_path("LUMEN_WE_ASSETS");
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

    assert_eq!(pkg_report.total, 9);
    assert!(pkg_report.failures.is_empty() && assets_report.failures.is_empty());
}

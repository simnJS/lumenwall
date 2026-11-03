//! Test support: in-memory files, and a layered asset source over directories and `.pkg`
//! archives (spec 02 Annex A) for the real-content tests. Real Wallpaper Engine files are read
//! from their install location only, never copied into the repository.

#![allow(dead_code)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lumenwall_scene::{AssetError, AssetSource};

/// Builds an in-memory file map.
pub fn files(entries: &[(&str, &str)]) -> HashMap<String, Vec<u8>> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.as_bytes().to_vec()))
        .collect()
}

/// One layer of [`Layers`].
pub enum Layer {
    Dir(PathBuf),
    Pkg(HashMap<String, Vec<u8>>),
}

/// First layer containing a path wins.
pub struct Layers(pub Vec<Layer>);

impl AssetSource for Layers {
    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>, AssetError> {
        for l in &self.0 {
            match l {
                Layer::Dir(d) => {
                    let p = d.join(path);
                    if p.is_file() {
                        return std::fs::read(p).map(Cow::Owned).map_err(AssetError::Io);
                    }
                }
                Layer::Pkg(m) => {
                    if let Some(b) = m.get(path) {
                        return Ok(Cow::Borrowed(b));
                    }
                }
            }
        }
        Err(AssetError::NotFound)
    }
}

/// Reads a `.pkg` archive (Annex A): sized header string starting with `PKGV`, entry count,
/// entries {sized path, offset, size}, then the data area. JSON entries only are kept.
pub fn read_pkg(path: &Path) -> Option<HashMap<String, Vec<u8>>> {
    let data = std::fs::read(path).ok()?;
    let mut pos = 0usize;
    let u32_at = |pos: &mut usize| -> Option<u32> {
        let b = data.get(*pos..*pos + 4)?;
        *pos += 4;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let str_at = |pos: &mut usize| -> Option<String> {
        let n = u32_at(pos)? as usize;
        let b = data.get(*pos..*pos + n)?;
        *pos += n;
        Some(String::from_utf8_lossy(b).into_owned())
    };
    let header = str_at(&mut pos)?;
    if !header.starts_with("PKGV") {
        return None;
    }
    let count = u32_at(&mut pos)? as usize;
    let mut entries = Vec::with_capacity(count.min(100_000));
    for _ in 0..count {
        let name = str_at(&mut pos)?;
        let offset = u32_at(&mut pos)? as usize;
        let size = u32_at(&mut pos)? as usize;
        entries.push((name, offset, size));
    }
    let base = pos;
    let mut out = HashMap::new();
    for (name, offset, size) in entries {
        if name.ends_with(".json") {
            let start = base.checked_add(offset)?;
            out.insert(name, data.get(start..start.checked_add(size)?)?.to_vec());
        }
    }
    Some(out)
}

pub fn env_path(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map_or_else(|| PathBuf::from(fallback), PathBuf::from)
}

pub fn assets_dir() -> PathBuf {
    env_path(
        "LUMENWALL_WE_ASSETS",
        "/home/simnjs/snap/steam/common/.local/share/Steam/steamapps/common/wallpaper_engine/assets",
    )
}

pub fn workshop_dir() -> PathBuf {
    env_path(
        "LUMENWALL_WE_WORKSHOP",
        "/home/simnjs/snap/steam/common/.local/share/Steam/steamapps/workshop/content/431960",
    )
}

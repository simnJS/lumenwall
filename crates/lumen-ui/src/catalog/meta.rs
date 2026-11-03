//! Native resolution of a wallpaper, for the resolution filter. Reads only headers:
//! the scene's `orthogonalprojection` (loose `scene.json` or inside `scene.pkg`,
//! layout per specs/01-formats-binaires.md §1) and the `tkhd` box of an MP4.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use super::project::Kind;

const MAX_SCENE_JSON: u32 = 32 << 20;

pub fn resolution(dir: &Path, kind: Kind, file: Option<&str>) -> Option<(u32, u32)> {
    match kind {
        Kind::Scene => scene_resolution(dir, file.unwrap_or("scene.json")),
        Kind::Video => {
            let name = file?;
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".mp4") || lower.ends_with(".m4v") || lower.ends_with(".mov") {
                mp4_resolution(&dir.join(name)).ok().flatten()
            } else {
                None
            }
        }
        _ => None,
    }
}

fn scene_resolution(dir: &Path, entry: &str) -> Option<(u32, u32)> {
    let json = match std::fs::read(dir.join(entry)) {
        Ok(bytes) => bytes,
        Err(_) => ["scene.pkg", "gifscene.pkg"]
            .iter()
            .find_map(|pkg| pkg_entry(&dir.join(pkg), entry).ok().flatten())?,
    };
    let v: serde_json::Value = serde_json::from_slice(&json).ok()?;
    let proj = v.get("general")?.get("orthogonalprojection")?;
    let w = proj.get("width")?.as_f64()?;
    let h = proj.get("height")?.as_f64()?;
    (w >= 1.0 && h >= 1.0).then_some((w as u32, h as u32))
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_sstr(r: &mut impl Read, max: u32) -> io::Result<Vec<u8>> {
    let len = read_u32(r)?;
    if len > max {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut s = vec![0; len as usize];
    r.read_exact(&mut s)?;
    Ok(s)
}

/// Contents of `name` inside a `.pkg` archive (first matching entry), if present.
pub fn pkg_entry(path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    let mut f = io::BufReader::new(File::open(path)?);
    let header = read_sstr(&mut f, 64)?;
    if !header.starts_with(b"PKGV") {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let count = read_u32(&mut f)?;
    if count > 1_000_000 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut found = None;
    for _ in 0..count {
        let entry = read_sstr(&mut f, 4096)?;
        let offset = read_u32(&mut f)?;
        let size = read_u32(&mut f)?;
        if found.is_none() && entry == name.as_bytes() {
            found = Some((offset, size));
        }
    }
    let Some((offset, size)) = found else {
        return Ok(None);
    };
    if size > MAX_SCENE_JSON {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let base = f.stream_position()?;
    f.seek(SeekFrom::Start(base + u64::from(offset)))?;
    let mut data = vec![0; size as usize];
    f.read_exact(&mut data)?;
    Ok(Some(data))
}

/// Width and height from the first video track header (`moov/trak/tkhd`).
pub fn mp4_resolution(path: &Path) -> io::Result<Option<(u32, u32)>> {
    let mut f = File::open(path)?;
    let end = f.metadata()?.len();
    find_tkhd(&mut f, 0, end, 0)
}

fn find_tkhd(f: &mut File, start: u64, end: u64, depth: u32) -> io::Result<Option<(u32, u32)>> {
    let mut pos = start;
    while pos + 8 <= end {
        f.seek(SeekFrom::Start(pos))?;
        let mut head = [0u8; 8];
        f.read_exact(&mut head)?;
        let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
        let kind = [head[4], head[5], head[6], head[7]];
        let mut header_len = 8;
        if size == 1 {
            let mut big = [0u8; 8];
            f.read_exact(&mut big)?;
            size = u64::from_be_bytes(big);
            header_len = 16;
        } else if size == 0 {
            size = end - pos;
        }
        if size < header_len || pos + size > end {
            return Ok(None);
        }
        match &kind {
            b"moov" | b"trak" if depth < 4 => {
                if let Some(r) = find_tkhd(f, pos + header_len, pos + size, depth + 1)? {
                    return Ok(Some(r));
                }
            }
            b"tkhd" => {
                let mut body = vec![0u8; (size - header_len).min(128) as usize];
                f.read_exact(&mut body)?;
                // Width and height are the last 8 bytes, 16.16 fixed point.
                let n = (size - header_len) as usize;
                if n >= 84 && body.len() >= n.min(128) {
                    let wh = &body[n - 8..n];
                    let w = u32::from_be_bytes([wh[0], wh[1], wh[2], wh[3]]) >> 16;
                    let h = u32::from_be_bytes([wh[4], wh[5], wh[6], wh[7]]) >> 16;
                    if w > 0 && h > 0 {
                        return Ok(Some((w, h)));
                    }
                }
            }
            _ => {}
        }
        pos += size;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-ui-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn reads_scene_json_from_a_pkg() {
        let json = br#"{"general":{"orthogonalprojection":{"width":3840,"height":2160}}}"#;
        let mut pkg = Vec::new();
        let sstr = |v: &mut Vec<u8>, s: &[u8]| {
            v.extend((s.len() as u32).to_le_bytes());
            v.extend(s);
        };
        sstr(&mut pkg, b"PKGV0019");
        pkg.extend(2u32.to_le_bytes());
        sstr(&mut pkg, b"a.txt");
        pkg.extend(0u32.to_le_bytes());
        pkg.extend(2u32.to_le_bytes());
        sstr(&mut pkg, b"scene.json");
        pkg.extend(2u32.to_le_bytes());
        pkg.extend((json.len() as u32).to_le_bytes());
        pkg.extend(b"hi");
        pkg.extend(json);
        let path = tmp("scene.pkg");
        std::fs::write(&path, &pkg).unwrap();
        assert_eq!(
            pkg_entry(&path, "a.txt").unwrap().as_deref(),
            Some(&b"hi"[..])
        );
        assert_eq!(pkg_entry(&path, "nope").unwrap(), None);
        let dir = path.parent().unwrap();
        assert_eq!(scene_resolution(dir, "scene.json"), Some((3840, 2160)));
    }

    #[test]
    fn reads_mp4_track_size() {
        let mut tkhd = vec![0u8; 84];
        tkhd[76..80].copy_from_slice(&(1920u32 << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(1080u32 << 16).to_be_bytes());
        let boxed = |kind: &[u8], body: &[u8]| {
            let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            v.extend(kind);
            v.extend(body);
            v
        };
        let trak = boxed(b"trak", &boxed(b"tkhd", &tkhd));
        let mut file = boxed(b"ftyp", b"isom0000");
        file.extend(boxed(b"moov", &trak));
        let path = tmp("v.mp4");
        std::fs::write(&path, file).unwrap();
        assert_eq!(mp4_resolution(&path).unwrap(), Some((1920, 1080)));
    }
}

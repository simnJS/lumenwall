//! What the library shows about a wallpaper without playing it: its native resolution
//! (resolution filter) and whether it can make sound (per-wallpaper volume). Reads only
//! headers: the scene's `orthogonalprojection` and sound objects (loose `scene.json` or
//! inside `scene.pkg`, layout per specs/01-formats-binaires.md §1, sound objects per
//! specs/02-modele-scene.md §7.6), and the `tkhd` / `hdlr` boxes of an MP4.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use super::project::Kind;

const MAX_SCENE_JSON: u32 = 32 << 20;

/// Header facts about one wallpaper.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Meta {
    pub resolution: Option<(u32, u32)>,
    /// The wallpaper can make sound: a scene with sound objects, a video with an audio
    /// track (or one that could not be read), any web wallpaper.
    pub sound: bool,
}

pub fn read(dir: &Path, kind: Kind, file: Option<&str>) -> Meta {
    match kind {
        Kind::Scene => match scene_json(dir, file.unwrap_or("scene.json")) {
            Some(v) => Meta {
                resolution: scene_resolution(&v),
                sound: scene_has_sound(&v),
            },
            None => Meta::default(),
        },
        Kind::Video => {
            let Some(name) = file else {
                return Meta::default();
            };
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".mp4") || lower.ends_with(".m4v") || lower.ends_with(".mov") {
                let (resolution, audio) = mp4_tracks(&dir.join(name)).unwrap_or((None, None));
                Meta {
                    resolution,
                    sound: audio.unwrap_or(true),
                }
            } else {
                // Other containers are not read: they may have a sound track.
                Meta {
                    resolution: None,
                    sound: true,
                }
            }
        }
        // Pages can play sound at any time.
        Kind::Web => Meta {
            resolution: None,
            sound: true,
        },
        Kind::Application | Kind::Unknown => Meta::default(),
    }
}

fn scene_json(dir: &Path, entry: &str) -> Option<serde_json::Value> {
    let json = match std::fs::read(dir.join(entry)) {
        Ok(bytes) => bytes,
        Err(_) => ["scene.pkg", "gifscene.pkg"]
            .iter()
            .find_map(|pkg| pkg_entry(&dir.join(pkg), entry).ok().flatten())?,
    };
    serde_json::from_slice(&json).ok()
}

fn scene_resolution(v: &serde_json::Value) -> Option<(u32, u32)> {
    let proj = v.get("general")?.get("orthogonalprojection")?;
    let w = proj.get("width")?.as_f64()?;
    let h = proj.get("height")?.as_f64()?;
    (w >= 1.0 && h >= 1.0).then_some((w as u32, h as u32))
}

/// A scene object with a non-empty `sound` array (spec 02 §7.6).
fn scene_has_sound(v: &serde_json::Value) -> bool {
    v.get("objects")
        .and_then(|o| o.as_array())
        .is_some_and(|objects| {
            objects.iter().any(|o| {
                o.get("sound")
                    .and_then(|s| s.as_array())
                    .is_some_and(|files| files.iter().any(|f| f.is_string()))
            })
        })
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
#[cfg(test)]
pub fn mp4_resolution(path: &Path) -> io::Result<Option<(u32, u32)>> {
    mp4_tracks(path).map(|(res, _)| res)
}

/// Size of the first video track, and whether some track is audio (`None`: unknown).
type TrackInfo = (Option<(u32, u32)>, Option<bool>);

/// Resolution of the first video track (`moov/trak/tkhd`) and whether a track is audio
/// (`moov/trak/mdia/hdlr` of type `soun`); `None` for the latter when no `moov` was read.
fn mp4_tracks(path: &Path) -> io::Result<TrackInfo> {
    let mut f = File::open(path)?;
    let end = f.metadata()?.len();
    let mut found = Tracks::default();
    walk(&mut f, 0, end, 0, &mut found)?;
    Ok((found.size, found.moov.then_some(found.audio)))
}

#[derive(Default)]
struct Tracks {
    size: Option<(u32, u32)>,
    audio: bool,
    moov: bool,
}

fn walk(f: &mut File, start: u64, end: u64, depth: u32, found: &mut Tracks) -> io::Result<()> {
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
            return Ok(());
        }
        match &kind {
            b"moov" | b"trak" | b"mdia" if depth < 4 => {
                if &kind == b"moov" {
                    found.moov = true;
                }
                walk(f, pos + header_len, pos + size, depth + 1, found)?;
                if found.size.is_some() && found.audio {
                    return Ok(());
                }
            }
            // version/flags (4), pre_defined (4), handler_type (4).
            b"hdlr" if size >= header_len + 12 => {
                let mut body = [0u8; 12];
                f.read_exact(&mut body)?;
                if &body[8..12] == b"soun" {
                    found.audio = true;
                }
            }
            b"tkhd" if found.size.is_none() => {
                let mut body = vec![0u8; (size - header_len).min(128) as usize];
                f.read_exact(&mut body)?;
                // Width and height are the last 8 bytes, 16.16 fixed point.
                let n = (size - header_len) as usize;
                if n >= 84 && body.len() >= n.min(128) {
                    let wh = &body[n - 8..n];
                    let w = u32::from_be_bytes([wh[0], wh[1], wh[2], wh[3]]) >> 16;
                    let h = u32::from_be_bytes([wh[4], wh[5], wh[6], wh[7]]) >> 16;
                    if w > 0 && h > 0 {
                        found.size = Some((w, h));
                    }
                }
            }
            _ => {}
        }
        pos += size;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lumenwall-ui-meta-{}", std::process::id()));
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
        assert_eq!(
            read(dir, Kind::Scene, None),
            Meta {
                resolution: Some((3840, 2160)),
                sound: false
            }
        );
    }

    #[test]
    fn scene_sound_objects() {
        let v = serde_json::json!({"objects": [
            {"image": "models/a.json"},
            {"sound": ["sounds/rain.mp3"], "playbackmode": "loop"}
        ]});
        assert!(scene_has_sound(&v));
        let v = serde_json::json!({"objects": [{"sound": []}, {"sound": "x"}]});
        assert!(!scene_has_sound(&v));
        assert!(!scene_has_sound(&serde_json::json!({})));
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
        let hdlr = |kind: &[u8; 4]| {
            let mut body = vec![0u8; 8];
            body.extend(kind);
            body.extend([0u8; 13]);
            boxed(b"mdia", &boxed(b"hdlr", &body))
        };
        let mut video = boxed(b"tkhd", &tkhd);
        video.extend(hdlr(b"vide"));
        let trak = boxed(b"trak", &video);
        let mut file = boxed(b"ftyp", b"isom0000");
        file.extend(boxed(b"moov", &trak));
        let path = tmp("v.mp4");
        std::fs::write(&path, &file).unwrap();
        assert_eq!(mp4_resolution(&path).unwrap(), Some((1920, 1080)));
        assert_eq!(
            mp4_tracks(&path).unwrap(),
            (Some((1920, 1080)), Some(false))
        );
        let dir = path.parent().unwrap();
        assert!(!read(dir, Kind::Video, Some("v.mp4")).sound, "silent video");

        // A second track with a sound handler.
        let mut moov = trak.clone();
        moov.extend(boxed(b"trak", &hdlr(b"soun")));
        let mut file = boxed(b"ftyp", b"isom0000");
        file.extend(boxed(b"moov", &moov));
        let path = tmp("a.mp4");
        std::fs::write(&path, &file).unwrap();
        assert_eq!(mp4_tracks(&path).unwrap(), (Some((1920, 1080)), Some(true)));
        assert!(read(dir, Kind::Video, Some("a.mp4")).sound);
        // Unreadable or other containers: maybe.
        assert!(read(dir, Kind::Video, Some("missing.mp4")).sound);
        assert!(read(dir, Kind::Video, Some("clip.webm")).sound);
        assert!(read(dir, Kind::Web, Some("index.html")).sound);
        assert!(!read(dir, Kind::Application, Some("game.exe")).sound);
    }
}

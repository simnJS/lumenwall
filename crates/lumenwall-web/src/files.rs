//! Private URL scheme `wp<id>://` and the file service behind it (spec 07 §15.2).
//!
//! Every request is confined to the wallpaper folder: `..` segments, absolute paths, drive
//! prefixes and symbolic links that resolve outside the folder are refused with 404.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use crate::mime::mime_for_path;

/// Host used in the initial URL; the host is ignored when resolving requests.
pub const ROOT_HOST: &str = "root";

/// Scheme name for a wallpaper id: `wp` + id (`wp2317494988`, `wp-1`).
///
/// URL schemes are case-insensitive and canonicalised to lower case by Chromium; characters not
/// allowed in a scheme (`[a-z0-9+.-]`) are replaced by `-`.
pub fn scheme_name(wallpaper_id: &str) -> String {
    let mut s = String::with_capacity(2 + wallpaper_id.len());
    s.push_str("wp");
    for c in wallpaper_id.chars() {
        let c = c.to_ascii_lowercase();
        s.push(
            if c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-') {
                c
            } else {
                '-'
            },
        );
    }
    s
}

/// Initial page URL: `wp<id>://root/<file>`, path segments percent-encoded.
pub fn initial_url(wallpaper_id: &str, file: &str) -> String {
    let mut url = format!("{}://{ROOT_HOST}/", scheme_name(wallpaper_id));
    let file = file.replace('\\', "/");
    for (i, segment) in file.split('/').filter(|s| !s.is_empty()).enumerate() {
        if i > 0 {
            url.push('/');
        }
        percent_encode_into(segment, &mut url);
    }
    url
}

fn percent_encode_into(segment: &str, out: &mut String) {
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}

/// Path part of a `scheme://host/path?query#fragment` URL (query and fragment dropped).
pub fn url_path(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = after_scheme.find('/').map_or("", |i| &after_scheme[i..]);
    let end = path.find(['?', '#']).unwrap_or(path.len());
    &path[..end]
}

/// Decodes `%XX` sequences; `None` when the result is not UTF-8 or a sequence is malformed.
pub fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_owned());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Why a request could not be served.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    /// Missing, not a regular file, or outside the wallpaper folder (all answered with 404).
    #[error("file not found")]
    NotFound,
    /// Unsatisfiable `Range` header (416).
    #[error("range not satisfiable")]
    RangeNotSatisfiable { total: u64 },
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

impl FileError {
    /// HTTP status for the response.
    pub fn status(&self) -> u16 {
        match self {
            Self::NotFound | Self::Io(_) => 404,
            Self::RangeNotSatisfiable { .. } => 416,
        }
    }
}

/// An open file ready to be streamed.
#[derive(Debug)]
pub struct FileResponse {
    /// 200, or 206 for a range request.
    pub status: u16,
    pub mime: &'static str,
    /// Bytes that will be returned by [`FileResponse::read`].
    pub length: u64,
    /// Full file size.
    pub total: u64,
    /// `Content-Range` value for 206 responses.
    pub content_range: Option<String>,
    file: File,
    remaining: u64,
}

impl FileResponse {
    /// Reads the next chunk of the body; 0 at the end.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let want = buf
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let n = self.file.read(&mut buf[..want])?;
        self.remaining -= n as u64;
        Ok(n)
    }

    /// Skips `n` body bytes; returns how many were skipped.
    pub fn skip(&mut self, n: u64) -> io::Result<u64> {
        let n = n.min(self.remaining);
        self.file.seek(SeekFrom::Current(n as i64))?;
        self.remaining -= n;
        Ok(n)
    }

    /// Response headers besides the status line.
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![
            ("Accept-Ranges", "bytes".to_owned()),
            ("Content-Length", self.length.to_string()),
            ("Cache-Control", "no-cache".to_owned()),
        ];
        if let Some(range) = &self.content_range {
            h.push(("Content-Range", range.clone()));
        }
        h
    }
}

/// Serves files of one wallpaper folder.
#[derive(Debug, Clone)]
pub struct FileService {
    root: PathBuf,
}

impl FileService {
    /// `root` must be an existing directory; it is canonicalised once.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "wallpaper root is not a directory",
            ));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves the path of a request URL (host ignored, query/fragment ignored) to a regular
    /// file inside the root.
    ///
    /// The path is percent-decoded first; when the decoded form is not found the raw form is
    /// tried (the reference does not decode, spec 07 QO-18). When no exact match exists, a
    /// case-insensitive match is looked up segment by segment (wallpapers are authored on
    /// case-insensitive Windows file systems).
    pub fn resolve_url(&self, url: &str) -> Result<PathBuf, FileError> {
        let raw = url_path(url);
        if let Some(decoded) = percent_decode(raw)
            && let Ok(path) = self.resolve_relative(&decoded)
        {
            return Ok(path);
        }
        self.resolve_relative(raw)
    }

    /// Resolves a relative path (`/` or `\` separated) inside the root.
    pub fn resolve_relative(&self, relative: &str) -> Result<PathBuf, FileError> {
        let segments = sanitize(relative).ok_or(FileError::NotFound)?;
        if segments.is_empty() {
            return Err(FileError::NotFound);
        }
        let mut candidate = self.root.clone();
        segments.iter().for_each(|s| candidate.push(s));
        let candidate = match self.confine(&candidate) {
            Some(path) => path,
            None => {
                let fallback = self
                    .case_insensitive(&segments)
                    .ok_or(FileError::NotFound)?;
                self.confine(&fallback).ok_or(FileError::NotFound)?
            }
        };
        Ok(candidate)
    }

    /// Canonical path of `path` if it exists, is a regular file and lies inside the root.
    fn confine(&self, path: &Path) -> Option<PathBuf> {
        let canonical = path.canonicalize().ok()?;
        if !canonical.starts_with(&self.root) {
            return None;
        }
        canonical.metadata().ok()?.is_file().then_some(canonical)
    }

    fn case_insensitive(&self, segments: &[&str]) -> Option<PathBuf> {
        let mut dir = self.root.clone();
        for segment in segments {
            let exact = dir.join(segment);
            if exact.symlink_metadata().is_ok() {
                dir = exact;
                continue;
            }
            let wanted = segment.to_lowercase();
            let found = std::fs::read_dir(&dir).ok()?.flatten().find_map(|entry| {
                let name = entry.file_name();
                let name = name.to_str()?;
                (name.to_lowercase() == wanted).then(|| entry.path())
            })?;
            dir = found;
        }
        Some(dir)
    }

    /// Opens the file for a request URL, honouring an optional `Range` header value.
    pub fn open(&self, url: &str, range: Option<&str>) -> Result<FileResponse, FileError> {
        let path = self.resolve_url(url)?;
        let mut file = File::open(&path)?;
        let total = file.metadata()?.len();
        let mime = mime_for_path(path.to_str().unwrap_or_default());
        let (status, start, length, content_range) = match range.and_then(|r| parse_range(r, total))
        {
            None => (200, 0, total, None),
            Some(Err(())) => return Err(FileError::RangeNotSatisfiable { total }),
            Some(Ok((start, end))) => (
                206,
                start,
                end - start + 1,
                Some(format!("bytes {start}-{end}/{total}")),
            ),
        };
        if start > 0 {
            file.seek(SeekFrom::Start(start))?;
        }
        Ok(FileResponse {
            status,
            mime,
            length,
            total,
            content_range,
            file,
            remaining: length,
        })
    }
}

/// Splits a relative URL path into safe segments. `None` if any segment could escape the root.
fn sanitize(relative: &str) -> Option<Vec<&str>> {
    let mut segments = Vec::new();
    for segment in relative.split(['/', '\\']) {
        match segment {
            "" | "." => continue,
            ".." => return None,
            s if s.contains('\0') || s.contains(':') => return None,
            s => {
                // Defence in depth: the segment must be a single normal component.
                let mut components = Path::new(s).components();
                match (components.next(), components.next()) {
                    (Some(Component::Normal(_)), None) => segments.push(s),
                    _ => return None,
                }
            }
        }
    }
    Some(segments)
}

/// Parses a single `bytes=` range. `None`: header ignored (serve 200); `Some(Err)`: 416.
fn parse_range(header: &str, total: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None; // Multiple ranges: serve the whole file.
    }
    let (first, last) = spec.split_once('-')?;
    let (first, last) = (first.trim(), last.trim());
    let range = if first.is_empty() {
        let suffix: u64 = last.parse().ok()?;
        if suffix == 0 || total == 0 {
            return Some(Err(()));
        }
        (total.saturating_sub(suffix), total - 1)
    } else {
        let start: u64 = first.parse().ok()?;
        let end = if last.is_empty() {
            total.saturating_sub(1)
        } else {
            last.parse::<u64>().ok()?.min(total.saturating_sub(1))
        };
        if start >= total || end < start {
            return Some(Err(()));
        }
        (start, end)
    };
    Some(Ok(range))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> (tempfile::TempDir, FileService) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("wallpaper");
        fs::create_dir_all(root.join("js")).unwrap();
        fs::create_dir_all(root.join("Images")).unwrap();
        fs::write(root.join("index.html"), "<html></html>").unwrap();
        fs::write(root.join("js/app.js"), "console.log(1)").unwrap();
        fs::write(root.join("Images/Photo.PNG"), b"\x89PNG....").unwrap();
        fs::write(root.join("my file.txt"), "0123456789").unwrap();
        fs::write(dir.path().join("secret.txt"), "secret").unwrap();
        let service = FileService::new(&root).unwrap();
        (dir, service)
    }

    #[test]
    fn scheme_and_initial_url() {
        assert_eq!(scheme_name("2317494988"), "wp2317494988");
        assert_eq!(scheme_name("-1"), "wp-1");
        assert_eq!(scheme_name("My_Id"), "wpmy-id");
        assert_eq!(initial_url("12", "index.html"), "wp12://root/index.html");
        assert_eq!(
            initial_url("12", "sub dir\\päge.html"),
            "wp12://root/sub%20dir/p%C3%A4ge.html"
        );
    }

    #[test]
    fn url_path_ignores_host_query_fragment() {
        assert_eq!(url_path("wp1://root/a/b.js?x=1#f"), "/a/b.js");
        assert_eq!(url_path("wp1://anything/a"), "/a");
        assert_eq!(url_path("wp1://root"), "");
    }

    #[test]
    fn serves_with_mime_types() {
        let (_d, s) = tree();
        let r = s.open("wp1://root/index.html", None).unwrap();
        assert_eq!((r.status, r.mime, r.length), (200, "text/html", 13));
        let mut r = s.open("wp1://other-host/js/app.js?v=3", None).unwrap();
        assert_eq!(r.mime, "text/javascript");
        let mut body = Vec::new();
        let mut buf = [0u8; 4];
        loop {
            let n = r.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
        }
        assert_eq!(body, b"console.log(1)");
    }

    #[test]
    fn not_found_is_404() {
        let (_d, s) = tree();
        let e = s.open("wp1://root/missing.js", None).unwrap_err();
        assert!(matches!(e, FileError::NotFound));
        assert_eq!(e.status(), 404);
        assert!(matches!(
            s.open("wp1://root/js", None),
            Err(FileError::NotFound)
        ));
        assert!(matches!(
            s.open("wp1://root/", None),
            Err(FileError::NotFound)
        ));
    }

    #[test]
    fn percent_decoding_and_case_fallback() {
        let (_d, s) = tree();
        assert!(s.open("wp1://root/my%20file.txt", None).is_ok());
        assert!(s.open("wp1://root/images/photo.png", None).is_ok());
        assert_eq!(
            s.open("wp1://root/images/photo.png", None).unwrap().mime,
            "image/png"
        );
    }

    #[test]
    fn traversal_is_refused() {
        let (_d, s) = tree();
        for url in [
            "wp1://root/../secret.txt",
            "wp1://root/%2e%2e/secret.txt",
            "wp1://root/js/..%2f..%2fsecret.txt",
            "wp1://root/..\\secret.txt",
            "wp1://root/js/%2E%2E/%2E%2E/secret.txt",
            "wp1://root/C:/Windows/win.ini",
            "wp1://root/a%00b",
        ] {
            assert!(
                matches!(s.open(url, None), Err(FileError::NotFound)),
                "{url}"
            );
        }
        // An absolute path is re-rooted under the wallpaper folder, never used as is.
        let abs = s.root().parent().unwrap().join("secret.txt");
        assert!(s.resolve_relative(abs.to_str().unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_must_stay_inside() {
        let (d, s) = tree();
        std::os::unix::fs::symlink(d.path().join("secret.txt"), s.root().join("leak.txt")).unwrap();
        std::os::unix::fs::symlink(d.path(), s.root().join("updir")).unwrap();
        std::os::unix::fs::symlink(s.root().join("js/app.js"), s.root().join("alias.js")).unwrap();
        assert!(matches!(
            s.open("wp1://root/leak.txt", None),
            Err(FileError::NotFound)
        ));
        assert!(matches!(
            s.open("wp1://root/updir/secret.txt", None),
            Err(FileError::NotFound)
        ));
        assert!(matches!(
            s.open("wp1://root/LEAK.TXT", None),
            Err(FileError::NotFound)
        ));
        assert_eq!(
            s.open("wp1://root/alias.js", None).unwrap().mime,
            "text/javascript"
        );
    }

    #[test]
    fn range_requests() {
        let (_d, s) = tree();
        let mut r = s
            .open("wp1://root/my%20file.txt", Some("bytes=2-5"))
            .unwrap();
        assert_eq!((r.status, r.length, r.total), (206, 4, 10));
        assert_eq!(r.content_range.as_deref(), Some("bytes 2-5/10"));
        let mut buf = [0u8; 16];
        let n = r.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"2345");
        assert_eq!(r.read(&mut buf).unwrap(), 0);

        let r = s
            .open("wp1://root/my%20file.txt", Some("bytes=7-"))
            .unwrap();
        assert_eq!(r.content_range.as_deref(), Some("bytes 7-9/10"));
        let r = s
            .open("wp1://root/my%20file.txt", Some("bytes=-3"))
            .unwrap();
        assert_eq!(r.content_range.as_deref(), Some("bytes 7-9/10"));
        let r = s
            .open("wp1://root/my%20file.txt", Some("bytes=0-99"))
            .unwrap();
        assert_eq!(r.content_range.as_deref(), Some("bytes 0-9/10"));
        let r = s
            .open("wp1://root/my%20file.txt", Some("bytes=0-1,4-5"))
            .unwrap();
        assert_eq!(r.status, 200);
        let e = s
            .open("wp1://root/my%20file.txt", Some("bytes=10-"))
            .unwrap_err();
        assert_eq!(e.status(), 416);
    }

    #[test]
    fn percent_decode_edge_cases() {
        assert_eq!(percent_decode("a%20b").as_deref(), Some("a b"));
        assert_eq!(percent_decode("%C3%A4").as_deref(), Some("ä"));
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%2"), None);
        assert_eq!(percent_decode("%FF"), None);
    }
}

//! Extraction of a verified release archive (`.tar.gz` or `.zip`) into a staging
//! folder. The archive's signature was checked before this runs; the checks here are
//! still strict so that a packaging mistake can never write outside the folder:
//! one top-level folder, relative paths without `..`, regular files and folders only
//! (no links, no devices), bounded size and entry count.
//!
//! Each platform reads only its own format (`.tar.gz` on Linux, `.zip` on Windows);
//! tests read both everywhere.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};

use crate::Error;

const MAX_ENTRIES: usize = 20_000;
const MAX_TOTAL: u64 = 4 << 30;

/// Extracts `archive` into `dest`, dropping the archive's top-level folder. Returns the
/// regular files written, relative to `dest`.
pub(crate) fn extract(archive: &Path, dest: &Path) -> Result<Vec<PathBuf>, Error> {
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = Extractor {
        dest,
        top: None,
        files: Vec::new(),
        entries: 0,
        total: 0,
    };
    #[cfg(any(not(windows), test))]
    if name.ends_with(".tar.gz") {
        out.tar(archive)?;
        return Ok(out.files);
    }
    #[cfg(any(windows, test))]
    if name.ends_with(".zip") {
        out.zip(archive)?;
        return Ok(out.files);
    }
    Err(Error::BadArchive(format!("unknown archive format: {name}")))
}

struct Extractor<'a> {
    dest: &'a Path,
    top: Option<OsString>,
    files: Vec<PathBuf>,
    entries: usize,
    total: u64,
}

fn bad(why: &str) -> Error {
    Error::BadArchive(why.to_owned())
}

impl Extractor<'_> {
    #[cfg(any(not(windows), test))]
    fn tar(&mut self, archive: &Path) -> Result<(), Error> {
        let file = File::open(archive).map_err(|e| Error::io(archive.display(), &e))?;
        let gz = flate2::read::GzDecoder::new(BufReader::new(file));
        let mut tar = tar::Archive::new(gz);
        let entries = tar.entries().map_err(|_| bad("unreadable tar archive"))?;
        for entry in entries {
            let mut entry = entry.map_err(|_| bad("unreadable tar entry"))?;
            let path = entry
                .path()
                .map_err(|_| bad("unreadable entry name"))?
                .into_owned();
            let kind = entry.header().entry_type();
            if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() {
                continue;
            }
            let Some(rel) = self.relative(&path)? else {
                continue;
            };
            if kind.is_dir() {
                self.dir(&rel)?;
            } else if kind.is_file() {
                let mode = entry.header().mode().unwrap_or(0o644);
                self.file(&rel, &mut entry, mode)?;
            } else {
                return Err(bad("links and special files are not allowed"));
            }
        }
        Ok(())
    }

    #[cfg(any(windows, test))]
    fn zip(&mut self, archive: &Path) -> Result<(), Error> {
        let file = File::open(archive).map_err(|e| Error::io(archive.display(), &e))?;
        let mut zip = zip::ZipArchive::new(BufReader::new(file))
            .map_err(|_| bad("unreadable zip archive"))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|_| bad("unreadable zip entry"))?;
            let path = entry
                .enclosed_name()
                .ok_or_else(|| bad("unsafe path in the archive"))?;
            let Some(rel) = self.relative(&path)? else {
                continue;
            };
            if entry.is_dir() {
                self.dir(&rel)?;
            } else if entry.is_file() && !entry.is_symlink() {
                let mode = entry.unix_mode().unwrap_or(0o644);
                self.file(&rel, &mut entry, mode)?;
            } else {
                return Err(bad("links and special files are not allowed"));
            }
        }
        Ok(())
    }

    /// Path relative to the single top-level folder; `None` for that folder itself.
    fn relative(&mut self, path: &Path) -> Result<Option<PathBuf>, Error> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            return Err(bad("too many entries"));
        }
        let mut parts = Vec::new();
        for c in path.components() {
            match c {
                Component::Normal(s) => {
                    let text = s.to_string_lossy();
                    if text.contains([':', '\\']) {
                        return Err(bad("unsafe path in the archive"));
                    }
                    parts.push(s.to_os_string());
                }
                Component::CurDir => {}
                _ => return Err(bad("unsafe path in the archive")),
            }
        }
        let Some((first, rest)) = parts.split_first() else {
            return Ok(None);
        };
        match &self.top {
            None => self.top = Some(first.clone()),
            Some(top) if top != first => return Err(bad("more than one top-level folder")),
            Some(_) => {}
        }
        Ok((!rest.is_empty()).then(|| rest.iter().collect()))
    }

    fn dir(&self, rel: &Path) -> Result<(), Error> {
        let path = self.dest.join(rel);
        std::fs::create_dir_all(&path).map_err(|e| Error::io(path.display(), &e))
    }

    fn file(&mut self, rel: &Path, data: &mut impl Read, mode: u32) -> Result<(), Error> {
        let path = self.dest.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), &e))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 });
        }
        #[cfg(not(unix))]
        let _ = mode;
        let mut out = options
            .open(&path)
            .map_err(|e| Error::io(path.display(), &e))?;
        let budget = MAX_TOTAL - self.total;
        let n = std::io::copy(&mut data.take(budget + 1), &mut out)
            .map_err(|_| bad("unreadable archive data"))?;
        self.total += n;
        if self.total > MAX_TOTAL {
            return Err(bad("the archive is too large once extracted"));
        }
        self.files.push(rel.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_gz(path: &Path, entries: &[(&str, &[u8], u32)]) {
        let gz =
            flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        for (name, data, mode) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(*mode);
            h.set_entry_type(tar::EntryType::Regular);
            // Bypasses `set_path`'s own `..` check, to build hostile archives.
            let bytes = name.as_bytes();
            h.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn extracts_below_the_top_folder() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("x.tar.gz");
        tar_gz(
            &a,
            &[
                ("lumenwall-1/lumenwall", b"daemon", 0o755),
                ("lumenwall-1/share/a.txt", b"text", 0o644),
            ],
        );
        let dest = dir.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        let mut files = extract(&a, &dest).unwrap();
        files.sort();
        assert_eq!(
            files,
            [PathBuf::from("lumenwall"), PathBuf::from("share/a.txt")]
        );
        assert_eq!(std::fs::read(dest.join("lumenwall")).unwrap(), b"daemon");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dest.join("lumenwall"))
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o100, 0, "executable bit kept");
        }
    }

    #[test]
    fn refuses_escapes_and_several_roots() {
        let dir = tempfile::tempdir().unwrap();
        for (i, entries) in [
            vec![("lumenwall-1/../../evil", &b"x"[..], 0o644)],
            vec![("/etc/evil", &b"x"[..], 0o644)],
            vec![("a/x", &b"x"[..], 0o644), ("b/y", &b"y"[..], 0o644)],
        ]
        .into_iter()
        .enumerate()
        {
            let a = dir.path().join(format!("{i}.tar.gz"));
            tar_gz(&a, &entries);
            let dest = dir.path().join(format!("out{i}"));
            std::fs::create_dir(&dest).unwrap();
            assert!(
                matches!(extract(&a, &dest), Err(Error::BadArchive(_))),
                "case {i}"
            );
        }
        assert!(!dir.path().parent().unwrap().join("evil").exists());
    }

    #[test]
    fn refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("l.tar.gz");
        let gz =
            flate2::write::GzEncoder::new(File::create(&a).unwrap(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        b.append_link(&mut h, "lumenwall-1/link", "/etc/passwd")
            .unwrap();
        b.into_inner().unwrap().finish().unwrap();
        let dest = dir.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        assert!(matches!(extract(&a, &dest), Err(Error::BadArchive(_))));
    }

    #[test]
    fn extracts_zip() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("x.zip");
        let mut z = zip::ZipWriter::new(File::create(&a).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        z.add_directory("lumenwall-1/", opts).unwrap();
        z.start_file("lumenwall-1/lumenwall.exe", opts).unwrap();
        z.write_all(b"MZ daemon").unwrap();
        z.start_file("lumenwall-1/locales/en.pak", opts).unwrap();
        z.write_all(b"pak").unwrap();
        z.finish().unwrap();
        let dest = dir.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        let mut files = extract(&a, &dest).unwrap();
        files.sort();
        assert_eq!(
            files,
            [
                PathBuf::from("locales/en.pak"),
                PathBuf::from("lumenwall.exe")
            ]
        );
        assert_eq!(
            std::fs::read(dest.join("lumenwall.exe")).unwrap(),
            b"MZ daemon"
        );
    }
}

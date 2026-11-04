//! Download of a release archive, verified while it streams to disk.
//!
//! Order: `SHA256SUMS.minisig` and `SHA256SUMS` first (small): the list must verify
//! with the release key and name the archive, before a byte of the archive is
//! fetched. Then the archive itself, hashed (SHA-256) as it is written. The file keeps
//! a `.part` name until its hash equals the signed line; on any failure it is deleted.
//!
//! The signed list is the only signature: it binds the archive's name to its SHA-256,
//! so an archive that matches its line is exactly the file the key holder listed.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use semver::Version;

use crate::http::{Http, Kind};
use crate::{Error, Release, SUMS_NAME, Source, verify};

const MAX_SIGNATURE: u64 = 4 << 10;
const MAX_SUMS: u64 = 64 << 10;
/// Ceiling for an archive, whatever the API announces.
const MAX_ARCHIVE: u64 = 2 << 30;
const CHUNK: usize = 64 << 10;

/// An archive whose SHA-256 matched its line in the signed `SHA256SUMS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub archive: PathBuf,
    pub version: Version,
    pub sha256: String,
}

/// Downloads `release`'s archive for this platform into `dir` (created if needed;
/// earlier downloads there are removed) and verifies it with `source`'s key.
///
/// `progress(done, total)` is called from this thread every few hundred kilobytes.
/// Setting `cancel` stops the download at the next chunk ([`Error::Cancelled`]).
pub fn download(
    source: &Source,
    current: &Version,
    release: &Release,
    dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<Verified, Error> {
    let name = release.archive.name.as_str();
    if name.is_empty()
        || name.contains(['/', '\\'])
        || name.starts_with('.')
        || !name.starts_with("lumenwall-")
    {
        return Err(Error::BadResponse("bad archive name".into()));
    }
    let http = Http::new(source, &crate::user_agent(current), Kind::Download);
    let key = source.key();

    let sums_sig = small(&http, &release.sums_signature.url, MAX_SIGNATURE)?;
    let sums = small(&http, &release.sums.url, MAX_SUMS)?;
    let expected = expected_sum(key, &sums, &sums_sig, name)?;

    if release.archive.size > MAX_ARCHIVE {
        return Err(Error::TooLarge);
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir.display(), &e))?;
    clear(dir);
    let part = dir.join(format!("{name}.part"));
    let result = (|| {
        let mut resp = http.get(
            &release.archive.url,
            &[("Accept", "application/octet-stream")],
        )?;
        match resp.status().as_u16() {
            200 => {}
            s => return Err(Error::Status(s)),
        }
        let total = release.archive.size;
        // ureq's limit fails a read once it is reached, even at the end of the body:
        // one byte of slack, and the exact check below.
        let mut reader = resp
            .body_mut()
            .with_config()
            .limit(total.saturating_add(1))
            .reader();
        let mut file = File::create(&part).map_err(|e| Error::io(part.display(), &e))?;
        let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0u8; CHUNK];
        let step = (total / 200).max(256 << 10);
        let (mut done, mut shown) = (0u64, 0u64);
        progress(0, total);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(read_error(e)),
            };
            done += n as u64;
            if done > total {
                return Err(Error::TooLarge);
            }
            sha.update(&buf[..n]);
            file.write_all(&buf[..n])
                .map_err(|e| Error::io(part.display(), &e))?;
            if done - shown >= step {
                shown = done;
                progress(done, total);
            }
        }
        progress(done, total);
        file.sync_all().map_err(|e| Error::io(part.display(), &e))?;
        let sha256 = verify::hex(sha.finish().as_ref());
        if sha256 != expected {
            return Err(Error::Checksum(format!(
                "the SHA-256 of {name} differs from {SUMS_NAME}"
            )));
        }
        Ok(sha256)
    })();
    match result {
        Ok(sha256) => {
            let archive = dir.join(name);
            std::fs::rename(&part, &archive).map_err(|e| Error::io(archive.display(), &e))?;
            tracing::info!(version = %release.version, "update downloaded and verified");
            Ok(Verified {
                archive,
                version: release.version.clone(),
                sha256,
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// The SHA-256 that the signed `SHA256SUMS` lists for `name`.
fn expected_sum(
    key: &minisign_verify::PublicKey,
    sums: &[u8],
    sums_signature: &[u8],
    name: &str,
) -> Result<String, Error> {
    let sig = verify::signature(sums_signature)?;
    verify::bytes(key, sums, &sig, SUMS_NAME)?;
    let sums = std::str::from_utf8(sums)
        .map_err(|_| Error::Checksum(format!("{SUMS_NAME} is not text")))?;
    verify::listed_sum(sums, name)
        .ok_or_else(|| Error::Checksum(format!("{name} is not listed in {SUMS_NAME}")))
}

/// Verifies release files already on disk exactly as [`download`] does: `dir` holds
/// the archive of `version` for `target`, `SHA256SUMS` and `SHA256SUMS.minisig`.
/// Used by the release workflow before publishing.
pub fn verify_local(
    source: &Source,
    dir: &Path,
    version: &Version,
    target: &str,
) -> Result<Verified, Error> {
    let name = crate::archive_name(version, target);
    let read = |file: &str, limit: u64| -> Result<Vec<u8>, Error> {
        let path = dir.join(file);
        let len = std::fs::metadata(&path)
            .map_err(|e| Error::io(path.display(), &e))?
            .len();
        if len > limit {
            return Err(Error::TooLarge);
        }
        std::fs::read(&path).map_err(|e| Error::io(path.display(), &e))
    };
    let sig_suffix = crate::SIGNATURE_SUFFIX;
    let expected = expected_sum(
        source.key(),
        &read(SUMS_NAME, MAX_SUMS)?,
        &read(&format!("{SUMS_NAME}{sig_suffix}"), MAX_SIGNATURE)?,
        &name,
    )?;
    let archive = dir.join(&name);
    let mut file = File::open(&archive).map_err(|e| Error::io(archive.display(), &e))?;
    let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| Error::io(archive.display(), &e))?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    let sha256 = verify::hex(sha.finish().as_ref());
    if sha256 != expected {
        return Err(Error::Checksum(format!(
            "the SHA-256 of {name} differs from {SUMS_NAME}"
        )));
    }
    Ok(Verified {
        archive,
        version: version.clone(),
        sha256,
    })
}

/// A small file (signature, checksum list), whole.
fn small(http: &Http<'_>, url: &str, limit: u64) -> Result<Vec<u8>, Error> {
    let mut resp = http.get(url, &[("Accept", "application/octet-stream")])?;
    match resp.status().as_u16() {
        200 => resp
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(crate::http::map_err),
        s => Err(Error::Status(s)),
    }
}

fn read_error(e: std::io::Error) -> Error {
    match e.get_ref().and_then(|i| i.downcast_ref::<ureq::Error>()) {
        Some(ureq::Error::BodyExceedsLimit(_)) => Error::TooLarge,
        _ => Error::Offline,
    }
}

/// Removes earlier downloads (this directory belongs to the updater).
fn clear(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

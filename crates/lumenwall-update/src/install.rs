//! Replacing the installed files with a verified archive's content.
//!
//! The archive is extracted into a staging folder inside the installation folder
//! (same file system, so moves are renames). Then, file by file:
//!
//! - Linux: the current file gets a hard-linked backup, and the new one is renamed over
//!   it: the path always names a complete program, and running processes keep the old
//!   one open.
//! - Windows: a running `.exe` or `.dll` can be renamed but not overwritten, so the
//!   current file is renamed to `<name>.lumenwall-old` and the new one moved in place.
//!   Backups still in use are deleted by [`cleanup`] at the next start.
//!
//! If any step fails, the files already replaced are put back.

use std::fs::File;
use std::path::{Path, PathBuf};

use semver::Version;

use crate::{Error, Verified, archive};

/// The programs of a Lumenwall installation (without `.exe`).
pub const PROGRAMS: [&str; 3] = ["lumenwall", "lumenwall-ui", "lumenwall-web-host"];
/// Present in the folder extracted from a release archive. In such a folder the whole
/// archive is installed (programs, web runtime, licences, icons); elsewhere (programs
/// copied to `~/.local/bin`, say) only the programs already there are replaced.
pub const BUNDLE_MARKER: &str = "lumenwall.bundle";

const STAGING: &str = ".lumenwall-update";
const OLD_SUFFIX: &str = ".lumenwall-old";
const EXE: &str = std::env::consts::EXE_SUFFIX;

/// Where Lumenwall is installed and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub dir: PathBuf,
    pub bundle: bool,
}

impl Layout {
    pub fn of_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let bundle = dir.join(BUNDLE_MARKER).is_file();
        Self { dir, bundle }
    }

    /// Path of a program of this installation (`lumenwall-ui` → `lumenwall-ui.exe` on
    /// Windows).
    pub fn program(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}{EXE}"))
    }
}

/// What [`install`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: Version,
    pub layout: Layout,
    /// Files replaced or added, relative to the installation folder.
    pub files: Vec<PathBuf>,
}

/// Installs a verified archive into `layout`. On success the archive is deleted.
pub fn install(verified: &Verified, layout: &Layout) -> Result<Installed, Error> {
    let staging = layout.dir.join(STAGING);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| Error::io(staging.display(), &e))?;
    let result = (|| {
        let files = archive::extract(&verified.archive, &staging)?;
        let program = |name: &str| PathBuf::from(format!("{name}{EXE}"));
        if !files.contains(&program("lumenwall")) || !files.contains(&program("lumenwall-ui")) {
            return Err(Error::BadArchive(
                "the Lumenwall programs are missing".into(),
            ));
        }
        let plan: Vec<PathBuf> = if layout.bundle {
            files
        } else {
            PROGRAMS
                .iter()
                .map(|p| program(p))
                .filter(|p| files.contains(p) && layout.dir.join(p).is_file())
                .collect()
        };
        if plan.is_empty() {
            return Err(Error::BadArchive(
                "no Lumenwall program in the installation folder".into(),
            ));
        }
        replace_all(&staging, &layout.dir, &plan)?;
        Ok(plan)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    let files = result?;
    let _ = std::fs::remove_file(&verified.archive);
    tracing::info!(
        version = %verified.version,
        dir = %layout.dir.display(),
        files = files.len(),
        "update installed"
    );
    Ok(Installed {
        version: verified.version.clone(),
        layout: layout.clone(),
        files,
    })
}

/// Removes what an earlier update left behind in `dir`: the staging folder, and
/// backups of programs that were still running then (Windows). Cheap; call it at start.
pub fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir.join(STAGING));
    remove_backups(dir, 0);
}

fn remove_backups(dir: &Path, depth: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() && depth < 3 {
            remove_backups(&path, depth + 1);
        } else if kind.is_file()
            && entry.file_name().to_string_lossy().ends_with(OLD_SUFFIX)
            && let Err(e) = std::fs::remove_file(&path)
        {
            tracing::debug!("{} still in use: {e}", path.display());
        }
    }
}

struct Replaced {
    target: PathBuf,
    backup: Option<PathBuf>,
}

fn replace_all(staging: &Path, dir: &Path, plan: &[PathBuf]) -> Result<(), Error> {
    let mut done: Vec<Replaced> = Vec::with_capacity(plan.len());
    for rel in plan {
        match replace_one(&staging.join(rel), &dir.join(rel)) {
            Ok(r) => done.push(r),
            Err(e) => {
                rollback(&done);
                return Err(e);
            }
        }
    }
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    for r in &done {
        if let Some(b) = &r.backup
            && let Err(e) = std::fs::remove_file(b)
        {
            // Windows: the old program is still running; `cleanup` retries later.
            tracing::debug!("backup {} kept for now: {e}", b.display());
        }
    }
    Ok(())
}

fn replace_one(new: &Path, target: &Path) -> Result<Replaced, Error> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), &e))?;
    }
    // The content is on disk before the name points to it.
    File::open(new)
        .and_then(|f| f.sync_all())
        .map_err(|e| Error::io(new.display(), &e))?;
    let backup = if std::fs::symlink_metadata(target).is_ok() {
        let b = backup_path(target);
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&b);
            if std::fs::hard_link(target, &b).is_err() {
                std::fs::copy(target, &b).map_err(|e| Error::io(target.display(), &e))?;
            }
        }
        #[cfg(not(unix))]
        std::fs::rename(target, &b).map_err(|e| Error::io(target.display(), &e))?;
        Some(b)
    } else {
        None
    };
    if let Err(e) = std::fs::rename(new, target) {
        if let Some(b) = &backup {
            #[cfg(unix)]
            let _ = std::fs::remove_file(b);
            #[cfg(not(unix))]
            let _ = std::fs::rename(b, target);
        }
        return Err(Error::io(target.display(), &e));
    }
    Ok(Replaced {
        target: target.to_owned(),
        backup,
    })
}

fn rollback(done: &[Replaced]) {
    for r in done.iter().rev() {
        let restored = match &r.backup {
            Some(b) => {
                // Windows cannot rename over a file; the new one is not running.
                #[cfg(not(unix))]
                let _ = std::fs::remove_file(&r.target);
                std::fs::rename(b, &r.target)
            }
            None => std::fs::remove_file(&r.target),
        };
        if let Err(e) = restored {
            tracing::error!("could not restore {}: {e}", r.target.display());
        }
    }
}

fn backup_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let first = target.with_file_name(format!("{name}{OLD_SUFFIX}"));
    if cfg!(unix) {
        return first;
    }
    // A backup from an earlier update may still be held by a running old process.
    let free = |p: &Path| !p.exists() || std::fs::remove_file(p).is_ok();
    if free(&first) {
        return first;
    }
    (1..100)
        .map(|n| target.with_file_name(format!("{name}.{n}{OLD_SUFFIX}")))
        .find(|p| free(p))
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_programs_and_bundle_marker() {
        let dir = tempfile::tempdir().unwrap();
        let l = Layout::of_dir(dir.path());
        assert!(!l.bundle);
        assert_eq!(
            l.program("lumenwall-ui"),
            dir.path().join(format!("lumenwall-ui{EXE}"))
        );
        std::fs::write(dir.path().join(BUNDLE_MARKER), "x").unwrap();
        assert!(Layout::of_dir(dir.path()).bundle);
    }

    #[test]
    fn rollback_restores_replaced_files() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join(STAGING);
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(dir.path().join("a"), "old a").unwrap();
        std::fs::write(staging.join("a"), "new a").unwrap();
        std::fs::write(staging.join("b"), "new b").unwrap();
        // "c" is missing from staging: the third step fails.
        let plan = [PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        assert!(replace_all(&staging, dir.path(), &plan).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a")).unwrap(),
            "old a"
        );
        assert!(!dir.path().join("b").exists());
    }

    #[test]
    fn cleanup_removes_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(STAGING).join("x")).unwrap();
        std::fs::create_dir(dir.path().join("locales")).unwrap();
        std::fs::write(dir.path().join(format!("lumenwall.exe{OLD_SUFFIX}")), "").unwrap();
        std::fs::write(dir.path().join(format!("locales/en.pak{OLD_SUFFIX}")), "").unwrap();
        std::fs::write(dir.path().join("lumenwall"), "keep").unwrap();
        cleanup(dir.path());
        let mut left: Vec<_> = walk(dir.path());
        left.sort();
        assert_eq!(left, ["locales".to_owned(), "lumenwall".to_owned()]);
    }

    fn walk(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }
}

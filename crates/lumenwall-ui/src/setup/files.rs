//! File operations shared by the Linux and Windows setup: atomic writes, program
//! replacement that works while the program runs, comparisons.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::State;

/// Suffix of a replaced program still in use on Windows (the updater's convention:
/// `lumenwall_update::cleanup` deletes these at the next start).
const OLD_SUFFIX: &str = ".lumenwall-old";

pub fn err(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

/// Same folder, through links and `..`.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub fn same_bytes(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) if ma.len() == mb.len() => {
            matches!((std::fs::read(a), std::fs::read(b)), (Ok(x), Ok(y)) if x == y)
        }
        _ => false,
    }
}

/// `Ok` if every program of `src` has an identical copy in `dst`, `Outdated` if some
/// copy exists but differs or is missing, `Missing` if none exists.
pub fn copies_state(src: &Path, dst: &Path, names: &[&str]) -> State {
    let mut any = false;
    let mut same = true;
    for n in names {
        let d = dst.join(n);
        if d.is_file() {
            any = true;
            same &= same_bytes(&src.join(n), &d);
        } else {
            same = false;
        }
    }
    match (any, same) {
        (false, _) => State::Missing,
        (true, true) => State::Ok,
        (true, false) => State::Outdated,
    }
}

fn temp_name(dst: &Path) -> PathBuf {
    let name = dst.file_name().unwrap_or_default().to_string_lossy();
    dst.with_file_name(format!(".{name}.lumenwall-tmp"))
}

/// Writes `bytes` beside `path`, flushes, then renames over it.
fn write_atomic(path: &Path, bytes: &[u8], executable: bool) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| err(dir, e))?;
    }
    let tmp = temp_name(path);
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        set_mode(&tmp, executable)?;
        move_into_place(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|e| err(path, e))
}

#[cfg(unix)]
fn set_mode(p: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: bool) -> std::io::Result<()> {
    Ok(())
}

/// Renames `tmp` to `dst`. On Windows a program in use cannot be overwritten but can
/// be renamed: then the current file is moved aside to `<name>.lumenwall-old` first.
fn move_into_place(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    let first = std::fs::rename(tmp, dst);
    if first.is_ok() || !cfg!(windows) || !dst.exists() {
        return first;
    }
    let name = dst.file_name().unwrap_or_default().to_string_lossy();
    for n in 0..10 {
        let old = if n == 0 {
            dst.with_file_name(format!("{name}{OLD_SUFFIX}"))
        } else {
            dst.with_file_name(format!("{name}.{n}{OLD_SUFFIX}"))
        };
        // A backup from last time may still be in use: try the next name.
        if old.exists() && std::fs::remove_file(&old).is_err() {
            continue;
        }
        if std::fs::rename(dst, &old).is_ok() {
            return std::fs::rename(tmp, dst);
        }
    }
    first
}

/// Copies the program `src` to `dst`, replacing it even while it runs.
pub fn replace(src: &Path, dst: &Path, executable: bool) -> Result<(), String> {
    let bytes = std::fs::read(src).map_err(|e| err(src, e))?;
    write_atomic(dst, &bytes, executable)
}

/// Writes `bytes` to `path` unless it already holds exactly them.
#[cfg_attr(windows, allow(dead_code))]
pub fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if std::fs::read(path).is_ok_and(|b| b == bytes) {
        return Ok(());
    }
    write_atomic(path, bytes, false)
}

/// Replaces the folder `dir` with exactly `files`, built beside it first.
#[cfg_attr(windows, allow(dead_code))]
pub fn replace_dir(dir: &Path, files: &[(&str, &[u8])]) -> Result<(), String> {
    let parent = dir
        .parent()
        .ok_or_else(|| format!("{}: no parent", dir.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| err(parent, e))?;
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    let tmp = parent.join(format!(".{name}.lumenwall-tmp"));
    let old = parent.join(format!(".{name}.lumenwall-old"));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&old);
    let build = (|| -> std::io::Result<()> {
        std::fs::create_dir(&tmp)?;
        for (rel, bytes) in files {
            let p = tmp.join(rel);
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&p, bytes)?;
        }
        Ok(())
    })();
    if let Err(e) = build {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(err(dir, e));
    }
    if dir.exists()
        && let Err(e) = std::fs::rename(dir, &old)
    {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(err(dir, e));
    }
    if let Err(e) = std::fs::rename(&tmp, dir) {
        // Put the previous version back.
        let _ = std::fs::rename(&old, dir);
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(err(dir, e));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Removes a file or a link; nothing there is fine.
pub fn remove(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(err(path, e)),
        _ => Ok(()),
    }
}

/// Copies the folder `src` into `dst` (created if needed), file by file with
/// [`replace`]. Skips the updater's staging folder and backups. (Windows: the
/// extracted zip into the installation folder.)
#[cfg(any(windows, test))]
pub fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| err(dst, e))?;
    let entries = std::fs::read_dir(src).map_err(|e| err(src, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| err(src, e))?;
        let name = entry.file_name();
        let lossy = name.to_string_lossy();
        if lossy == ".lumenwall-update"
            || lossy.ends_with(OLD_SUFFIX)
            || lossy.ends_with(".lumenwall-tmp")
        {
            continue;
        }
        let kind = entry.file_type().map_err(|e| err(&entry.path(), e))?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &dst.join(&name))?;
        } else if kind.is_file() {
            let target = dst.join(&name);
            if !same_bytes(&entry.path(), &target) {
                let exe = lossy.ends_with(".exe") || lossy.ends_with(".dll");
                replace(&entry.path(), &target, exe || is_executable(&entry.path()))?;
            }
        }
    }
    Ok(())
}

#[cfg(all(unix, test))]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable(_: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::TempDir;

    #[test]
    fn writes_are_atomic_and_idempotent() {
        let tmp = TempDir::new("files");
        let p = tmp.path().join("a/b/file.txt");
        write_if_changed(&p, b"one").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        write_if_changed(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        remove(&p).unwrap();
        remove(&p).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn copies_and_comparisons() {
        let tmp = TempDir::new("files");
        let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("prog"), "p").unwrap();
        std::fs::write(src.join("sub/data"), "d").unwrap();
        std::fs::write(src.join("prog.lumenwall-old"), "old").unwrap();
        std::fs::create_dir_all(src.join(".lumenwall-update")).unwrap();
        assert_eq!(copies_state(&src, &dst, &["prog"]), State::Missing);
        copy_tree(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("sub/data")).unwrap(), "d");
        assert!(!dst.join("prog.lumenwall-old").exists());
        assert!(!dst.join(".lumenwall-update").exists());
        assert_eq!(copies_state(&src, &dst, &["prog"]), State::Ok);
        std::fs::write(dst.join("prog"), "other").unwrap();
        assert_eq!(copies_state(&src, &dst, &["prog"]), State::Outdated);
        assert!(same_dir(&src, &src.join("sub/..")));
        assert!(!same_dir(&src, &dst));
    }
}

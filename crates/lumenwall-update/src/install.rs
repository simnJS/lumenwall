//! Replacing the installed files with a verified archive's content.
//!
//! The archive is extracted into a staging folder inside the installation folder
//! (same file system, so moves are renames). Files whose content is already installed,
//! byte for byte, are left alone: an update that keeps the same FFmpeg and CEF touches
//! none of their ≈ 500 MB, nor the files a running engine or web host has open. Then,
//! file by file, new files (and their folders, `locales\`) are moved in, and changed
//! ones replaced:
//!
//! - Linux: the current file gets a hard-linked backup, and the new one is renamed over
//!   it: the path always names a complete program, and running processes keep the old
//!   one open.
//! - Windows: a running `.exe` or a loaded `.dll` (`libcef.dll` in a web host, FFmpeg's
//!   DLLs in the engine) can be renamed but not overwritten, so the current file is
//!   renamed to `<name>.lumenwall-old` and the new one moved in place. Backups still in
//!   use are deleted by [`cleanup`] at the next start. A file another program holds open
//!   without allowing that rename cannot be replaced: the update stops with a message
//!   naming it.
//!
//! If any step fails, the files already replaced are put back.

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
    /// Files replaced or added, relative to the installation folder (not the ones that
    /// were already installed with the same content).
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
        let plan: Vec<PathBuf> = plan
            .into_iter()
            .filter(|rel| !unchanged(&staging.join(rel), &layout.dir.join(rel)))
            .collect();
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

/// Whether `current` already holds exactly `new` (Unix: with the same executable bit),
/// so that installing `new` would change nothing.
fn unchanged(new: &Path, current: &Path) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::metadata(new), std::fs::symlink_metadata(current)) else {
        return false;
    };
    if !b.is_file() || a.len() != b.len() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let exec = |m: &std::fs::Metadata| m.permissions().mode() & 0o111 != 0;
        if exec(&a) != exec(&b) {
            return false;
        }
    }
    same_bytes(new, current).unwrap_or(false)
}

/// Compares two files of the same length, chunk by chunk (stops at the first difference).
fn same_bytes(a: &Path, b: &Path) -> std::io::Result<bool> {
    const CHUNK: usize = 1 << 20;
    let (mut a, mut b) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    let (mut x, mut y) = (vec![0; CHUNK], vec![0; CHUNK]);
    loop {
        let n = fill(&mut a, &mut x)?;
        let m = fill(&mut b, &mut y)?;
        if x[..n] != y[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buf` is full or the file ends.
fn fill(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read;
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// The error for a file that could not be moved aside or into place. On Windows, a
/// program holding the file open without sharing its deletion (ERROR_SHARING_VIOLATION,
/// ERROR_LOCK_VIOLATION) blocks any rename: say so, the user can close it and retry.
fn move_error(path: &Path, e: &std::io::Error) -> Error {
    if cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)) {
        return Error::Io(format!(
            "{} is open in another program and cannot be replaced; the update was undone, \
             close that program and update again",
            path.display()
        ));
    }
    Error::io(path.display(), e)
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
    if let Ok(d) = std::fs::File::open(dir) {
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
    // The content is on disk before the name points to it (write access: Windows
    // refuses to flush a file opened read-only).
    std::fs::OpenOptions::new()
        .write(true)
        .open(new)
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
        std::fs::rename(target, &b).map_err(|e| move_error(target, &e))?;
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
        return Err(move_error(target, &e));
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

    /// A release zip (`lumenwall-0.2.0/<name>` for each entry) and its `Verified`.
    fn release(dir: &Path, entries: &[(&str, &[u8])]) -> Verified {
        use std::io::Write;
        let archive = dir.join("lumenwall-0.2.0-test.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            z.start_file(format!("lumenwall-0.2.0/{name}"), opts)
                .unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        Verified {
            archive,
            version: Version::new(0, 2, 0),
            sha256: String::new(),
        }
    }

    fn exe(name: &str) -> String {
        format!("{name}{EXE}")
    }

    /// A bundle folder holding version 0.1.0's programs.
    fn bundle(dir: &Path) -> Layout {
        let app = dir.join("app");
        std::fs::create_dir(&app).unwrap();
        std::fs::write(app.join(exe("lumenwall")), "old engine").unwrap();
        std::fs::write(app.join(exe("lumenwall-ui")), "old app").unwrap();
        std::fs::write(app.join(BUNDLE_MARKER), "0.1.0").unwrap();
        let layout = Layout::of_dir(app);
        assert!(layout.bundle);
        layout
    }

    #[test]
    fn unchanged_detects_identical_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        // Larger than one comparison chunk, differing only in the last byte.
        let mut big = vec![7u8; (1 << 20) + 5];
        std::fs::write(p("a"), &big).unwrap();
        std::fs::write(p("b"), &big).unwrap();
        *big.last_mut().unwrap() = 8;
        std::fs::write(p("c"), &big).unwrap();
        std::fs::write(p("short"), [7u8; 3]).unwrap();
        std::fs::create_dir(p("folder")).unwrap();
        assert!(unchanged(&p("a"), &p("b")));
        assert!(!unchanged(&p("a"), &p("c")));
        assert!(!unchanged(&p("a"), &p("short")));
        assert!(!unchanged(&p("a"), &p("missing")));
        assert!(!unchanged(&p("a"), &p("folder")));
        assert!(unchanged(&p("short"), &p("short")));
    }

    #[test]
    fn new_files_and_folders_are_added_and_unchanged_ones_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let layout = bundle(dir.path());
        // CEF was not updated: its DLL is already there, byte for byte.
        std::fs::write(layout.dir.join("libcef.dll"), "cef 156").unwrap();
        let held = hold_open(&layout.dir.join("libcef.dll"));
        let verified = release(
            dir.path(),
            &[
                (&exe("lumenwall"), b"new engine"),
                (&exe("lumenwall-ui"), b"new app"),
                (BUNDLE_MARKER, b"0.2.0"),
                ("libcef.dll", b"cef 156"),
                ("avcodec-62.dll", b"ffmpeg 8"),
                ("locales/en-US.pak", b"en-US"),
                ("locales/fr.pak", b"fr"),
            ],
        );
        let installed = install(&verified, &layout).expect("installed");
        drop(held);
        let mut files: Vec<String> = installed
            .files
            .iter()
            .map(|f| f.to_string_lossy().replace('\\', "/"))
            .collect();
        files.sort();
        let mut want = vec![
            exe("lumenwall"),
            exe("lumenwall-ui"),
            BUNDLE_MARKER.to_owned(),
            "avcodec-62.dll".to_owned(),
            "locales/en-US.pak".to_owned(),
            "locales/fr.pak".to_owned(),
        ];
        want.sort();
        assert_eq!(files, want, "libcef.dll is not in the plan");
        let read = |n: &str| std::fs::read_to_string(layout.dir.join(n)).unwrap();
        assert_eq!(read(&exe("lumenwall")), "new engine");
        assert_eq!(read("avcodec-62.dll"), "ffmpeg 8");
        assert_eq!(read("locales/fr.pak"), "fr");
        assert_eq!(read("libcef.dll"), "cef 156");
        assert!(!layout.dir.join(STAGING).exists());
        assert!(!verified.archive.exists());
    }

    /// Opens `path` without sharing its deletion, as a program reading it might (a
    /// virus scanner, a backup tool; Python's `open` does): on Windows no rename of it
    /// can succeed while the handle lives. (CEF itself shares deletion: every file of a
    /// release folder can be renamed while its web host runs.)
    fn hold_open(path: &Path) -> std::fs::File {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::share_mode(
            &mut options,
            1, /* FILE_SHARE_READ */
        );
        options.open(path).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_changed_file_held_open_stops_the_update_and_everything_is_put_back() {
        let dir = tempfile::tempdir().unwrap();
        let layout = bundle(dir.path());
        std::fs::write(layout.dir.join("resources.pak"), "cef 156").unwrap();
        let held = hold_open(&layout.dir.join("resources.pak"));
        let verified = release(
            dir.path(),
            &[
                (&exe("lumenwall"), b"new engine"),
                (&exe("lumenwall-ui"), b"new app"),
                ("locales/en-US.pak", b"en-US"),
                ("resources.pak", b"cef 157"),
            ],
        );
        let err = install(&verified, &layout).expect_err("resources.pak is in use");
        let Error::Io(msg) = &err else {
            panic!("{err:?}")
        };
        assert!(
            msg.contains("resources.pak") && msg.contains("open in another program"),
            "{msg}"
        );
        drop(held);
        let read = |n: &str| std::fs::read_to_string(layout.dir.join(n)).unwrap();
        assert_eq!(read(&exe("lumenwall")), "old engine", "put back");
        assert_eq!(read(&exe("lumenwall-ui")), "old app", "put back");
        assert_eq!(read("resources.pak"), "cef 156");
        assert!(!layout.dir.join("locales").join("en-US.pak").exists());
        let mut left = walk(&layout.dir);
        left.sort();
        // The emptied locales folder may stay; no backup, no staging folder.
        left.retain(|n| n != "locales");
        let mut want = vec![
            exe("lumenwall"),
            exe("lumenwall-ui"),
            BUNDLE_MARKER.into(),
            "resources.pak".into(),
        ];
        want.sort();
        assert_eq!(left, want);
        assert!(verified.archive.exists(), "kept for another try");
    }

    /// A DLL loaded by a running program (`libcef.dll` in a web host, FFmpeg's DLLs in
    /// the engine): mapped as an image, it cannot be deleted or overwritten, only renamed.
    #[cfg(windows)]
    #[test]
    fn a_loaded_dll_is_moved_aside_and_its_backup_removed_once_unloaded() {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LoadLibraryExW(name: *const u16, file: isize, flags: u32) -> isize;
            fn FreeLibrary(module: isize) -> i32;
        }
        const LOAD_LIBRARY_AS_IMAGE_RESOURCE: u32 = 0x20;

        let dir = tempfile::tempdir().unwrap();
        let layout = bundle(dir.path());
        let system = std::env::var_os("SystemRoot").expect("SystemRoot");
        let dll = layout.dir.join("libcef.dll");
        std::fs::copy(
            Path::new(&system).join("System32").join("version.dll"),
            &dll,
        )
        .unwrap();
        let wide: Vec<u16> = dll.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: a NUL-terminated path; mapped as an image resource, no code runs.
        let module = unsafe { LoadLibraryExW(wide.as_ptr(), 0, LOAD_LIBRARY_AS_IMAGE_RESOURCE) };
        assert_ne!(module, 0, "{}", std::io::Error::last_os_error());
        assert!(
            std::fs::remove_file(&dll).is_err(),
            "a loaded DLL cannot be deleted"
        );

        let verified = release(
            dir.path(),
            &[
                (&exe("lumenwall"), b"new engine"),
                (&exe("lumenwall-ui"), b"new app"),
                ("libcef.dll", b"cef 157"),
            ],
        );
        let installed = install(&verified, &layout).expect("installed");
        assert!(installed.files.contains(&PathBuf::from("libcef.dll")));
        assert_eq!(std::fs::read(&dll).unwrap(), b"cef 157");
        let backup = layout.dir.join(format!("libcef.dll{OLD_SUFFIX}"));
        assert!(backup.is_file(), "the loaded one was moved aside");
        cleanup(&layout.dir);
        assert!(backup.is_file(), "still loaded: kept");
        // SAFETY: the module loaded above, freed once.
        assert_ne!(unsafe { FreeLibrary(module) }, 0);
        cleanup(&layout.dir);
        assert!(!backup.exists(), "removed at the next start");
    }
}

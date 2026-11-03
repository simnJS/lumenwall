//! Configuration file watcher (Lumenwall extension): OS change notifications through `notify`
//! (inotify, ReadDirectoryChangesW, FSEvents), never polling.
//!
//! The parent directory is watched rather than the file, so that editors replacing the file
//! (write to a temporary file, then rename) and a file created after startup are seen.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Quiet time after the last event before the file is read: editors save in several steps.
const SETTLE: Duration = Duration::from_millis(150);

/// A change of the watched file, reported once its contents actually changed.
#[derive(Debug)]
pub enum FileChange {
    Contents(Vec<u8>),
    Removed,
    Unreadable(io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("'{0}' has no file name")]
    NoFileName(PathBuf),
    #[error(transparent)]
    Notify(#[from] notify::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Watches one file until dropped.
#[derive(Debug)]
pub struct FileWatcher {
    watcher: Option<RecommendedWatcher>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        // Dropping the watcher closes the event channel, which ends the thread.
        drop(self.watcher.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Calls `on_change` on a background thread whenever the contents of `path` change.
/// `initial` is the content already loaded, so that touching the file is not a change.
pub fn watch_file(
    path: &Path,
    initial: Option<Vec<u8>>,
    mut on_change: impl FnMut(FileChange) + Send + 'static,
) -> Result<FileWatcher, WatchError> {
    let name: OsString = path
        .file_name()
        .ok_or_else(|| WatchError::NoFileName(path.to_owned()))?
        .to_owned();
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_owned(),
        _ => PathBuf::from("."),
    };
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(tx)?;
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;

    let path = path.to_owned();
    let thread = thread::Builder::new()
        .name("lumenwall-config-watch".into())
        .spawn(move || {
            let mut last = initial;
            while let Ok(event) = rx.recv() {
                let relevant = match &event {
                    Ok(event) => {
                        !matches!(event.kind, EventKind::Access(_))
                            && event
                                .paths
                                .iter()
                                .any(|p| p.file_name() == Some(name.as_os_str()))
                    }
                    // Lost events (queue overflow...): re-read to be safe.
                    Err(_) => true,
                };
                if !relevant {
                    continue;
                }
                loop {
                    match rx.recv_timeout(SETTLE) {
                        Ok(_) => {}
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                let change = match std::fs::read(&path) {
                    Ok(bytes) if last.as_ref() == Some(&bytes) => continue,
                    Ok(bytes) => {
                        last = Some(bytes.clone());
                        FileChange::Contents(bytes)
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        if last.take().is_none() {
                            continue;
                        }
                        FileChange::Removed
                    }
                    Err(e) => FileChange::Unreadable(e),
                };
                on_change(change);
            }
        })?;
    Ok(FileWatcher {
        watcher: Some(watcher),
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    const WAIT: Duration = Duration::from_secs(10);

    fn next(rx: &Receiver<FileChange>) -> FileChange {
        rx.recv_timeout(WAIT).expect("no change reported")
    }

    #[test]
    fn reports_creation_modification_rename_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (tx, rx) = mpsc::channel();
        let _watcher = watch_file(&path, None, move |change| tx.send(change).unwrap()).unwrap();

        std::fs::write(&path, "fps = 10").unwrap();
        assert!(matches!(next(&rx), FileChange::Contents(b) if b == b"fps = 10"));

        std::fs::write(&path, "fps = 20").unwrap();
        assert!(matches!(next(&rx), FileChange::Contents(b) if b == b"fps = 20"));

        // Same contents: no report. Then an atomic replace, as editors do.
        std::fs::write(&path, "fps = 20").unwrap();
        let tmp = dir.path().join(".config.toml.swp");
        std::fs::write(&tmp, "fps = 30").unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        assert!(matches!(next(&rx), FileChange::Contents(b) if b == b"fps = 30"));

        std::fs::remove_file(&path).unwrap();
        assert!(matches!(next(&rx), FileChange::Removed));

        // Unrelated files in the directory are ignored.
        std::fs::write(dir.path().join("other.toml"), "x").unwrap();
        assert!(rx.recv_timeout(SETTLE * 4).is_err());
    }

    #[test]
    fn initial_contents_are_not_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "a = 1").unwrap();
        let (tx, rx) = mpsc::channel();
        let watcher =
            watch_file(&path, Some(b"a = 1".to_vec()), move |c| tx.send(c).unwrap()).unwrap();
        std::fs::write(&path, "a = 1").unwrap();
        assert!(rx.recv_timeout(SETTLE * 4).is_err());
        drop(watcher); // joins the thread
        assert!(rx.recv().is_err());
    }

    #[test]
    fn missing_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("config.toml");
        assert!(watch_file(&path, None, |_| {}).is_err());
    }
}

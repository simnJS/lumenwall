//! Notices new Workshop subscriptions through file-system events (inotify on Linux,
//! ReadDirectoryChangesW on Windows): no polling. The `431960` content folders are
//! watched (not recursively); a new item folder gets its own watch so that files
//! written into it are seen too. Whether an item is complete is decided by the UI
//! (quiet for a moment and `project.json` readable).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use iced::futures::channel::mpsc::unbounded;
use iced::futures::{SinkExt, StreamExt};
use notify::{EventKind, RecursiveMode, Watcher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// Something appeared or changed in this item folder.
    Changed(PathBuf),
    /// The item folder is gone (unsubscribed).
    Removed(PathBuf),
}

/// Watches `dirs` for as long as the subscription lives.
pub fn subscription(dirs: Vec<PathBuf>) -> iced::Subscription<WatchEvent> {
    iced::Subscription::run_with(dirs, |dirs| {
        let dirs = dirs.clone();
        iced::stream::channel(32, async move |mut output| {
            let (tx, mut rx) = unbounded();
            let spawned = std::thread::Builder::new()
                .name("lumen-ui-watch".into())
                .spawn(move || {
                    run(&dirs, |e| tx.unbounded_send(e).is_ok());
                });
            if spawned.is_err() {
                return;
            }
            while let Some(e) = rx.next().await {
                if output.send(e).await.is_err() {
                    break;
                }
            }
        })
    })
}

/// Blocks, translating file events under `dirs` into item events until `send`
/// returns false or the watcher fails.
pub fn run(dirs: &[PathBuf], send: impl Fn(WatchEvent) -> bool) {
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = ev_tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("cannot watch the Workshop folders: {e}");
            return;
        }
    };
    for d in dirs {
        if let Err(e) = watcher.watch(d, RecursiveMode::NonRecursive) {
            tracing::warn!("cannot watch {}: {e}", d.display());
        }
    }
    let mut watched: HashSet<PathBuf> = HashSet::new();
    for res in ev_rx {
        let Ok(event) = res else { continue };
        for path in &event.paths {
            let Some(item) = item_dir(dirs, path) else {
                continue;
            };
            let gone =
                matches!(event.kind, EventKind::Remove(_)) && path == &item && !item.exists();
            let msg = if gone {
                if watched.remove(&item) {
                    let _ = watcher.unwatch(&item);
                }
                WatchEvent::Removed(item)
            } else {
                if item.is_dir() && watched.insert(item.clone()) {
                    let _ = watcher.watch(&item, RecursiveMode::NonRecursive);
                }
                WatchEvent::Changed(item)
            };
            if !send(msg) {
                return;
            }
        }
    }
}

/// The item folder (direct child of a watched dir) that `path` belongs to.
fn item_dir(dirs: &[PathBuf], path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if dirs.iter().any(|d| d == parent) {
        return Some(path.to_owned());
    }
    let grand = parent.parent()?;
    dirs.iter().any(|d| d == grand).then(|| parent.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn new_items_are_reported_by_events() {
        let root = std::env::temp_dir().join(format!("lumen-ui-watch-{}", std::process::id()));
        let ws = root.join("431960");
        std::fs::create_dir_all(&ws).unwrap();
        let (tx, rx) = mpsc::channel();
        let dirs = vec![ws.clone()];
        std::thread::spawn(move || run(&dirs, |e| tx.send(e).is_ok()));
        std::thread::sleep(Duration::from_millis(200));

        let item = ws.join("3100000099");
        std::fs::create_dir(&item).unwrap();
        let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first, WatchEvent::Changed(item.clone()));
        // Files written inside the new folder are seen through its own watch.
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(item.join("project.json"), "{}").unwrap();
        let got = (0..10)
            .filter_map(|_| rx.recv_timeout(Duration::from_secs(2)).ok())
            .find(|e| *e == WatchEvent::Changed(item.clone()));
        assert!(got.is_some());
        std::fs::remove_dir_all(&item).unwrap();
        let removed = (0..20)
            .filter_map(|_| rx.recv_timeout(Duration::from_secs(2)).ok())
            .any(|e| e == WatchEvent::Removed(item.clone()));
        assert!(removed);
        let _ = std::fs::remove_dir_all(&root);
    }
}

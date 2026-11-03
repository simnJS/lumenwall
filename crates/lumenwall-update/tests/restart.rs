//! Engine restart over a real IPC endpoint: the old daemon is asked to stop, the new
//! one is started only once the old one is gone, and its version is reported.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use lumenwall_ipc::{Endpoint, Handler, RemoteError, Server};
use lumenwall_update::{DaemonRestart, restart_daemon};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn endpoint() -> Endpoint {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let id = std::process::id();
    if cfg!(unix) {
        let dir = std::env::temp_dir().join(format!("lumenwall-update-test-{id}"));
        Endpoint::Path(dir.join(format!("s{n}.sock")))
    } else {
        Endpoint::Name(format!("lumenwall-update-test-{id}-{n}"))
    }
}

/// A daemon that only knows how to stop.
struct Engine {
    stop: Mutex<Option<mpsc::Sender<()>>>,
}

impl Handler for Engine {
    fn shutdown(&self) -> Result<(), RemoteError> {
        if let Some(tx) = self.stop.lock().unwrap().take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}

#[test]
fn restarts_the_engine_on_the_new_version() {
    let ep = endpoint();
    let (tx, rx) = mpsc::channel();
    let old = Server::bind(
        &ep,
        Arc::new(Engine {
            stop: Mutex::new(Some(tx)),
        }),
        "lumenwall",
        "0.1.0",
    )
    .unwrap();
    // The old engine leaves a moment after being asked, like the real one.
    let stopper = std::thread::spawn(move || {
        rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        drop(old);
    });
    let started: Arc<Mutex<Option<Server>>> = Arc::default();
    let (slot, ep2) = (Arc::clone(&started), ep.clone());
    let result = restart_daemon(&ep, Path::new("/opt/lumenwall/lumenwall"), move |exe| {
        assert_eq!(exe, Path::new("/opt/lumenwall/lumenwall"));
        let new = Server::bind(
            &ep2,
            Arc::new(Engine {
                stop: Mutex::new(None),
            }),
            "lumenwall",
            "0.2.0",
        )
        .map_err(std::io::Error::other)?;
        *slot.lock().unwrap() = Some(new);
        Ok(())
    });
    stopper.join().unwrap();
    assert_eq!(
        result,
        Ok(DaemonRestart::Restarted {
            version: "0.2.0".into()
        })
    );
    assert!(started.lock().unwrap().is_some());
}

#[test]
fn nothing_runs_nothing_starts() {
    let result = restart_daemon(&endpoint(), Path::new("lumenwall"), |_| {
        panic!("must not start an engine that was not running")
    });
    assert_eq!(result, Ok(DaemonRestart::NotRunning));
}

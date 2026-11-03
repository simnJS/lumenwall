//! Restarting the engine on its new version, through the existing IPC: `shutdown`,
//! wait until nothing answers on the endpoint any more (a second `lumenwall` started
//! while the old one still listens would only hand its wallpaper over and exit), then
//! start the new program and wait for its handshake.
//!
//! Every wait is bounded; nothing here loops on its own.

use std::path::Path;
use std::time::{Duration, Instant};

use lumenwall_ipc::{Client, Endpoint, Error as IpcError};

use crate::Error;

const CLIENT_NAME: &str = "lumenwall-update";
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const START_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonRestart {
    /// No engine was running: nothing was started.
    NotRunning,
    /// The new engine answers, reporting `version`.
    Restarted { version: String },
}

/// Stops the engine listening on `endpoint` and starts `exe` instead, with `spawn`
/// (which must detach the process, e.g. `lumenwall-ui`'s `spawn_daemon`).
pub fn restart_daemon(
    endpoint: &Endpoint,
    exe: &Path,
    spawn: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<DaemonRestart, Error> {
    let client = match Client::connect(endpoint, CLIENT_NAME) {
        Ok(c) => c,
        Err(IpcError::NotRunning) => return Ok(DaemonRestart::NotRunning),
        Err(e) => return Err(Error::Restart(format!("cannot reach the engine: {e}"))),
    };
    match client.shutdown() {
        // The engine may hang up before answering.
        Ok(()) | Err(IpcError::Disconnected | IpcError::Timeout) => {}
        Err(e) => return Err(Error::Restart(format!("the engine refused to stop: {e}"))),
    }
    drop(client);
    let stopped = wait(STOP_TIMEOUT, || {
        matches!(
            Client::connect(endpoint, CLIENT_NAME),
            Err(IpcError::NotRunning)
        )
    });
    if !stopped {
        return Err(Error::Restart(format!(
            "the engine did not stop within {} s",
            STOP_TIMEOUT.as_secs()
        )));
    }
    spawn(exe).map_err(|e| Error::Restart(format!("cannot start {}: {e}", exe.display())))?;
    let mut version = None;
    wait(START_TIMEOUT, || {
        match Client::connect(endpoint, CLIENT_NAME) {
            Ok(c) => {
                version = Some(c.server_info().version.clone());
                true
            }
            Err(_) => false,
        }
    });
    let version = version.ok_or_else(|| Error::Restart("the new engine does not answer".into()))?;
    tracing::info!(version, "engine restarted");
    Ok(DaemonRestart::Restarted { version })
}

/// Calls `done` with growing pauses (50 ms to 1 s) until it returns true or `limit`
/// has passed.
fn wait(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    let mut pause = Duration::from_millis(50);
    loop {
        if done() {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_secs(1));
    }
}

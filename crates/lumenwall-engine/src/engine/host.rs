//! The engine process: `lumenwall --lumenwall-engine-host`, started by the daemon
//! ([`super::remote`]) with pipes on stdin and stdout. It runs the engine loop ([`Local`]) on its
//! main thread and nothing else: no command line, no configuration, no IPC server.
//!
//! stdin carries [`ToHost`] lines (the first one is [`ToHost::Init`]), stdout [`FromHost`]
//! lines: the answer to each plan, the monitor's report whenever it changes, frame counters on
//! request and the log records ([`super::logfwd`]). End of stdin (the daemon went away) stops
//! the engine like a stop request.

use std::ffi::OsStr;
use std::io::Write as _;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use super::logfwd::Forwarder;
use super::wire::{FromHost, ToHost};
use super::{EngineWaker, Local};
use crate::WallpaperResolver;
use crate::monitor::EngineMonitor;
use crate::plan::EngineControl;

/// Hidden first argument of an engine process.
pub(super) const HOST_ARG: &str = "--lumenwall-engine-host";

/// Set when `main` called [`run_engine_host`] in a daemon: its executable can host engines.
static AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Whether this executable can start engine processes (see [`run_engine_host`]).
pub(super) fn available() -> bool {
    AVAILABLE.load(Ordering::Relaxed)
}

/// Call first thing in `main`, after [`crate::restrict_vulkan_loader`]. In an engine process
/// started by a daemon, runs the engine and returns the process exit code; otherwise returns
/// `None`, and the daemon runs its engine in processes of its own on Windows (an executable
/// that never calls this keeps the engine in the daemon).
///
/// `install_logger` makes the given subscriber the process's global one (the engine process
/// sends its records to the daemon through it); `main` also routes the `log` crate's records
/// (wgpu, naga) to it, as for the daemon's own subscriber.
pub fn run_engine_host(install_logger: impl FnOnce(tracing::Dispatch)) -> Option<ExitCode> {
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new(HOST_ARG)) {
        AVAILABLE.store(true, Ordering::Relaxed);
        return None;
    }
    Some(host_main(install_logger))
}

/// Writes one message to the daemon. Never logs (the logger writes through it).
fn send(msg: &FromHost) -> bool {
    let line = msg.encode();
    let mut out = std::io::stdout().lock();
    out.write_all(line.as_bytes())
        .and_then(|()| out.flush())
        .is_ok()
}

fn host_main(install_logger: impl FnOnce(tracing::Dispatch)) -> ExitCode {
    let mut first = String::new();
    if std::io::stdin().read_line(&mut first).is_err() {
        return ExitCode::from(2);
    }
    let Ok(ToHost::Init(init)) = ToHost::decode(&first) else {
        return ExitCode::from(2);
    };
    install_logger(tracing::Dispatch::new(Forwarder::new(
        init.log_level,
        Box::new(|level, target, message| {
            send(&FromHost::Log {
                level,
                target: target.to_owned(),
                message,
            });
        }),
    )));
    // Release builds abort on panic: say why before the process ends.
    std::panic::set_hook(Box::new(|info| {
        send(&FromHost::Log {
            level: 1,
            target: "lumenwall_engine::engine::host".into(),
            message: format!("engine process panicked: {info}"),
        });
    }));

    let waker = EngineWaker::default();
    let monitor = EngineMonitor::default();
    monitor.set_sampling(init.sampling);
    {
        let m = monitor.clone();
        monitor.set_listener(Some(Arc::new(move |change| {
            send(&FromHost::Report {
                change,
                report: m.report(),
            });
        })));
    }
    let (tx, rx) = mpsc::channel::<ToHost>();
    {
        let waker = waker.clone();
        let reader = std::thread::Builder::new()
            .name("lumenwall-engine-input".into())
            .spawn(move || {
                for line in std::io::stdin().lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    match ToHost::decode(&line) {
                        Ok(msg) => {
                            if tx.send(msg).is_err() {
                                return;
                            }
                            waker.wake();
                        }
                        Err(e) => tracing::warn!("unreadable message from the daemon: {e}"),
                    }
                }
                // The daemon is gone (or closed the pipe): stop.
                let _ = tx.send(ToHost::Stop(None));
                waker.wake();
            });
        if let Err(e) = reader {
            send(&FromHost::Started(Err(format!("engine process: {e}"))));
            return ExitCode::FAILURE;
        }
    }

    tracing::debug!("engine process {} running", std::process::id());
    let mut engine = Local::new(WallpaperResolver::new(), waker, monitor.clone());
    engine.preset_user_pause(init.user_paused);
    let started = engine.load(&init.plan).and_then(|()| engine.start());
    send(&FromHost::Started(started.clone()));
    if started.is_err() {
        return ExitCode::FAILURE;
    }

    // Reloads applied by the loop, waiting for their answer to be sent.
    let mut pending: Vec<(u64, mpsc::Receiver<Result<(), String>>)> = Vec::new();
    let mut control = || -> Option<EngineControl> {
        pending.retain(|(id, done)| match done.try_recv() {
            Ok(result) => {
                send(&FromHost::Done { id: *id, result });
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => {
                send(&FromHost::Done {
                    id: *id,
                    result: Err("the engine dropped the new settings".into()),
                });
                false
            }
        });
        loop {
            match rx.try_recv().ok()? {
                ToHost::Sampling(on) => monitor.set_sampling(on),
                ToHost::Counters(id) => {
                    send(&FromHost::Counters {
                        id,
                        counters: monitor.take_counters(),
                    });
                }
                ToHost::Init(_) => {}
                ToHost::Reload { id, plan } => {
                    let (done, answer) = mpsc::sync_channel(1);
                    pending.push((id, answer));
                    return Some(EngineControl::Reload {
                        plan,
                        done: Some(done),
                    });
                }
                ToHost::Fps(fps) => return Some(EngineControl::SetFps(fps)),
                ToHost::Properties(p) => return Some(EngineControl::SetProperties(p)),
                ToHost::Paused(p) => return Some(EngineControl::SetPaused(p)),
                ToHost::Stop(sig) => return Some(EngineControl::Stop(sig)),
            }
        }
    };
    let result = engine.run(&mut control);
    drop(engine);
    match result {
        Ok(stopped) => {
            send(&FromHost::Stopped(stopped));
            ExitCode::SUCCESS
        }
        Err(e) => {
            send(&FromHost::Failed(e));
            ExitCode::FAILURE
        }
    }
}

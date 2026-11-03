//! Stop requests from the OS (spec 07 §1.2): SIGINT/SIGTERM on Unix, console control events
//! on Windows. The first one asks the engine to stop cleanly; a second one exits at once
//! (deviation: the reference has no escape hatch if the clean stop hangs).
//!
//! Windows GUI builds have no console: session end and window close reach the platform
//! layer as window messages, which it reports as [`StopReason::Driver`].

use std::io;
use std::thread::JoinHandle;

use crate::services::{Control, ControlSender, StopReason};

/// Keeps the handlers installed until dropped.
#[derive(Debug)]
pub struct SignalGuard {
    #[cfg(unix)]
    handle: signal_hook::iterator::Handle,
    thread: Option<JoinHandle<()>>,
}

/// Forwards stop requests to `control`.
#[cfg(unix)]
pub fn install(control: ControlSender) -> io::Result<SignalGuard> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    let handle = signals.handle();
    let thread = std::thread::Builder::new()
        .name("lumenwall-signals".into())
        .spawn(move || {
            // Blocks on a self-pipe: no wake-up until a signal arrives.
            for (count, signal) in signals.forever().enumerate() {
                forward(count, signal, &control);
            }
        })?;
    Ok(SignalGuard {
        handle,
        thread: Some(thread),
    })
}

fn forward(already_received: usize, code: i32, control: &ControlSender) {
    if already_received == 0 {
        tracing::info!("received signal {code}, stopping");
        control.send(Control::Stop(StopReason::Signal(code)));
    } else {
        tracing::warn!("received signal {code} again, exiting immediately");
        std::process::exit(1);
    }
}

#[cfg(unix)]
impl Drop for SignalGuard {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::core::BOOL;

    use super::*;

    static CONTROL: Mutex<Option<ControlSender>> = Mutex::new(None);
    static RECEIVED: AtomicUsize = AtomicUsize::new(0);

    /// Runs on a thread created by the system for each console event.
    extern "system" fn handler(ctrl_type: u32) -> BOOL {
        let count = RECEIVED.fetch_add(1, Ordering::SeqCst);
        if let Ok(control) = CONTROL.lock()
            && let Some(control) = control.as_ref()
        {
            forward(count, ctrl_type as i32, control);
            return 1;
        }
        0
    }

    pub(super) fn install(control: ControlSender) -> io::Result<SignalGuard> {
        *CONTROL.lock().unwrap_or_else(|e| e.into_inner()) = Some(control);
        // SAFETY: `handler` is a valid handler routine with a 'static lifetime.
        if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SignalGuard { thread: None })
    }

    pub(super) fn uninstall() {
        // SAFETY: removes the routine registered by `install`.
        unsafe { SetConsoleCtrlHandler(Some(handler), 0) };
        *CONTROL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Forwards console control events (Ctrl+C, Ctrl+Break, close, logoff, shutdown) to
/// `control`; the event code is reported as the signal number.
#[cfg(windows)]
pub fn install(control: ControlSender) -> io::Result<SignalGuard> {
    windows::install(control)
}

#[cfg(windows)]
impl Drop for SignalGuard {
    fn drop(&mut self) {
        windows::uninstall();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub fn install(_control: ControlSender) -> io::Result<SignalGuard> {
    Ok(SignalGuard { thread: None })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::services::{Waker, control_channel};
    use std::sync::Arc;
    use std::time::Duration;

    struct NoWake;
    impl Waker for NoWake {
        fn wake(&self) {}
    }

    #[test]
    fn sigterm_becomes_a_stop_request() {
        let (control, rx) = control_channel(Arc::new(NoWake));
        let guard = install(control).unwrap();
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            Control::Stop(StopReason::Signal(code)) => {
                assert_eq!(code, signal_hook::consts::SIGTERM)
            }
            other => panic!("unexpected {other:?}"),
        }
        drop(guard);
    }
}

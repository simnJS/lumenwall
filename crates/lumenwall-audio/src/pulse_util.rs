//! PulseAudio-protocol connection thread shared by the capture and automute backends.
//!
//! One dedicated thread per client runs a standard libpulse main loop. It blocks in `poll`
//! while idle (no timer wake-ups), connects asynchronously, gives up after 2 s, retries every
//! 5 s, logs the loss of the server once and its return (spec 07 §12.2). Stopping wakes the
//! loop through `pa_mainloop_wakeup`, which libpulse documents as callable from other threads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use libpulse_binding as pulse;
use pulse::context::{Context, FlagSet as ContextFlags, State as ContextState};
use pulse::mainloop::api::MainloopInnerType;
use pulse::mainloop::standard::Mainloop;
use pulse::proplist::{Proplist, properties};
use pulse::time::MicroSeconds;

/// Connection attempt timeout (spec §12.2).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Delay between connection attempts (spec §12.2).
pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(5);

/// Behaviour plugged into a [`PulseThread`]. All methods run on the Pulse thread.
pub(crate) trait PulseClient {
    /// A connection attempt starts.
    fn connecting(&mut self) {}
    /// The context became ready: subscribe, query, open streams.
    fn ready(&mut self, ctx: &mut Context);
    /// Called after every main-loop dispatch while ready: act on flags set by callbacks.
    fn dispatched(&mut self, ctx: &mut Context);
    /// The connection was lost or closed: drop streams and pending state.
    fn lost(&mut self);
}

struct RawLoop(*mut libpulse_sys::mainloop::standard::pa_mainloop);
// SAFETY: the pointer is only used for `pa_mainloop_wakeup`, which is thread-safe, and only
// while the owning thread keeps the main loop alive (guarded by the mutex in `Shared`).
unsafe impl Send for RawLoop {}

struct Shared {
    stop: AtomicBool,
    waker: Mutex<Option<RawLoop>>,
}

/// Handle to a running Pulse client thread.
pub(crate) struct PulseThread {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl PulseThread {
    /// Spawns the thread. `server` overrides the server address (`None` = libpulse default,
    /// which honours `PULSE_SERVER`).
    pub(crate) fn spawn<C, F>(
        name: &str,
        server: Option<String>,
        make_client: F,
    ) -> std::io::Result<Self>
    where
        C: PulseClient,
        F: FnOnce() -> C + Send + 'static,
    {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            waker: Mutex::new(None),
        });
        let s = shared.clone();
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let mut client = make_client();
                run(&s, server.as_deref(), &mut client);
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Stops and joins the thread.
    pub(crate) fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Ok(waker) = self.shared.waker.lock()
            && let Some(raw) = waker.as_ref()
        {
            // SAFETY: the main loop is alive while the pointer is published (see RawLoop).
            unsafe { libpulse_sys::mainloop::standard::pa_mainloop_wakeup(raw.0) };
        }
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

impl Drop for PulseThread {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One blocking main-loop iteration with an optional timeout.
fn iterate(ml: &mut Mainloop, timeout: Option<Duration>) -> bool {
    let timeout = timeout.map(|d| MicroSeconds(d.as_micros().min(u128::from(u64::MAX)) as u64));
    ml.prepare(timeout).is_ok() && ml.poll().is_ok() && ml.dispatch().is_ok()
}

fn run(shared: &Shared, server: Option<&str>, client: &mut impl PulseClient) {
    let stopped = || shared.stop.load(Ordering::SeqCst);
    let mut lost_logged = false;
    while !stopped() {
        client.connecting();
        let mut ready = false;
        if let Some(mut ml) = Mainloop::new() {
            let ctx = Proplist::new().and_then(|mut p| {
                p.set_str(properties::APPLICATION_NAME, crate::CLIENT_NAME)
                    .ok()?;
                Context::new_with_proplist(&ml, crate::CLIENT_NAME, &p)
            });
            if let Some(mut ctx) = ctx {
                if let Ok(mut w) = shared.waker.lock() {
                    *w = Some(RawLoop(ml._inner.get_ptr()));
                }
                // Asynchronous: returns at once, progress happens in the loop below.
                let connected = ctx.connect(server, ContextFlags::NOAUTOSPAWN, None).is_ok();
                let deadline = Instant::now() + CONNECT_TIMEOUT;
                while connected && !stopped() {
                    match ctx.get_state() {
                        ContextState::Ready => {
                            if !ready {
                                ready = true;
                                if lost_logged {
                                    tracing::info!("sound server connection restored");
                                    lost_logged = false;
                                }
                                client.ready(&mut ctx);
                            }
                        }
                        ContextState::Failed | ContextState::Terminated => break,
                        _ => {
                            if Instant::now() >= deadline {
                                break;
                            }
                        }
                    }
                    if ready {
                        client.dispatched(&mut ctx);
                    }
                    if stopped() {
                        break;
                    }
                    let timeout = if ready {
                        None
                    } else {
                        Some(deadline.saturating_duration_since(Instant::now()))
                    };
                    if !iterate(&mut ml, timeout) {
                        break;
                    }
                }
                client.lost();
                if let Ok(mut w) = shared.waker.lock() {
                    *w = None;
                }
                ctx.disconnect();
            }
        }
        if stopped() {
            break;
        }
        if !lost_logged {
            tracing::warn!(
                "sound server unavailable; retrying every {} s",
                RETRY_DELAY.as_secs()
            );
            lost_logged = true;
        }
        let until = Instant::now() + RETRY_DELAY;
        while !stopped() {
            let now = Instant::now();
            if now >= until {
                break;
            }
            thread::park_timeout(until - now);
        }
    }
}

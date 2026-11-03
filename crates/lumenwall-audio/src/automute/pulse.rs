//! Automute over the PulseAudio protocol (PulseAudio or pipewire-pulse), spec 07 §12.1.
//!
//! Subscribes to sink-input events; each burst of events triggers one asynchronous re-read of
//! the sink-input list on the watcher thread. Nothing runs between events.

use std::cell::RefCell;
use std::rc::Rc;

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::Context;
use pulse::context::introspect::SinkInputInfo;
use pulse::context::subscribe::{Facility, InterestMaskSet};
use pulse::proplist::properties;

use super::{ChangeCallback, PlaybackWatcher, PlayingFlag, is_own_pid};
use crate::pulse_util::{PulseClient, PulseThread};

/// Watcher thread for the PulseAudio protocol.
pub struct PulseWatcher {
    flag: PlayingFlag,
    _thread: Option<PulseThread>,
}

impl PulseWatcher {
    /// Starts the watcher. `server` overrides the server address (`None` = default).
    pub fn new(server: Option<String>, on_change: Option<ChangeCallback>) -> Self {
        let flag = PlayingFlag::new(on_change);
        let f = flag.clone();
        let thread =
            match PulseThread::spawn("lumenwall-automute", server, move || WatchClient::new(f)) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::error!("automute: cannot spawn thread: {e}");
                    None
                }
            };
        Self {
            flag,
            _thread: thread,
        }
    }
}

impl PlaybackWatcher for PulseWatcher {
    fn other_app_playing(&self) -> bool {
        self.flag.playing()
    }

    fn is_connected(&self) -> bool {
        self.flag.connected()
    }
}

/// Whether a sink input counts as "another application plays" (§12.1 rule 3).
pub(crate) fn counts(pid: Option<&str>, volume_is_muted: bool, own_pid: u32) -> bool {
    // parity: needs a PID property different from ours and a non-zero average volume; the
    // corked state and the mute flag are not examined (QO-13).
    match pid {
        Some(pid) => !is_own_pid(pid, own_pid) && !volume_is_muted,
        None => false,
    }
}

#[derive(Default)]
struct Query {
    dirty: bool,
    in_flight: bool,
    acc: bool,
    result: Option<bool>,
}

struct WatchClient {
    flag: PlayingFlag,
    own_pid: u32,
    query: Rc<RefCell<Query>>,
}

impl WatchClient {
    fn new(flag: PlayingFlag) -> Self {
        Self {
            flag,
            own_pid: std::process::id(),
            query: Rc::default(),
        }
    }
}

impl PulseClient for WatchClient {
    fn ready(&mut self, ctx: &mut Context) {
        self.flag.set_connected(true);
        let q = self.query.clone();
        ctx.set_subscribe_callback(Some(Box::new(move |facility, _op, _index| {
            if facility == Some(Facility::SinkInput) {
                q.borrow_mut().dirty = true;
            }
        })));
        let _ = ctx.subscribe(InterestMaskSet::SINK_INPUT, |_| {});
        self.query.borrow_mut().dirty = true;
    }

    fn dispatched(&mut self, ctx: &mut Context) {
        let start = {
            let mut q = self.query.borrow_mut();
            if let Some(r) = q.result.take() {
                self.flag.set_playing(r);
            }
            let start = q.dirty && !q.in_flight;
            if start {
                q.dirty = false;
                q.in_flight = true;
                q.acc = false;
            }
            start
        };
        if start {
            let q = self.query.clone();
            let own = self.own_pid;
            ctx.introspect().get_sink_input_info_list(
                move |res: ListResult<&SinkInputInfo<'_>>| {
                    let mut q = q.borrow_mut();
                    match res {
                        ListResult::Item(info) => {
                            let pid = info.proplist.get_str(properties::APPLICATION_PROCESS_ID);
                            if counts(pid.as_deref(), info.volume.avg().is_muted(), own) {
                                q.acc = true;
                            }
                        }
                        ListResult::End | ListResult::Error => {
                            q.in_flight = false;
                            q.result = Some(q.acc);
                        }
                    }
                },
            );
        }
    }

    fn lost(&mut self) {
        *self.query.borrow_mut() = Query::default();
        self.flag.set_connected(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn counting_rule() {
        assert!(counts(Some("4242"), false, 1));
        assert!(!counts(Some("1"), false, 1), "own stream");
        assert!(!counts(Some(" 1 "), false, 1), "own stream, padded");
        assert!(!counts(None, false, 1), "no PID");
        assert!(!counts(Some("4242"), true, 1), "zero volume");
        assert!(
            counts(Some("abc"), false, 1),
            "unparsable PID differs from ours"
        );
    }

    fn assert_never_blocks(server: String, watch_for: Duration) {
        let t0 = Instant::now();
        let mut d = super::super::AutomuteDetector::with_watcher(
            super::super::AutomuteSettings::default(),
            Some(Box::new(PulseWatcher::new(Some(server), None))),
        );
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "construction took {:?}",
            t0.elapsed()
        );
        let end = Instant::now() + watch_for;
        while Instant::now() < end {
            let t = Instant::now();
            assert!(!d.update(false));
            assert!(t.elapsed() < Duration::from_millis(250));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!d.is_connected());
        assert!(!d.is_playing());
        let t = Instant::now();
        drop(d);
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "shutdown took {:?}",
            t.elapsed()
        );
    }

    /// Spec §21.1 test 4: server pointed at a missing socket.
    #[test]
    fn no_server() {
        assert_never_blocks(
            "unix:/nonexistent/lumenwall-test-pulse".into(),
            Duration::from_millis(1500),
        );
    }

    /// Spec §21.1 test 5: a socket that accepts but never answers (covers the 2 s timeout).
    #[test]
    fn mute_server() {
        let dir = std::env::temp_dir().join(format!("lumenwall-audio-mute-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("native");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let accept = std::thread::spawn(move || {
            let mut held = Vec::new();
            listener.set_nonblocking(true).ok();
            let end = Instant::now() + Duration::from_secs(4);
            while Instant::now() < end {
                if let Ok((s, _)) = listener.accept() {
                    held.push(s);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        assert_never_blocks(format!("unix:{}", path.display()), Duration::from_secs(3));
        let _ = accept.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Against the session's real sound server: the watcher connects without blocking.
    #[test]
    #[ignore = "needs a running PulseAudio/pipewire-pulse server"]
    fn live_server_connects() {
        let w = PulseWatcher::new(None, None);
        let end = Instant::now() + Duration::from_secs(2);
        while !w.is_connected() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(w.is_connected());
    }
}

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

use super::{ChangeCallback, PlaybackWatcher, PlayingFlag, Stream, Verdict, classify, is_own_pid};
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

/// The properties of a sink input that [`classify`] reads.
#[derive(Debug, Default)]
pub(crate) struct SinkInput {
    pub pid: Option<String>,
    pub app_name: Option<String>,
    pub binary: Option<String>,
    pub role: Option<String>,
    pub corked: bool,
    pub muted: bool,
    /// Linear gain of the average channel volume.
    pub volume: f32,
}

impl SinkInput {
    fn from_info(info: &SinkInputInfo<'_>) -> Self {
        let p = &info.proplist;
        Self {
            pid: p.get_str(properties::APPLICATION_PROCESS_ID),
            app_name: p.get_str(properties::APPLICATION_NAME),
            binary: p.get_str(properties::APPLICATION_PROCESS_BINARY),
            role: p.get_str(properties::MEDIA_ROLE),
            // pipewire-pulse also reports a stream whose node is not running as corked.
            corked: info.corked,
            muted: info.mute,
            volume: pulse_gain(info.volume.avg().0),
        }
    }

    pub(crate) fn stream(&self, own_pid: u32) -> Stream<'_> {
        Stream {
            ours: self.pid.as_deref().is_some_and(|p| is_own_pid(p, own_pid)),
            has_owner: self.pid.is_some(),
            app_name: self.app_name.as_deref(),
            binary: self.binary.as_deref(),
            role: self.role.as_deref(),
            paused: self.corked,
            muted: self.muted,
            volume: self.volume,
        }
    }
}

/// PulseAudio software volumes are cubic: linear gain = (v / NORM)³.
fn pulse_gain(v: u32) -> f32 {
    let x = v as f32 / pulse::volume::Volume::NORMAL.0 as f32;
    x * x * x
}

#[derive(Default)]
struct Query {
    dirty: bool,
    in_flight: bool,
    /// First stream of the current listing that counts.
    acc: Option<String>,
    result: Option<Option<String>>,
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
                self.flag.set_media(r);
            }
            let start = q.dirty && !q.in_flight;
            if start {
                q.dirty = false;
                q.in_flight = true;
                q.acc = None;
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
                            let input = SinkInput::from_info(info);
                            let stream = input.stream(own);
                            let verdict = classify(&stream);
                            tracing::debug!(
                                "automute: sink input {} {}: {verdict:?} (role {:?}, corked {}, muted {}, gain {:.3})",
                                info.index,
                                super::describe(&stream),
                                input.role,
                                input.corked,
                                input.muted,
                                input.volume
                            );
                            if verdict == Verdict::Media && q.acc.is_none() {
                                q.acc = Some(super::describe(&stream));
                            }
                        }
                        ListResult::End | ListResult::Error => {
                            q.in_flight = false;
                            q.result = Some(q.acc.take());
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

    fn input(pid: Option<&str>) -> SinkInput {
        SinkInput {
            pid: pid.map(str::to_owned),
            app_name: Some("mpv".into()),
            binary: Some("mpv".into()),
            volume: 1.0,
            ..SinkInput::default()
        }
    }

    #[test]
    fn counting_rule() {
        let counts = |i: &SinkInput| classify(&i.stream(1)) == Verdict::Media;
        assert!(counts(&input(Some("4242"))));
        assert!(!counts(&input(Some("1"))), "own stream");
        assert!(!counts(&input(Some(" 1 "))), "own stream, padded");
        assert!(!counts(&input(None)), "no PID");
        assert!(
            counts(&input(Some("abc"))),
            "unparsable PID differs from ours"
        );
        let silent = SinkInput {
            volume: pulse_gain(0),
            ..input(Some("4242"))
        };
        assert!(!counts(&silent), "zero volume");
        let corked = SinkInput {
            corked: true,
            ..input(Some("4242"))
        };
        assert!(!counts(&corked), "corked");
        let muted = SinkInput {
            muted: true,
            ..input(Some("4242"))
        };
        assert!(!counts(&muted), "muted");
        let call = SinkInput {
            app_name: Some("WEBRTC VoiceEngine".into()),
            binary: Some("Discord".into()),
            ..input(Some("4242"))
        };
        assert!(!counts(&call), "Discord call");
    }

    #[test]
    fn cubic_volume() {
        assert_eq!(pulse_gain(pulse::volume::Volume::NORMAL.0), 1.0);
        assert_eq!(pulse_gain(0), 0.0);
        // 10 % on the cubic scale is −60 dB: inaudible.
        assert!(pulse_gain(pulse::volume::Volume::NORMAL.0 / 10) < 1.01e-3);
    }

    /// Lists the session's sink inputs with their verdicts (run with `--nocapture`).
    #[test]
    #[ignore = "needs a running PulseAudio/pipewire-pulse server"]
    fn live_list_streams() {
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        struct Dump(Arc<Mutex<Vec<String>>>, Rc<RefCell<bool>>);
        impl PulseClient for Dump {
            fn ready(&mut self, ctx: &mut Context) {
                let out = self.0.clone();
                let done = self.1.clone();
                ctx.introspect().get_sink_input_info_list(
                    move |res: ListResult<&SinkInputInfo<'_>>| match res {
                        ListResult::Item(info) => {
                            let i = SinkInput::from_info(info);
                            let line = format!("{:?} -> {:?}", i, classify(&i.stream(0)));
                            out.lock().unwrap().push(line);
                        }
                        _ => *done.borrow_mut() = true,
                    },
                );
            }
            fn dispatched(&mut self, _: &mut Context) {}
            fn lost(&mut self) {}
        }
        let s = seen.clone();
        let _t = PulseThread::spawn("lumenwall-automute-dump", None, move || {
            Dump(s, Rc::default())
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let lines = seen.lock().unwrap();
        for l in lines.iter() {
            println!("{l}");
        }
        assert!(!lines.is_empty(), "no sink input listed");
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

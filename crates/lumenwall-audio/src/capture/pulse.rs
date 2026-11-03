//! PulseAudio-protocol capture of the default sink's monitor (spec 07 §13.2).
//!
//! The record stream asks the server for the reference format itself (8-bit unsigned mono at
//! 44.1 kHz, 4410-byte fragments), so the server performs the same conversion as for the
//! reference and packets go straight into the window accumulator.

use std::cell::RefCell;
use std::rc::Rc;

use libpulse_binding as pulse;
use pulse::context::Context;
use pulse::context::subscribe::{Facility, InterestMaskSet};
use pulse::def::BufferAttr;
use pulse::sample::{Format, Spec};
use pulse::stream::{FlagSet as StreamFlags, PeekResult, State as StreamState, Stream};

use super::{AudioCapture, CaptureSink, CaptureState, SharedState};
use crate::pulse_util::{PulseClient, PulseThread};
use crate::spectrum::{FRAGMENT_BYTES, REFERENCE_RATE, SpectrumFeed};

/// Seconds of data the server may buffer beyond one fragment (§13.2).
const MAX_BUFFER_SECONDS: f64 = 7.5;

/// Capture backend speaking the PulseAudio protocol (PulseAudio or pipewire-pulse).
pub struct PulseCapture {
    feed: SpectrumFeed,
    server: Option<String>,
    state: SharedState,
    thread: Option<PulseThread>,
}

impl PulseCapture {
    /// Backend publishing to `feed`. `server` overrides the server address.
    pub fn new(feed: SpectrumFeed, server: Option<String>) -> Self {
        Self {
            feed,
            server,
            state: SharedState::new(),
            thread: None,
        }
    }
}

impl AudioCapture for PulseCapture {
    fn start(&mut self) {
        if self.thread.is_some() {
            return;
        }
        let feed = self.feed.clone();
        let state = self.state.clone();
        match PulseThread::spawn("lumenwall-capture", self.server.clone(), move || {
            CaptureClient::new(feed, state)
        }) {
            Ok(t) => self.thread = Some(t),
            Err(e) => {
                tracing::error!("audio capture: cannot spawn thread: {e}");
                self.state.set(CaptureState::Unavailable);
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut t) = self.thread.take() {
            t.stop();
        }
        self.state.set(CaptureState::Stopped);
    }

    fn state(&self) -> CaptureState {
        self.state.get()
    }

    fn backend_name(&self) -> &'static str {
        "pulseaudio"
    }
}

impl Drop for PulseCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct Flags {
    /// A sink/source/server event arrived: the default sink may have changed.
    server_dirty: bool,
    /// A server-info query is in flight.
    query_in_flight: bool,
    /// Latest `<default sink>.monitor` name.
    default_monitor: Option<String>,
    /// The default monitor was (re)read.
    target_changed: bool,
}

struct CaptureClient {
    sink: CaptureSink,
    state: SharedState,
    flags: Rc<RefCell<Flags>>,
    stream: Option<Stream>,
    target: Option<String>,
}

impl CaptureClient {
    fn new(feed: SpectrumFeed, state: SharedState) -> Self {
        Self {
            sink: CaptureSink::new(feed),
            state,
            flags: Rc::default(),
            stream: None,
            target: None,
        }
    }

    fn close_stream(&mut self) {
        if let Some(mut s) = self.stream.take() {
            let _ = s.disconnect();
        }
        self.target = None;
        self.sink.reset();
    }

    fn open_stream(&mut self, ctx: &mut Context, monitor: &str) {
        self.close_stream();
        // parity: 8-bit unsigned mono 44.1 kHz, fragments of 4410 bytes, max buffer
        // fragment + 7.5 s (§13.2).
        let spec = Spec {
            format: Format::U8,
            channels: 1,
            rate: REFERENCE_RATE,
        };
        let attr = BufferAttr {
            maxlength: FRAGMENT_BYTES as u32
                + (f64::from(REFERENCE_RATE) * MAX_BUFFER_SECONDS) as u32,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: FRAGMENT_BYTES as u32,
        };
        let Some(mut stream) = Stream::new(ctx, "lumenwall audio capture", &spec, None) else {
            tracing::warn!("audio capture: cannot create record stream");
            return;
        };
        if let Err(e) = stream.connect_record(Some(monitor), Some(&attr), StreamFlags::NOFLAGS) {
            tracing::warn!("audio capture: cannot record {monitor}: {e}");
            return;
        }
        tracing::debug!("audio capture: recording {monitor}");
        self.stream = Some(stream);
        self.target = Some(monitor.to_owned());
    }

    /// `true` when the current stream already records (or is connecting to) `monitor`.
    fn records(&self, monitor: &str) -> bool {
        let Some(stream) = &self.stream else {
            return false;
        };
        match stream.get_state() {
            StreamState::Creating => self.target.as_deref() == Some(monitor),
            StreamState::Ready => stream.get_device_name().as_deref() == Some(monitor),
            _ => false,
        }
    }

    fn drain(&mut self) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        loop {
            match stream.peek() {
                Ok(PeekResult::Data(data)) => {
                    self.sink.push_reference_packet(data);
                    let _ = stream.discard();
                }
                // parity: holes reported by the server are ignored (§13.3).
                Ok(PeekResult::Hole(_)) => {
                    let _ = stream.discard();
                }
                Ok(PeekResult::Empty) | Err(_) => break,
            }
        }
    }
}

impl PulseClient for CaptureClient {
    fn connecting(&mut self) {
        if self.state.get() != CaptureState::Unavailable {
            self.state.set(CaptureState::Connecting);
        }
    }

    fn ready(&mut self, ctx: &mut Context) {
        let flags = self.flags.clone();
        ctx.set_subscribe_callback(Some(Box::new(move |facility, _op, _index| {
            if matches!(
                facility,
                Some(Facility::Sink | Facility::Source | Facility::Server)
            ) {
                flags.borrow_mut().server_dirty = true;
            }
        })));
        // Follow default-output changes (§13.2).
        let _ = ctx.subscribe(
            InterestMaskSet::SINK | InterestMaskSet::SOURCE | InterestMaskSet::SERVER,
            |_| {},
        );
        self.flags.borrow_mut().server_dirty = true;
    }

    fn dispatched(&mut self, ctx: &mut Context) {
        if let Some(stream) = &self.stream {
            match stream.get_state() {
                StreamState::Ready => self.state.set(CaptureState::Running),
                StreamState::Failed | StreamState::Terminated => {
                    // The monitor went away: re-read the default sink.
                    self.close_stream();
                    self.flags.borrow_mut().server_dirty = true;
                }
                _ => {}
            }
        }

        let query = {
            let mut f = self.flags.borrow_mut();
            let q = f.server_dirty && !f.query_in_flight;
            if q {
                f.server_dirty = false;
                f.query_in_flight = true;
            }
            q
        };
        if query {
            let flags = self.flags.clone();
            ctx.introspect().get_server_info(move |info| {
                let mut f = flags.borrow_mut();
                f.query_in_flight = false;
                f.default_monitor = info
                    .default_sink_name
                    .as_ref()
                    .map(|s| format!("{s}.monitor"));
                f.target_changed = true;
            });
        }

        let target = {
            let mut f = self.flags.borrow_mut();
            if std::mem::take(&mut f.target_changed) {
                Some(f.default_monitor.clone())
            } else {
                None
            }
        };
        if let Some(target) = target {
            match target {
                Some(monitor) if !self.records(&monitor) => self.open_stream(ctx, &monitor),
                Some(_) => {}
                None => self.close_stream(),
            }
        }
        self.drain();
    }

    fn lost(&mut self) {
        self.close_stream();
        *self.flags.borrow_mut() = Flags::default();
        self.state.set(CaptureState::Unavailable);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Without a server, starting never blocks, the state settles on Unavailable and stopping
    /// is immediate (§13.1: degrade like the automute detector).
    #[test]
    fn no_server_does_not_block() {
        let mut c = PulseCapture::new(
            SpectrumFeed::new(),
            Some("unix:/nonexistent/lumenwall-test-pulse".into()),
        );
        let t = Instant::now();
        c.start();
        assert!(t.elapsed() < Duration::from_millis(100));
        let deadline = Instant::now() + Duration::from_secs(1);
        while c.state() != CaptureState::Unavailable && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(c.state(), CaptureState::Unavailable);
        let t = Instant::now();
        c.stop();
        assert!(
            t.elapsed() < Duration::from_millis(200),
            "stop took {:?}",
            t.elapsed()
        );
        assert_eq!(c.state(), CaptureState::Stopped);
    }

    /// Against the session's real sound server: the monitor stream opens and windows arrive at
    /// the reference cadence. Window contents are not inspected.
    #[test]
    #[ignore = "needs a running PulseAudio/pipewire-pulse server"]
    fn live_server_delivers_windows() {
        let feed = SpectrumFeed::new();
        let mut c = PulseCapture::new(feed.clone(), None);
        c.start();
        let mut out = [0u8; crate::spectrum::WINDOW_SIZE];
        let mut windows = 0;
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if feed.take(&mut out) {
                windows += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(c.state(), CaptureState::Running);
        c.stop();
        // ~10 windows per second once the stream runs.
        assert!((10..=25).contains(&windows), "{windows} windows in 2 s");
    }
}

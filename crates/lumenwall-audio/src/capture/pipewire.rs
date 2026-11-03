//! PipeWire native capture of the default sink's monitor (spec 07 §13.2).
//!
//! The stream sets `stream.capture.sink = true` without a target, so the session manager links
//! it to the monitor of the default sink and moves it when the default output changes. It
//! requests the reference format (U8 mono 44.1 kHz) so that PipeWire's converter does the same
//! work as for the reference running on pipewire-pulse; the data then goes through the
//! reference fragmenter. `node.latency` asks for ~100 ms quanta: as few wake-ups as possible.
//!
//! Not compiled on the development machine (no libpipewire headers); see README.

use std::cell::Cell;
use std::io::Cursor;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::param::ParamType;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::serialize::PodSerializer;
use spa::pod::{Object, Pod, Value};
use spa::utils::{Direction, SpaTypes};

use super::{AudioCapture, CaptureSink, CaptureState, SharedState};
use crate::spectrum::{REFERENCE_RATE, SpectrumFeed};

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_DELAY: Duration = Duration::from_secs(5);

/// `true` when a PipeWire daemon socket exists (cheap, non-blocking check).
pub fn server_available() -> bool {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let remote = std::env::var_os("PIPEWIRE_REMOTE")
        .map(PathBuf::from)
        .unwrap_or_else(|| "pipewire-0".into());
    if remote.is_absolute() {
        return remote.exists();
    }
    runtime.is_some_and(|dir| dir.join(remote).exists())
}

struct Terminate;

/// Runs `body` on a fresh PipeWire thread with a main loop that quits on [`Terminate`].
pub(crate) struct PwThread {
    sender: pw::channel::Sender<Terminate>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PwThread {
    pub(crate) fn spawn<F>(name: &str, body: F) -> std::io::Result<Self>
    where
        F: FnOnce(&pw::main_loop::MainLoopRc, &AtomicBool) + Send + 'static,
    {
        let (sender, receiver) = pw::channel::channel::<Terminate>();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                pw::init();
                let Ok(mainloop) = pw::main_loop::MainLoopRc::new(None) else {
                    tracing::warn!("pipewire: cannot create a main loop");
                    return;
                };
                let _terminate = receiver.attach(mainloop.loop_(), {
                    let ml = mainloop.clone();
                    move |_| ml.quit()
                });
                body(&mainloop, &s);
            })?;
        Ok(Self {
            sender,
            stop,
            thread: Some(thread),
        })
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.sender.send(Terminate);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for PwThread {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs `session` until it ends, then waits [`RETRY_DELAY`] (interruptible) and retries.
pub(crate) fn retry_loop(
    mainloop: &pw::main_loop::MainLoopRc,
    stop: &AtomicBool,
    mut session: impl FnMut(&pw::main_loop::MainLoopRc) -> bool,
    mut on_lost: impl FnMut(),
) {
    let mut warned = false;
    while !stop.load(Ordering::SeqCst) {
        if session(mainloop) {
            warned = false;
        }
        on_lost();
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if !warned {
            tracing::warn!(
                "pipewire unavailable; retrying every {} s",
                RETRY_DELAY.as_secs()
            );
            warned = true;
        }
        let timer = mainloop.loop_().add_timer({
            let ml = mainloop.clone();
            move |_| ml.quit()
        });
        let _ = timer.update_timer(Some(RETRY_DELAY), None);
        mainloop.run();
    }
}

/// Connects a core and arms the connection timeout: the loop quits if the daemon does not
/// answer a sync within [`CONNECT_TIMEOUT`] or reports a fatal core error.
pub(crate) fn connect(
    mainloop: &pw::main_loop::MainLoopRc,
) -> Option<(
    pw::context::ContextRc,
    pw::core::CoreRc,
    pw::core::Listener,
    Rc<Cell<bool>>,
)> {
    let context = pw::context::ContextRc::new(mainloop, None).ok()?;
    let core = context.connect_rc(None).ok()?;
    let alive = Rc::new(Cell::new(false));
    let pending = core.sync(0).ok()?;
    let listener = core
        .add_listener_local()
        .done({
            let alive = alive.clone();
            move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    alive.set(true);
                }
            }
        })
        .error({
            let ml = mainloop.clone();
            move |id, _seq, _res, message| {
                if id == pw::core::PW_ID_CORE {
                    tracing::warn!("pipewire: {message}");
                    ml.quit();
                }
            }
        })
        .register();
    Some((context, core, listener, alive))
}

/// PipeWire capture backend.
pub struct PipeWireCapture {
    feed: SpectrumFeed,
    state: SharedState,
    thread: Option<PwThread>,
}

impl PipeWireCapture {
    /// Backend publishing to `feed`.
    pub fn new(feed: SpectrumFeed) -> Self {
        Self {
            feed,
            state: SharedState::new(),
            thread: None,
        }
    }
}

impl AudioCapture for PipeWireCapture {
    fn start(&mut self) {
        if self.thread.is_some() {
            return;
        }
        let feed = self.feed.clone();
        let state = self.state.clone();
        self.state.set(CaptureState::Connecting);
        let body = move |ml: &pw::main_loop::MainLoopRc, stop: &AtomicBool| {
            let lost_state = state.clone();
            retry_loop(
                ml,
                stop,
                |ml| session(ml, &feed, &state),
                || lost_state.set(CaptureState::Unavailable),
            );
        };
        match PwThread::spawn("lumenwall-capture", body) {
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
        "pipewire"
    }
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

struct StreamData {
    sink: CaptureSink,
    state: SharedState,
    format: AudioInfoRaw,
    ml: pw::main_loop::MainLoopRc,
    scratch: Vec<f32>,
}

fn reference_format_pod() -> Option<Vec<u8>> {
    let mut info = AudioInfoRaw::new();
    // parity: request the reference capture format; PipeWire converts (§13.2).
    info.set_format(AudioFormat::U8);
    info.set_rate(REFERENCE_RATE);
    info.set_channels(1);
    let mut position = [0u32; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(position);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj))
        .ok()
        .map(|(c, _)| c.into_inner())
}

/// One connection: returns `true` if the stream ran.
fn session(mainloop: &pw::main_loop::MainLoopRc, feed: &SpectrumFeed, state: &SharedState) -> bool {
    let Some((_context, core, _core_listener, alive)) = connect(mainloop) else {
        return false;
    };
    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::STREAM_CAPTURE_SINK => "true",
        *pw::keys::NODE_LATENCY => "4410/44100",
        *pw::keys::APP_NAME => crate::CLIENT_NAME,
    };
    let Ok(stream) = pw::stream::StreamBox::new(&core, "lumenwall audio capture", props) else {
        return false;
    };
    let data = StreamData {
        sink: CaptureSink::new(feed.clone()),
        state: state.clone(),
        format: AudioInfoRaw::new(),
        ml: mainloop.clone(),
        scratch: vec![0.0; 1024],
    };
    let ran = Rc::new(Cell::new(false));
    let listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed({
            let ran = ran.clone();
            move |_, d, _old, new| match new {
                pw::stream::StreamState::Streaming => {
                    ran.set(true);
                    d.state.set(CaptureState::Running);
                }
                pw::stream::StreamState::Error(e) => {
                    tracing::warn!("audio capture: pipewire stream error: {e}");
                    d.ml.quit();
                }
                _ => {}
            }
        })
        .param_changed(|_, d, id, param| {
            let Some(param) = param else { return };
            if id != ParamType::Format.as_raw() {
                return;
            }
            if let Ok((media_type, media_subtype)) = format_utils::parse_format(param)
                && media_type == MediaType::Audio
                && media_subtype == MediaSubtype::Raw
            {
                let _ = d.format.parse(param);
                d.sink.reset();
            }
        })
        .process(|stream, d| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let offset = data.chunk().offset() as usize;
            let size = data.chunk().size() as usize;
            let Some(bytes) = data.data() else { return };
            let end = offset.saturating_add(size).min(bytes.len());
            let bytes = &bytes[offset.min(end)..end];
            let rate = d.format.rate();
            let channels = d.format.channels() as usize;
            let format = d.format.format();
            if format == AudioFormat::U8 && rate == REFERENCE_RATE && channels == 1 {
                d.sink.push_reference_stream(bytes);
            } else if format == AudioFormat::U8 {
                d.sink.push_native(bytes, rate, channels);
            } else if format == AudioFormat::F32LE {
                for block in bytes.chunks(d.scratch.len() * 4) {
                    let n = block.len() / 4;
                    for (dst, src) in d.scratch.iter_mut().zip(block.chunks_exact(4)) {
                        *dst = f32::from_le_bytes([src[0], src[1], src[2], src[3]]);
                    }
                    d.sink.push_native(&d.scratch[..n], rate, channels);
                }
            } else if format == AudioFormat::S16LE {
                for block in bytes.chunks(d.scratch.len() * 2) {
                    let n = block.len() / 2;
                    for (dst, src) in d.scratch.iter_mut().zip(block.chunks_exact(2)) {
                        *dst = f32::from(i16::from_le_bytes([src[0], src[1]])) / 32768.0;
                    }
                    d.sink.push_native(&d.scratch[..n], rate, channels);
                }
            }
        })
        .register();
    let Ok(_listener) = listener else {
        return false;
    };

    let Some(pod_bytes) = reference_format_pod() else {
        return false;
    };
    let Some(pod) = Pod::from_bytes(&pod_bytes) else {
        return false;
    };
    let mut params = [pod];
    if stream
        .connect(
            Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .is_err()
    {
        return false;
    }

    // Give up if the daemon does not answer in time (§12.2 robustness).
    let timeout = mainloop.loop_().add_timer({
        let ml = mainloop.clone();
        let alive = alive.clone();
        move |_| {
            if !alive.get() {
                ml.quit();
            }
        }
    });
    let _ = timeout.update_timer(Some(CONNECT_TIMEOUT), None);
    mainloop.run();
    let _ = stream.disconnect();
    alive.get() && ran.get()
}

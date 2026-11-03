//! Daemon side: accept clients, dispatch requests to a [`Handler`], push events.
//!
//! Threads, all blocked in the kernel while idle (no polling, no timer):
//! - one accept thread;
//! - per connected client, one reader thread (runs the handler) and one writer thread.
//!
//! [`Publisher::publish`] serializes an event once and hands the bytes to the writer
//! threads of the subscribed clients without blocking the caller (typically the
//! render or event loop). With no subscriber for a topic it returns after one atomic load.

use std::io::{self, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use interprocess::local_socket::{ListenerNonblockingMode, ListenerOptions, Stream, prelude::*};

use crate::proto::*;
use crate::transport::{Endpoint, encode_frame, read_frame, write_frame};
use crate::{Error, PROTOCOL_VERSION};

/// Per-client queue length. A client that falls this far behind loses events
/// (replies always get through: the reader waits for room).
const CLIENT_QUEUE: usize = 64;

/// What the daemon implements. Every method has a default that answers
/// [`ErrorCode::Unsupported`], so the daemon can grow its support one request at a time.
///
/// Methods run on the client's reader thread: keep them short (forward the request
/// to the daemon's own loop and wait for its answer, or read a snapshot).
#[allow(unused_variables)]
pub trait Handler: Send + Sync + 'static {
    fn outputs(&self) -> Result<Vec<Output>, RemoteError> {
        Err(RemoteError::unsupported("list_outputs"))
    }
    fn state(&self) -> Result<State, RemoteError> {
        Err(RemoteError::unsupported("get_state"))
    }
    fn apply(
        &self,
        output: Option<&str>,
        wallpaper: &Path,
        scaling: Scaling,
    ) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("apply"))
    }
    fn clear(&self, output: Option<&str>) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("clear"))
    }
    fn properties(&self, output: &str) -> Result<PropertyMap, RemoteError> {
        Err(RemoteError::unsupported("get_properties"))
    }
    fn set_properties(
        &self,
        output: &str,
        values: PropertyMap,
        persist: bool,
    ) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("set_properties"))
    }
    fn reset_properties(&self, output: &str) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("reset_properties"))
    }
    fn set_paused(&self, paused: bool) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("set_paused"))
    }
    fn set_muted(&self, muted: bool) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("set_muted"))
    }
    fn set_volume(&self, volume: f32) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("set_volume"))
    }
    /// `volume` is finite and within 0.0..=1.0 when present.
    fn set_wallpaper_audio(
        &self,
        wallpaper: &Path,
        volume: Option<f32>,
        muted: Option<bool>,
    ) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("set_wallpaper_audio"))
    }
    fn settings(&self) -> Result<Settings, RemoteError> {
        Err(RemoteError::unsupported("get_settings"))
    }
    fn set_settings(&self, settings: Settings) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("set_settings"))
    }
    fn stats(&self) -> Result<Stats, RemoteError> {
        Err(RemoteError::unsupported("get_stats"))
    }
    fn shutdown(&self) -> Result<(), RemoteError> {
        Err(RemoteError::unsupported("shutdown"))
    }
    /// The number of clients subscribed to `topic` changed. Lets the daemon start
    /// sampling [`Stats`] only while someone looks at them, and stop afterwards.
    fn subscribers_changed(&self, topic: Topic, count: usize) {}
}

/// A running IPC server. Dropping it stops accepting connections, tells connected
/// clients the daemon is shutting down and removes the socket file.
pub struct Server {
    shared: Arc<Shared>,
    endpoint: Endpoint,
    accept_thread: Option<JoinHandle<()>>,
}

/// Cheap handle to push events; clone it into whichever thread detects changes.
#[derive(Clone)]
pub struct Publisher {
    shared: Arc<Shared>,
}

struct Shared {
    handler: Arc<dyn Handler>,
    info: ServerInfo,
    clients: Mutex<Vec<ClientSlot>>,
    /// Subscriber count per topic, readable without the lock.
    counts: [AtomicUsize; Topic::ALL.len()],
    next_client: AtomicU64,
    stopping: AtomicBool,
}

struct ClientSlot {
    id: u64,
    topics: Arc<AtomicU8>,
    tx: SyncSender<Arc<[u8]>>,
}

impl Server {
    /// Starts listening on `endpoint`.
    ///
    /// Fails with [`Error::AlreadyRunning`] when another daemon answers there. A stale
    /// socket file left by a crashed daemon is replaced.
    pub fn bind(
        endpoint: &Endpoint,
        handler: Arc<dyn Handler>,
        server_name: &str,
        server_version: &str,
    ) -> Result<Self, Error> {
        endpoint.prepare()?;
        if let Endpoint::Path(path) = endpoint
            && path.exists()
        {
            if Stream::connect(endpoint.name()?).is_ok() {
                return Err(Error::AlreadyRunning);
            }
            // Nobody listens: a leftover from a crash.
            let _ = std::fs::remove_file(path);
        }
        let listener = ListenerOptions::new()
            .name(endpoint.name()?)
            .nonblocking(ListenerNonblockingMode::Neither)
            .create_sync()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::AddrInUse {
                    Error::AlreadyRunning
                } else {
                    Error::Io(e)
                }
            })?;
        let shared = Arc::new(Shared {
            handler,
            info: ServerInfo {
                protocol: PROTOCOL_VERSION,
                server: server_name.to_owned(),
                version: server_version.to_owned(),
            },
            clients: Mutex::new(Vec::new()),
            counts: Default::default(),
            next_client: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
        });
        let accept_shared = Arc::clone(&shared);
        let accept_thread = std::thread::Builder::new()
            .name("lumenwall-ipc-accept".into())
            .spawn(move || accept_loop(listener, accept_shared))?;
        tracing::info!(%endpoint, "IPC server listening");
        Ok(Self {
            shared,
            endpoint: endpoint.clone(),
            accept_thread: Some(accept_thread),
        })
    }

    pub fn publisher(&self) -> Publisher {
        Publisher {
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Number of connected clients.
    pub fn client_count(&self) -> usize {
        lock(&self.shared.clients).len()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        // Tell clients, then close their queues: writer threads flush and exit.
        self.publisher().publish(&Event::ShuttingDown);
        lock(&self.shared.clients).clear();
        // Wake the accept thread, blocked in accept(), with a throwaway connection.
        if let Ok(name) = self.endpoint.name() {
            let _ = Stream::connect(name);
        }
        if let Some(thread) = self.accept_thread.take() {
            let _ = thread.join();
        }
        // The listener removed its socket file when the accept thread dropped it.
    }
}

impl Publisher {
    /// True when at least one client listens to `topic`. Check it before building
    /// an expensive event (e.g. sampling [`Stats`]).
    pub fn has_subscribers(&self, topic: Topic) -> bool {
        self.shared.counts[topic as usize].load(Ordering::Relaxed) > 0
    }

    /// Sends `event` to every client subscribed to its topic. Never blocks: a client
    /// whose queue is full misses the event (it gets the next full state anyway).
    pub fn publish(&self, event: &Event) {
        let topic = event.topic();
        if let Some(topic) = topic
            && !self.has_subscribers(topic)
        {
            return;
        }
        let line: Arc<[u8]> = encode_frame(&ServerFrame::Event(event.clone())).into();
        let clients = lock(&self.shared.clients);
        for client in clients.iter() {
            let wanted = topic.is_none_or(|t| client.topics.load(Ordering::Relaxed) & t.bit() != 0);
            if wanted {
                match client.tx.try_send(Arc::clone(&line)) {
                    Ok(()) | Err(TrySendError::Disconnected(_)) => {}
                    Err(TrySendError::Full(_)) => {
                        tracing::debug!(client = client.id, "IPC client lagging, event dropped");
                    }
                }
            }
        }
    }
}

fn accept_loop(listener: interprocess::local_socket::Listener, shared: Arc<Shared>) {
    for conn in listener.incoming() {
        if shared.stopping.load(Ordering::SeqCst) {
            break;
        }
        match conn {
            Ok(stream) => {
                if let Err(e) = spawn_client(stream, &shared) {
                    tracing::warn!("IPC: cannot start client threads: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("IPC accept failed: {e}");
                if shared.stopping.load(Ordering::SeqCst) {
                    break;
                }
            }
        }
    }
}

fn spawn_client(stream: Stream, shared: &Arc<Shared>) -> io::Result<()> {
    let id = shared.next_client.fetch_add(1, Ordering::Relaxed);
    // `&Stream` implements Read and Write: the two threads share one stream.
    let stream = Arc::new(stream);
    let send = Arc::clone(&stream);
    let (tx, rx) = mpsc::sync_channel::<Arc<[u8]>>(CLIENT_QUEUE);
    let topics = Arc::new(AtomicU8::new(0));
    lock(&shared.clients).push(ClientSlot {
        id,
        topics: Arc::clone(&topics),
        tx: tx.clone(),
    });

    std::thread::Builder::new()
        .name(format!("lumenwall-ipc-w{id}"))
        .spawn(move || {
            for line in rx {
                if write_frame(&mut &*send, &line).is_err() {
                    break;
                }
            }
        })?;

    let shared = Arc::clone(shared);
    std::thread::Builder::new()
        .name(format!("lumenwall-ipc-r{id}"))
        .spawn(move || {
            let mut session = Session {
                id,
                shared,
                topics,
                tx,
            };
            session.run(BufReader::new(&*stream));
            session.leave();
        })?;
    Ok(())
}

struct Session {
    id: u64,
    shared: Arc<Shared>,
    topics: Arc<AtomicU8>,
    tx: SyncSender<Arc<[u8]>>,
}

impl Session {
    fn run(&mut self, mut reader: impl io::BufRead) {
        let mut buf = Vec::with_capacity(256);
        loop {
            match read_frame(&mut reader, &mut buf) {
                Ok(true) => {}
                Ok(false) => return,
                Err(e) => {
                    tracing::debug!(client = self.id, "IPC client dropped: {e}");
                    return;
                }
            }
            if buf.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let (id, outcome) = match serde_json::from_slice::<ClientFrame>(&buf) {
                Ok(frame) => {
                    if frame.request == Request::Bye {
                        return;
                    }
                    (frame.id, self.dispatch(frame.request))
                }
                Err(e) => (
                    salvage_id(&buf),
                    Outcome::Error(RemoteError::new(ErrorCode::InvalidRequest, e.to_string())),
                ),
            };
            let line: Arc<[u8]> = encode_frame(&ServerFrame::Reply(Reply { id, outcome })).into();
            if self.tx.send(line).is_err() {
                return;
            }
        }
    }

    fn dispatch(&mut self, request: Request) -> Outcome {
        let h = &*self.shared.handler;
        let done = |r: Result<(), RemoteError>| r.map(|()| Response::Done);
        let result = match request {
            Request::Hello { protocol, client } => {
                if protocol != PROTOCOL_VERSION {
                    Err(RemoteError::new(
                        ErrorCode::VersionMismatch,
                        format!(
                            "client {client:?} speaks protocol {protocol}, daemon speaks {PROTOCOL_VERSION}"
                        ),
                    ))
                } else {
                    tracing::debug!(client = self.id, name = %client, "IPC hello");
                    Ok(Response::Welcome(self.shared.info.clone()))
                }
            }
            Request::Bye => Ok(Response::Done),
            Request::ListOutputs => h.outputs().map(|outputs| Response::Outputs { outputs }),
            Request::GetState => h.state().map(Response::State),
            Request::Apply {
                output,
                wallpaper,
                scaling,
            } => done(h.apply(output.as_deref(), &wallpaper, scaling)),
            Request::Clear { output } => done(h.clear(output.as_deref())),
            Request::GetProperties { output } => h
                .properties(&output)
                .map(|values| Response::Properties { output, values }),
            Request::SetProperties {
                output,
                values,
                persist,
            } => done(h.set_properties(&output, values, persist)),
            Request::ResetProperties { output } => done(h.reset_properties(&output)),
            Request::SetPaused { paused } => done(h.set_paused(paused)),
            Request::SetMuted { muted } => done(h.set_muted(muted)),
            Request::SetVolume { volume } => {
                if volume.is_finite() {
                    done(h.set_volume(volume.clamp(0.0, 1.0)))
                } else {
                    Err(RemoteError::new(
                        ErrorCode::InvalidRequest,
                        "volume must be finite",
                    ))
                }
            }
            Request::SetWallpaperAudio {
                wallpaper,
                volume,
                muted,
            } => match volume {
                Some(v) if !v.is_finite() => Err(RemoteError::new(
                    ErrorCode::InvalidRequest,
                    "volume must be finite",
                )),
                _ => done(h.set_wallpaper_audio(
                    &wallpaper,
                    volume.map(|v| v.clamp(0.0, 1.0)),
                    muted,
                )),
            },
            Request::GetSettings => h.settings().map(Response::Settings),
            Request::SetSettings { settings } => done(h.set_settings(settings)),
            Request::GetStats => h.stats().map(Response::Stats),
            Request::Subscribe { topics } => {
                self.update_topics(&topics, true);
                Ok(Response::Done)
            }
            Request::Unsubscribe { topics } => {
                self.update_topics(&topics, false);
                Ok(Response::Done)
            }
            Request::Shutdown => done(h.shutdown()),
        };
        match result {
            Ok(r) => Outcome::Ok(r),
            Err(e) => Outcome::Error(e),
        }
    }

    fn update_topics(&self, topics: &[Topic], subscribe: bool) {
        for &topic in topics {
            let bit = topic.bit();
            let old = if subscribe {
                self.topics.fetch_or(bit, Ordering::SeqCst)
            } else {
                self.topics.fetch_and(!bit, Ordering::SeqCst)
            };
            let was = old & bit != 0;
            if was != subscribe {
                self.change_count(topic, subscribe);
            }
        }
    }

    fn change_count(&self, topic: Topic, up: bool) {
        let counter = &self.shared.counts[topic as usize];
        let count = if up {
            counter.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            counter.fetch_sub(1, Ordering::SeqCst) - 1
        };
        self.shared.handler.subscribers_changed(topic, count);
    }

    fn leave(&self) {
        let topics = self.topics.swap(0, Ordering::SeqCst);
        for topic in Topic::ALL {
            if topics & topic.bit() != 0 {
                self.change_count(topic, false);
            }
        }
        lock(&self.shared.clients).retain(|c| c.id != self.id);
        tracing::debug!(client = self.id, "IPC client left");
    }
}

/// Best effort: the `id` of a frame that failed to parse, so the error reaches the
/// right caller. 0 when even that is unreadable.
fn salvage_id(buf: &[u8]) -> u64 {
    #[derive(serde::Deserialize)]
    struct IdOnly {
        id: u64,
    }
    serde_json::from_slice::<IdOnly>(buf)
        .map(|f| f.id)
        .unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

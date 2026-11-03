//! Client side: blocking calls with a timeout, events delivered to a callback.
//!
//! One background thread reads the connection; it sleeps in `read()` between frames,
//! so an idle client costs nothing.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use interprocess::ConnectWaitMode;
use interprocess::local_socket::{ConnectOptions, Stream};

use crate::proto::*;
use crate::transport::{Endpoint, encode_frame, read_frame, write_frame};
use crate::{Error, PROTOCOL_VERSION};

/// Default time to wait for a reply.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// What the event callback receives.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    Event(Event),
    /// The connection closed (daemon exited or crashed). Reconnect with a new [`Client`].
    Disconnected,
}

/// A connection to the daemon. Calls may come from several threads at once.
pub struct Client {
    shared: Arc<Shared>,
    info: ServerInfo,
    timeout: Duration,
}

struct Shared {
    stream: Arc<Stream>,
    /// Serializes writers; the reader thread reads through its own `&Stream`.
    write_lock: Mutex<()>,
    pending: Mutex<HashMap<u64, SyncSender<Outcome>>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl Client {
    /// Connects and performs the version handshake. Events are discarded.
    pub fn connect(endpoint: &Endpoint, client_name: &str) -> Result<Self, Error> {
        Self::connect_with_events(endpoint, client_name, |_| {})
    }

    /// Connects, performs the version handshake, and calls `on_event` from the
    /// reader thread for every event (after [`Client::subscribe`]) and once with
    /// [`ClientEvent::Disconnected`] when the connection ends.
    pub fn connect_with_events(
        endpoint: &Endpoint,
        client_name: &str,
        mut on_event: impl FnMut(ClientEvent) + Send + 'static,
    ) -> Result<Self, Error> {
        let name = endpoint.name()?;
        let stream = ConnectOptions::new()
            .name(name)
            .wait_mode(ConnectWaitMode::Timeout(Duration::from_secs(2)))
            .connect_sync()
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => Error::NotRunning,
                _ => Error::Io(e),
            })?;
        let stream = Arc::new(stream);
        let recv = Arc::clone(&stream);
        let shared = Arc::new(Shared {
            stream,
            write_lock: Mutex::new(()),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });

        let reader_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("lumenwall-ipc-client".into())
            .spawn(move || {
                let mut reader = BufReader::new(&*recv);
                let mut buf = Vec::with_capacity(512);
                while let Ok(true) = read_frame(&mut reader, &mut buf) {
                    match serde_json::from_slice::<ServerFrame>(&buf) {
                        Ok(ServerFrame::Reply(reply)) => {
                            let waiter = lock(&reader_shared.pending).remove(&reply.id);
                            if let Some(tx) = waiter {
                                let _ = tx.send(reply.outcome);
                            }
                        }
                        Ok(ServerFrame::Event(event)) => {
                            let last = event == Event::ShuttingDown;
                            on_event(ClientEvent::Event(event));
                            if last {
                                // Hang up now rather than when the daemon process is gone.
                                break;
                            }
                        }
                        // A newer daemon may send event types this build does not know.
                        Err(e) => tracing::debug!("IPC: ignored unreadable frame: {e}"),
                    }
                }
                reader_shared.closed.store(true, Ordering::SeqCst);
                // Wake every waiting caller: their sender is dropped.
                lock(&reader_shared.pending).clear();
                on_event(ClientEvent::Disconnected);
            })?;

        let mut client = Client {
            shared,
            info: ServerInfo {
                protocol: 0,
                server: String::new(),
                version: String::new(),
            },
            timeout: DEFAULT_TIMEOUT,
        };
        let hello = Request::Hello {
            protocol: PROTOCOL_VERSION,
            client: client_name.to_owned(),
        };
        match client.call(hello) {
            Ok(Response::Welcome(info)) => client.info = info,
            Ok(other) => return Err(unexpected(&other)),
            Err(Error::Remote(e)) if e.code == ErrorCode::VersionMismatch => {
                return Err(Error::VersionMismatch(e.message));
            }
            Err(e) => return Err(e),
        }
        Ok(client)
    }

    /// The daemon's name, version and protocol version.
    pub fn server_info(&self) -> &ServerInfo {
        &self.info
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// False once the daemon closed the connection.
    pub fn is_connected(&self) -> bool {
        !self.shared.closed.load(Ordering::SeqCst)
    }

    /// Sends one request and waits for its reply.
    pub fn call(&self, request: Request) -> Result<Response, Error> {
        if !self.is_connected() {
            return Err(Error::Disconnected);
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(1);
        lock(&self.shared.pending).insert(id, tx);
        let line = encode_frame(&ClientFrame { id, request });
        if let Err(e) = self.shared.write(&line) {
            lock(&self.shared.pending).remove(&id);
            return Err(if is_disconnect(&e) {
                Error::Disconnected
            } else {
                Error::Io(e)
            });
        }
        match rx.recv_timeout(self.timeout) {
            Ok(Outcome::Ok(response)) => Ok(response),
            Ok(Outcome::Error(e)) => Err(Error::Remote(e)),
            Err(RecvTimeoutError::Timeout) => {
                lock(&self.shared.pending).remove(&id);
                Err(Error::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => Err(Error::Disconnected),
        }
    }

    fn call_done(&self, request: Request) -> Result<(), Error> {
        match self.call(request)? {
            Response::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    pub fn outputs(&self) -> Result<Vec<Output>, Error> {
        match self.call(Request::ListOutputs)? {
            Response::Outputs { outputs } => Ok(outputs),
            other => Err(unexpected(&other)),
        }
    }

    pub fn state(&self) -> Result<State, Error> {
        match self.call(Request::GetState)? {
            Response::State(state) => Ok(state),
            other => Err(unexpected(&other)),
        }
    }

    /// Plays `wallpaper` on `output` (`None`: every output).
    pub fn apply(
        &self,
        output: Option<&str>,
        wallpaper: impl Into<PathBuf>,
        scaling: Scaling,
    ) -> Result<(), Error> {
        self.call_done(Request::Apply {
            output: output.map(str::to_owned),
            wallpaper: wallpaper.into(),
            scaling,
        })
    }

    pub fn clear(&self, output: Option<&str>) -> Result<(), Error> {
        self.call_done(Request::Clear {
            output: output.map(str::to_owned),
        })
    }

    pub fn properties(&self, output: &str) -> Result<PropertyMap, Error> {
        match self.call(Request::GetProperties {
            output: output.to_owned(),
        })? {
            Response::Properties { values, .. } => Ok(values),
            other => Err(unexpected(&other)),
        }
    }

    pub fn set_properties(
        &self,
        output: &str,
        values: PropertyMap,
        persist: bool,
    ) -> Result<(), Error> {
        self.call_done(Request::SetProperties {
            output: output.to_owned(),
            values,
            persist,
        })
    }

    pub fn reset_properties(&self, output: &str) -> Result<(), Error> {
        self.call_done(Request::ResetProperties {
            output: output.to_owned(),
        })
    }

    pub fn set_paused(&self, paused: bool) -> Result<(), Error> {
        self.call_done(Request::SetPaused { paused })
    }

    pub fn set_muted(&self, muted: bool) -> Result<(), Error> {
        self.call_done(Request::SetMuted { muted })
    }

    pub fn set_volume(&self, volume: f32) -> Result<(), Error> {
        self.call_done(Request::SetVolume { volume })
    }

    /// Sound level of one wallpaper (see [`Request::SetWallpaperAudio`]).
    pub fn set_wallpaper_audio(
        &self,
        wallpaper: impl Into<PathBuf>,
        volume: Option<f32>,
        muted: Option<bool>,
    ) -> Result<(), Error> {
        self.call_done(Request::SetWallpaperAudio {
            wallpaper: wallpaper.into(),
            volume,
            muted,
        })
    }

    pub fn settings(&self) -> Result<Settings, Error> {
        match self.call(Request::GetSettings)? {
            Response::Settings(settings) => Ok(settings),
            other => Err(unexpected(&other)),
        }
    }

    pub fn set_settings(&self, settings: Settings) -> Result<(), Error> {
        self.call_done(Request::SetSettings { settings })
    }

    pub fn stats(&self) -> Result<Stats, Error> {
        match self.call(Request::GetStats)? {
            Response::Stats(stats) => Ok(stats),
            other => Err(unexpected(&other)),
        }
    }

    pub fn subscribe(&self, topics: &[Topic]) -> Result<(), Error> {
        self.call_done(Request::Subscribe {
            topics: topics.to_vec(),
        })
    }

    pub fn unsubscribe(&self, topics: &[Topic]) -> Result<(), Error> {
        self.call_done(Request::Unsubscribe {
            topics: topics.to_vec(),
        })
    }

    /// Asks the daemon to exit.
    pub fn shutdown(&self) -> Result<(), Error> {
        self.call_done(Request::Shutdown)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Lets the daemon close the connection now, which also ends our reader thread.
        if self.is_connected() {
            let line = encode_frame(&ClientFrame {
                id: 0,
                request: Request::Bye,
            });
            let _ = self.shared.write(&line);
        }
    }
}

impl Shared {
    fn write(&self, line: &[u8]) -> io::Result<()> {
        let _guard = lock(&self.write_lock);
        write_frame(&mut &*self.stream, line)
    }
}

fn unexpected(response: &Response) -> Error {
    Error::Protocol(format!("unexpected reply: {response:?}"))
}

fn is_disconnect(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::NotConnected
    )
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

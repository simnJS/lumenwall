//! End-to-end tests over a real local socket (Unix socket / named pipe).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use interprocess::local_socket::{GenericFilePath, GenericNamespaced, Stream, prelude::*};
use lumen_ipc::*;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A unique endpoint per test, so tests can run in parallel.
fn test_endpoint() -> Endpoint {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    if cfg!(unix) {
        let dir = std::env::temp_dir().join(format!("lumen-ipc-test-{}", std::process::id()));
        Endpoint::Path(dir.join(format!("s{n}.sock")))
    } else {
        Endpoint::Name(format!("lumen-ipc-test-{}-{n}", std::process::id()))
    }
}

fn raw_connect(endpoint: &Endpoint) -> Stream {
    let name = match endpoint {
        Endpoint::Path(p) => p.as_path().to_fs_name::<GenericFilePath>().unwrap(),
        Endpoint::Name(n) => n.as_str().to_ns_name::<GenericNamespaced>().unwrap(),
    };
    Stream::connect(name).unwrap()
}

/// In-memory daemon double. Publishes state changes like the real daemon should.
#[derive(Default)]
struct FakeDaemon {
    state: Mutex<State>,
    props: Mutex<PropertyMap>,
    settings: Mutex<Settings>,
    publisher: OnceLock<Publisher>,
    subscribers: Mutex<Vec<(Topic, usize)>>,
}

impl FakeDaemon {
    fn changed(&self) {
        if let Some(p) = self.publisher.get() {
            p.publish(&Event::State(self.state.lock().unwrap().clone()));
        }
    }
}

impl Handler for FakeDaemon {
    fn outputs(&self) -> Result<Vec<Output>, RemoteError> {
        Ok(vec![
            Output {
                id: "DP-1".into(),
                name: "Left".into(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
                scale: 1.0,
                refresh_mhz: 144_000,
                primary: true,
            },
            Output {
                id: "HDMI-A-1".into(),
                name: "Right".into(),
                x: 2560,
                y: 0,
                width: 1920,
                height: 1080,
                scale: 1.0,
                refresh_mhz: 60_000,
                primary: false,
            },
        ])
    }

    fn state(&self) -> Result<State, RemoteError> {
        Ok(self.state.lock().unwrap().clone())
    }

    fn apply(
        &self,
        output: Option<&str>,
        wallpaper: &Path,
        scaling: Scaling,
    ) -> Result<(), RemoteError> {
        if !wallpaper.is_absolute() {
            return Err(RemoteError::new(ErrorCode::NotFound, "relative path"));
        }
        let ids: Vec<String> = match output {
            Some(o) if o != "DP-1" && o != "HDMI-A-1" => {
                return Err(RemoteError::new(ErrorCode::UnknownOutput, o));
            }
            Some(o) => vec![o.to_owned()],
            None => vec!["DP-1".into(), "HDMI-A-1".into()],
        };
        {
            let mut state = self.state.lock().unwrap();
            for id in ids {
                state.outputs.retain(|o| o.output != id);
                state.outputs.push(OutputState {
                    output: id,
                    wallpaper: Some(wallpaper.to_owned()),
                    scaling,
                    status: WallpaperStatus::Running,
                    message: None,
                });
            }
        }
        self.changed();
        Ok(())
    }

    fn properties(&self, _output: &str) -> Result<PropertyMap, RemoteError> {
        Ok(self.props.lock().unwrap().clone())
    }

    fn set_properties(
        &self,
        output: &str,
        values: PropertyMap,
        _persist: bool,
    ) -> Result<(), RemoteError> {
        self.props.lock().unwrap().extend(values);
        if let Some(p) = self.publisher.get() {
            p.publish(&Event::Properties {
                output: output.to_owned(),
                values: self.props.lock().unwrap().clone(),
            });
        }
        Ok(())
    }

    fn set_paused(&self, paused: bool) -> Result<(), RemoteError> {
        {
            let mut s = self.state.lock().unwrap();
            s.paused = paused;
            s.pause_reason = paused.then_some(PauseReason::User);
        }
        self.changed();
        Ok(())
    }

    fn set_muted(&self, muted: bool) -> Result<(), RemoteError> {
        self.state.lock().unwrap().muted = muted;
        self.changed();
        Ok(())
    }

    fn set_volume(&self, volume: f32) -> Result<(), RemoteError> {
        self.state.lock().unwrap().volume = volume;
        self.changed();
        Ok(())
    }

    fn settings(&self) -> Result<Settings, RemoteError> {
        Ok(self.settings.lock().unwrap().clone())
    }

    fn set_settings(&self, settings: Settings) -> Result<(), RemoteError> {
        *self.settings.lock().unwrap() = settings;
        Ok(())
    }

    fn stats(&self) -> Result<Stats, RemoteError> {
        Ok(Stats {
            fps: 30.0,
            cpu_percent: 0.5,
            rss_bytes: 1 << 20,
            ..Stats::default()
        })
    }

    fn subscribers_changed(&self, topic: Topic, count: usize) {
        self.subscribers.lock().unwrap().push((topic, count));
    }
}

fn start() -> (Server, Arc<FakeDaemon>, Endpoint) {
    let endpoint = test_endpoint();
    let daemon = Arc::new(FakeDaemon::default());
    let server = Server::bind(&endpoint, daemon.clone(), "fake", "9.9.9").unwrap();
    daemon.publisher.set(server.publisher()).ok().unwrap();
    (server, daemon, endpoint)
}

fn connect_with_channel(endpoint: &Endpoint) -> (Client, Receiver<ClientEvent>) {
    let (tx, rx) = mpsc::channel();
    let client = Client::connect_with_events(endpoint, "test", move |e| {
        let _ = tx.send(e);
    })
    .unwrap();
    (client, rx)
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::yield_now();
        std::thread::park_timeout(Duration::from_millis(5));
    }
}

#[test]
fn handshake_and_every_call() {
    let (_server, _daemon, endpoint) = start();
    let client = Client::connect(&endpoint, "test").unwrap();
    assert_eq!(
        client.server_info(),
        &ServerInfo {
            protocol: PROTOCOL_VERSION,
            server: "fake".into(),
            version: "9.9.9".into()
        }
    );

    let outputs = client.outputs().unwrap();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[1].x, 2560);

    client
        .apply(Some("HDMI-A-1"), "/w/3018340632", Scaling::Fit)
        .unwrap();
    let state = client.state().unwrap();
    assert_eq!(state.outputs.len(), 1);
    assert_eq!(
        state.outputs[0].wallpaper.as_deref(),
        Some(Path::new("/w/3018340632"))
    );
    assert_eq!(state.outputs[0].scaling, Scaling::Fit);

    client.set_paused(true).unwrap();
    client.set_muted(true).unwrap();
    client.set_volume(7.0).unwrap(); // clamped by the server
    let state = client.state().unwrap();
    assert!(state.paused && state.muted);
    assert_eq!(state.pause_reason, Some(PauseReason::User));
    assert_eq!(state.volume, 1.0);

    let values = PropertyMap::from([("rain".to_owned(), PropertyValue::Bool(false))]);
    client.set_properties("DP-1", values.clone(), true).unwrap();
    assert_eq!(client.properties("DP-1").unwrap(), values);

    let settings = Settings {
        fps_limit: 60,
        pause_on_battery: false,
        ..Settings::default()
    };
    client.set_settings(settings.clone()).unwrap();
    assert_eq!(client.settings().unwrap(), settings);
    assert_eq!(client.stats().unwrap().fps, 30.0);
}

#[test]
fn remote_errors_keep_the_connection() {
    let (_server, _daemon, endpoint) = start();
    let client = Client::connect(&endpoint, "test").unwrap();
    let err = client
        .apply(Some("DP-9"), "/w/1", Scaling::Fill)
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Remote(RemoteError {
                code: ErrorCode::UnknownOutput,
                ..
            })
        ),
        "{err}"
    );
    // Not implemented by the fake daemon: default trait method.
    let err = client.clear(None).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Remote(RemoteError {
                code: ErrorCode::Unsupported,
                ..
            })
        ),
        "{err}"
    );
    let err = client.set_volume(f32::NAN).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Remote(RemoteError {
                code: ErrorCode::InvalidRequest,
                ..
            })
        ),
        "{err}"
    );
    assert!(client.is_connected());
    client.state().unwrap();
}

#[test]
fn events_reach_subscribers_only() {
    let (_server, daemon, endpoint) = start();
    let (watcher, events) = connect_with_channel(&endpoint);
    let (other, other_events) = connect_with_channel(&endpoint);
    watcher.subscribe(&[Topic::State]).unwrap();

    other.set_paused(true).unwrap();
    let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
    let ClientEvent::Event(Event::State(state)) = event else {
        panic!("{event:?}")
    };
    assert!(state.paused);
    // The client that made the change did not subscribe.
    assert!(
        other_events
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    );

    // Properties are a different topic.
    other
        .set_properties(
            "DP-1",
            PropertyMap::from([("a".into(), PropertyValue::Number(1.0))]),
            false,
        )
        .unwrap();
    watcher.unsubscribe(&[Topic::State]).unwrap();
    other.set_paused(false).unwrap();
    assert!(events.recv_timeout(Duration::from_millis(100)).is_err());
    drop(daemon);
}

#[test]
fn subscriber_counts_drive_the_daemon() {
    let (server, daemon, endpoint) = start();
    let publisher = server.publisher();
    assert!(!publisher.has_subscribers(Topic::Stats));

    let client = Client::connect(&endpoint, "test").unwrap();
    client.subscribe(&[Topic::Stats, Topic::State]).unwrap();
    client.subscribe(&[Topic::Stats]).unwrap(); // idempotent
    assert!(publisher.has_subscribers(Topic::Stats));
    assert_eq!(server.client_count(), 1);

    drop(client); // sends `bye`: the daemon notices at once
    wait_until("unsubscribe on disconnect", || {
        !publisher.has_subscribers(Topic::Stats)
    });
    wait_until("client removal", || server.client_count() == 0);
    let log = daemon.subscribers.lock().unwrap().clone();
    assert!(
        log.contains(&(Topic::Stats, 1)) && log.contains(&(Topic::Stats, 0)),
        "{log:?}"
    );
    assert_eq!(
        log.iter()
            .filter(|(t, c)| *t == Topic::Stats && *c == 1)
            .count(),
        1
    );
    assert!(!publisher.has_subscribers(Topic::State));
}

#[test]
fn malformed_frames_get_an_error_reply() {
    let (_server, _daemon, endpoint) = start();
    let stream = raw_connect(&endpoint);
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();

    (&stream)
        .write_all(b"{\"id\":7,\"method\":\"teleport\"}\n")
        .unwrap();
    reader.read_line(&mut line).unwrap();
    let frame: ServerFrame = serde_json::from_str(&line).unwrap();
    let ServerFrame::Reply(Reply {
        id: 7,
        outcome: Outcome::Error(e),
    }) = frame
    else {
        panic!("{line}")
    };
    assert_eq!(e.code, ErrorCode::InvalidRequest);

    // Blank lines are ignored, the connection still works, and `hello` is optional.
    line.clear();
    (&stream)
        .write_all(b"\n{\"id\":8,\"method\":\"get_state\"}\n")
        .unwrap();
    reader.read_line(&mut line).unwrap();
    assert!(
        line.starts_with(r#"{"reply":{"id":8,"ok":{"type":"state""#),
        "{line}"
    );
}

#[test]
fn version_mismatch_is_reported() {
    let (_server, _daemon, endpoint) = start();
    let stream = raw_connect(&endpoint);
    (&stream)
        .write_all(
            b"{\"id\":1,\"method\":\"hello\",\"params\":{\"protocol\":999,\"client\":\"x\"}}\n",
        )
        .unwrap();
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).unwrap();
    assert!(line.contains("version_mismatch"), "{line}");
}

#[test]
fn oversized_frames_close_the_connection() {
    let (server, _daemon, endpoint) = start();
    let stream = raw_connect(&endpoint);
    let big = vec![b'x'; MAX_FRAME_LEN + 10];
    // The server may hang up before reading everything: ignore write errors.
    let _ = (&stream).write_all(&big);
    let mut line = String::new();
    let n = BufReader::new(&stream).read_line(&mut line).unwrap_or(0);
    assert_eq!(n, 0, "expected EOF, got {line:?}");
    wait_until("client removal", || server.client_count() == 0);
}

#[test]
fn concurrent_calls_get_their_own_replies() {
    let (_server, _daemon, endpoint) = start();
    let client = Arc::new(Client::connect(&endpoint, "test").unwrap());
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let client = Arc::clone(&client);
            std::thread::spawn(move || {
                for _ in 0..50 {
                    if i % 2 == 0 {
                        assert_eq!(client.outputs().unwrap().len(), 2);
                    } else {
                        assert_eq!(client.stats().unwrap().fps, 30.0);
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}

#[test]
fn not_running_and_already_running() {
    let endpoint = test_endpoint();
    assert!(matches!(
        Client::connect(&endpoint, "test"),
        Err(Error::NotRunning)
    ));

    let (server, _daemon, endpoint) = start();
    let second = Server::bind(&endpoint, Arc::new(FakeDaemon::default()), "fake", "1");
    assert!(matches!(second, Err(Error::AlreadyRunning)));
    drop(server);
}

#[test]
fn shutdown_notifies_clients_and_cleans_up() {
    let (server, _daemon, endpoint) = start();
    let (client, events) = connect_with_channel(&endpoint);
    drop(server);
    let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(event, ClientEvent::Event(Event::ShuttingDown));
    if let Endpoint::Path(path) = &endpoint {
        assert!(!path.exists(), "socket file left behind");
    }
    // A new daemon can take over the endpoint.
    let daemon = Arc::new(FakeDaemon::default());
    let _again = Server::bind(&endpoint, daemon, "fake", "2").unwrap();
    drop(client);
}

#[cfg(unix)]
#[test]
fn stale_socket_file_is_replaced() {
    let endpoint = test_endpoint();
    let Endpoint::Path(path) = &endpoint else {
        unreachable!()
    };
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // A socket file nobody listens on, as left by a crashed daemon.
    let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
    drop(listener);
    assert!(path.exists());
    let server = Server::bind(&endpoint, Arc::new(FakeDaemon::default()), "fake", "1").unwrap();
    let client = Client::connect(&endpoint, "test").unwrap();
    assert_eq!(client.server_info().server, "fake");
    drop(client);
    drop(server);
    let _ = std::fs::remove_dir(path.parent().unwrap_or(&std::path::PathBuf::new()));
}

#[test]
fn disconnect_is_reported_and_calls_fail_fast() {
    let (server, _daemon, endpoint) = start();
    let (client, events) = connect_with_channel(&endpoint);
    client.subscribe(&[Topic::State]).unwrap();
    drop(server);
    assert_eq!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        ClientEvent::Event(Event::ShuttingDown)
    );
    assert_eq!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        ClientEvent::Disconnected
    );
    assert!(!client.is_connected());
    assert!(matches!(client.state(), Err(Error::Disconnected)));
}

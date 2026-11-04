//! The UI's view of the daemon. A worker thread owns the IPC client, so a slow or
//! hung daemon never blocks a frame; results and events come back as messages
//! through an iced subscription. Nothing here runs on a timer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use iced::futures::channel::mpsc::{UnboundedSender, unbounded};
use iced::futures::{SinkExt, StreamExt};

use lumenwall_ipc::{
    Client, ClientEvent, Endpoint, Error, Event, Output, Playlist, PlaylistPosition, PropertyMap,
    Request, Response, Scaling, ServerInfo, Settings, State, Stats, Topic, WallpaperAudio,
    wallpaper_key,
};

use crate::platform;

#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    Connecting,
    Starting,
    Online(ServerInfo),
    Offline(Offline),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Offline {
    NotRunning,
    /// The `lumenwall` binary was not found.
    NoBinary,
    Incompatible(String),
    /// Started, but the IPC endpoint never answered.
    NoAnswer,
    Failed(String),
}

pub enum Cmd {
    Connect,
    Launch,
    Call(Request),
    /// Live property preview; consecutive previews are merged.
    Preview {
        output: String,
        values: PropertyMap,
    },
    FetchProperties(String),
    /// Wallpaper Engine playlists, read again by the daemon.
    FetchPlaylists,
    Stats(bool),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Online {
        info: ServerInfo,
        outputs: Vec<Output>,
        state: State,
        settings: Option<Settings>,
    },
    Offline(Offline),
    Starting,
    Event(Event),
    State(State),
    Settings(Settings),
    Properties(String, PropertyMap),
    /// `Err`: the daemon cannot read them (no Wallpaper Engine installation...); `None`
    /// inside: the daemon has no playlists at all (older build).
    Playlists(Option<Result<Vec<Playlist>, String>>),
    Error(String),
}

pub struct DaemonLink {
    pub link: Link,
    pub outputs: Vec<Output>,
    pub state: Option<State>,
    pub settings: Option<Settings>,
    pub stats: Option<Stats>,
    /// Property values reported by the daemon, by output.
    pub properties: HashMap<String, PropertyMap>,
    /// Wallpaper Engine playlists: `None` until known or when the daemon has none (then
    /// hide the controls), `Some(Err)` when the daemon cannot read them.
    pub playlists: Option<Result<Vec<Playlist>, String>>,
    /// Errors to show once (drained by the UI).
    pub errors: Vec<String>,
    cmd: Option<Sender<Cmd>>,
    stats_wanted: bool,
}

/// What the daemon subscription produces.
#[derive(Debug, Clone)]
pub enum DaemonEvent {
    /// The worker is running; commands go to this sender.
    Ready(Sender<Cmd>),
    Msg(Msg),
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cmd")
    }
}

/// Runs the IPC worker for as long as the subscription is active.
pub fn subscription(endpoint: lumenwall_ipc::Endpoint) -> iced::Subscription<DaemonEvent> {
    iced::Subscription::run_with(endpoint, |endpoint| {
        let endpoint = endpoint.clone();
        iced::stream::channel(64, async move |mut output| {
            let (cmd_tx, cmd_rx) = mpsc::channel();
            let (ui_tx, mut ui_rx) = unbounded();
            let spawned = std::thread::Builder::new()
                .name("lumenwall-ui-daemon".into())
                .spawn(move || {
                    Worker {
                        endpoint,
                        client: None,
                        tx: ui_tx,
                        stats: false,
                    }
                    .run(cmd_rx)
                });
            if let Err(e) = spawned {
                tracing::error!("cannot start the daemon link: {e}");
                return;
            }
            let _ = output.send(DaemonEvent::Ready(cmd_tx)).await;
            while let Some(msg) = ui_rx.next().await {
                if output.send(DaemonEvent::Msg(msg)).await.is_err() {
                    break;
                }
            }
        })
    })
}

impl DaemonLink {
    pub fn new() -> Self {
        Self {
            link: Link::Connecting,
            outputs: Vec::new(),
            state: None,
            settings: None,
            stats: None,
            properties: HashMap::new(),
            playlists: None,
            errors: Vec::new(),
            cmd: None,
            stats_wanted: false,
        }
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.cmd {
            let _ = tx.send(cmd);
        }
    }

    pub fn handle(&mut self, event: DaemonEvent) {
        match event {
            DaemonEvent::Ready(tx) => {
                self.cmd = Some(tx);
                self.send(Cmd::Connect);
            }
            DaemonEvent::Msg(msg) => self.apply_msg(msg),
        }
    }

    pub fn is_online(&self) -> bool {
        matches!(self.link, Link::Online(_))
    }

    fn apply_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Online {
                info,
                outputs,
                state,
                settings,
            } => {
                self.link = Link::Online(info);
                self.outputs = outputs;
                self.state = Some(state);
                self.settings = settings;
                if self.stats_wanted {
                    self.send(Cmd::Stats(true));
                }
                self.send(Cmd::FetchPlaylists);
            }
            Msg::Offline(reason) => {
                self.link = Link::Offline(reason);
                self.state = None;
                self.stats = None;
                self.outputs.clear();
                self.properties.clear();
                self.playlists = None;
            }
            Msg::Starting => self.link = Link::Starting,
            Msg::Event(event) => match event {
                Event::State(s) => self.state = Some(s),
                Event::Outputs { outputs } => self.outputs = outputs,
                Event::Settings(s) => self.settings = Some(s),
                Event::Properties { output, values } => {
                    self.properties.insert(output, values);
                }
                Event::Stats(s) => self.stats = Some(s),
                Event::ShuttingDown => {}
            },
            Msg::State(s) => self.state = Some(s),
            Msg::Settings(s) => self.settings = Some(s),
            Msg::Properties(output, values) => {
                self.properties.insert(output, values);
            }
            Msg::Playlists(list) => self.playlists = list,
            Msg::Error(e) => self.errors.push(e),
        }
    }

    /// Asks the daemon for the playlists again (they may have been made in Wallpaper Engine
    /// meanwhile). Only when online.
    pub fn fetch_playlists(&self) {
        if self.is_online() {
            self.send(Cmd::FetchPlaylists);
        }
    }

    /// The playlist that drives `output`, if any.
    pub fn playlist_on(&self, output: &str) -> Option<&PlaylistPosition> {
        self.state
            .as_ref()?
            .outputs
            .iter()
            .find(|o| o.output == output)?
            .playlist
            .as_ref()
    }

    pub fn apply_playlist(&self, output: Option<String>, name: String, scaling: Scaling) {
        self.send(Cmd::Call(Request::ApplyPlaylist {
            output,
            name,
            scaling,
        }));
    }

    pub fn playlist_step(&self, output: Option<String>, forward: bool) {
        self.send(Cmd::Call(if forward {
            Request::PlaylistNext { output }
        } else {
            Request::PlaylistPrevious { output }
        }));
    }

    /// Tries again after "not running" (window focused, user asked). Never loops.
    pub fn reconnect(&mut self) {
        if matches!(self.link, Link::Offline(_)) {
            self.link = Link::Connecting;
            self.send(Cmd::Connect);
        }
    }

    pub fn launch(&mut self) {
        self.link = Link::Starting;
        self.send(Cmd::Launch);
    }

    /// Stats are subscribed only while someone looks at them.
    pub fn want_stats(&mut self, wanted: bool) {
        if wanted != self.stats_wanted {
            self.stats_wanted = wanted;
            if !wanted {
                self.stats = None;
            }
            if self.is_online() {
                self.send(Cmd::Stats(wanted));
            }
        }
    }

    pub fn apply(&self, output: Option<String>, wallpaper: PathBuf, scaling: Scaling) {
        self.send(Cmd::Call(Request::Apply {
            output,
            wallpaper,
            scaling,
        }));
    }

    pub fn clear(&self, output: Option<String>) {
        self.send(Cmd::Call(Request::Clear { output }));
    }

    pub fn set_paused(&self, paused: bool) {
        self.send(Cmd::Call(Request::SetPaused { paused }));
    }

    pub fn set_muted(&self, muted: bool) {
        self.send(Cmd::Call(Request::SetMuted { muted }));
    }

    pub fn set_volume(&self, volume: f32) {
        self.send(Cmd::Call(Request::SetVolume { volume }));
    }

    /// Sound level of the wallpaper in `dir`: `None` when the daemon has no per-wallpaper
    /// levels (offline or older), else its level (the default when never set).
    pub fn wallpaper_audio(&self, dir: &Path) -> Option<WallpaperAudio> {
        let levels = self.state.as_ref()?.wallpaper_audio.as_ref()?;
        Some(levels.get(&wallpaper_key(dir)).copied().unwrap_or_default())
    }

    pub fn set_wallpaper_audio(
        &self,
        wallpaper: PathBuf,
        volume: Option<f32>,
        muted: Option<bool>,
    ) {
        self.send(Cmd::Call(Request::SetWallpaperAudio {
            wallpaper,
            volume,
            muted,
        }));
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = Some(settings.clone());
        self.send(Cmd::Call(Request::SetSettings { settings }));
    }

    pub fn preview_properties(&self, output: String, values: PropertyMap) {
        self.send(Cmd::Preview { output, values });
    }

    pub fn save_properties(&self, output: String, values: PropertyMap) {
        self.send(Cmd::Call(Request::SetProperties {
            output,
            values,
            persist: true,
        }));
    }

    pub fn reset_properties(&self, output: String) {
        self.send(Cmd::Call(Request::ResetProperties { output }));
    }

    pub fn fetch_properties(&self, output: String) {
        self.send(Cmd::FetchProperties(output));
    }
}

/// What the UI makes of a `list_playlists` answer: daemons without playlists (an unknown
/// request, or not implemented) hide the controls; other errors are shown.
fn playlists_reply(reply: Result<Vec<Playlist>, Error>) -> Option<Result<Vec<Playlist>, String>> {
    use lumenwall_ipc::ErrorCode;
    match reply {
        Ok(list) => Some(Ok(list)),
        Err(Error::Remote(e))
            if matches!(e.code, ErrorCode::InvalidRequest | ErrorCode::Unsupported) =>
        {
            None
        }
        Err(Error::Remote(e)) => Some(Err(e.message)),
        Err(e) => {
            tracing::debug!("IPC: {e}");
            None
        }
    }
}

/// Whether `next` makes `queued` pointless: the same volume control set again.
fn supersedes(next: &Request, queued: &Request) -> bool {
    match (next, queued) {
        (Request::SetVolume { .. }, Request::SetVolume { .. }) => true,
        (
            Request::SetWallpaperAudio {
                wallpaper: a,
                volume: Some(_),
                muted: None,
            },
            Request::SetWallpaperAudio {
                wallpaper: b,
                volume: Some(_),
                muted: None,
            },
        ) => a == b,
        _ => false,
    }
}

struct Worker {
    endpoint: Endpoint,
    client: Option<Client>,
    tx: UnboundedSender<Msg>,
    stats: bool,
}

impl Worker {
    fn run(mut self, rx: Receiver<Cmd>) {
        let mut pending: Option<Cmd> = None;
        loop {
            let cmd = match pending.take() {
                Some(c) => c,
                None => match rx.recv() {
                    Ok(c) => c,
                    Err(_) => return,
                },
            };
            match cmd {
                Cmd::Preview { output, mut values } => {
                    // Merge every preview already queued for the same output: only the
                    // latest slider position matters.
                    loop {
                        match rx.try_recv() {
                            Ok(Cmd::Preview {
                                output: o,
                                values: v,
                            }) if o == output => values.extend(v),
                            Ok(other) => {
                                pending = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    self.call(Request::SetProperties {
                        output,
                        values,
                        persist: false,
                    });
                }
                Cmd::Connect => self.connect(),
                Cmd::Launch => self.launch(),
                Cmd::Call(mut req) => {
                    // A slider being dragged queues one call per step: only the latest
                    // position of the same control matters.
                    while let Ok(next) = rx.try_recv() {
                        match next {
                            Cmd::Call(n) if supersedes(&n, &req) => req = n,
                            other => {
                                pending = Some(other);
                                break;
                            }
                        }
                    }
                    let refresh = matches!(
                        req,
                        Request::Apply { .. }
                            | Request::ApplyPlaylist { .. }
                            | Request::PlaylistNext { .. }
                            | Request::PlaylistPrevious { .. }
                            | Request::Clear { .. }
                            | Request::SetPaused { .. }
                            | Request::SetMuted { .. }
                            | Request::SetVolume { .. }
                            | Request::SetWallpaperAudio { .. }
                    );
                    let settings = matches!(req, Request::SetSettings { .. });
                    if self.call(req).is_some() {
                        // The daemon also publishes these; asking keeps the UI right with
                        // a daemon that does not (yet) publish events.
                        if refresh && let Some(Response::State(s)) = self.call(Request::GetState) {
                            self.send(Msg::State(s));
                        }
                        if settings
                            && let Some(Response::Settings(s)) = self.call(Request::GetSettings)
                        {
                            self.send(Msg::Settings(s));
                        }
                    }
                }
                Cmd::FetchProperties(output) => {
                    if let Some(Response::Properties { output, values }) =
                        self.call_quiet(Request::GetProperties { output })
                    {
                        self.send(Msg::Properties(output, values));
                    }
                }
                Cmd::FetchPlaylists => {
                    let Some(client) = &self.client else {
                        continue;
                    };
                    match client.playlists() {
                        Err(Error::Disconnected) => {
                            self.client = None;
                            self.send(Msg::Offline(Offline::NotRunning));
                        }
                        reply => self.send(Msg::Playlists(playlists_reply(reply))),
                    }
                }
                Cmd::Stats(on) => {
                    self.stats = on;
                    let topics = vec![Topic::Stats];
                    let req = if on {
                        Request::Subscribe { topics }
                    } else {
                        Request::Unsubscribe { topics }
                    };
                    self.call_quiet(req);
                }
            }
        }
    }

    fn send(&self, msg: Msg) {
        let _ = self.tx.unbounded_send(msg);
    }

    fn connect(&mut self) {
        match self.try_connect() {
            Ok(()) => {}
            Err(reason) => self.send(Msg::Offline(reason)),
        }
    }

    fn try_connect(&mut self) -> Result<(), Offline> {
        self.client = None;
        let tx = self.tx.clone();
        let client = Client::connect_with_events(&self.endpoint, "lumenwall-ui", move |e| {
            let msg = match e {
                ClientEvent::Event(ev) => Msg::Event(ev),
                ClientEvent::Disconnected => Msg::Offline(Offline::NotRunning),
            };
            let _ = tx.unbounded_send(msg);
        })
        .map_err(|e| match e {
            Error::NotRunning => Offline::NotRunning,
            Error::VersionMismatch(m) => Offline::Incompatible(m),
            other => Offline::Failed(other.to_string()),
        })?;
        let fail = |e: Error| Offline::Failed(e.to_string());
        let mut topics = vec![
            Topic::State,
            Topic::Outputs,
            Topic::Settings,
            Topic::Properties,
        ];
        if self.stats {
            topics.push(Topic::Stats);
        }
        // A daemon that does not implement some of these still gets a usable UI.
        let _ = client.subscribe(&topics);
        let outputs = client.outputs().unwrap_or_default();
        let state = client.state().map_err(fail)?;
        let settings = client.settings().ok();
        let info = client.server_info().clone();
        self.client = Some(client);
        self.send(Msg::Online {
            info,
            outputs,
            state,
            settings,
        });
        Ok(())
    }

    fn launch(&mut self) {
        let Some(exe) = platform::find_daemon() else {
            self.send(Msg::Offline(Offline::NoBinary));
            return;
        };
        self.send(Msg::Starting);
        if let Err(e) = platform::spawn_daemon(&exe) {
            self.send(Msg::Offline(Offline::Failed(e.to_string())));
            return;
        }
        // Bounded wait for the daemon to open its endpoint (about 6 s in total).
        let mut delay = Duration::from_millis(100);
        for _ in 0..7 {
            std::thread::sleep(delay);
            match self.try_connect() {
                Ok(()) => return,
                Err(Offline::NotRunning) => delay *= 2,
                Err(other) => {
                    self.send(Msg::Offline(other));
                    return;
                }
            }
        }
        self.send(Msg::Offline(Offline::NoAnswer));
    }

    /// Calls the daemon; errors are reported to the user.
    fn call(&mut self, req: Request) -> Option<Response> {
        match self.call_inner(req) {
            Ok(r) => Some(r),
            Err(e) => {
                self.send(Msg::Error(e));
                None
            }
        }
    }

    /// Calls the daemon; errors are only logged (background refreshes).
    fn call_quiet(&mut self, req: Request) -> Option<Response> {
        self.call_inner(req)
            .map_err(|e| tracing::debug!("IPC: {e}"))
            .ok()
    }

    fn call_inner(&mut self, req: Request) -> Result<Response, String> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| "not connected".to_owned())?;
        match client.call(req) {
            Ok(r) => Ok(r),
            Err(Error::Disconnected) => {
                self.client = None;
                self.send(Msg::Offline(Offline::NotRunning));
                Err(Error::Disconnected.to_string())
            }
            Err(Error::Remote(e)) => Err(e.message),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_controls_hide_for_daemons_without_playlists() {
        use lumenwall_ipc::{ErrorCode, RemoteError};
        let remote = |code| Err(Error::Remote(RemoteError::new(code, "x")));
        // Unknown request (older daemon) or not implemented: no controls.
        assert_eq!(playlists_reply(remote(ErrorCode::InvalidRequest)), None);
        assert_eq!(playlists_reply(remote(ErrorCode::Unsupported)), None);
        assert_eq!(playlists_reply(Err(Error::Timeout)), None);
        // The daemon cannot read Wallpaper Engine's settings: say why.
        assert_eq!(
            playlists_reply(remote(ErrorCode::NotFound)),
            Some(Err("x".to_owned()))
        );
        assert_eq!(playlists_reply(Ok(vec![])), Some(Ok(vec![])));
    }

    #[test]
    fn only_the_latest_volume_is_sent() {
        let vol = |volume| Request::SetVolume { volume };
        let level = |w: &str, volume, muted| Request::SetWallpaperAudio {
            wallpaper: w.into(),
            volume,
            muted,
        };
        assert!(supersedes(&vol(0.2), &vol(0.1)));
        assert!(supersedes(
            &level("/w/1", Some(0.2), None),
            &level("/w/1", Some(0.1), None)
        ));
        assert!(!supersedes(
            &level("/w/2", Some(0.2), None),
            &level("/w/1", Some(0.1), None)
        ));
        // A mute toggle is never dropped.
        assert!(!supersedes(
            &level("/w/1", None, Some(true)),
            &level("/w/1", Some(0.1), None)
        ));
        assert!(!supersedes(
            &level("/w/1", Some(0.1), None),
            &level("/w/1", None, Some(true))
        ));
        assert!(!supersedes(&vol(0.2), &Request::SetMuted { muted: true }));
    }
}

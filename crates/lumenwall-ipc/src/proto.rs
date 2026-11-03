//! Wire types. Every frame is one JSON object on one line (see the crate docs).
//!
//! Compatibility rules, so that a client and a daemon of different builds keep working:
//! - unknown object fields are ignored (no `deny_unknown_fields`);
//! - new fields are added with `#[serde(default)]`;
//! - unit enums that travel from the daemon to clients have an `Unknown` catch-all;
//! - anything else (renaming, removing, changing a type) bumps [`PROTOCOL_VERSION`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Protocol version, exchanged in [`Request::Hello`]. Bumped only on breaking changes.
pub const PROTOCOL_VERSION: u16 = 1;

/// Largest accepted frame, newline excluded. Bigger frames close the connection.
pub const MAX_FRAME_LEN: usize = 1 << 20;

/// Client to daemon: `{"id":1,"method":"apply","params":{...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientFrame {
    /// Chosen by the client, echoed in the reply. Unique per connection.
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// Daemon to client: either the reply to a request, or an event the client subscribed to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerFrame {
    /// `{"reply":{"id":1,"ok":{...}}}` or `{"reply":{"id":1,"error":{...}}}`.
    Reply(Reply),
    /// `{"event":{"type":"state",...}}`.
    Event(Event),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Response),
    Error(RemoteError),
}

/// What a client can ask. Methods without parameters omit `params`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Optional handshake; the [`crate::Client`] always sends it first.
    Hello {
        protocol: u16,
        client: String,
    },
    /// Tells the daemon the client is leaving, so it can close the connection at once.
    Bye,
    ListOutputs,
    GetState,
    /// Plays `wallpaper` (a wallpaper directory or its `project.json`) on `output`,
    /// or on every output when `output` is `None`.
    Apply {
        #[serde(default)]
        output: Option<String>,
        wallpaper: PathBuf,
        #[serde(default)]
        scaling: Scaling,
    },
    /// Removes the wallpaper of `output`, or of every output.
    Clear {
        #[serde(default)]
        output: Option<String>,
    },
    /// Current user property values of the wallpaper shown on `output`.
    GetProperties {
        output: String,
    },
    /// Changes user properties live. With `persist: false` the change is a preview
    /// (slider being dragged) that [`Request::ResetProperties`] can undo.
    SetProperties {
        output: String,
        values: PropertyMap,
        #[serde(default)]
        persist: bool,
    },
    /// Drops unsaved previews and returns to the saved values.
    ResetProperties {
        output: String,
    },
    SetPaused {
        paused: bool,
    },
    SetMuted {
        muted: bool,
    },
    /// 0.0 to 1.0.
    SetVolume {
        volume: f32,
    },
    GetSettings,
    SetSettings {
        settings: Settings,
    },
    GetStats,
    Subscribe {
        topics: Vec<Topic>,
    },
    Unsubscribe {
        topics: Vec<Topic>,
    },
    /// Stops the daemon.
    Shutdown,
}

/// Successful replies, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Reply to [`Request::Hello`].
    Welcome(ServerInfo),
    /// The request was carried out; nothing to return.
    Done,
    Outputs {
        outputs: Vec<Output>,
    },
    State(State),
    Properties {
        output: String,
        values: PropertyMap,
    },
    Settings(Settings),
    Stats(Stats),
}

/// Pushed to subscribed clients, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Full state after any change (pause, mute, volume, wallpaper, status).
    State(State),
    /// Monitors were plugged, unplugged or rearranged.
    Outputs {
        outputs: Vec<Output>,
    },
    Settings(Settings),
    Properties {
        output: String,
        values: PropertyMap,
    },
    /// Sent about once per second while at least one client subscribes to [`Topic::Stats`].
    Stats(Stats),
    /// The daemon is about to exit; the connection will close.
    ShuttingDown,
}

impl Event {
    pub fn topic(&self) -> Option<Topic> {
        match self {
            Event::State(_) => Some(Topic::State),
            Event::Outputs { .. } => Some(Topic::Outputs),
            Event::Settings(_) => Some(Topic::Settings),
            Event::Properties { .. } => Some(Topic::Properties),
            Event::Stats(_) => Some(Topic::Stats),
            // Delivered to every connected client.
            Event::ShuttingDown => None,
        }
    }
}

/// Event families a client can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    State,
    Outputs,
    Settings,
    Properties,
    Stats,
}

impl Topic {
    pub const ALL: [Topic; 5] = [
        Topic::State,
        Topic::Outputs,
        Topic::Settings,
        Topic::Properties,
        Topic::Stats,
    ];

    pub(crate) const fn bit(self) -> u8 {
        1 << self as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub protocol: u16,
    /// Program name, e.g. `lumenwall`.
    pub server: String,
    /// Program version, e.g. `0.1.0`.
    pub version: String,
}

/// A monitor as the daemon sees it. Coordinates are in physical pixels in the
/// desktop's global space, so clients can draw the monitor layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    /// Stable identifier: connector name on Linux (`DP-1`), device name on Windows.
    pub id: String,
    /// Human-readable name (make and model) when known.
    #[serde(default)]
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default = "one")]
    pub scale: f32,
    /// Refresh rate in millihertz (60 Hz = 60000), 0 when unknown.
    #[serde(default)]
    pub refresh_mhz: u32,
    #[serde(default)]
    pub primary: bool,
}

fn one() -> f32 {
    1.0
}

/// How the wallpaper is fitted to the output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scaling {
    /// Cover the output, cropping what overflows (keeps the aspect ratio).
    #[default]
    Fill,
    /// Show the whole wallpaper, with bars (keeps the aspect ratio).
    Fit,
    /// Cover the output exactly, distorting the aspect ratio.
    Stretch,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct State {
    pub paused: bool,
    /// Why playback is paused, when it is.
    #[serde(default)]
    pub pause_reason: Option<PauseReason>,
    pub muted: bool,
    /// 0.0 to 1.0.
    pub volume: f32,
    #[serde(default)]
    pub outputs: Vec<OutputState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// Paused from a client (UI, tray, command line).
    User,
    /// A fullscreen window covers the desktop.
    Fullscreen,
    /// Running on battery.
    Battery,
    /// The session is locked.
    Locked,
    /// Every output is hidden or covered.
    Hidden,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputState {
    pub output: String,
    /// Wallpaper directory, `None` when the output shows nothing.
    #[serde(default)]
    pub wallpaper: Option<PathBuf>,
    #[serde(default)]
    pub scaling: Scaling,
    #[serde(default)]
    pub status: WallpaperStatus,
    /// Detail for [`WallpaperStatus::Error`] and [`WallpaperStatus::Unsupported`].
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperStatus {
    #[default]
    Empty,
    Loading,
    Running,
    Paused,
    Error,
    /// The wallpaper uses a feature Lumenwall does not support yet.
    Unsupported,
    #[serde(other)]
    Unknown,
}

/// Daemon settings. Every field has a default, so older and newer peers can exchange it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Frame-rate cap, aligned to the display refresh rate by the daemon.
    pub fps_limit: u32,
    pub pause_on_fullscreen: bool,
    pub pause_on_battery: bool,
    pub pause_on_lock: bool,
    /// Mute wallpapers while another application plays sound.
    pub automute: bool,
    pub muted: bool,
    /// 0.0 to 1.0.
    pub volume: f32,
    /// Start the daemon with the desktop session.
    pub autostart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps_limit: 30,
            pause_on_fullscreen: true,
            pause_on_battery: true,
            pause_on_lock: true,
            automute: true,
            muted: false,
            volume: 0.5,
            autostart: false,
        }
    }
}

/// Performance counters of the daemon process. Optional fields are `None` when the
/// platform or driver does not report them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    /// Frames rendered per second, summed over outputs.
    pub fps: f32,
    /// Mean CPU time per frame on the render thread, in milliseconds.
    pub frame_time_ms: f32,
    /// Process CPU usage, 100 = one full core.
    pub cpu_percent: f32,
    pub gpu_percent: Option<f32>,
    /// Resident memory of the daemon.
    pub rss_bytes: u64,
    pub vram_bytes: Option<u64>,
    pub outputs: Vec<OutputStats>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputStats {
    pub output: String,
    pub fps: f32,
    pub frame_time_ms: f32,
    /// False while the output is paused or hidden (0 frames rendered).
    pub rendering: bool,
}

/// User property values keyed by property name (the keys of
/// `general.properties` in `project.json`).
pub type PropertyMap = BTreeMap<String, PropertyValue>;

/// A user property value, in its natural JSON form:
/// `true`, `0.5`, `[0.1, 0.2, 0.3]` (RGB 0..1) or `"text"`.
/// Combo values travel as their option value (number or text); files as their path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropertyValue {
    Bool(bool),
    Number(f64),
    Color([f32; 3]),
    Text(String),
}

/// Error returned by the daemon for one request. The connection stays usable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct RemoteError {
    pub code: ErrorCode,
    #[serde(default)]
    pub message: String,
}

impl RemoteError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn unsupported(what: &str) -> Self {
        Self::new(
            ErrorCode::Unsupported,
            format!("{what} is not supported by this daemon"),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The daemon does not implement this request.
    Unsupported,
    /// Malformed JSON, unknown method or bad parameters.
    InvalidRequest,
    /// The client speaks an incompatible protocol version.
    VersionMismatch,
    /// No output with this id.
    UnknownOutput,
    /// The path does not exist or is not a wallpaper.
    NotFound,
    /// The wallpaper type or one of its features cannot be played.
    UnsupportedWallpaper,
    Internal,
    #[serde(other)]
    Unknown,
}

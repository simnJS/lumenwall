//! Protocol between the main Lumenwall process and the web host process.
//!
//! One bidirectional byte stream (a Unix socket on Linux, the child's stdin/stdout pipes on
//! Windows). Each message is an envelope `[u32 LE json length][u8 handle count][3 bytes 0][json]`.
//! On Unix the handle count tells how many file descriptors (sent as `SCM_RIGHTS` with the
//! envelope) belong to the message; on Windows it is always 0 (the main process duplicates the
//! host's handles itself, see [`crate::process`]).

pub mod frame;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{Receiver, Sender, pair, take_stdin_socket};
#[cfg(windows)]
pub use windows::{Receiver, Sender, take_std_pipes};

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::media::{MediaEvent, MediaListeners};
use crate::mouse::MouseInput;
use crate::properties::{PropertySet, UserProperty};

pub use frame::{FrameInfo, PixelFormat, PlaneInfo};

/// Largest accepted message (album art thumbnails are the biggest payloads).
pub const MAX_MESSAGE_LEN: usize = 32 << 20;

/// Header length of an envelope.
pub const HEADER_LEN: usize = 8;

/// How the browser's frame production is clocked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FramePacing {
    /// The main process sends [`HostCommand::BeginFrame`] for each frame it wants (follows the
    /// compositor's frame callbacks; no frame is produced while none is requested).
    #[default]
    External,
    /// The browser's own timer at the wallpaper's fps.
    Internal,
}

/// How painted frames travel from the browser to the main process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameTransport {
    /// GPU textures shared by handle (Linux DMA-BUF, Windows NT handle): zero copy.
    #[default]
    Gpu,
    /// Software painting copied into one shared memory buffer (Linux `memfd`), uploaded by the
    /// main process: the fallback when the GPU texture cannot be imported.
    Memory,
}

/// Effective audio state of the page (`--volume`, `--silent`, automute, pause).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioOutput {
    /// Linear gain 0–1 (CLI volume / 128 for parity with scene sounds).
    pub volume: f32,
    pub muted: bool,
}

impl Default for AudioOutput {
    fn default() -> Self {
        Self {
            volume: 15.0 / 128.0,
            muted: false,
        }
    }
}

/// Everything the host needs to show one web wallpaper.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchParams {
    /// Wallpaper id (`workshopid` or synthetic `-1`, `-2`…); names the `wp<id>://` scheme.
    pub wallpaper_id: String,
    /// Wallpaper folder (the only folder served by the private scheme).
    pub root: PathBuf,
    /// `file` of `project.json`, relative to `root`.
    pub file: String,
    /// View size in physical pixels.
    pub width: u32,
    pub height: u32,
    /// Target frame rate of the wallpaper (no 60 fps floor).
    pub fps: u32,
    #[serde(default)]
    pub pacing: FramePacing,
    #[serde(default)]
    pub transport: FrameTransport,
    pub properties: PropertySet,
    #[serde(default)]
    pub audio: AudioOutput,
    #[serde(default)]
    pub paused: bool,
    /// Browser profile (cache, localStorage). `None`: a temporary folder removed on exit. Must be
    /// unique per running host.
    #[serde(default)]
    pub profile_dir: Option<PathBuf>,
    /// Chromium remote debugging port bound to localhost; `None` disables it.
    #[serde(default)]
    pub devtools_port: Option<u16>,
}

/// Main process → host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostCommand {
    /// Always the first message.
    Init(Box<LaunchParams>),
    Resize {
        width: u32,
        height: u32,
    },
    SetFps {
        fps: u32,
    },
    /// Produce one frame (external pacing).
    BeginFrame,
    SetPaused {
        paused: bool,
    },
    SetAudio(AudioOutput),
    /// Changed user properties.
    Properties {
        changes: Vec<UserProperty>,
    },
    /// 128 values, see [`crate::audio`].
    Audio {
        values: Vec<f32>,
    },
    Media(MediaEvent),
    /// A `fetchall` directory changed on disk.
    RescanDirectories,
    Mouse(MouseInput),
    /// The main process finished copying frame `id`; the host may let the browser reuse it.
    FrameReleased {
        id: u64,
    },
    Reload,
    Shutdown,
}

/// Host → main process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostEvent {
    /// The browser exists; `pid` is the host process id (exclude it from automute detection).
    Started {
        pid: u32,
    },
    /// A frame is ready; the host blocks until [`HostCommand::FrameReleased`] (or a timeout).
    Frame(FrameInfo),
    /// The host stopped waiting for the release of frame `id`: do not import it any more.
    FrameRevoked {
        id: u64,
    },
    Loaded {
        url: String,
        status: i32,
    },
    LoadFailed {
        url: String,
        error: String,
    },
    AudioListener {
        active: bool,
    },
    MediaListeners {
        listeners: MediaListeners,
    },
    Error {
        message: String,
    },
}

/// Encodes one envelope into `out` (cleared first).
pub fn encode<T: Serialize>(
    message: &T,
    handle_count: u8,
    out: &mut Vec<u8>,
) -> std::io::Result<()> {
    out.clear();
    out.extend_from_slice(&[0; HEADER_LEN]);
    serde_json::to_writer(&mut *out, message).map_err(std::io::Error::other)?;
    let len = out.len() - HEADER_LEN;
    if len > MAX_MESSAGE_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    out[..4].copy_from_slice(&(len as u32).to_le_bytes());
    out[4] = handle_count;
    Ok(())
}

/// Parses an envelope header: (json length, handle count).
pub fn decode_header(header: &[u8; HEADER_LEN]) -> std::io::Result<(usize, u8)> {
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if len > MAX_MESSAGE_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    Ok((len, header[4]))
}

/// Decodes a message body.
pub fn decode<T: for<'de> Deserialize<'de>>(json: &[u8]) -> std::io::Result<T> {
    serde_json::from_slice(json)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::PlaybackState;
    use crate::mouse::MouseButton;
    use crate::properties::PropertyValue;

    fn roundtrip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(m: T) {
        let mut buf = Vec::new();
        encode(&m, 0, &mut buf).unwrap();
        let header: [u8; HEADER_LEN] = buf[..HEADER_LEN].try_into().unwrap();
        let (len, n) = decode_header(&header).unwrap();
        assert_eq!((len, n), (buf.len() - HEADER_LEN, 0));
        let back: T = decode(&buf[HEADER_LEN..]).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn commands_roundtrip() {
        let params = LaunchParams {
            wallpaper_id: "-1".into(),
            root: "/tmp/w".into(),
            file: "index.html".into(),
            width: 1920,
            height: 1080,
            fps: 30,
            pacing: FramePacing::External,
            transport: FrameTransport::Memory,
            properties: PropertySet::new(),
            audio: AudioOutput::default(),
            paused: false,
            profile_dir: None,
            devtools_port: Some(8080),
        };
        roundtrip(HostCommand::Init(Box::new(params)));
        roundtrip(HostCommand::BeginFrame);
        roundtrip(HostCommand::Properties {
            changes: vec![UserProperty::new("a", PropertyValue::Bool(true))],
        });
        roundtrip(HostCommand::Audio {
            values: vec![0.5; 128],
        });
        roundtrip(HostCommand::Media(MediaEvent::Playback {
            state: PlaybackState::Paused,
        }));
        roundtrip(HostCommand::Mouse(MouseInput::Button {
            x: 1,
            y: 2,
            button: MouseButton::Right,
            down: true,
        }));
        roundtrip(HostCommand::FrameReleased { id: 9 });
    }

    #[test]
    fn events_roundtrip() {
        roundtrip(HostEvent::Started { pid: 42 });
        roundtrip(HostEvent::Frame(FrameInfo {
            id: 1,
            width: 1920,
            height: 1080,
            visible: [0, 0, 1920, 1080],
            format: PixelFormat::Bgra8,
            modifier: 0,
            planes: vec![PlaneInfo {
                offset: 0,
                stride: 7680,
                size: 7680 * 1080,
            }],
            handle: 0,
            memory: false,
        }));
        roundtrip(HostEvent::MediaListeners {
            listeners: MediaListeners::default(),
        });
    }

    #[test]
    fn rejects_oversized_header() {
        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_header(&header).is_err());
    }
}

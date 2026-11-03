//! "Another application is playing" detection (automute, spec 07 §12, §22).
//!
//! The reference queries the stream list synchronously every frame; Lumenwall subscribes to
//! sound-server events on a background thread instead (PARITY-POLICY rule 1). The result is a
//! flag read with one atomic load by [`AutomuteDetector::update`], plus an optional callback
//! fired on each change so a player can react while nothing is rendered.
//!
//! Every backend describes its streams as a [`Stream`] and asks [`classify`] whether it is
//! media being played. Only music, video and games count: calls, notification and
//! accessibility sounds, paused, muted or silent streams and Lumenwall's own streams do not
//! (PARITY-POLICY rule 5, see the README's deviations).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub mod pipewire;
#[cfg(all(target_os = "linux", feature = "pulse"))]
pub mod pulse;
#[cfg(all(windows, feature = "wasapi"))]
pub mod windows;

/// `true` if an `application.process.id` value designates this process.
#[cfg(all(target_os = "linux", any(feature = "pulse", feature = "pipewire")))]
pub(crate) fn is_own_pid(value: &str, own: u32) -> bool {
    value.trim().parse::<u32>() == Ok(own)
}

/// One playback stream as the sound server describes it: a PulseAudio sink input, a PipeWire
/// `Stream/Output/Audio` node or a Windows audio session.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stream<'a> {
    /// The stream belongs to this process.
    pub ours: bool,
    /// The server names the owning process (streams without one are server internals).
    pub has_owner: bool,
    /// `application.name` (Windows: the session's display name).
    pub app_name: Option<&'a str>,
    /// `application.process.binary` (Windows: the executable's file name).
    pub binary: Option<&'a str>,
    /// `media.role` (`music`, `video`, `game`, `phone`, `event`… or PipeWire's `Music`,
    /// `Movie`, `Communication`, `Notification`…). Windows: `communication` for a session the
    /// system reported as a communications stream.
    pub role: Option<&'a str>,
    /// Corked (PulseAudio), node not running (PipeWire), session inactive (Windows).
    pub paused: bool,
    pub muted: bool,
    /// Average linear gain of the stream, 0 to 1 (or more when amplified).
    pub volume: f32,
}

/// Why a stream counts or not, for the logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Music, video or a game being played: wallpaper sounds pause.
    Media,
    /// This process.
    Ours,
    /// No owning process named by the server.
    NoOwner,
    /// Another Lumenwall process (second engine, settings window).
    Lumenwall,
    Paused,
    /// Muted or below −60 dB.
    Silent,
    /// Voice or video call (communication role or a call program).
    Call,
    /// Speech synthesis and other accessibility output.
    Accessibility,
    /// A role other than music, video, game or production (notifications, alarms, tests…).
    NotMedia,
}

/// Below this linear gain (−60 dB) a stream is inaudible.
const SILENT_GAIN: f32 = 1e-3;

/// Media roles that count; any other declared role does not.
const MEDIA_ROLES: &[&str] = &["music", "video", "movie", "game", "production"];

/// Call roles (PulseAudio `phone`, PipeWire `Communication`).
const CALL_ROLES: &[&str] = &["phone", "communication"];

/// Voice and video call programs, by binary or application name (ASCII case-insensitive,
/// `.exe` ignored). Most do not declare a role: Discord's voice stream is an untagged
/// `WEBRTC VoiceEngine` stream of the `Discord` binary.
const CALL_APPS: &[&str] = &[
    "vesktop",
    "webcord",
    "armcord",
    "legcord",
    "zoom",
    "zoom.real",
    "teams",
    "ms-teams",
    "teams-for-linux",
    "skype",
    "skypeforlinux",
    "mumble",
    "linphone",
    "jami",
    "jami-qt",
    "baresip",
    "zoiper",
    "zoiper5",
    "element",
    "element-desktop",
    "slack",
    "signal",
    "signal-desktop",
    "jitsi-meet",
    "webex",
    "ciscocollabhost",
];

/// Prefixes of call programs with several builds (`DiscordPTB`, `ts3client_linux_amd64`).
const CALL_APP_PREFIXES: &[&str] = &["discord", "ts3client", "teamspeak"];

/// Speech synthesis (screen readers, browsers' text to speech).
const ACCESSIBILITY_APPS: &[&str] = &["orca", "espeak", "espeak-ng", "festival", "rhvoice"];

fn stem(name: &str) -> &str {
    let name = name.trim();
    let len = name.len();
    if len > 4 && name.is_char_boundary(len - 4) && name[len - 4..].eq_ignore_ascii_case(".exe") {
        &name[..len - 4]
    } else {
        name
    }
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn contains_ignore_case(s: &str, needle: &str) -> bool {
    s.as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

fn names_match(s: &Stream<'_>, f: impl Fn(&str) -> bool) -> bool {
    s.app_name.map(stem).is_some_and(&f) || s.binary.map(stem).is_some_and(&f)
}

fn is_call_app(name: &str) -> bool {
    CALL_APPS.iter().any(|a| name.eq_ignore_ascii_case(a))
        || CALL_APP_PREFIXES
            .iter()
            .any(|p| starts_with_ignore_case(name, p))
}

fn is_accessibility_app(name: &str) -> bool {
    ACCESSIBILITY_APPS
        .iter()
        .any(|a| name.eq_ignore_ascii_case(a))
        || starts_with_ignore_case(name, "speech-dispatcher")
        // Speech Dispatcher's output modules: sd_dummy, sd_espeak-ng, sd_rhvoice…
        || starts_with_ignore_case(name, "sd_")
}

/// Whether a stream is media being played by another application (spec 07 §12.1 rule 3,
/// narrowed to real media playback).
pub fn classify(s: &Stream<'_>) -> Verdict {
    let role = s.role.map(str::trim).filter(|r| !r.is_empty());
    let media_role = role.is_some_and(|r| MEDIA_ROLES.iter().any(|m| r.eq_ignore_ascii_case(m)));
    if s.ours {
        Verdict::Ours
    } else if names_match(s, |n| contains_ignore_case(n, "lumenwall")) {
        // Also through the ALSA plug-in: `PipeWire ALSA [lumenwall]`, without a process id.
        Verdict::Lumenwall
    } else if !s.has_owner && !media_role {
        // parity: streams without an `application.process.id` do not count (§12.1), except
        // those that declare a media role: Spotify's stream has none but says `music`.
        Verdict::NoOwner
    } else if s.paused {
        Verdict::Paused
    } else if s.muted || s.volume.is_nan() || s.volume < SILENT_GAIN {
        Verdict::Silent
    } else if role.is_some_and(|r| CALL_ROLES.iter().any(|c| r.eq_ignore_ascii_case(c)))
        || names_match(s, is_call_app)
        || s.app_name
            .is_some_and(|n| contains_ignore_case(n, "webrtc"))
    {
        Verdict::Call
    } else if names_match(s, is_accessibility_app)
        || role.is_some_and(|r| {
            r.eq_ignore_ascii_case("a11y") || r.eq_ignore_ascii_case("accessibility")
        })
    {
        Verdict::Accessibility
    } else if role.is_some() && !media_role {
        Verdict::NotMedia
    } else {
        Verdict::Media
    }
}

/// Executable file name in a Windows audio session identifier
/// (`{0.0.0.00000000}.{guid}|\Device\HarddiskVolume3\…\Discord.exe%b{guid}`); `None` for the
/// system sounds session (`…|#%b{guid}`) or an unexpected layout.
#[cfg(any(windows, test))]
pub(crate) fn exe_from_session_id(id: &str) -> Option<&str> {
    let (_, path) = id.split_once('|')?;
    let path = path.split_once("%b").map_or(path, |(p, _)| p);
    let name = path.rsplit(['\\', '/']).next()?.trim();
    (!name.is_empty() && name != "#").then_some(name)
}

/// Short description of a stream for the logs: `'Spotify' (spotify)`.
pub fn describe(s: &Stream<'_>) -> String {
    match (s.app_name, s.binary) {
        (Some(a), Some(b)) if !a.eq_ignore_ascii_case(b) => format!("'{a}' ({b})"),
        (Some(a), _) => format!("'{a}'"),
        (None, Some(b)) => format!("'{b}'"),
        (None, None) => "a stream".to_owned(),
    }
}

/// Automute settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutomuteSettings {
    /// `false` with `--noautomute`: neutral detector, never "playing", no server connection.
    pub enabled: bool,
}

impl Default for AutomuteSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Callback fired (on a backend thread) when the "another application plays" state changes.
pub type ChangeCallback = Arc<dyn Fn(bool) + Send + Sync>;

/// Event-driven source of the "another application plays" state.
pub trait PlaybackWatcher: Send {
    /// Latest state from server events; never blocks.
    fn other_app_playing(&self) -> bool;
    /// Whether the watcher currently has a server connection.
    fn is_connected(&self) -> bool;
}

/// Shared flag written by watcher backends.
#[derive(Clone)]
pub struct PlayingFlag {
    inner: Arc<FlagInner>,
}

struct FlagInner {
    playing: AtomicBool,
    connected: AtomicBool,
    on_change: Option<ChangeCallback>,
}

impl PlayingFlag {
    /// New flag ("nothing plays", disconnected).
    pub fn new(on_change: Option<ChangeCallback>) -> Self {
        Self {
            inner: Arc::new(FlagInner {
                playing: AtomicBool::new(false),
                connected: AtomicBool::new(false),
                on_change,
            }),
        }
    }

    /// Updates the state; fires the callback on change. Returns whether it changed.
    pub fn set_playing(&self, playing: bool) -> bool {
        let changed = self.inner.playing.swap(playing, Ordering::AcqRel) != playing;
        if changed && let Some(cb) = &self.inner.on_change {
            cb(playing);
        }
        changed
    }

    /// [`set_playing`](Self::set_playing) from a stream list: `media` describes the first
    /// stream that counts. Changes are logged with the stream that caused them.
    pub fn set_media(&self, media: Option<String>) {
        let playing = media.is_some();
        if self.set_playing(playing) {
            match media {
                Some(who) => {
                    tracing::info!("automute: {who} is playing media, wallpaper sounds pause")
                }
                None => tracing::info!("automute: no other media playing, wallpaper sounds resume"),
            }
        }
    }

    /// Updates the connection state. Losing the server means "nothing plays" (§12.2).
    pub fn set_connected(&self, connected: bool) {
        self.inner.connected.store(connected, Ordering::Release);
        if !connected {
            self.set_playing(false);
        }
    }

    /// Current state.
    pub fn playing(&self) -> bool {
        self.inner.playing.load(Ordering::Acquire)
    }

    /// Connection state.
    pub fn connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }
}

/// Automute decision (§12.1).
pub struct AutomuteDetector {
    enabled: bool,
    watcher: Option<Box<dyn PlaybackWatcher>>,
    playing: bool,
}

impl AutomuteDetector {
    /// Builds the detector and, when enabled, starts the platform watcher. Never blocks.
    pub fn new(settings: AutomuteSettings, on_change: Option<ChangeCallback>) -> Self {
        let watcher = if settings.enabled {
            default_watcher(on_change)
        } else {
            None
        };
        Self {
            enabled: settings.enabled,
            watcher,
            playing: false,
        }
    }

    /// Detector using a given watcher (tests, custom backends).
    pub fn with_watcher(
        settings: AutomuteSettings,
        watcher: Option<Box<dyn PlaybackWatcher>>,
    ) -> Self {
        Self {
            enabled: settings.enabled,
            watcher: if settings.enabled { watcher } else { None },
            playing: false,
        }
    }

    /// Recomputes the state; call once per non-paused frame with the fullscreen detector's
    /// answer. Costs one atomic load.
    pub fn update(&mut self, fullscreen: bool) -> bool {
        self.playing = if !self.enabled {
            // `--noautomute`: neutral, even in fullscreen.
            false
        } else if fullscreen {
            // parity: a fullscreen window counts as "another application plays" (§12.1 rule 2).
            true
        } else {
            self.watcher.as_ref().is_some_and(|w| w.other_app_playing())
        };
        self.playing
    }

    /// State computed by the last [`update`](Self::update).
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Whether a sound-server connection is established.
    pub fn is_connected(&self) -> bool {
        self.watcher.as_ref().is_some_and(|w| w.is_connected())
    }
}

/// Starts the best watcher compiled in and available, or `None`.
pub fn default_watcher(on_change: Option<ChangeCallback>) -> Option<Box<dyn PlaybackWatcher>> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        if crate::capture::pipewire::server_available() {
            return Some(Box::new(pipewire::PipeWireWatcher::new(on_change)));
        }
    }
    #[cfg(all(target_os = "linux", feature = "pulse"))]
    {
        return Some(Box::new(pulse::PulseWatcher::new(None, on_change)));
    }
    #[cfg(all(windows, feature = "wasapi"))]
    {
        return Some(Box::new(windows::SessionWatcher::new(on_change)));
    }
    #[allow(unreachable_code)]
    {
        let _ = on_change;
        tracing::info!("automute: no backend compiled in, other applications are never detected");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(bool);
    impl PlaybackWatcher for Fixed {
        fn other_app_playing(&self) -> bool {
            self.0
        }
        fn is_connected(&self) -> bool {
            false
        }
    }

    /// Spec §21.1 test 6: settings without a server.
    #[test]
    fn rules_without_server() {
        let mut d = AutomuteDetector::with_watcher(AutomuteSettings { enabled: true }, None);
        assert!(!d.update(false));
        assert!(d.update(true), "fullscreen forces 'playing'");
        let mut d = AutomuteDetector::with_watcher(
            AutomuteSettings { enabled: false },
            Some(Box::new(Fixed(true))),
        );
        assert!(
            !d.update(true),
            "automute disabled + fullscreen → nothing plays"
        );
        assert!(!d.update(false));
    }

    #[test]
    fn watcher_state_is_used() {
        let mut d = AutomuteDetector::with_watcher(
            AutomuteSettings::default(),
            Some(Box::new(Fixed(true))),
        );
        assert!(d.update(false));
        assert!(d.is_playing());
    }

    fn media(app: &'static str, binary: &'static str) -> Stream<'static> {
        Stream {
            ours: false,
            has_owner: true,
            app_name: Some(app),
            binary: Some(binary),
            role: None,
            paused: false,
            muted: false,
            volume: 1.0,
        }
    }

    #[test]
    fn media_players_count() {
        assert_eq!(classify(&media("Spotify", "spotify")), Verdict::Media);
        assert_eq!(classify(&media("mpv", "mpv")), Verdict::Media);
        assert_eq!(classify(&media("Firefox", "firefox")), Verdict::Media);
        for role in ["music", "Music", "video", "Movie", "game", "Production"] {
            let s = Stream {
                role: Some(role),
                ..media("Player", "player")
            };
            assert_eq!(classify(&s), Verdict::Media, "{role}");
        }
        // Windows sessions: executable names.
        assert_eq!(classify(&media("", "vlc.exe")), Verdict::Media);
    }

    #[test]
    fn calls_do_not_count() {
        // Discord's voice stream as pipewire-pulse lists it (no media.role).
        assert_eq!(
            classify(&media("WEBRTC VoiceEngine", "Discord")),
            Verdict::Call
        );
        assert_eq!(classify(&media("Chromium", "DiscordPTB")), Verdict::Call);
        assert_eq!(classify(&media("Discord", "Discord.exe")), Verdict::Call);
        assert_eq!(classify(&media("", "ts3client_linux_amd64")), Verdict::Call);
        assert_eq!(classify(&media("Teams", "ms-teams.exe")), Verdict::Call);
        for role in ["phone", "Communication", " communication "] {
            let s = Stream {
                role: Some(role),
                ..media("Firefox", "firefox")
            };
            assert_eq!(classify(&s), Verdict::Call, "{role}");
        }
    }

    #[test]
    fn other_sounds_do_not_count() {
        // Speech Dispatcher keeps an idle stream open (seen on GNOME with Chromium).
        assert_eq!(
            classify(&media("speech-dispatcher-dummy", "sd_dummy")),
            Verdict::Accessibility
        );
        assert_eq!(classify(&media("orca", "orca")), Verdict::Accessibility);
        for role in ["event", "Notification", "test", "alarm", "a11y"] {
            let s = Stream {
                role: Some(role),
                ..media("GNOME Shell", "gnome-shell")
            };
            assert_ne!(classify(&s), Verdict::Media, "{role}");
        }
        // Empty role: treated as absent.
        let s = Stream {
            role: Some(""),
            ..media("Spotify", "spotify")
        };
        assert_eq!(classify(&s), Verdict::Media);
    }

    #[test]
    fn idle_and_own_streams_do_not_count() {
        let base = media("Spotify", "spotify");
        let paused = Stream {
            paused: true,
            ..base
        };
        assert_eq!(classify(&paused), Verdict::Paused);
        let muted = Stream {
            muted: true,
            ..base
        };
        assert_eq!(classify(&muted), Verdict::Silent);
        for volume in [0.0, 0.0005, f32::NAN] {
            let s = Stream { volume, ..base };
            assert_eq!(classify(&s), Verdict::Silent, "{volume}");
        }
        let ours = Stream { ours: true, ..base };
        assert_eq!(classify(&ours), Verdict::Ours);
        let orphan = Stream {
            has_owner: false,
            ..base
        };
        assert_eq!(classify(&orphan), Verdict::NoOwner);
        // Seen live: Spotify's stream has no process id but declares `music`.
        let spotify = Stream {
            has_owner: false,
            binary: None,
            role: Some("music"),
            ..base
        };
        assert_eq!(classify(&spotify), Verdict::Media);
        assert_eq!(
            classify(&Stream {
                paused: true,
                ..spotify
            }),
            Verdict::Paused
        );
        // Seen live: Lumenwall's own output through the ALSA plug-in, without a process id.
        let alsa = Stream {
            has_owner: false,
            app_name: Some("PipeWire ALSA [lumenwall]"),
            binary: None,
            role: Some("music"),
            ..base
        };
        assert_eq!(classify(&alsa), Verdict::Lumenwall);
        assert_eq!(
            classify(&media("lumenwall", "lumenwall")),
            Verdict::Lumenwall
        );
        assert_eq!(
            classify(&media("ALSA plug-in [lumenwall-ui]", "lumenwall-ui")),
            Verdict::Lumenwall
        );
        assert_eq!(classify(&media("", "Lumenwall.exe")), Verdict::Lumenwall);
    }

    #[test]
    fn windows_session_identifiers() {
        assert_eq!(
            exe_from_session_id(
                r"{0.0.0.00000000}.{5e0c8a0e-1111-2222-3333-444455556666}|\Device\HarddiskVolume3\Users\me\AppData\Local\Discord\app-1.0.9\Discord.exe%b{00000000-0000-0000-0000-000000000000}"
            ),
            Some("Discord.exe")
        );
        assert_eq!(
            exe_from_session_id(
                r"{0.0.0.00000000}.{5e0c8a0e-1111-2222-3333-444455556666}|#%b{A9EF3FD9-4240-455E-A4D5-F2B3301887B2}"
            ),
            None,
            "system sounds"
        );
        assert_eq!(exe_from_session_id("no separator"), None);
        let s = Stream {
            binary: exe_from_session_id(r"x|\Device\HarddiskVolume1\Discord.exe%b{0}"),
            app_name: None,
            ..media("", "")
        };
        assert_eq!(classify(&s), Verdict::Call);
    }

    #[test]
    fn descriptions() {
        assert_eq!(describe(&media("Spotify", "spotify")), "'Spotify'");
        assert_eq!(
            describe(&media("WEBRTC VoiceEngine", "Discord")),
            "'WEBRTC VoiceEngine' (Discord)"
        );
        assert_eq!(describe(&Stream::default()), "a stream");
    }

    #[test]
    fn flag_fires_on_change_only() {
        use std::sync::atomic::AtomicUsize;
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let flag = PlayingFlag::new(Some(Arc::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        })));
        flag.set_playing(false);
        flag.set_playing(true);
        flag.set_playing(true);
        flag.set_connected(false);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert!(!flag.playing());
    }
}

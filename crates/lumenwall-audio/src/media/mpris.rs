//! MPRIS interpretation (spec 07 §16.2–16.3), kept apart from the D-Bus library so that it is
//! tested on every system.
// Elsewhere than on Linux, this module is only compiled for its own tests.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::PathBuf;

use super::PlaybackState;

/// Bus names of MPRIS players start with this.
pub(crate) const NAME_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// Object every player exports.
pub(crate) const PATH: &str = "/org/mpris/MediaPlayer2";
/// Interface of the playback properties.
pub(crate) const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// A D-Bus value reduced to what MPRIS uses.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Prop {
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A string or an object path.
    Str(String),
    Strs(Vec<String>),
    Map(Vec<(String, Prop)>),
    Other,
}

impl Prop {
    fn text(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// A string, or the first element of a string list.
    fn first_text(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            Self::Strs(l) => l.first().map(String::as_str),
            _ => None,
        }
    }

    fn joined(&self) -> Option<String> {
        match self {
            Self::Str(s) => Some(s.clone()),
            Self::Strs(l) => Some(l.join(",")),
            _ => None,
        }
    }

    fn int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Float(f) if f.is_finite() => Some(*f as i64),
            _ => None,
        }
    }
}

/// What one player reports through `org.mpris.MediaPlayer2.Player`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PlayerProps {
    pub state: PlaybackState,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    /// Comma-separated.
    pub genres: String,
    /// Microseconds; 0 when unknown.
    pub length_us: i64,
    pub art_url: String,
    /// Microseconds, as last read or signalled.
    pub position_us: i64,
}

/// What [`PlayerProps::apply`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    pub status: bool,
    pub metadata: bool,
    pub position: bool,
}

impl PlayerProps {
    /// Applies Player properties (a `GetAll` reply or the `changed` part of
    /// `PropertiesChanged`).
    pub(crate) fn apply(&mut self, props: &[(String, Prop)]) -> Changes {
        let mut c = Changes::default();
        for (name, value) in props {
            match name.as_str() {
                "PlaybackStatus" => {
                    if let Some(s) = value.text() {
                        let state = playback_state(s);
                        c.status |= state != self.state;
                        self.state = state;
                    }
                }
                "Metadata" => {
                    if let Prop::Map(m) = value {
                        let before = self.clone();
                        self.metadata(m);
                        c.metadata |= *self != before;
                    }
                }
                "Position" => {
                    if let Some(p) = value.int() {
                        c.position |= p != self.position_us;
                        self.position_us = p;
                    }
                }
                _ => {}
            }
        }
        c
    }

    /// `Metadata` replaces the whole track description: missing keys become empty.
    fn metadata(&mut self, map: &[(String, Prop)]) {
        let get = |key: &str| map.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        let text = |key: &str| get(key).and_then(Prop::text).unwrap_or("").to_owned();
        // Spec §16.3: the first artist only.
        let first = |key: &str| get(key).and_then(Prop::first_text).unwrap_or("").to_owned();
        self.title = text("xesam:title");
        self.artist = first("xesam:artist");
        self.album = text("xesam:album");
        self.album_artist = first("xesam:albumArtist");
        self.genres = get("xesam:genre")
            .and_then(Prop::joined)
            .unwrap_or_default();
        self.length_us = get("mpris:length").and_then(Prop::int).unwrap_or(0).max(0);
        self.art_url = text("mpris:artUrl");
    }
}

/// `PlaybackStatus`: `Playing`, `Paused`, anything else is stopped (§16.3).
pub(crate) fn playback_state(status: &str) -> PlaybackState {
    match status {
        "Playing" => PlaybackState::Playing,
        "Paused" => PlaybackState::Paused,
        _ => PlaybackState::Stopped,
    }
}

/// Whether a bus name is an MPRIS player.
pub(crate) fn is_player(name: &str) -> bool {
    name.len() > NAME_PREFIX.len() && name.starts_with(NAME_PREFIX)
}

/// The player to follow among `states` (in bus order): the first playing one (§16.2), else the
/// first paused one, else the first. The reference keeps no player until one plays; keeping a
/// paused player's track is what Windows' current media session does.
pub(crate) fn pick(states: &[PlaybackState]) -> Option<usize> {
    states
        .iter()
        .position(|s| *s == PlaybackState::Playing)
        .or_else(|| states.iter().position(|s| *s == PlaybackState::Paused))
        .or(if states.is_empty() { None } else { Some(0) })
}

/// The local file behind a `file://` URL (percent-escapes decoded; `localhost` or no host).
/// `None` for other schemes (artwork on the network is not downloaded) and other hosts.
pub(crate) fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else {
        let (host, path) = rest.split_at(rest.find('/')?);
        if !host.eq_ignore_ascii_case("localhost") {
            return None;
        }
        path
    };
    Some(path_from_bytes(percent_decode(path)))
}

fn percent_decode(input: &str) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Prop {
        Prop::Str(v.into())
    }

    fn metadata(entries: &[(&str, Prop)]) -> Prop {
        Prop::Map(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn metadata_follows_the_spec() {
        let mut p = PlayerProps::default();
        let c = p.apply(&[
            ("PlaybackStatus".into(), s("Playing")),
            (
                "Metadata".into(),
                metadata(&[
                    ("xesam:title", s("Soren")),
                    ("xesam:artist", Prop::Strs(vec!["A".into(), "B".into()])),
                    ("xesam:album", s("Album")),
                    ("xesam:albumArtist", Prop::Strs(vec!["AA".into()])),
                    ("xesam:genre", Prop::Strs(vec!["rock".into(), "pop".into()])),
                    ("mpris:length", Prop::Int(215_000_000)),
                    ("mpris:artUrl", s("file:///tmp/cover%20art.png")),
                    ("mpris:trackid", s("/org/x/1")),
                ]),
            ),
            ("Position".into(), Prop::Int(12_500_000)),
        ]);
        assert_eq!(
            c,
            Changes {
                status: true,
                metadata: true,
                position: true
            }
        );
        assert_eq!(p.state, PlaybackState::Playing);
        assert_eq!(p.title, "Soren");
        assert_eq!(p.artist, "A", "first artist only (§16.3)");
        assert_eq!(p.album, "Album");
        assert_eq!(p.album_artist, "AA");
        assert_eq!(p.genres, "rock,pop");
        assert_eq!(p.length_us, 215_000_000);
        assert_eq!(p.position_us, 12_500_000);
        assert_eq!(
            file_url_path(&p.art_url).unwrap(),
            PathBuf::from("/tmp/cover art.png")
        );

        // Same metadata again: nothing changed. A new track replaces every field.
        let again = p.clone();
        assert!(p.apply(&[("Metadata".into(), metadata(&[]))]).metadata);
        assert_eq!(p.title, "");
        assert_eq!(p.length_us, 0);
        let mut q = again.clone();
        let c = q.apply(&[("PlaybackStatus".into(), s("Paused"))]);
        assert!(c.status && !c.metadata);
        assert_eq!(q.state, PlaybackState::Paused);
        q.apply(&[("PlaybackStatus".into(), s("Stopped"))]);
        assert_eq!(q.state, PlaybackState::Stopped);
        // Unknown properties and wrong types are ignored.
        let c = q.apply(&[
            ("Volume".into(), Prop::Float(0.5)),
            ("PlaybackStatus".into(), Prop::Int(3)),
        ]);
        assert_eq!(c, Changes::default());
    }

    #[test]
    fn players_are_picked_like_the_reference_then_paused_ones() {
        use PlaybackState::*;
        assert_eq!(pick(&[]), None);
        assert_eq!(pick(&[Paused, Playing, Playing]), Some(1));
        assert_eq!(pick(&[Stopped, Paused]), Some(1));
        assert_eq!(pick(&[Stopped, Stopped]), Some(0));
        assert!(is_player("org.mpris.MediaPlayer2.spotify"));
        assert!(!is_player("org.mpris.MediaPlayer2."));
        assert!(!is_player("org.freedesktop.UPower"));
    }

    #[test]
    fn only_local_artwork_urls_are_read() {
        assert_eq!(
            file_url_path("file://localhost/a/b.jpg"),
            Some(PathBuf::from("/a/b.jpg"))
        );
        assert_eq!(file_url_path("file://example.org/a.jpg"), None);
        assert_eq!(file_url_path("https://i.scdn.co/image/ab67"), None);
        assert_eq!(file_url_path(""), None);
        assert_eq!(
            file_url_path("file:///a/%e2%9c%93%zz"),
            Some(PathBuf::from("/a/\u{2713}%zz"))
        );
    }
}

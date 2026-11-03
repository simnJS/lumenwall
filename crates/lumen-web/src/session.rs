//! Per-page state of the Wallpaper Engine API, independent of the browser engine.
//!
//! The host feeds it commands from the main process and messages from the page; it answers with
//! scripts to evaluate in the page and notifications for the main process.

use std::collections::BTreeMap;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::audio::AUDIO_FRAME_LEN;
use crate::bridge::{self, ListenTarget, PageMessage};
use crate::media::{MediaEvent, MediaListeners};
use crate::properties::{
    DirectoryFileKind, DirectoryMode, GeneralProperties, PropertySet, PropertyValue, UserProperty,
    file_url_suffix,
};

/// Output of the session.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEffect {
    /// Evaluate this script in the page's main frame.
    Script(String),
    /// The page registered or cleared its audio listener: start/stop sending audio frames.
    AudioListener(bool),
    /// The set of registered media listeners changed: start/stop sending media events.
    MediaListeners(MediaListeners),
}

type Listing = BTreeMap<PathBuf, Option<SystemTime>>;

/// State of the Wallpaper Engine web API for one page.
#[derive(Debug)]
pub struct BridgeSession {
    properties: PropertySet,
    general: GeneralProperties,
    paused: bool,
    page_ready: bool,
    audio_listener: bool,
    media_listeners: MediaListeners,
    media_last: [Option<MediaEvent>; 5],
    fetchall: BTreeMap<String, Listing>,
    rng: u64,
    audio_buf: String,
}

impl BridgeSession {
    pub fn new(properties: PropertySet, general: GeneralProperties) -> Self {
        let seed = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
            | 1;
        Self {
            properties,
            general,
            paused: false,
            page_ready: false,
            audio_listener: false,
            media_listeners: MediaListeners::default(),
            media_last: Default::default(),
            fetchall: BTreeMap::new(),
            rng: seed,
            audio_buf: String::new(),
        }
    }

    pub fn properties(&self) -> &PropertySet {
        &self.properties
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn audio_listener(&self) -> bool {
        self.audio_listener
    }

    pub fn media_listeners(&self) -> MediaListeners {
        self.media_listeners
    }

    /// Handles a raw message posted by the bridge.
    pub fn page_message(&mut self, json: &str, out: &mut Vec<SessionEffect>) {
        let Some(message) = PageMessage::parse(json) else {
            tracing::debug!("ignoring malformed bridge message");
            return;
        };
        match message {
            PageMessage::Ready => self.on_ready(out),
            PageMessage::Listen {
                target: ListenTarget::Audio,
                on,
            } => {
                if self.audio_listener != on {
                    self.audio_listener = on;
                    out.push(SessionEffect::AudioListener(on));
                }
            }
            PageMessage::Listen {
                target: ListenTarget::Media(kind),
                on,
            } => {
                let before = self.media_listeners;
                self.media_listeners.set(kind, on);
                if before != self.media_listeners {
                    out.push(SessionEffect::MediaListeners(self.media_listeners));
                }
                // Replay the current state so a late listener is not left empty.
                if on && let Some(event) = &self.media_last[kind as usize] {
                    out.push(SessionEffect::Script(bridge::media_script(event)));
                }
            }
            PageMessage::RandomFile { id, name } => {
                let file = self.random_file(&name).unwrap_or_default();
                out.push(SessionEffect::Script(bridge::random_file_script(
                    id, &name, &file,
                )));
            }
        }
    }

    fn on_ready(&mut self, out: &mut Vec<SessionEffect>) {
        // A new JavaScript context: listeners of the previous document are gone.
        self.reset_listeners(out);
        self.page_ready = true;
        out.push(SessionEffect::Script(bridge::user_properties_script(
            PropertySet::to_we_json(self.properties.iter()),
        )));
        out.push(SessionEffect::Script(bridge::general_properties_script(
            self.general.to_we_json(),
        )));
        if self.paused {
            out.push(SessionEffect::Script(bridge::paused_script(true)));
        }
        self.fetchall.clear();
        let keys: Vec<String> = self.fetchall_properties().map(|p| p.key.clone()).collect();
        for key in keys {
            self.sync_directory(&key, out);
        }
    }

    /// The page's JavaScript context was destroyed (navigation, reload, crash).
    pub fn context_released(&mut self, out: &mut Vec<SessionEffect>) {
        self.page_ready = false;
        self.reset_listeners(out);
    }

    fn reset_listeners(&mut self, out: &mut Vec<SessionEffect>) {
        if self.audio_listener {
            self.audio_listener = false;
            out.push(SessionEffect::AudioListener(false));
        }
        if !self.media_listeners.is_empty() {
            self.media_listeners = MediaListeners::default();
            out.push(SessionEffect::MediaListeners(self.media_listeners));
        }
    }

    /// New values for some user properties (only changed ones reach the page).
    pub fn set_properties(&mut self, changes: Vec<UserProperty>, out: &mut Vec<SessionEffect>) {
        let changed = self.properties.merge(changes);
        if changed.is_empty() || !self.page_ready {
            return;
        }
        out.push(SessionEffect::Script(bridge::user_properties_script(
            PropertySet::to_we_json(&changed),
        )));
        for property in &changed {
            if matches!(
                property.value,
                PropertyValue::Directory {
                    mode: DirectoryMode::Fetchall,
                    ..
                }
            ) {
                self.sync_directory(&property.key, out);
            }
        }
    }

    /// New frame-rate limit (`applyGeneralProperties({ fps })`).
    pub fn set_fps(&mut self, fps: u32, out: &mut Vec<SessionEffect>) {
        if self.general.fps == fps {
            return;
        }
        self.general.fps = fps;
        if self.page_ready {
            out.push(SessionEffect::Script(bridge::general_properties_script(
                self.general.to_we_json(),
            )));
        }
    }

    /// Pause state (`setPaused`).
    pub fn set_paused(&mut self, paused: bool, out: &mut Vec<SessionEffect>) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        if self.page_ready {
            out.push(SessionEffect::Script(bridge::paused_script(paused)));
        }
    }

    /// One audio frame; the returned script borrows an internal buffer reused across calls.
    pub fn audio(&mut self, frame: &[f32; AUDIO_FRAME_LEN]) -> Option<&str> {
        if !self.page_ready || !self.audio_listener || self.paused {
            return None;
        }
        bridge::audio_script_into(frame, &mut self.audio_buf);
        Some(&self.audio_buf)
    }

    /// Media state change; forwarded when the page listens for it, remembered otherwise.
    pub fn media(&mut self, event: MediaEvent, out: &mut Vec<SessionEffect>) {
        let kind = event.kind();
        if self.page_ready && self.media_listeners.contains(kind) {
            out.push(SessionEffect::Script(bridge::media_script(&event)));
        }
        self.media_last[kind as usize] = Some(event);
    }

    /// Re-lists `fetchall` directories (call when the file system reports a change).
    pub fn rescan_directories(&mut self, out: &mut Vec<SessionEffect>) {
        if !self.page_ready {
            return;
        }
        let keys: Vec<String> = self.fetchall_properties().map(|p| p.key.clone()).collect();
        for key in keys {
            self.sync_directory(&key, out);
        }
    }

    /// Directories the page may read through `file://` URLs, plus individual files.
    pub fn local_file_access(&self, wallpaper_root: &Path) -> crate::access::LocalFileAccess {
        crate::access::LocalFileAccess::from_properties(wallpaper_root, &self.properties)
    }

    fn fetchall_properties(&self) -> impl Iterator<Item = &UserProperty> {
        self.properties.iter().filter(|p| {
            matches!(
                p.value,
                PropertyValue::Directory {
                    mode: DirectoryMode::Fetchall,
                    ..
                }
            )
        })
    }

    fn sync_directory(&mut self, key: &str, out: &mut Vec<SessionEffect>) {
        let Some(PropertyValue::Directory { path, kind, .. }) =
            self.properties.get(key).map(|p| &p.value)
        else {
            return;
        };
        let current = list_directory(Path::new(path), *kind);
        let previous = self.fetchall.remove(key).unwrap_or_default();
        let removed: Vec<String> = previous
            .keys()
            .filter(|p| !current.contains_key(*p))
            .map(|p| file_url_suffix(&p.to_string_lossy()))
            .collect();
        let changed: Vec<String> = current
            .iter()
            .filter(|(p, mtime)| previous.get(*p) != Some(*mtime))
            .map(|(p, _)| file_url_suffix(&p.to_string_lossy()))
            .collect();
        if !removed.is_empty() {
            out.push(SessionEffect::Script(bridge::directory_script(
                false, key, &removed,
            )));
        }
        if !changed.is_empty() {
            out.push(SessionEffect::Script(bridge::directory_script(
                true, key, &changed,
            )));
        }
        self.fetchall.insert(key.to_owned(), current);
    }

    fn random_file(&mut self, key: &str) -> Option<String> {
        let PropertyValue::Directory { path, kind, .. } = &self.properties.get(key)?.value else {
            return None;
        };
        let files = list_directory(Path::new(path), *kind);
        if files.is_empty() {
            return None;
        }
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let r = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let index = (r % files.len() as u64) as usize;
        files
            .keys()
            .nth(index)
            .map(|p| file_url_suffix(&p.to_string_lossy()))
    }
}

/// Regular files of `dir` (not recursive) accepted by `kind`, with modification times.
fn list_directory(dir: &Path, kind: DirectoryFileKind) -> Listing {
    let mut out = Listing::new();
    if dir.as_os_str().is_empty() {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_file() && kind.accepts(&path) {
            out.insert(path, meta.modified().ok());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{MediaKind, PlaybackState};
    use serde_json::json;

    fn scripts(effects: &[SessionEffect]) -> Vec<&str> {
        effects
            .iter()
            .filter_map(|e| match e {
                SessionEffect::Script(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The message object of a dispatch script.
    fn msg(script: &str) -> serde_json::Value {
        let body = script
            .strip_prefix("window.__lumenWE&&window.__lumenWE(")
            .and_then(|s| s.strip_suffix(')'))
            .expect("dispatch script");
        serde_json::from_str(body).unwrap()
    }

    fn session(props: serde_json::Value) -> BridgeSession {
        BridgeSession::new(
            PropertySet::from_project_properties(&props),
            GeneralProperties { fps: 30 },
        )
    }

    #[test]
    fn ready_sends_full_state() {
        let mut s = session(json!({ "a": { "type": "bool", "value": true } }));
        let mut out = Vec::new();
        s.set_paused(true, &mut out);
        assert!(out.is_empty(), "nothing is sent before the bridge is ready");
        s.page_message(r#"{"t":"ready"}"#, &mut out);
        let sc = scripts(&out);
        assert_eq!(sc.len(), 3);
        assert_eq!(
            msg(sc[0]),
            json!({ "t": "user", "p": { "a": { "type": "bool", "value": true } } })
        );
        assert_eq!(msg(sc[1]), json!({ "t": "general", "p": { "fps": 30 } }));
        assert_eq!(msg(sc[2]), json!({ "t": "paused", "v": true }));
    }

    #[test]
    fn listeners_drive_audio_and_media() {
        let mut s = session(json!({}));
        let mut out = Vec::new();
        s.page_message(r#"{"t":"ready"}"#, &mut out);
        assert!(s.audio(&[0.0; AUDIO_FRAME_LEN]).is_none());
        out.clear();
        s.page_message(r#"{"t":"listen","k":"audio","on":true}"#, &mut out);
        assert_eq!(out, [SessionEffect::AudioListener(true)]);
        assert!(s.audio(&[0.25; AUDIO_FRAME_LEN]).unwrap().contains("0.25"));
        out.clear();
        s.set_paused(true, &mut out);
        assert!(s.audio(&[0.25; AUDIO_FRAME_LEN]).is_none());

        out.clear();
        s.media(
            MediaEvent::Playback {
                state: PlaybackState::Playing,
            },
            &mut out,
        );
        assert!(out.is_empty(), "no listener yet");
        s.page_message(r#"{"t":"listen","k":"playback","on":true}"#, &mut out);
        assert!(
            matches!(out[0], SessionEffect::MediaListeners(l) if l.contains(MediaKind::Playback))
        );
        assert_eq!(
            msg(scripts(&out)[0]),
            json!({ "t": "media", "k": "playback", "e": { "state": 1 } }),
            "current state replayed"
        );

        out.clear();
        s.context_released(&mut out);
        assert!(out.contains(&SessionEffect::AudioListener(false)));
        assert!(out.contains(&SessionEffect::MediaListeners(MediaListeners::default())));
    }

    #[test]
    fn property_changes_only_send_deltas() {
        let mut s = session(json!({
            "a": { "type": "slider", "value": 1 },
            "b": { "type": "bool", "value": false }
        }));
        let mut out = Vec::new();
        s.page_message(r#"{"t":"ready"}"#, &mut out);
        out.clear();
        s.set_properties(
            vec![
                UserProperty::new("a", PropertyValue::Slider(1.0)),
                UserProperty::new("b", PropertyValue::Bool(true)),
            ],
            &mut out,
        );
        let sc = scripts(&out);
        assert_eq!(sc.len(), 1);
        assert_eq!(
            msg(sc[0]),
            json!({ "t": "user", "p": { "b": { "type": "bool", "value": true } } })
        );
        out.clear();
        s.set_fps(30, &mut out);
        assert!(out.is_empty());
        s.set_fps(60, &mut out);
        assert_eq!(msg(scripts(&out)[0])["p"]["fps"], 60);
    }

    #[test]
    fn directories_fetchall_and_ondemand() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"1").unwrap();
        std::fs::write(dir.path().join("b.JPG"), b"1").unwrap();
        std::fs::write(dir.path().join("c.txt"), b"1").unwrap();
        let path = dir.path().to_str().unwrap().to_owned();
        let mut s = session(json!({
            "all": { "type": "directory", "mode": "fetchall", "value": path },
            "one": { "type": "directory", "mode": "ondemand", "value": path }
        }));
        let mut out = Vec::new();
        s.page_message(r#"{"t":"ready"}"#, &mut out);
        let sc = scripts(&out);
        let listing = sc
            .iter()
            .find(|s| s.contains("dirChanged"))
            .expect("fetchall listing");
        assert!(
            listing.contains("a.png") && listing.contains("b.JPG") && !listing.contains("c.txt")
        );
        assert!(
            msg(listing)["f"]
                .as_array()
                .unwrap()
                .iter()
                .all(|f| !f.as_str().unwrap().starts_with('/'))
        );

        out.clear();
        std::fs::remove_file(dir.path().join("a.png")).unwrap();
        s.rescan_directories(&mut out);
        let sc = scripts(&out);
        assert_eq!(sc.len(), 1);
        assert!(sc[0].contains("dirRemoved") && sc[0].contains("a.png"));

        out.clear();
        s.page_message(r#"{"t":"randomFile","id":7,"n":"one"}"#, &mut out);
        let sc = scripts(&out);
        assert_eq!(msg(sc[0])["id"], 7);
        assert!(msg(sc[0])["f"].as_str().unwrap().ends_with("b.JPG"));
        out.clear();
        s.page_message(r#"{"t":"randomFile","id":8,"n":"missing"}"#, &mut out);
        assert_eq!(msg(scripts(&out)[0])["f"], "");
    }
}

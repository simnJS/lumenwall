//! The injected JavaScript bridge and the messages crossing it.
//!
//! - [`BRIDGE_JS`] is evaluated in the page's main frame when its JavaScript context is
//!   created, before any page script. The host must first define `window.__lumenHostPost` as a
//!   native function taking one string (page → host channel).
//! - Host → page messages are small scripts calling `window.__lumenWE(message)`, built here.
//! - Page → host messages are JSON strings parsed by [`PageMessage::parse`].

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::audio::AUDIO_FRAME_LEN;
use crate::media::{MediaEvent, MediaKind};

/// Script injected before page scripts.
pub const BRIDGE_JS: &str = include_str!("bridge.js");

/// URL attributed to the bridge in stack traces.
pub const BRIDGE_SCRIPT_URL: &str = "lumen://bridge.js";

/// Name of the native page → host function the host installs before [`BRIDGE_JS`].
pub const HOST_POST_FUNCTION: &str = "__lumenHostPost";

/// What a page asked to listen to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenTarget {
    Audio,
    Media(MediaKind),
}

/// Message sent by the bridge to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageMessage {
    /// The bridge is installed in a fresh JavaScript context (page load or reload).
    Ready,
    /// A listener was registered (`on`) or cleared.
    Listen { target: ListenTarget, on: bool },
    /// `wallpaperRequestRandomFileForProperty(name, cb)`.
    RandomFile { id: u64, name: String },
}

impl PageMessage {
    /// Parses a message posted by the bridge; `None` for anything malformed or unknown.
    pub fn parse(json: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(json).ok()?;
        match v.get("t")?.as_str()? {
            "ready" => Some(Self::Ready),
            "listen" => {
                let on = v.get("on")?.as_bool()?;
                let target = match v.get("k")?.as_str()? {
                    "audio" => ListenTarget::Audio,
                    other => ListenTarget::Media(MediaKind::from_key(other)?),
                };
                Some(Self::Listen { target, on })
            }
            "randomFile" => Some(Self::RandomFile {
                id: v.get("id")?.as_u64()?,
                name: v.get("n")?.as_str()?.to_owned(),
            }),
            _ => None,
        }
    }
}

/// `window.__lumenWE&&window.__lumenWE(<message>)`.
pub fn dispatch_script(message: &Value) -> String {
    let mut s = String::from("window.__lumenWE&&window.__lumenWE(");
    push_json(&mut s, message);
    s.push(')');
    s
}

/// Appends JSON, escaping U+2028/U+2029 so the text is a valid script in any engine.
fn push_json(out: &mut String, value: &Value) {
    let text = value.to_string();
    if text.contains(['\u{2028}', '\u{2029}']) {
        out.push_str(
            &text
                .replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029"),
        );
    } else {
        out.push_str(&text);
    }
}

/// `applyUserProperties(properties)` (properties in WE format).
pub fn user_properties_script(properties: Value) -> String {
    dispatch_script(&json!({ "t": "user", "p": properties }))
}

/// `applyGeneralProperties(properties)`.
pub fn general_properties_script(properties: Value) -> String {
    dispatch_script(&json!({ "t": "general", "p": properties }))
}

/// `setPaused(paused)`.
pub fn paused_script(paused: bool) -> String {
    dispatch_script(&json!({ "t": "paused", "v": paused }))
}

/// `userDirectoryFilesAddedOrChanged` (`changed`) or `userDirectoryFilesRemoved`.
pub fn directory_script(changed: bool, property: &str, files: &[String]) -> String {
    dispatch_script(
        &json!({ "t": if changed { "dirChanged" } else { "dirRemoved" }, "n": property, "f": files }),
    )
}

/// Answer to `wallpaperRequestRandomFileForProperty`; an empty `file` drops the request.
pub fn random_file_script(id: u64, property: &str, file: &str) -> String {
    dispatch_script(&json!({ "t": "randomFile", "id": id, "n": property, "f": file }))
}

/// Media listener event.
pub fn media_script(event: &MediaEvent) -> String {
    dispatch_script(&json!({ "t": "media", "k": event.kind().key(), "e": event.to_js_event() }))
}

/// Audio listener call, written into a reused buffer (no allocation once warmed up).
pub fn audio_script_into(frame: &[f32; AUDIO_FRAME_LEN], out: &mut String) {
    out.clear();
    out.push_str("window.__lumenWE&&window.__lumenWE({\"t\":\"audio\",\"d\":[");
    for (i, v) in frame.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if v.is_finite() {
            let _ = write!(out, "{v}");
        } else {
            out.push('0');
        }
    }
    out.push_str("]})");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_page_messages() {
        assert_eq!(
            PageMessage::parse(r#"{"t":"ready"}"#),
            Some(PageMessage::Ready)
        );
        assert_eq!(
            PageMessage::parse(r#"{"t":"listen","k":"audio","on":true}"#),
            Some(PageMessage::Listen {
                target: ListenTarget::Audio,
                on: true
            })
        );
        assert_eq!(
            PageMessage::parse(r#"{"t":"listen","k":"timeline","on":false}"#),
            Some(PageMessage::Listen {
                target: ListenTarget::Media(MediaKind::Timeline),
                on: false
            })
        );
        assert_eq!(
            PageMessage::parse(r#"{"t":"randomFile","id":3,"n":"dir"}"#),
            Some(PageMessage::RandomFile {
                id: 3,
                name: "dir".into()
            })
        );
        assert_eq!(
            PageMessage::parse(r#"{"t":"listen","k":"nope","on":true}"#),
            None
        );
        assert_eq!(PageMessage::parse("not json"), None);
        assert_eq!(PageMessage::parse(r#"{"t":"other"}"#), None);
    }

    #[test]
    fn scripts_are_single_calls() {
        assert_eq!(
            paused_script(true),
            r#"window.__lumenWE&&window.__lumenWE({"t":"paused","v":true})"#
        );
        let s = user_properties_script(json!({ "a": { "value": "x\u{2028}y" } }));
        assert!(s.contains("\\u2028"));
        assert!(!s.contains('\u{2028}'));
    }

    #[test]
    fn audio_script_format() {
        let mut frame = [0.0f32; AUDIO_FRAME_LEN];
        frame[0] = 0.5;
        frame[127] = f32::NAN;
        let mut s = String::new();
        audio_script_into(&frame, &mut s);
        assert!(s.starts_with(r#"window.__lumenWE&&window.__lumenWE({"t":"audio","d":[0.5,0,"#));
        assert!(s.ends_with(",0]})"));
        let cap = s.capacity();
        audio_script_into(&frame, &mut s);
        assert_eq!(s.capacity(), cap);
    }
}

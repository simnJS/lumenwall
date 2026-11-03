//! Scene sound objects (`sound` in a scene, spec 07 §11.2, spec 02 §7.6).
//!
//! The reference reads only `sound` and `playbackmode`. Following PARITY-POLICY rule 4, Lumen
//! also honours the keys Wallpaper Engine documents for sound layers and the reference ignores
//! (QO-12): `volume`, `mintime`/`maxtime` with the `random` mode, and `startsilent`.

use serde_json::Value;

/// How the files of a sound object are played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackMode {
    /// `"loop"`: every file starts at once and restarts at its end (reference behaviour).
    Loop,
    /// `"random"` (Wallpaper Engine): one random file at a time, separated by a random pause
    /// in `[mintime, maxtime]` seconds. The reference plays it like [`PlaybackMode::Single`].
    Random,
    /// Any other value or absent: every file starts at once and plays once.
    Single,
}

/// Parsed scene sound object.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundObject {
    /// Asset paths in the wallpaper file space, in declaration order.
    pub files: Vec<String>,
    /// Playback mode.
    pub mode: PlaybackMode,
    /// Per-object gain (Wallpaper Engine `volume`, default 1).
    pub volume: f32,
    /// Minimum pause between random plays, seconds.
    pub min_time: f32,
    /// Maximum pause between random plays, seconds.
    pub max_time: f32,
    /// Wallpaper Engine `startsilent`: loaded but not started until a script starts it.
    pub start_silent: bool,
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        // User-property binding: `{"user": "...", "value": x}` carries its current value.
        Value::Object(o) => o.get("value").and_then(Value::as_f64),
        _ => None,
    }
}

fn boolean(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::Object(o) => o.get("value").and_then(Value::as_bool),
        _ => None,
    }
}

impl SoundObject {
    /// Parses a scene object; `None` unless `sound` is an array (spec 02 §7.6 dispatch).
    /// Non-string entries of `sound` are skipped.
    pub fn from_json(object: &Value) -> Option<Self> {
        let files = object.get("sound")?.as_array()?;
        let files: Vec<String> = files
            .iter()
            .filter_map(|f| f.as_str().map(str::to_owned))
            .collect();
        let mode = match object.get("playbackmode").and_then(Value::as_str) {
            // parity: exact, case-sensitive "loop"; anything else plays once.
            Some("loop") => PlaybackMode::Loop,
            Some("random") => PlaybackMode::Random,
            _ => PlaybackMode::Single,
        };
        let volume = number(object.get("volume")).unwrap_or(1.0);
        let volume = if volume.is_finite() {
            volume.max(0.0) as f32
        } else {
            1.0
        };
        let min_time = number(object.get("mintime"))
            .filter(|t| t.is_finite())
            .unwrap_or(0.0)
            .max(0.0) as f32;
        let max_time = number(object.get("maxtime"))
            .filter(|t| t.is_finite())
            .unwrap_or(f64::from(min_time)) as f32;
        let max_time = max_time.max(min_time);
        let start_silent = boolean(object.get("startsilent")).unwrap_or(false);
        Some(Self {
            files,
            mode,
            volume,
            min_time,
            max_time,
            start_silent,
        })
    }

    /// The reference's reading of the object: `volume`, timing, `random` and `startsilent`
    /// ignored (useful for A/B parity checks).
    pub fn reference_only(mut self) -> Self {
        if self.mode == PlaybackMode::Random {
            self.mode = PlaybackMode::Single;
        }
        self.volume = 1.0;
        self.start_silent = false;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn modes() {
        let p = |m: Value| {
            SoundObject::from_json(&json!({"sound": ["a.mp3"], "playbackmode": m}))
                .unwrap()
                .mode
        };
        assert_eq!(p(json!("loop")), PlaybackMode::Loop);
        assert_eq!(p(json!("Loop")), PlaybackMode::Single);
        assert_eq!(p(json!("single")), PlaybackMode::Single);
        assert_eq!(p(json!("random")), PlaybackMode::Random);
        assert_eq!(p(json!(3)), PlaybackMode::Single);
        let none = SoundObject::from_json(&json!({"sound": []})).unwrap();
        assert_eq!(none.mode, PlaybackMode::Single);
    }

    #[test]
    fn requires_sound_array() {
        assert!(SoundObject::from_json(&json!({"sound": "a.mp3"})).is_none());
        assert!(SoundObject::from_json(&json!({"image": "x"})).is_none());
    }

    #[test]
    fn we_fields() {
        let o = SoundObject::from_json(&json!({
            "sound": ["a.ogg", 5, "b.wav"],
            "playbackmode": "random",
            "volume": {"user": "vol", "value": 0.5},
            "mintime": 2,
            "maxtime": 1,
            "startsilent": true
        }))
        .unwrap();
        assert_eq!(o.files, vec!["a.ogg".to_owned(), "b.wav".to_owned()]);
        assert_eq!(o.volume, 0.5);
        assert_eq!((o.min_time, o.max_time), (2.0, 2.0));
        assert!(o.start_silent);
        let r = o.reference_only();
        assert_eq!(
            (r.mode, r.volume, r.start_silent),
            (PlaybackMode::Single, 1.0, false)
        );
    }
}

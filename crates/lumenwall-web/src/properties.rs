//! User properties in the shape the Wallpaper Engine web API delivers them.
//!
//! `window.wallpaperPropertyListener.applyUserProperties(props)` receives an object keyed by
//! property name; each entry carries at least `value` (public WE documentation):
//!
//! | `type`       | `value`                                                            |
//! |--------------|--------------------------------------------------------------------|
//! | `color`      | string `"r g b"`, floats 0–1 separated by spaces                   |
//! | `slider`     | number                                                             |
//! | `bool`       | boolean                                                            |
//! | `combo`      | the selected option's value (as authored); `text` = option label  |
//! | `textinput`  | string                                                             |
//! | `file`       | path such that `'file:///' + value` is a valid URL                 |
//! | `directory`  | same path convention; files are delivered by dedicated callbacks   |

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// How a `directory` property hands its files to the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirectoryMode {
    /// The page asks for one random file with `wallpaperRequestRandomFileForProperty`.
    #[default]
    Ondemand,
    /// The host lists every file and calls `userDirectoryFilesAddedOrChanged` / `…Removed`.
    Fetchall,
}

/// File types a `directory` property accepts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirectoryFileKind {
    /// jpeg, jpg, png, pnga, bmp, gif, svg, webp (public WE documentation).
    #[default]
    Images,
    /// webm, ogg, ogv (public WE documentation).
    Videos,
}

impl DirectoryFileKind {
    /// Lower-case extensions accepted for this kind.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Images => &["jpeg", "jpg", "png", "pnga", "bmp", "gif", "svg", "webp"],
            Self::Videos => &["webm", "ogg", "ogv"],
        }
    }

    /// Whether `path` has one of [`Self::extensions`] (ASCII case-insensitive).
    pub fn accepts(self, path: &std::path::Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                self.extensions()
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case(ext))
            })
    }
}

/// Typed value of one user property.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum PropertyValue {
    Bool(bool),
    Slider(f64),
    /// Kept as authored (`"r g b"` floats 0–1 in WE-saved projects).
    Color(String),
    /// The selected option's value, as authored (string or number).
    Combo(Value),
    /// Any JSON value (a string in practice).
    TextInput(Value),
    /// Absolute path, or empty when unset.
    File(String),
    Directory {
        path: String,
        mode: DirectoryMode,
        kind: DirectoryFileKind,
    },
    SceneTexture(String),
}

impl PropertyValue {
    /// The `type` string Wallpaper Engine uses in `project.json`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Slider(_) => "slider",
            Self::Color(_) => "color",
            Self::Combo(_) => "combo",
            Self::TextInput(_) => "textinput",
            Self::File(_) => "file",
            Self::Directory { .. } => "directory",
            Self::SceneTexture(_) => "scenetexture",
        }
    }
}

/// One combo option: value as authored, display label.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComboOption {
    pub value: Value,
    pub label: String,
}

/// One user property with its current value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserProperty {
    pub key: String,
    #[serde(flatten)]
    pub value: PropertyValue,
    /// Combo options (empty for other types).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ComboOption>,
}

impl UserProperty {
    pub fn new(key: impl Into<String>, value: PropertyValue) -> Self {
        Self {
            key: key.into(),
            value,
            options: Vec::new(),
        }
    }

    /// Entry of the object passed to `applyUserProperties`.
    pub fn to_we_json(&self) -> Value {
        let mut entry = Map::new();
        entry.insert("type".into(), Value::from(self.value.type_name()));
        let value = match &self.value {
            PropertyValue::Bool(b) => Value::Bool(*b),
            PropertyValue::Slider(v) => json_number(*v),
            PropertyValue::Color(s) | PropertyValue::SceneTexture(s) => Value::from(s.as_str()),
            PropertyValue::Combo(v) | PropertyValue::TextInput(v) => v.clone(),
            PropertyValue::File(p) => Value::from(file_url_suffix(p)),
            PropertyValue::Directory { path, mode, .. } => {
                entry.insert(
                    "mode".into(),
                    Value::from(match mode {
                        DirectoryMode::Ondemand => "ondemand",
                        DirectoryMode::Fetchall => "fetchall",
                    }),
                );
                Value::from(file_url_suffix(path))
            }
        };
        if let PropertyValue::Combo(selected) = &self.value
            && let Some(option) = self.options.iter().find(|o| combo_eq(&o.value, selected))
        {
            entry.insert("text".into(), Value::from(option.label.as_str()));
        }
        entry.insert("value".into(), value);
        Value::Object(entry)
    }
}

fn json_number(v: f64) -> Value {
    serde_json::Number::from_f64(v).map_or(Value::from(0), Value::Number)
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n
            .as_f64()
            .filter(|f| f.fract() == 0.0 && f.abs() < 1e15)
            .map_or_else(|| n.to_string(), |f| format!("{}", f as i64)),
        other => other.to_string(),
    }
}

fn combo_eq(a: &Value, b: &Value) -> bool {
    a == b || value_text(a) == value_text(b)
}

/// Path written so that `'file:///' + result` is a valid file URL, as the WE documentation
/// tells pages to do: separators become `/` and the leading `/` of a Unix absolute path is
/// dropped (`/home/a.png` → `home/a.png`). Windows paths (`C:\a.png` → `C:/a.png`) keep their
/// drive letter.
pub fn file_url_suffix(path: &str) -> String {
    let forward = path.replace('\\', "/");
    forward.trim_start_matches('/').to_owned()
}

/// Ordered set of user properties (sorted by key).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertySet {
    properties: BTreeMap<String, UserProperty>,
}

/// Error from [`PropertySet::set_from_text`].
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PropertyError {
    #[error("unknown property {0}")]
    Unknown(String),
    #[error("property {key}: {reason}")]
    Invalid { key: String, reason: String },
}

impl PropertySet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the set from `project.json` → `general.properties`.
    ///
    /// Entries without a known `type`, `group` and decorative `text` entries are skipped.
    pub fn from_project_properties(properties: &Value) -> Self {
        let mut set = Self::new();
        let Some(object) = properties.as_object() else {
            return set;
        };
        for (key, def) in object {
            if let Some(property) = parse_definition(key, def) {
                set.insert(property);
            }
        }
        set
    }

    pub fn insert(&mut self, property: UserProperty) {
        self.properties.insert(property.key.clone(), property);
    }

    pub fn get(&self, key: &str) -> Option<&UserProperty> {
        self.properties.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = &UserProperty> {
        self.properties.values()
    }

    pub fn len(&self) -> usize {
        self.properties.len()
    }

    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    /// Replaces the stored value of each given property; returns those whose value changed.
    pub fn merge(&mut self, changes: impl IntoIterator<Item = UserProperty>) -> Vec<UserProperty> {
        let mut changed = Vec::new();
        for property in changes {
            match self.properties.get_mut(&property.key) {
                Some(current) if current.value == property.value => {}
                Some(current) => {
                    current.value = property.value.clone();
                    if !property.options.is_empty() {
                        current.options = property.options.clone();
                    }
                    changed.push(current.clone());
                }
                None => {
                    self.properties
                        .insert(property.key.clone(), property.clone());
                    changed.push(property);
                }
            }
        }
        changed
    }

    /// Updates a property from a text value (`--set-property key=value`), with the same text
    /// interpretation as scene properties (spec 02 §3.3). Returns the updated property.
    pub fn set_from_text(&mut self, key: &str, text: &str) -> Result<&UserProperty, PropertyError> {
        let property = self
            .properties
            .get_mut(key)
            .ok_or_else(|| PropertyError::Unknown(key.to_owned()))?;
        let invalid = |reason: &str| PropertyError::Invalid {
            key: key.to_owned(),
            reason: reason.to_owned(),
        };
        match &mut property.value {
            PropertyValue::Bool(b) => *b = text == "true" || text == "1",
            PropertyValue::Slider(v) => {
                *v = parse_float_prefix(text).ok_or_else(|| invalid("not a number"))?;
            }
            PropertyValue::Color(s) | PropertyValue::SceneTexture(s) | PropertyValue::File(s) => {
                *s = text.to_owned();
            }
            PropertyValue::Directory { path, .. } => *path = text.to_owned(),
            PropertyValue::TextInput(v) => *v = Value::from(text),
            PropertyValue::Combo(v) => {
                let option = property
                    .options
                    .iter()
                    .find(|o| value_text(&o.value) == text)
                    .ok_or_else(|| invalid("not one of the combo options"))?;
                *v = option.value.clone();
            }
        }
        Ok(property)
    }

    /// Object passed to `applyUserProperties` for the given properties.
    pub fn to_we_json<'a>(properties: impl IntoIterator<Item = &'a UserProperty>) -> Value {
        let mut map = Map::new();
        for property in properties {
            map.insert(property.key.clone(), property.to_we_json());
        }
        Value::Object(map)
    }
}

fn parse_definition(key: &str, def: &Value) -> Option<UserProperty> {
    let def = def.as_object()?;
    let kind = def.get("type")?.as_str()?;
    let raw = def.get("value");
    let text_of = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_owned();
    let value = match kind {
        "bool" => PropertyValue::Bool(match raw {
            Some(Value::Bool(b)) => *b,
            // Quoted booleans exist in shipped wallpapers (corsair_o_tron: `"value": "true"`).
            Some(Value::String(s)) => s == "true" || s == "1",
            Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
            _ => false,
        }),
        "slider" => PropertyValue::Slider(match raw {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(Value::Bool(b)) => f64::from(u8::from(*b)),
            Some(Value::String(s)) => parse_float_prefix(s).unwrap_or(0.0),
            _ => 0.0,
        }),
        "color" => PropertyValue::Color(text_of(raw)),
        "combo" => PropertyValue::Combo(raw.cloned().unwrap_or(Value::Null)),
        "textinput" | "usershortcut" => {
            PropertyValue::TextInput(raw.cloned().unwrap_or_else(|| Value::from("")))
        }
        "file" => PropertyValue::File(text_of(raw)),
        "directory" => PropertyValue::Directory {
            path: text_of(raw),
            mode: match def.get("mode").and_then(Value::as_str) {
                Some(m) if m.eq_ignore_ascii_case("fetchall") => DirectoryMode::Fetchall,
                _ => DirectoryMode::Ondemand,
            },
            kind: match def
                .get("filetype")
                .or_else(|| def.get("fileType"))
                .and_then(Value::as_str)
            {
                Some(t) if t.to_ascii_lowercase().starts_with("video") => DirectoryFileKind::Videos,
                _ => DirectoryFileKind::Images,
            },
        },
        "scenetexture" => PropertyValue::SceneTexture(text_of(raw)),
        _ => return None,
    };
    let options = if kind == "combo" {
        def.get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|o| {
                        let o = o.as_object()?;
                        Some(ComboOption {
                            value: o.get("value")?.clone(),
                            label: o
                                .get("label")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Some(UserProperty {
        key: key.to_owned(),
        value,
        options,
    })
}

/// Longest leading float (after leading whitespace), like a C `strtod` prefix parse.
fn parse_float_prefix(text: &str) -> Option<f64> {
    let t = text.trim_start();
    let bytes = t.as_bytes();
    let mut end = 0;
    let mut best = None;
    while end < bytes.len() {
        end += 1;
        if t.is_char_boundary(end)
            && let Ok(v) = t[..end].parse::<f64>()
        {
            best = Some(v);
        }
    }
    best
}

/// General properties (`applyGeneralProperties`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeneralProperties {
    /// The frame-rate limit configured by the user.
    pub fps: u32,
}

impl GeneralProperties {
    pub fn to_we_json(self) -> Value {
        json!({ "fps": self.fps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Value {
        json!({
            "schemecolor": { "type": "color", "text": "ui_browse_properties_scheme_color", "value": "0.2 0.4 0.8" },
            "showclock": { "type": "bool", "text": "Clock", "value": true },
            "speed": { "type": "slider", "value": 1.5, "min": 0, "max": 10 },
            "mode": { "type": "combo", "value": 1, "options": [
                { "label": "Calm", "value": 0 }, { "label": "Wild", "value": 1 } ] },
            "style": { "type": "combo", "value": "b", "options": [
                { "label": "A", "value": "a" }, { "label": "B", "value": "b" } ] },
            "greeting": { "type": "textinput", "value": "hello" },
            "image": { "type": "file", "value": "/home/me/Pictures/a b.png" },
            "pics": { "type": "directory", "value": "/home/me/Pictures", "mode": "fetchall" },
            "title": { "type": "text", "text": "decoration" },
            "grp": { "type": "group" },
            "bad": 3
        })
    }

    #[test]
    fn parses_project_properties() {
        let set = PropertySet::from_project_properties(&project());
        let keys: Vec<_> = set.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "greeting",
                "image",
                "mode",
                "pics",
                "schemecolor",
                "showclock",
                "speed",
                "style"
            ]
        );
        assert_eq!(set.get("speed").unwrap().value, PropertyValue::Slider(1.5));
        assert_eq!(set.get("mode").unwrap().options.len(), 2);
        assert!(matches!(
            set.get("pics").unwrap().value,
            PropertyValue::Directory {
                mode: DirectoryMode::Fetchall,
                kind: DirectoryFileKind::Images,
                ..
            }
        ));
    }

    #[test]
    fn serializes_in_we_format() {
        let set = PropertySet::from_project_properties(&project());
        let all = PropertySet::to_we_json(set.iter());
        assert_eq!(
            all["schemecolor"],
            json!({ "type": "color", "value": "0.2 0.4 0.8" })
        );
        assert_eq!(all["showclock"], json!({ "type": "bool", "value": true }));
        assert_eq!(all["speed"], json!({ "type": "slider", "value": 1.5 }));
        assert_eq!(
            all["mode"],
            json!({ "type": "combo", "value": 1, "text": "Wild" })
        );
        assert_eq!(
            all["style"],
            json!({ "type": "combo", "value": "b", "text": "B" })
        );
        assert_eq!(
            all["greeting"],
            json!({ "type": "textinput", "value": "hello" })
        );
        assert_eq!(
            all["image"],
            json!({ "type": "file", "value": "home/me/Pictures/a b.png" })
        );
        assert_eq!(
            all["pics"],
            json!({ "type": "directory", "mode": "fetchall", "value": "home/me/Pictures" })
        );
        assert!(all.get("title").is_none());
    }

    #[test]
    fn file_url_suffix_matches_documented_prefixing() {
        assert_eq!(file_url_suffix("/home/a.png"), "home/a.png");
        assert_eq!(file_url_suffix(r"C:\Users\me\a.png"), "C:/Users/me/a.png");
        assert_eq!(file_url_suffix(""), "");
    }

    #[test]
    fn text_overrides_follow_scene_rules() {
        let mut set = PropertySet::from_project_properties(&project());
        assert_eq!(
            set.set_from_text("showclock", "TRUE").unwrap().value,
            PropertyValue::Bool(false)
        );
        assert_eq!(
            set.set_from_text("showclock", "1").unwrap().value,
            PropertyValue::Bool(true)
        );
        assert_eq!(
            set.set_from_text("speed", " 0.5abc").unwrap().value,
            PropertyValue::Slider(0.5)
        );
        assert!(set.set_from_text("speed", "abc").is_err());
        assert_eq!(
            set.set_from_text("mode", "0").unwrap().value,
            PropertyValue::Combo(json!(0))
        );
        assert!(set.set_from_text("mode", "7").is_err());
        assert_eq!(
            set.get("mode").unwrap().value,
            PropertyValue::Combo(json!(0))
        );
        assert_eq!(
            set.set_from_text("schemecolor", "1 0 0").unwrap().value,
            PropertyValue::Color("1 0 0".into())
        );
        assert_eq!(
            set.set_from_text("nope", "1"),
            Err(PropertyError::Unknown("nope".into()))
        );
    }

    #[test]
    fn merge_reports_only_changes() {
        let mut set = PropertySet::from_project_properties(&project());
        let changed = set.merge([
            UserProperty::new("showclock", PropertyValue::Bool(true)),
            UserProperty::new("speed", PropertyValue::Slider(2.0)),
        ]);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].key, "speed");
    }

    #[test]
    fn ipc_roundtrip() {
        let set = PropertySet::from_project_properties(&project());
        let text = serde_json::to_string(&set).unwrap();
        let back: PropertySet = serde_json::from_str(&text).unwrap();
        assert_eq!(back, set);
    }

    #[test]
    fn directory_kinds() {
        use std::path::Path;
        assert!(DirectoryFileKind::Images.accepts(Path::new("a/B.JPG")));
        assert!(!DirectoryFileKind::Images.accepts(Path::new("a/b.webm")));
        assert!(DirectoryFileKind::Videos.accepts(Path::new("b.webm")));
        assert!(!DirectoryFileKind::Videos.accepts(Path::new("noext")));
    }
}

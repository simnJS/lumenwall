//! Minimal, lenient reading of a wallpaper's `project.json`: only what the library
//! and the properties panel show. Unknown or malformed fields are skipped, never fatal.
//! (Temporary: `lumenwall-scene` will own this model once integrated.)

use lumenwall_ipc::PropertyValue;
use serde_json::{Map, Value};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Scene,
    Video,
    Web,
    /// Executable wallpapers: never run by Lumenwall.
    Application,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub title: String,
    pub kind: Kind,
    /// Entry file (`scene.json`, `video.mp4`, `index.html`…).
    pub file: Option<String>,
    pub preview: Option<String>,
    pub tags: Vec<String>,
    pub workshop_id: Option<String>,
    pub description: String,
    pub properties: Vec<PropertyDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertyDef {
    /// Key in `general.properties`, sent to the daemon.
    pub key: String,
    pub label: String,
    pub order: i64,
    pub kind: PropertyKind,
    pub default: Option<PropertyValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropertyKind {
    Bool,
    Slider {
        min: f64,
        max: f64,
        step: f64,
        integer: bool,
    },
    Color,
    Combo {
        options: Vec<(String, PropertyValue)>,
    },
    Text,
    File,
    Directory,
    /// Static text or group heading, no value.
    Label,
}

pub fn parse(text: &str) -> Result<Project, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let obj = root.as_object().ok_or("project.json is not an object")?;
    let file = str_field(obj, "file");
    // Workshop "assets" are building blocks for the editor, not wallpapers.
    let asset = str_field(obj, "category").is_some_and(|c| c.eq_ignore_ascii_case("asset"));
    let kind = if asset {
        Kind::Unknown
    } else {
        kind_of(str_field(obj, "type").as_deref(), file.as_deref())
    };
    let workshop_id = match obj.get("workshopid") {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    let tags = match obj.get("tags") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|t| t.as_str())
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let mut properties: Vec<PropertyDef> = obj
        .get("general")
        .and_then(|g| g.get("properties"))
        .and_then(Value::as_object)
        .map(|props| props.iter().filter_map(|(k, v)| property(k, v)).collect())
        .unwrap_or_default();
    properties.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(&b.key)));
    Ok(Project {
        title: str_field(obj, "title")
            .map(|t| t.trim().to_owned())
            .unwrap_or_default(),
        kind,
        file,
        preview: str_field(obj, "preview").filter(|p| !p.is_empty()),
        tags,
        workshop_id,
        description: str_field(obj, "description").unwrap_or_default(),
        properties,
    })
}

fn str_field(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn kind_of(declared: Option<&str>, file: Option<&str>) -> Kind {
    match declared.map(str::to_ascii_lowercase).as_deref() {
        Some("scene") => return Kind::Scene,
        Some("video") => return Kind::Video,
        Some("web") => return Kind::Web,
        Some("application") => return Kind::Application,
        _ => {}
    }
    let ext = file
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "json" | "pkg" => Kind::Scene,
        "mp4" | "webm" | "mkv" | "avi" | "mov" | "m4v" => Kind::Video,
        "html" | "htm" => Kind::Web,
        "exe" => Kind::Application,
        _ => Kind::Unknown,
    }
}

fn property(key: &str, v: &Value) -> Option<PropertyDef> {
    let obj = v.as_object()?;
    let ty = obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let value = obj.get("value");
    let num = |k: &str| obj.get(k).and_then(number);
    let (kind, default) = match ty.as_str() {
        "bool" => (
            PropertyKind::Bool,
            value.and_then(boolean).map(PropertyValue::Bool),
        ),
        "slider" => {
            let min = num("min").unwrap_or(0.0);
            let max = num("max").unwrap_or(100.0).max(min);
            let integer = !obj.get("fraction").and_then(boolean).unwrap_or(true);
            let step = num("step")
                .filter(|s| *s > 0.0)
                .unwrap_or(if integer { 1.0 } else { 0.0 });
            (
                PropertyKind::Slider {
                    min,
                    max,
                    step,
                    integer,
                },
                value.and_then(number).map(PropertyValue::Number),
            )
        }
        "color" => (
            PropertyKind::Color,
            value.and_then(color).map(PropertyValue::Color),
        ),
        "combo" => {
            let options = obj
                .get("options")
                .and_then(Value::as_array)
                .map(|opts| {
                    opts.iter()
                        .filter_map(|o| {
                            let label = o.get("label").and_then(Value::as_str)?;
                            let value = scalar(o.get("value")?)?;
                            Some((clean_label(label), value))
                        })
                        .collect()
                })
                .unwrap_or_default();
            (PropertyKind::Combo { options }, value.and_then(scalar))
        }
        "textinput" => (PropertyKind::Text, value.and_then(scalar)),
        "file" | "scenetexture" => (PropertyKind::File, value.and_then(scalar)),
        "directory" => (PropertyKind::Directory, value.and_then(scalar)),
        "text" | "group" => (PropertyKind::Label, None),
        _ => return None,
    };
    let label = obj
        .get("text")
        .and_then(Value::as_str)
        .map(clean_label)
        .unwrap_or_default();
    Some(PropertyDef {
        key: key.to_owned(),
        label: if label.is_empty() {
            key.to_owned()
        } else {
            label
        },
        order: obj.get("order").and_then(Value::as_i64).unwrap_or(0),
        kind,
        default,
    })
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

fn boolean(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn scalar(v: &Value) -> Option<PropertyValue> {
    match v {
        Value::String(s) => Some(PropertyValue::Text(s.clone())),
        Value::Number(n) => n.as_f64().map(PropertyValue::Number),
        Value::Bool(b) => Some(PropertyValue::Bool(*b)),
        _ => None,
    }
}

/// `"0.16 0.15 0.26"` (0..1), or 0..255 components when any exceeds 1.
pub fn color(v: &Value) -> Option<[f32; 3]> {
    let parts: Vec<f32> = match v {
        Value::String(s) => s
            .split_whitespace()
            .filter_map(|p| p.parse().ok())
            .collect(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_f64().map(|f| f as f32))
            .collect(),
        _ => return None,
    };
    if parts.len() < 3 {
        return None;
    }
    let scale = if parts[..3].iter().any(|c| *c > 1.0) {
        255.0
    } else {
        1.0
    };
    Some([parts[0] / scale, parts[1] / scale, parts[2] / scale].map(|c| c.clamp(0.0, 1.0)))
}

/// Strips the small HTML subset authors use in labels and maps Wallpaper Engine's
/// built-in label keys to readable text (translated later by the UI).
fn clean_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let text = out.replace("&nbsp;", " ").replace("&amp;", "&");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(rest) = text.strip_prefix("ui_browse_properties_") {
        return rest.replace('_', " ");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_scene_with_properties() {
        let p = parse(
            r#"{
            "title": " Rainy city ", "type": "scene", "file": "scene.json",
            "preview": "preview.gif", "tags": ["Cyberpunk", ""], "workshopid": "123",
            "general": { "properties": {
                "schemecolor": {"order": 0, "text": "ui_browse_properties_scheme_color",
                                "type": "color", "value": "0.5 0.25 1"},
                "rain": {"order": 100, "text": "<b>Rain</b>", "type": "bool", "value": true},
                "speed": {"order": 50, "text": "Speed", "type": "slider", "min": 0, "max": 10,
                          "fraction": true, "value": 2.5},
                "count": {"order": 51, "text": "Count", "type": "slider", "max": 5,
                          "fraction": false, "value": "3"},
                "mode": {"order": 60, "text": "Mode", "type": "combo", "value": 1,
                         "options": [{"label": "Day", "value": 0}, {"label": "Night", "value": 1}]},
                "weird": {"type": "hologram"}
            }}}"#,
        )
        .unwrap();
        assert_eq!(p.title, "Rainy city");
        assert_eq!(p.kind, Kind::Scene);
        assert_eq!(p.tags, ["Cyberpunk"]);
        assert_eq!(p.workshop_id.as_deref(), Some("123"));
        let keys: Vec<&str> = p.properties.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["schemecolor", "speed", "count", "mode", "rain"]);
        assert_eq!(p.properties[0].label, "scheme color");
        assert_eq!(
            p.properties[0].default,
            Some(PropertyValue::Color([0.5, 0.25, 1.0]))
        );
        assert_eq!(p.properties[4].label, "Rain");
        assert!(matches!(
            p.properties[1].kind,
            PropertyKind::Slider { integer: false, .. }
        ));
        assert!(
            matches!(p.properties[2].kind, PropertyKind::Slider { integer: true, step, .. } if step == 1.0)
        );
        assert_eq!(p.properties[2].default, Some(PropertyValue::Number(3.0)));
        let PropertyKind::Combo { options } = &p.properties[3].kind else {
            panic!()
        };
        assert_eq!(options[1], ("Night".to_owned(), PropertyValue::Number(1.0)));
    }

    #[test]
    fn guesses_the_type_and_tolerates_garbage() {
        let p = parse(r#"{"title":"V","file":"loop.MP4","workshopid":42,"tags":"x"}"#).unwrap();
        assert_eq!(p.kind, Kind::Video);
        assert_eq!(p.workshop_id.as_deref(), Some("42"));
        assert!(p.tags.is_empty());
        assert_eq!(parse(r#"{"type":"Web"}"#).unwrap().kind, Kind::Web);
        assert_eq!(
            parse(r#"{"type":"application"}"#).unwrap().kind,
            Kind::Application
        );
        assert_eq!(parse(r#"{"type":"preset"}"#).unwrap().kind, Kind::Unknown);
        assert_eq!(
            parse(r#"{"category":"Asset","file":"assets.json"}"#)
                .unwrap()
                .kind,
            Kind::Unknown
        );
        assert!(parse("[1]").is_err());
        assert!(parse("{").is_err());
    }

    #[test]
    fn colors_accept_both_scales() {
        assert_eq!(color(&Value::from("255 0 127.5")), Some([1.0, 0.0, 0.5]));
        assert_eq!(color(&Value::from("0 0")), None);
    }
}

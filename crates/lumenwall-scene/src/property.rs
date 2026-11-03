//! User properties (spec 02 §3): declaration, update from text, command-line overrides,
//! subscriber lists and the `--list-properties` output (§15.1).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use crate::error::{At, DataErr, DataError, Expected, Warnings};
use crate::intern::{Interner, Sym};
use crate::raw::{Json, JsonKind, RawMap, RawPropertyDef};
use crate::setting::{SettingId, Settings, number_text};
use crate::text::{ColorError, fmt_g6, parse_color, scan_f32};
use crate::value::Value;

/// Index of a property in [`Properties`] (properties are sorted by name).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PropId(u32);

impl PropId {
    /// Position in [`Properties::iter`].
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One option of a combo property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComboOption {
    /// Value text (numbers are stored as decimal text).
    pub value: Box<str>,
    /// Label shown to the user.
    pub label: Box<str>,
}

/// Kind of a user property (§3.2).
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyKind {
    /// `color`.
    Color,
    /// `bool`.
    Bool,
    /// `slider`; bounds are stored, never applied.
    Slider {
        /// `min`.
        min: f32,
        /// `max`.
        max: f32,
        /// `step`.
        step: f32,
    },
    /// `combo`; options sorted by value text, first duplicate kept.
    Combo {
        /// Options.
        options: Vec<ComboOption>,
    },
    /// `text`: decorative label, no value.
    Text,
    /// `scenetexture`.
    SceneTexture,
    /// `file`.
    File,
    /// `directory`.
    Directory,
    /// `textinput`.
    TextInput,
    /// `usershortcut`.
    UserShortcut,
}

impl PropertyKind {
    fn from_type(t: &str) -> Option<Self> {
        Some(match t {
            "color" => Self::Color,
            "bool" => Self::Bool,
            "slider" => Self::Slider {
                min: 0.0,
                max: 0.0,
                step: 0.0,
            },
            "combo" => Self::Combo {
                options: Vec::new(),
            },
            "text" => Self::Text,
            "scenetexture" => Self::SceneTexture,
            "file" => Self::File,
            "directory" => Self::Directory,
            "textinput" => Self::TextInput,
            "usershortcut" => Self::UserShortcut,
            _ => return None,
        })
    }

    /// Name used by the property listing (§15.1).
    pub fn listing_name(&self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Bool => "boolean",
            Self::Slider { .. } => "slider",
            Self::Combo { .. } => "combo",
            Self::Text => "text",
            Self::SceneTexture => "scene texture",
            Self::File | Self::Directory => "file",
            Self::TextInput | Self::UserShortcut => "textinput",
        }
    }
}

/// A user property.
#[derive(Clone, Debug)]
pub struct Property {
    name: Box<str>,
    label: Box<str>,
    kind: PropertyKind,
    value: Value,
}

impl Property {
    /// Name (key in `general.properties`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `text` label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Kind.
    pub fn kind(&self) -> &PropertyKind {
        &self.kind
    }

    /// Current value.
    pub fn value(&self) -> &Value {
        &self.value
    }
}

/// A setting bound to a property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subscriber {
    /// Bound setting.
    pub setting: SettingId,
    /// Its condition.
    pub condition: Option<Sym>,
}

/// User properties of a project, sorted by name, with the settings bound to each.
#[derive(Clone, Debug, Default)]
pub struct Properties {
    list: Vec<Property>,
    subscribers: Vec<Vec<Subscriber>>,
}

/// Why a textual property update was refused (the property is left unchanged).
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PropertyUpdateError {
    /// No property of this name.
    #[error("unknown property {0:?}")]
    Unknown(String),
    /// Slider update without a leading number.
    #[error("{0:?} is not a number")]
    NotANumber(String),
    /// Invalid colour text.
    #[error("invalid colour {0:?}: {1}")]
    Color(String, ColorError),
    /// Combo update with a value that is not an option.
    #[error("{0:?} is not one of the combo values")]
    NotAnOption(String),
}

impl Properties {
    /// Number of properties.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether there is no property.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Properties in name order with their ids.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (PropId, &Property)> {
        self.list
            .iter()
            .enumerate()
            .map(|(i, p)| (PropId(i as u32), p))
    }

    /// Property by id. `id` must come from this set.
    pub fn get(&self, id: PropId) -> &Property {
        &self.list[id.index()]
    }

    /// Exact-name lookup (load time only).
    pub fn find(&self, name: &str) -> Option<PropId> {
        self.list
            .binary_search_by(|p| (*p.name).cmp(name))
            .ok()
            .map(|i| PropId(i as u32))
    }

    /// Settings bound to a property.
    pub fn subscribers(&self, id: PropId) -> &[Subscriber] {
        &self.subscribers[id.index()]
    }

    pub(crate) fn subscribe(&mut self, id: PropId, setting: SettingId, condition: Option<Sym>) {
        self.subscribers[id.index()].push(Subscriber { setting, condition });
    }

    /// Replaces a property value and propagates it synchronously to every bound setting (§3.4).
    /// Returns the updated settings.
    pub fn set_value<'s>(
        &'s mut self,
        id: PropId,
        value: Value,
        settings: &mut Settings,
        strings: &Interner,
    ) -> &'s [Subscriber] {
        self.list[id.index()].value = value;
        self.propagate(id, settings, strings)
    }

    /// Copies the property value into every bound setting.
    pub fn propagate<'s>(
        &'s self,
        id: PropId,
        settings: &mut Settings,
        strings: &Interner,
    ) -> &'s [Subscriber] {
        let source = &self.list[id.index()].value;
        let subs = &self.subscribers[id.index()];
        for s in subs {
            let condition = s.condition.map(|c| strings.resolve(c));
            settings
                .value_mut(s.setting)
                .assign_bound(source, condition);
        }
        subs
    }

    /// Computes the new value of a property from a text (§3.3), without storing it.
    pub fn value_from_text(&self, id: PropId, text: &str) -> Result<Value, PropertyUpdateError> {
        let p = &self.list[id.index()];
        let mut v = p.value.clone();
        match &p.kind {
            PropertyKind::Slider { .. } => match scan_f32(text) {
                Some((f, _)) => v.set_float(f),
                // Lumenwall: refused with a warning instead of aborting at startup (Q13).
                None => return Err(PropertyUpdateError::NotANumber(text.to_owned())),
            },
            PropertyKind::Bool => v.set_bool(text == "true" || text == "1"),
            PropertyKind::Color => match parse_color(text) {
                Ok(c) => v.set_color(c),
                Err(e) => return Err(PropertyUpdateError::Color(text.to_owned(), e)),
            },
            PropertyKind::Combo { options } => {
                if options.iter().any(|o| &*o.value == text) {
                    v.set_text(Arc::from(text));
                } else {
                    return Err(PropertyUpdateError::NotAnOption(text.to_owned()));
                }
            }
            PropertyKind::Text
            | PropertyKind::SceneTexture
            | PropertyKind::File
            | PropertyKind::Directory
            | PropertyKind::TextInput
            | PropertyKind::UserShortcut => v.set_text(Arc::from(text)),
        }
        Ok(v)
    }

    /// The `--list-properties` output (§15.1).
    pub fn listing(&self) -> String {
        let mut out = String::new();
        for p in &self.list {
            let _ = writeln!(out, "{} - {}", p.name, p.kind.listing_name());
            let _ = writeln!(out, "\tText: {}", p.label);
            match &p.kind {
                PropertyKind::Slider { min, max, step } => {
                    let _ = writeln!(out, "\tMin: {}", fmt_g6(*min));
                    let _ = writeln!(out, "\tMax: {}", fmt_g6(*max));
                    let _ = writeln!(out, "\tStep: {}", fmt_g6(*step));
                    let _ = writeln!(out, "\tValue: {}", p.value);
                }
                PropertyKind::Combo { options } => {
                    let _ = writeln!(out, "\tValue: {}", p.value);
                    out.push_str("Values: \n");
                    for o in options {
                        let _ = writeln!(out, "\t\t{} = {}", o.value, o.label);
                    }
                }
                PropertyKind::Text => {
                    let _ = writeln!(out, "\tValue: {}", p.label);
                }
                // Lumenwall shows the stored text; the reference always prints an empty value (Q10).
                PropertyKind::SceneTexture
                | PropertyKind::File
                | PropertyKind::Directory
                | PropertyKind::TextInput
                | PropertyKind::UserShortcut => {
                    let _ = writeln!(out, "\tValue: {}", p.value.text());
                }
                PropertyKind::Bool | PropertyKind::Color => {
                    let _ = writeln!(out, "\tValue: {}", p.value);
                }
            }
        }
        out
    }
}

fn json_value_kind(v: &serde_json::Value) -> JsonKind {
    match v {
        serde_json::Value::Null => JsonKind::Null,
        serde_json::Value::Bool(_) => JsonKind::Bool,
        serde_json::Value::Number(_) => JsonKind::Number,
        serde_json::Value::String(_) => JsonKind::String,
        serde_json::Value::Array(_) => JsonKind::Array,
        serde_json::Value::Object(_) => JsonKind::Object,
    }
}

fn type_err(expected: Expected, v: Option<&serde_json::Value>) -> DataErr {
    match v {
        None => DataErr::new(DataError::Missing),
        Some(v) => DataErr::new(DataError::Type {
            expected,
            found: json_value_kind(v),
        }),
    }
}

fn number_to_i32(n: &serde_json::Number) -> i32 {
    if let Some(i) = n.as_i64() {
        i as i32
    } else if let Some(u) = n.as_u64() {
        u as i32
    } else {
        n.as_f64().unwrap_or(0.0) as i32
    }
}

fn opt_f32(j: &Json<'_>) -> Result<f32, DataErr> {
    crate::convert::opt_f32(j, 0.0)
}

/// Parses `general.properties` (§3.1, §3.2).
pub(crate) fn parse_properties(
    raw: &RawMap<'_, RawPropertyDef<'_>>,
    warnings: &mut Warnings,
    file: &str,
) -> Result<Properties, DataErr> {
    let mut list = Vec::new();
    for (name, def) in raw.sorted() {
        if def.kind != JsonKind::Object {
            continue;
        }
        let ty = match &def.ty {
            Json::Absent | Json::Null => continue,
            Json::Str(s) if s == "group" => continue,
            Json::Str(s) => s,
            other => {
                warnings.push(
                    file,
                    format!("property {name:?}: ignored, type is {}", other.kind()),
                );
                continue;
            }
        };
        let Some(mut kind) = PropertyKind::from_type(ty) else {
            warnings.push(
                file,
                format!("property {name:?}: unknown type {ty:?}, ignored"),
            );
            continue;
        };
        let parsed = parse_property(name, def, &mut kind, warnings, file).at_key(name);
        let (label, value) = parsed?;
        list.push(Property {
            name: name.into(),
            label,
            kind,
            value,
        });
    }
    let subscribers = vec![Vec::new(); list.len()];
    Ok(Properties { list, subscribers })
}

fn parse_property(
    name: &str,
    def: &RawPropertyDef<'_>,
    kind: &mut PropertyKind,
    warnings: &mut Warnings,
    file: &str,
) -> Result<(Box<str>, Value), DataErr> {
    let label: Box<str> = crate::convert::opt_str(&def.text, "").at("text")?.into();
    let raw_value = def.value.as_ref();
    let value = match kind {
        PropertyKind::Color => match raw_value {
            Some(serde_json::Value::String(s)) => Value::from_color(
                parse_color(s)
                    .map_err(|reason| {
                        DataErr::new(DataError::Color {
                            value: s.clone(),
                            reason,
                        })
                    })
                    .at("value")?,
            ),
            other => return Err(type_err(Expected::String, other)).at("value"),
        },
        PropertyKind::Bool => match raw_value {
            None => Value::from_bool(false),
            Some(serde_json::Value::Bool(b)) => Value::from_bool(*b),
            // Wallpaper Engine accepts quoted booleans (`"value": "true"` in the default web
            // wallpaper corsair_o_tron); read them like override text.
            Some(serde_json::Value::String(s)) => Value::from_bool(s == "true" || s == "1"),
            other => return Err(type_err(Expected::Bool, other)).at("value"),
        },
        PropertyKind::Slider { min, max, step } => {
            *min = opt_f32(&def.min).at("min")?;
            *max = opt_f32(&def.max).at("max")?;
            *step = opt_f32(&def.step).at("step")?;
            match raw_value {
                Some(serde_json::Value::Number(n)) => {
                    Value::from_float(n.as_f64().unwrap_or(0.0) as f32)
                }
                Some(serde_json::Value::Bool(b)) => Value::from_float(if *b { 1.0 } else { 0.0 }),
                // Quoted numbers exist in shipped wallpapers (corsair_o_tron: `"value": "150"`).
                Some(serde_json::Value::String(s)) if s.trim().parse::<f32>().is_ok() => {
                    Value::from_float(s.trim().parse::<f32>().unwrap_or(0.0))
                }
                other => return Err(type_err(Expected::Float, other)).at("value"),
            }
        }
        PropertyKind::Combo { options } => {
            match def.options.kind {
                JsonKind::Array => {}
                JsonKind::Absent | JsonKind::Null => {
                    return Err(DataErr::new(DataError::Missing)).at("options");
                }
                _ => return Err(DataErr::new(DataError::ComboOptions)).at("options"),
            }
            let mut opts: Vec<ComboOption> = Vec::with_capacity(def.options.items.len());
            for (i, o) in def.options.items.iter().enumerate() {
                if o.kind != JsonKind::Object {
                    continue;
                }
                let value = match &o.value {
                    Json::Str(s) => s.to_string(),
                    n @ (Json::Int(_) | Json::UInt(_) | Json::Float(_)) => {
                        number_text(n).unwrap_or_default()
                    }
                    Json::Absent => {
                        return Err(DataErr::new(DataError::Missing))
                            .at("value")
                            .at_idx(i)
                            .at("options");
                    }
                    other => {
                        return Err(DataErr::new(DataError::Type {
                            expected: Expected::StringOrNumber,
                            found: other.kind(),
                        }))
                        .at("value")
                        .at_idx(i)
                        .at("options");
                    }
                };
                let label = crate::convert::req_str(&o.label)
                    .at("label")
                    .at_idx(i)
                    .at("options")?;
                opts.push(ComboOption {
                    value: value.into(),
                    label: label.into(),
                });
            }
            // Sorted by value text; the first option of a duplicated value is kept.
            opts.sort_by(|a, b| a.value.cmp(&b.value));
            opts.dedup_by(|later, earlier| later.value == earlier.value);
            let initial = match raw_value {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Number(n)) => number_to_i32(n).to_string(),
                other => return Err(type_err(Expected::StringOrNumber, other)).at("value"),
            };
            let found = opts.iter().any(|o| *o.value == *initial);
            *options = opts;
            if found {
                Value::from_text(&initial)
            } else {
                warnings.push(
                    file,
                    format!("property {name:?}: value {initial:?} is not one of its options"),
                );
                Value::null()
            }
        }
        PropertyKind::Text => Value::null(),
        PropertyKind::SceneTexture => match raw_value {
            Some(serde_json::Value::String(s)) => Value::from_text(s),
            other => return Err(type_err(Expected::String, other)).at("value"),
        },
        PropertyKind::File | PropertyKind::Directory => match raw_value {
            None => Value::from_text(""),
            Some(serde_json::Value::String(s)) => Value::from_text(s),
            other => return Err(type_err(Expected::String, other)).at("value"),
        },
        PropertyKind::TextInput | PropertyKind::UserShortcut => match raw_value {
            None => return Err(DataErr::new(DataError::Missing)).at("value"),
            // Lumenwall stores a string as its text, like a later update does; other JSON is
            // stored as compact JSON (Q9).
            Some(serde_json::Value::String(s)) => Value::from_text(s),
            Some(other) => Value::from_text(&serde_json::to_string(other).unwrap_or_default()),
        },
    };
    Ok((label, value))
}

/// `--set-property name=value` overrides (§3.5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyOverrides {
    map: BTreeMap<String, String>,
}

impl PropertyOverrides {
    /// No override.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one `--set-property` argument: split at the first `=`; without `=` the value is
    /// `"1"`. A later argument for the same name wins.
    pub fn push_arg(&mut self, arg: &str) {
        let (name, value) = arg.split_once('=').unwrap_or((arg, "1"));
        self.map.insert(name.to_owned(), value.to_owned());
    }

    /// Sets one override.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.map.insert(name.into(), value.into());
    }

    /// Override for a property name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.map.get(name).map(String::as_str)
    }

    /// Whether there is no override.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Applies the overrides to `props` in property-name order (§3.5); unknown names are
    /// ignored. Invalid texts leave the property unchanged with a warning.
    pub(crate) fn apply(&self, props: &mut Properties, warnings: &mut Warnings, file: &str) {
        if self.map.is_empty() {
            return;
        }
        for i in 0..props.list.len() {
            let id = PropId(i as u32);
            let Some(text) = self.map.get(&*props.list[i].name) else {
                continue;
            };
            tracing::info!("applying override to property {}", props.list[i].name);
            match props.value_from_text(id, text) {
                Ok(v) => props.list[i].value = v,
                Err(e) => warnings.push(file, format!("property override ignored: {e}")),
            }
        }
    }
}

impl<'a> FromIterator<&'a str> for PropertyOverrides {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        let mut o = Self::new();
        for a in iter {
            o.push_arg(a);
        }
        o
    }
}

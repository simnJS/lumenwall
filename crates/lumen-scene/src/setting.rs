//! Settings ("réglages", spec 02 §5): bindable values stored contiguously and addressed by
//! [`SettingId`]. Bindings are resolved once, at analysis, into a property index plus an interned
//! condition; a property update walks only its own subscriber list (see
//! [`crate::Properties::subscribers`]).

use std::borrow::Cow;

use glam::{Vec2, Vec3};

use crate::error::{At, DataErr, DataError, Expected};
use crate::intern::{Interner, Sym};
use crate::property::{PropId, Properties};
use crate::raw::{Json, RawSetting, RawUser};
use crate::text::{parse_color, parse_full_f32, vector_components};
use crate::value::Value;

/// Index of a setting in [`Settings`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SettingId(u32);

impl SettingId {
    /// Position in the store.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Index of a script in [`Settings::scripts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScriptId(u32);

impl ScriptId {
    /// Position in [`Settings::scripts`].
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Cold data of a setting: its binding and script.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingMeta {
    /// Bound user property, if the name was found (§5.2-4).
    pub property: Option<PropId>,
    /// Condition, only kept when the property was found.
    pub condition: Option<Sym>,
    /// Attached script (data only; executed by `lumen-script`).
    pub script: Option<ScriptId>,
}

/// Script attached to a setting (§5.2-3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    /// JavaScript source.
    pub source: Box<str>,
    /// `scriptproperties`, by name in key order; each one is itself a setting.
    pub properties: Vec<(Sym, SettingId)>,
}

/// Store of every setting of a project. Values are contiguous for cache-friendly per-frame reads.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    values: Vec<Value>,
    meta: Vec<SettingMeta>,
    scripts: Vec<Script>,
    scripted: Vec<SettingId>,
}

impl Settings {
    /// Current value. `id` must come from this store.
    #[inline]
    pub fn value(&self, id: SettingId) -> &Value {
        &self.values[id.index()]
    }

    /// Current value, `None` for an id from another store.
    pub fn get(&self, id: SettingId) -> Option<&Value> {
        self.values.get(id.index())
    }

    /// Overwrites a value (scripts). The bound property is not updated (§4.4-3); its next update
    /// overwrites this value.
    pub fn set(&mut self, id: SettingId, value: Value) {
        if let Some(v) = self.values.get_mut(id.index()) {
            *v = value;
        }
    }

    /// Mutable access to a value. `id` must come from this store.
    #[inline]
    pub fn value_mut(&mut self, id: SettingId) -> &mut Value {
        &mut self.values[id.index()]
    }

    /// Binding and script of a setting. `id` must come from this store.
    pub fn meta(&self, id: SettingId) -> &SettingMeta {
        &self.meta[id.index()]
    }

    /// Script by id.
    pub fn script(&self, id: ScriptId) -> &Script {
        &self.scripts[id.index()]
    }

    /// All scripts.
    pub fn scripts(&self) -> &[Script] {
        &self.scripts
    }

    /// Settings carrying a script, in creation order.
    pub fn scripted(&self) -> &[SettingId] {
        &self.scripted
    }

    /// Number of settings.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub(crate) fn push(&mut self, value: Value, meta: SettingMeta) -> SettingId {
        let id = SettingId(self.values.len() as u32);
        self.values.push(value);
        self.meta.push(meta);
        if meta.script.is_some() {
            self.scripted.push(id);
        }
        id
    }

    /// Adds an unbound constant setting.
    pub fn push_constant(&mut self, value: Value) -> SettingId {
        self.push(value, SettingMeta::default())
    }
}

/// Converts a JSON number (or numeric string for combos) to the decimal text used by combo
/// values: truncated to a 32-bit integer (§3.2).
pub(crate) fn number_text(j: &Json<'_>) -> Option<String> {
    let i = match j {
        Json::Int(i) => *i as i32,
        Json::UInt(u) => *u as i32,
        Json::Float(f) => *f as i32,
        _ => return None,
    };
    Some(i.to_string())
}

/// Initial value of a setting from its raw value `B` (§5.2-2).
pub(crate) fn initial_value(b: &Json<'_>, color: bool) -> Result<Value, DataErr> {
    Ok(match b {
        Json::Str(s) if color => {
            let c = parse_color(s).map_err(|reason| DataError::Color {
                value: s.to_string(),
                reason,
            })?;
            Value::from_color(c)
        }
        Json::Str(s) => {
            let (n, c) = vector_components(s);
            match n {
                1 => match parse_full_f32(s) {
                    Some(f) => Value::from_float(f),
                    None => Value::from_text(s),
                },
                2 => Value::from_vec2(Vec2::new(c[0], c[1])),
                3 => Value::from_vec3(Vec3::new(c[0], c[1], c[2])),
                _ => Value::from_vec4(c.into()),
            }
        }
        Json::Int(i) => Value::from_int(*i as i32),
        Json::UInt(u) => Value::from_int(*u as i32),
        Json::Float(f) => Value::from_float(*f as f32),
        Json::Bool(v) => Value::from_bool(*v),
        Json::Absent | Json::Null | Json::Array | Json::Object => Value::null(),
    })
}

/// Everything needed to analyse and bind settings.
pub(crate) struct Binder<'p> {
    pub settings: &'p mut Settings,
    pub props: &'p mut Properties,
    pub strings: &'p mut Interner,
}

impl Binder<'_> {
    /// Analyses a setting and registers it (§5.2). `default` gives mode L (absent or `null` →
    /// unbound constant); `None` gives mode L! (absent → error, `null` → Null).
    pub fn setting(
        &mut self,
        raw: &RawSetting<'_>,
        default: Option<Value>,
        color: bool,
    ) -> Result<SettingId, DataErr> {
        let (value, meta) = self.analyse(raw, default, color, true)?;
        let id = self.settings.push(value, meta);
        if let Some(pid) = meta.property {
            self.props.subscribe(pid, id, meta.condition);
        }
        Ok(id)
    }

    /// Analyses a setting whose value is frozen at load ("instantané"): same algorithm, bound
    /// properties are read once, nothing is registered.
    pub fn snapshot(
        &mut self,
        raw: &RawSetting<'_>,
        default: Value,
        color: bool,
    ) -> Result<Value, DataErr> {
        Ok(self.analyse(raw, Some(default), color, false)?.0)
    }

    fn analyse(
        &mut self,
        raw: &RawSetting<'_>,
        default: Option<Value>,
        color: bool,
        register: bool,
    ) -> Result<(Value, SettingMeta), DataErr> {
        let obj = match raw {
            RawSetting::Absent | RawSetting::Scalar(Json::Null) if default.is_some() => {
                return Ok((default.unwrap_or_default(), SettingMeta::default()));
            }
            RawSetting::Absent => return Err(DataError::Missing.into()),
            RawSetting::Scalar(b) => return Ok((initial_value(b, color)?, SettingMeta::default())),
            RawSetting::Object(o) => o,
        };
        if obj.value == Json::Absent {
            return Err(DataErr::new(DataError::Missing)).at("value");
        }
        let mut value = initial_value(&obj.value, color).at("value")?;
        let mut meta = SettingMeta::default();

        match &obj.script {
            Json::Absent | Json::Null => {}
            Json::Str(src) => {
                if register {
                    let mut properties = Vec::new();
                    if obj.script_properties.kind == crate::raw::JsonKind::Object {
                        for (name, node) in obj.script_properties.sorted() {
                            let id = self
                                .setting(node, None, false)
                                .at_key(name)
                                .at("scriptproperties")?;
                            properties.push((self.strings.intern(name), id));
                        }
                    }
                    let sid = ScriptId(self.settings.scripts.len() as u32);
                    self.settings.scripts.push(Script {
                        source: src.as_ref().into(),
                        properties,
                    });
                    meta.script = Some(sid);
                }
            }
            other => {
                return Err(DataErr::new(DataError::Type {
                    expected: Expected::String,
                    found: other.kind(),
                }))
                .at("script");
            }
        }

        let binding: Option<(&str, Option<Cow<'_, str>>)> = match &obj.user {
            RawUser::Absent | RawUser::Null => None,
            RawUser::Name(n) => Some((n, None)),
            RawUser::Object(u) => {
                let name = match &u.name {
                    Json::Str(s) => &**s,
                    Json::Absent => {
                        return Err(DataErr::new(DataError::Missing)).at("name").at("user");
                    }
                    other => {
                        return Err(DataErr::new(DataError::Type {
                            expected: Expected::String,
                            found: other.kind(),
                        }))
                        .at("name")
                        .at("user");
                    }
                };
                // Lumen: a missing condition means "no condition" and a numeric one is read like
                // a combo value (Q30, fatal errors fixed).
                let condition = match &u.condition {
                    Json::Absent | Json::Null => None,
                    Json::Str(s) => Some(Cow::Borrowed(&**s)),
                    n @ (Json::Int(_) | Json::UInt(_) | Json::Float(_)) => {
                        number_text(n).map(Cow::Owned)
                    }
                    other => {
                        return Err(DataErr::new(DataError::Type {
                            expected: Expected::String,
                            found: other.kind(),
                        }))
                        .at("condition")
                        .at("user");
                    }
                };
                Some((name, condition))
            }
            RawUser::Other(kind) => {
                return Err(DataErr::new(DataError::Type {
                    expected: Expected::StringOrObject,
                    found: *kind,
                }))
                .at("user");
            }
        };

        if let Some((name, condition)) = binding
            && let Some(pid) = self.props.find(name)
        {
            meta.property = Some(pid);
            meta.condition = condition.as_deref().map(|c| self.strings.intern(c));
            value.assign_bound(self.props.get(pid).value(), condition.as_deref());
        }
        Ok((value, meta))
    }
}

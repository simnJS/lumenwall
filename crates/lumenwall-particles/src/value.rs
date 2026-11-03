//! Polymorphic JSON values (spec 05 §1.1).
//!
//! Three reading families coexist:
//! - **[U]** user-bindable values: [`UserValue`], read through [`Value`] conversions;
//! - **[C]** colours: like [U] but strings are parsed as colours;
//! - **[B]** raw values: plain JSON numbers/booleans/strings read once (`raw_*` helpers).

use glam::{IVec3, Vec2, Vec3, Vec4};
use serde_json::{Map, Value as Json};

pub(crate) type Obj = Map<String, Json>;

/// A resolved polymorphic value. Its *shape* is kept because conversions depend on it
/// (scalar broadcast, vec3 → vec4 with `w = 0`, …).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    /// Unrecognised JSON (array, null, object without `value`): reads as zero.
    #[default]
    Null,
    Bool(bool),
    Number(f32),
    Vec2(Vec2),
    Vec3(Vec3),
    Vec4(Vec4),
    /// Non-numeric string: reads as zero.
    Text(Box<str>),
}

impl Value {
    /// Reads the value as a scalar: a vector gives its `x` component.
    #[inline]
    pub fn as_f32(&self) -> f32 {
        match self {
            Value::Null | Value::Text(_) => 0.0,
            Value::Bool(b) => f32::from(u8::from(*b)),
            Value::Number(v) => *v,
            Value::Vec2(v) => v.x,
            Value::Vec3(v) => v.x,
            Value::Vec4(v) => v.x,
        }
    }

    /// Reads the value as a vec2: a scalar is broadcast.
    #[inline]
    pub fn as_vec2(&self) -> Vec2 {
        match self {
            Value::Vec2(v) => *v,
            Value::Vec3(v) => v.truncate(),
            Value::Vec4(v) => v.truncate().truncate(),
            other => Vec2::splat(other.as_f32()),
        }
    }

    /// Reads the value as a vec3: a scalar is broadcast, a vec2 gets `z = 0`.
    #[inline]
    pub fn as_vec3(&self) -> Vec3 {
        match self {
            Value::Vec2(v) => v.extend(0.0),
            Value::Vec3(v) => *v,
            Value::Vec4(v) => v.truncate(),
            other => Vec3::splat(other.as_f32()),
        }
    }

    /// Reads the value as a vec4: a scalar is broadcast, a vec3 gets `w = 0`.
    #[inline]
    pub fn as_vec4(&self) -> Vec4 {
        match self {
            Value::Vec2(v) => v.extend(0.0).extend(0.0),
            // parity: a vec3 read as vec4 gets w = 0 (visible in g_Color4 when instanceoverride is present).
            Value::Vec3(v) => v.extend(0.0),
            Value::Vec4(v) => *v,
            other => Vec4::splat(other.as_f32()),
        }
    }

    /// Reads the value as a boolean (non-zero numbers are true).
    #[inline]
    pub fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Text(s) => s.eq_ignore_ascii_case("true"),
            other => other.as_f32() != 0.0,
        }
    }

    /// Parses a JSON value with the [U] rules (objects are handled by [`UserValue`]).
    pub fn from_json(j: &Json) -> Value {
        match j {
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0) as f32),
            Json::String(s) => Value::parse_str(s),
            // parity: a JSON array (or null) is not recognised: the value is zero, with no fallback to the default.
            _ => Value::Null,
        }
    }

    /// Parses a JSON value with the [C] (colour) rules.
    pub fn color_from_json(j: &Json) -> Value {
        match j {
            Json::String(s) => Value::parse_color_str(s),
            other => Value::from_json(other),
        }
    }

    /// Parses a [U] string: one numeric component → number, 2/3/4 space-separated components → vector,
    /// anything non-numeric → text.
    pub fn parse_str(s: &str) -> Value {
        let mut c = [0.0f32; 4];
        let mut count = 0usize;
        for tok in s.split_whitespace() {
            if count == 4 {
                break;
            }
            match tok.parse::<f32>() {
                Ok(v) => c[count] = v,
                Err(_) => return Value::Text(s.into()),
            }
            count += 1;
        }
        match count {
            0 => Value::Text(s.into()),
            1 => Value::Number(c[0]),
            2 => Value::Vec2(Vec2::new(c[0], c[1])),
            3 => Value::Vec3(Vec3::new(c[0], c[1], c[2])),
            _ => Value::Vec4(Vec4::from_array(c)),
        }
    }

    /// Parses a [C] string: `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`, or `"r g b [a]"` (commas tolerated).
    /// Without any `.` the components are 0–255 integers divided by 255; otherwise floats taken as is.
    /// Alpha defaults to 1.
    pub fn parse_color_str(s: &str) -> Value {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            return parse_hex(hex).map_or(Value::Null, Value::Vec4);
        }
        let bytes = !s.contains('.');
        let mut c = [0.0f32, 0.0, 0.0, 1.0];
        let mut count = 0usize;
        for tok in s
            .split(|ch: char| ch.is_whitespace() || ch == ',')
            .filter(|t| !t.is_empty())
        {
            if count == 4 {
                break;
            }
            let v = tok.parse::<f32>().unwrap_or(0.0);
            c[count] = if bytes { v / 255.0 } else { v };
            count += 1;
        }
        match count {
            0 => Value::Null,
            1 => Value::Number(c[0]),
            _ => Value::Vec4(Vec4::from_array(c)),
        }
    }
}

fn parse_hex(hex: &str) -> Option<Vec4> {
    let digit = |i: usize| -> Option<u32> { char::from(*hex.as_bytes().get(i)?).to_digit(16) };
    let short = |i: usize| -> Option<f32> { Some((digit(i)? * 17) as f32 / 255.0) };
    let long = |i: usize| -> Option<f32> { Some((digit(i)? * 16 + digit(i + 1)?) as f32 / 255.0) };
    match hex.len() {
        3 => Some(Vec4::new(short(0)?, short(1)?, short(2)?, 1.0)),
        4 => Some(Vec4::new(short(0)?, short(1)?, short(2)?, short(3)?)),
        6 => Some(Vec4::new(long(0)?, long(2)?, long(4)?, 1.0)),
        8 => Some(Vec4::new(long(0)?, long(2)?, long(4)?, long(6)?)),
        _ => None,
    }
}

/// Binding metadata of a [U] value written in object form
/// (`{"value": …, "user": …, "script": …, "scriptproperties": …}`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Binding {
    /// Name of the user property the value follows (`user` as a string, or `user.name`).
    pub user: Option<String>,
    /// `user.condition`, kept raw for the property resolver.
    pub condition: Option<Json>,
    /// SceneScript source, kept raw for the script runtime.
    pub script: Option<String>,
    /// `scriptproperties`, kept raw for the script runtime.
    pub script_properties: Option<Json>,
}

/// A user-bindable value: the current resolved value plus an optional binding.
///
/// The simulation reads [`UserValue::value`] at each use (per spawn for initializers, per frame for
/// operators). Property resolution (another crate) updates it with [`UserValue::set`] or
/// [`UserValue::set_from_json`]; see `ParticleSystem::set_user_property`.
#[derive(Clone, Debug, PartialEq)]
pub struct UserValue {
    /// Current resolved value.
    pub value: Value,
    /// Binding metadata, `None` for a constant.
    pub binding: Option<Box<Binding>>,
    color: bool,
}

impl UserValue {
    /// A constant [U] value.
    pub fn new(value: Value) -> Self {
        Self {
            value,
            binding: None,
            color: false,
        }
    }

    /// A constant [U] scalar.
    pub fn number(v: f32) -> Self {
        Self::new(Value::Number(v))
    }

    /// A constant [U] vec3.
    pub fn from_vec3(v: Vec3) -> Self {
        Self::new(Value::Vec3(v))
    }

    /// A constant [C] value.
    pub fn new_color(value: Value) -> Self {
        Self {
            value,
            binding: None,
            color: true,
        }
    }

    /// Parses a JSON value (any of the accepted forms) with the [U] or [C] rules.
    pub fn from_json(j: &Json, color: bool) -> Self {
        let parse = |j: &Json| {
            if color {
                Value::color_from_json(j)
            } else {
                Value::from_json(j)
            }
        };
        let Json::Object(o) = j else {
            return Self {
                value: parse(j),
                binding: None,
                color,
            };
        };
        let value = o.get("value").map(parse).unwrap_or_default();
        let (user, condition) = match o.get("user") {
            Some(Json::String(s)) if !s.is_empty() => (Some(s.clone()), None),
            Some(Json::Object(u)) => (
                u.get("name")
                    .and_then(Json::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from),
                u.get("condition").cloned(),
            ),
            _ => (None, None),
        };
        let script = o.get("script").and_then(Json::as_str).map(String::from);
        let script_properties = o.get("scriptproperties").cloned();
        let binding =
            (user.is_some() || script.is_some() || script_properties.is_some()).then(|| {
                Box::new(Binding {
                    user,
                    condition,
                    script,
                    script_properties,
                })
            });
        Self {
            value,
            binding,
            color,
        }
    }

    /// Name of the bound user property, if any.
    pub fn property(&self) -> Option<&str> {
        self.binding.as_ref()?.user.as_deref()
    }

    /// True for a [C] (colour) value: strings are parsed as colours.
    pub fn is_color(&self) -> bool {
        self.color
    }

    /// Replaces the current value.
    pub fn set(&mut self, value: Value) {
        self.value = value;
    }

    /// Replaces the current value from raw JSON, with this value's family rules ([U] or [C]).
    ///
    /// A binding with a `condition` compares a text property (a combo value) with it and
    /// stores the boolean result (spec 02 §4.4-1, which spec 05 §1.1 defers to); for any other
    /// property type the condition is ignored (spec 02 Q8). A numeric condition reads like a
    /// combo value (spec 02 Q30).
    pub fn set_from_json(&mut self, j: &Json) {
        if let Json::String(text) = j
            && let Some(condition) = self.binding.as_ref().and_then(|b| b.condition.as_ref())
        {
            let matches = match condition {
                Json::String(c) => Some(c == text),
                Json::Number(n) => Some(n.to_string() == *text),
                _ => None,
            };
            if let Some(m) = matches {
                self.value = Value::Bool(m);
                return;
            }
        }
        self.value = if self.color {
            Value::color_from_json(j)
        } else {
            Value::from_json(j)
        };
    }

    #[inline]
    pub fn f32(&self) -> f32 {
        self.value.as_f32()
    }

    #[inline]
    pub fn vec2(&self) -> Vec2 {
        self.value.as_vec2()
    }

    #[inline]
    pub fn vec3(&self) -> Vec3 {
        self.value.as_vec3()
    }

    #[inline]
    pub fn vec4(&self) -> Vec4 {
        self.value.as_vec4()
    }

    #[inline]
    pub fn bool(&self) -> bool {
        self.value.as_bool()
    }
}

// ---------------------------------------------------------------------------------------------
// Readers used by the model.

/// [U] value, `default` when the key is absent.
pub(crate) fn user(o: &Obj, key: &str, default: Value) -> UserValue {
    o.get(key).map_or_else(
        || UserValue::new(default),
        |j| UserValue::from_json(j, false),
    )
}

pub(crate) fn user_f(o: &Obj, key: &str, default: f32) -> UserValue {
    user(o, key, Value::Number(default))
}

pub(crate) fn user_v3(o: &Obj, key: &str, default: Vec3) -> UserValue {
    user(o, key, Value::Vec3(default))
}

/// [C] value, `default` when the key is absent.
pub(crate) fn color(o: &Obj, key: &str, default: Vec3) -> UserValue {
    o.get(key).map_or_else(
        || UserValue::new_color(Value::Vec3(default)),
        |j| UserValue::from_json(j, true),
    )
}

/// [B] number. Booleans read as 0/1; any other type gives `default`.
pub(crate) fn raw_f32(o: &Obj, key: &str, default: f32) -> f32 {
    match o.get(key) {
        Some(Json::Number(n)) => n.as_f64().map_or(default, |v| v as f32),
        Some(Json::Bool(b)) => f32::from(u8::from(*b)),
        _ => default,
    }
}

/// [B] integer. Floats are truncated toward zero; booleans read as 0/1.
pub(crate) fn raw_i64(o: &Obj, key: &str, default: i64) -> i64 {
    match o.get(key) {
        Some(Json::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(default),
        Some(Json::Bool(b)) => i64::from(*b),
        _ => default,
    }
}

/// [B] integer flags.
pub(crate) fn raw_flags(o: &Obj, key: &str) -> u32 {
    raw_i64(o, key, 0) as u32
}

/// [B] integer clamped to `i32`.
pub(crate) fn raw_i32(o: &Obj, key: &str, default: i32) -> i32 {
    raw_i64(o, key, i64::from(default)).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// [B] boolean. Numbers read as `!= 0`.
pub(crate) fn raw_bool(o: &Obj, key: &str, default: bool) -> bool {
    match o.get(key) {
        Some(Json::Bool(b)) => *b,
        Some(Json::Number(n)) => n.as_f64().map_or(default, |v| v != 0.0),
        _ => default,
    }
}

/// [B] string.
pub(crate) fn raw_str<'a>(o: &'a Obj, key: &str) -> Option<&'a str> {
    o.get(key).and_then(Json::as_str)
}

/// Whitespace-separated `"x y z"`; missing or non-numeric components are 0.
fn parse_vec3_str(s: &str) -> Vec3 {
    let mut c = [0.0f32; 3];
    for (slot, tok) in c.iter_mut().zip(s.split_whitespace()) {
        *slot = tok.parse().unwrap_or(0.0);
    }
    Vec3::from_array(c)
}

/// [B] vec3 of emitters/children: `"x y z"`, number (broadcast), or array of at least 3 numbers;
/// anything else gives `default`.
pub(crate) fn raw_vec3(o: &Obj, key: &str, default: Vec3) -> Vec3 {
    match o.get(key) {
        Some(Json::String(s)) => parse_vec3_str(s),
        Some(Json::Number(n)) => n.as_f64().map_or(default, |v| Vec3::splat(v as f32)),
        Some(Json::Array(a)) if a.len() >= 3 => {
            match (a[0].as_f64(), a[1].as_f64(), a[2].as_f64()) {
                (Some(x), Some(y), Some(z)) => Vec3::new(x as f32, y as f32, z as f32),
                _ => default,
            }
        }
        _ => default,
    }
}

/// [B] `sign`: only an array of at least 3 JSON integers; anything else gives `(0,0,0)`.
pub(crate) fn raw_sign(o: &Obj, key: &str) -> IVec3 {
    match o.get(key) {
        Some(Json::Array(a)) if a.len() >= 3 => match (a[0].as_i64(), a[1].as_i64(), a[2].as_i64())
        {
            (Some(x), Some(y), Some(z)) => IVec3::new(x as i32, y as i32, z as i32),
            _ => IVec3::ZERO,
        },
        _ => IVec3::ZERO,
    }
}

/// Control point `offset`: `"x y z"` (missing → 0) or number (broadcast); anything else → `(0,0,0)`.
pub(crate) fn raw_offset(o: &Obj, key: &str) -> Vec3 {
    match o.get(key) {
        Some(Json::String(s)) => parse_vec3_str(s),
        Some(Json::Number(n)) => Vec3::splat(n.as_f64().unwrap_or(0.0) as f32),
        _ => Vec3::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(j: Json) -> Obj {
        j.as_object().unwrap().clone()
    }

    #[test]
    fn user_scalar_and_vector_forms() {
        let o = obj(json!({
            "n": 2.5, "b": true, "s1": "3", "s3": "1 2 3", "s4": "1 2 3 4", "t": "abc",
            "arr": [1, 2, 3], "o": {"value": "4 5 6", "user": "speedprop"},
            "o2": {"value": 1, "user": {"name": "p", "condition": "x"}}
        }));
        assert_eq!(user_f(&o, "n", 9.0).f32(), 2.5);
        assert_eq!(user_f(&o, "missing", 9.0).f32(), 9.0);
        assert_eq!(user_f(&o, "b", 9.0).f32(), 1.0);
        assert_eq!(user_f(&o, "s1", 9.0).f32(), 3.0);
        // scalar broadcast to vec3
        assert_eq!(user_f(&o, "n", 0.0).vec3(), Vec3::splat(2.5));
        assert_eq!(user_f(&o, "s3", 0.0).vec3(), Vec3::new(1.0, 2.0, 3.0));
        // vector read as scalar → x
        assert_eq!(user_f(&o, "s3", 0.0).f32(), 1.0);
        // vec3 read as vec4 → w = 0
        assert_eq!(user_f(&o, "s3", 0.0).vec4(), Vec4::new(1.0, 2.0, 3.0, 0.0));
        assert_eq!(user_f(&o, "s4", 0.0).vec4(), Vec4::new(1.0, 2.0, 3.0, 4.0));
        assert_eq!(user_f(&o, "t", 7.0).f32(), 0.0);
        // array → zero, no fallback to the default
        assert_eq!(user_v3(&o, "arr", Vec3::ONE).vec3(), Vec3::ZERO);
        let b = user_f(&o, "o", 0.0);
        assert_eq!(b.vec3(), Vec3::new(4.0, 5.0, 6.0));
        assert_eq!(b.property(), Some("speedprop"));
        let b2 = user_f(&o, "o2", 0.0);
        assert_eq!(b2.property(), Some("p"));
        assert_eq!(b2.binding.as_ref().unwrap().condition, Some(json!("x")));
    }

    #[test]
    fn conditions_compare_text_properties() {
        // `visible` bound to combo `style` with condition "0" (shimmering_particles).
        let o = obj(json!({
            "v": {"value": true, "user": {"name": "style", "condition": "0"}},
            "n": {"value": true, "user": {"name": "style", "condition": 1}},
            "f": {"value": 2.0, "user": {"name": "speed", "condition": "1"}}
        }));
        let mut v = user(&o, "v", Value::Bool(false));
        v.set_from_json(&json!("0"));
        assert!(v.bool());
        v.set_from_json(&json!("1"));
        assert!(!v.bool());
        let mut n = user(&o, "n", Value::Bool(false));
        n.set_from_json(&json!("1"));
        assert!(n.bool());
        // A non-text property ignores the condition (spec 02 Q8).
        let mut f = user(&o, "f", Value::Number(0.0));
        f.set_from_json(&json!(0.5));
        assert_eq!(f.f32(), 0.5);
    }

    #[test]
    fn colors() {
        let close = |a: Vec3, b: Vec3| (a - b).abs().max_element() < 1e-6;
        let o = obj(json!({
            "h3": "#f00", "h6": "#00ff80", "h8": "#ffffff00", "ints": "255 0 51",
            "commas": "255,128,0", "floats": "1.0 0.5 0", "num": 0.25, "obj": {"value": "#fff"}
        }));
        assert!(close(
            color(&o, "h3", Vec3::ZERO).vec3(),
            Vec3::new(1.0, 0.0, 0.0)
        ));
        assert!(close(
            color(&o, "h6", Vec3::ZERO).vec3(),
            Vec3::new(0.0, 1.0, 128.0 / 255.0)
        ));
        assert_eq!(color(&o, "h8", Vec3::ZERO).vec4().w, 0.0);
        assert_eq!(color(&o, "h6", Vec3::ZERO).vec4().w, 1.0);
        assert!(close(
            color(&o, "ints", Vec3::ZERO).vec3(),
            Vec3::new(1.0, 0.0, 0.2)
        ));
        assert!(close(
            color(&o, "commas", Vec3::ZERO).vec3(),
            Vec3::new(1.0, 128.0 / 255.0, 0.0)
        ));
        assert!(close(
            color(&o, "floats", Vec3::ZERO).vec3(),
            Vec3::new(1.0, 0.5, 0.0)
        ));
        assert_eq!(color(&o, "num", Vec3::ZERO).vec3(), Vec3::splat(0.25));
        assert_eq!(color(&o, "obj", Vec3::ZERO).vec3(), Vec3::ONE);
        assert_eq!(
            color(&o, "missing", Vec3::new(0.1, 0.2, 0.3)).vec3(),
            Vec3::new(0.1, 0.2, 0.3)
        );
        let mut c = color(&o, "num", Vec3::ZERO);
        c.set_from_json(&json!("0 255 0"));
        assert_eq!(c.vec3(), Vec3::new(0.0, 1.0, 0.0));
    }

    #[test]
    fn raw_readers() {
        let o = obj(json!({
            "s": "1 -2 3", "short": "4", "n": 5, "a": [1, 2, 3, 4], "a2": [1, 2], "bad": {"x": 1},
            "sign": [1, -1, 0], "signf": [1.0, -1.0, 0.0], "i": 7.9, "neg": -3, "b": 1, "str": "x"
        }));
        assert_eq!(raw_vec3(&o, "s", Vec3::ONE), Vec3::new(1.0, -2.0, 3.0));
        assert_eq!(raw_vec3(&o, "short", Vec3::ONE), Vec3::new(4.0, 0.0, 0.0));
        assert_eq!(raw_vec3(&o, "n", Vec3::ONE), Vec3::splat(5.0));
        assert_eq!(raw_vec3(&o, "a", Vec3::ONE), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(raw_vec3(&o, "a2", Vec3::ONE), Vec3::ONE);
        assert_eq!(raw_vec3(&o, "bad", Vec3::ONE), Vec3::ONE);
        assert_eq!(raw_sign(&o, "sign"), IVec3::new(1, -1, 0));
        assert_eq!(raw_sign(&o, "signf"), IVec3::ZERO);
        assert_eq!(raw_i64(&o, "i", 0), 7);
        assert_eq!(raw_i64(&o, "neg", 0), -3);
        assert_eq!(raw_i64(&o, "str", 9), 9);
        assert!(raw_bool(&o, "b", false));
        assert_eq!(raw_offset(&o, "a"), Vec3::ZERO);
        assert_eq!(raw_offset(&o, "short"), Vec3::new(4.0, 0.0, 0.0));
        assert_eq!(raw_offset(&o, "n"), Vec3::splat(5.0));
    }
}

//! Multi-view dynamic value (spec 02 §4).
//!
//! Every view is computed when the value is written, so reading any view is a field load. The
//! float view is always `vec4.x` and the vec2/vec3 views are always prefixes of the vec4 view
//! (§4.2), so a single `Vec4` holds all vector and float views.

use std::fmt;
use std::sync::Arc;

use glam::{Vec2, Vec3, Vec4};

use crate::text::fmt_fixed6;

/// Type tag of a [`Value`] (§4.1). `Null` has numeric code 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ValueTag {
    /// No value: every view is zero / false.
    #[default]
    Null = 0,
    /// Float.
    Float = 1,
    /// 32-bit integer.
    Int = 2,
    /// Boolean.
    Bool = 3,
    /// 2-component vector.
    Vec2 = 4,
    /// 3-component vector.
    Vec3 = 5,
    /// 4-component vector (also colours).
    Vec4 = 6,
    /// Text.
    Text = 7,
}

/// A value seen simultaneously as float, integer, boolean, vec2, vec3, vec4 and text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Value {
    vec: Vec4,
    int: i32,
    boolean: bool,
    tag: ValueTag,
    text: Option<Arc<str>>,
}

/// Float to int by truncation toward zero (saturating; NaN gives 0).
#[inline]
fn trunc(f: f32) -> i32 {
    f as i32
}

impl Value {
    /// Null value (all views zero, empty text).
    pub const fn null() -> Self {
        Self {
            vec: Vec4::ZERO,
            int: 0,
            boolean: false,
            tag: ValueTag::Null,
            text: None,
        }
    }

    /// Float value.
    pub fn from_float(f: f32) -> Self {
        let mut v = Self::null();
        v.set_float(f);
        v
    }

    /// Integer value.
    pub fn from_int(i: i32) -> Self {
        let mut v = Self::null();
        v.set_int(i);
        v
    }

    /// Boolean value.
    pub fn from_bool(b: bool) -> Self {
        let mut v = Self::null();
        v.set_bool(b);
        v
    }

    /// Vec2 value.
    pub fn from_vec2(x: Vec2) -> Self {
        let mut v = Self::null();
        v.set_vec2(x);
        v
    }

    /// Vec3 value.
    pub fn from_vec3(x: Vec3) -> Self {
        let mut v = Self::null();
        v.set_vec3(x);
        v
    }

    /// Vec4 value.
    pub fn from_vec4(x: Vec4) -> Self {
        let mut v = Self::null();
        v.set_vec4(x);
        v
    }

    /// Colour value (tag Vec4 with the colour projection of §4.2).
    pub fn from_color(c: Vec4) -> Self {
        let mut v = Self::null();
        v.set_color(c);
        v
    }

    /// Text value.
    pub fn from_text(t: &str) -> Self {
        let mut v = Self::null();
        v.set_text(t.into());
        v
    }

    /// Type tag.
    #[inline]
    pub fn tag(&self) -> ValueTag {
        self.tag
    }

    /// Float view.
    #[inline]
    pub fn float(&self) -> f32 {
        self.vec.x
    }

    /// Integer view.
    #[inline]
    pub fn int(&self) -> i32 {
        self.int
    }

    /// Boolean view.
    #[inline]
    pub fn boolean(&self) -> bool {
        self.boolean
    }

    /// Vec2 view.
    #[inline]
    pub fn vec2(&self) -> Vec2 {
        self.vec.truncate().truncate()
    }

    /// Vec3 view.
    #[inline]
    pub fn vec3(&self) -> Vec3 {
        self.vec.truncate()
    }

    /// Vec4 view.
    #[inline]
    pub fn vec4(&self) -> Vec4 {
        self.vec
    }

    /// Text view.
    #[inline]
    pub fn text(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }

    /// Shared text view (no copy).
    pub fn text_arc(&self) -> Option<&Arc<str>> {
        self.text.as_ref()
    }

    fn set_numeric(&mut self, tag: ValueTag, vec: Vec4, int: i32, boolean: bool) {
        self.tag = tag;
        self.vec = vec;
        self.int = int;
        self.boolean = boolean;
        self.text = None;
    }

    /// Stores a float.
    pub fn set_float(&mut self, f: f32) {
        // parity: the boolean view of a float uses truncation, so 0.5 is false (Q24).
        let i = trunc(f);
        self.set_numeric(ValueTag::Float, Vec4::splat(f), i, i != 0);
    }

    /// Stores an integer.
    pub fn set_int(&mut self, i: i32) {
        self.set_numeric(ValueTag::Int, Vec4::splat(i as f32), i, i != 0);
    }

    /// Stores a boolean.
    pub fn set_bool(&mut self, b: bool) {
        let f = if b { 1.0 } else { 0.0 };
        self.set_numeric(ValueTag::Bool, Vec4::splat(f), i32::from(b), b);
    }

    /// Stores a vec2 (`z = w = 0`).
    pub fn set_vec2(&mut self, v: Vec2) {
        self.set_numeric(
            ValueTag::Vec2,
            v.extend(0.0).extend(0.0),
            trunc(v.x),
            v.x != 0.0,
        );
    }

    /// Stores a vec3 (`w = 0`).
    pub fn set_vec3(&mut self, v: Vec3) {
        self.set_numeric(ValueTag::Vec3, v.extend(0.0), trunc(v.x), v.x != 0.0);
    }

    /// Stores a vec4.
    pub fn set_vec4(&mut self, v: Vec4) {
        self.set_numeric(ValueTag::Vec4, v, trunc(v.x), v.x != 0.0);
    }

    /// Stores a colour: tag Vec4, integer view `trunc(255·r)`, boolean view `a ≠ 0`.
    pub fn set_color(&mut self, c: Vec4) {
        self.set_numeric(ValueTag::Vec4, c, trunc(255.0 * c.x), c.w != 0.0);
    }

    /// Stores a text (numeric views zero).
    pub fn set_text(&mut self, t: Arc<str>) {
        self.set_numeric(ValueTag::Text, Vec4::ZERO, 0, false);
        self.text = if t.is_empty() { None } else { Some(t) };
    }

    /// Stores a text compared with a condition: tag Bool, views `t == condition`, text kept.
    pub fn set_text_condition(&mut self, t: Option<Arc<str>>, condition: &str) {
        let m = t.as_deref().unwrap_or("") == condition;
        self.set_bool(m);
        self.text = t;
    }

    /// Makes the value Null. The text view is left unchanged.
    pub fn set_null(&mut self) {
        // parity: becoming Null does not clear the text view (Q23).
        self.tag = ValueTag::Null;
        self.vec = Vec4::ZERO;
        self.int = 0;
        self.boolean = false;
    }

    /// Copy performed by a binding (§4.4-1): Null source makes this Null; otherwise every view
    /// and the tag are copied, then a condition turns a Text source into a Bool.
    pub fn assign_bound(&mut self, source: &Value, condition: Option<&str>) {
        if source.tag == ValueTag::Null {
            self.set_null();
            return;
        }
        // parity: a Text source (combo) copied without condition gives zero numeric views, and a
        // condition is ignored for non-Text sources (Q8).
        self.clone_from(source);
        if let Some(c) = condition
            && source.tag == ValueTag::Text
        {
            self.set_text_condition(source.text.clone(), c);
        }
    }
}

/// Display form (§4.3): floats and vector components with 6 decimals.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::new();
        match self.tag {
            ValueTag::Null => s.push_str("unknown conversion for value of type 0"),
            ValueTag::Float => fmt_fixed6(&mut s, self.vec.x),
            ValueTag::Int => s.push_str(&self.int.to_string()),
            ValueTag::Bool => s.push(if self.boolean { '1' } else { '0' }),
            ValueTag::Vec2 | ValueTag::Vec3 | ValueTag::Vec4 => {
                let n = match self.tag {
                    ValueTag::Vec2 => 2,
                    ValueTag::Vec3 => 3,
                    _ => 4,
                };
                for (i, c) in self.vec.to_array().into_iter().take(n).enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    fmt_fixed6(&mut s, c);
                }
            }
            ValueTag::Text => s.push_str(self.text()),
        }
        f.write_str(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_table() {
        let v = Value::from_float(0.5);
        assert_eq!((v.tag(), v.int(), v.boolean()), (ValueTag::Float, 0, false));
        assert_eq!(v.vec3(), Vec3::splat(0.5));
        let v = Value::from_float(-1.7);
        assert_eq!((v.int(), v.boolean()), (-1, true));
        let v = Value::from_int(3);
        assert_eq!(
            (v.float(), v.boolean(), v.vec2()),
            (3.0, true, Vec2::splat(3.0))
        );
        let v = Value::from_bool(true);
        assert_eq!((v.float(), v.int(), v.vec4()), (1.0, 1, Vec4::ONE));
        let v = Value::from_vec2(Vec2::new(0.5, 2.0));
        assert_eq!((v.float(), v.int(), v.boolean()), (0.5, 0, true));
        assert_eq!(v.vec3(), Vec3::new(0.5, 2.0, 0.0));
        assert_eq!(v.vec4(), Vec4::new(0.5, 2.0, 0.0, 0.0));
        let v = Value::from_vec3(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(v.vec4(), Vec4::new(1.0, 2.0, 3.0, 0.0));
        let v = Value::from_color(Vec4::new(0.5, 0.0, 0.0, 0.0));
        assert_eq!(
            (v.tag(), v.int(), v.boolean()),
            (ValueTag::Vec4, 127, false)
        );
        let v = Value::from_text("abc");
        assert_eq!(
            (v.float(), v.int(), v.boolean(), v.text()),
            (0.0, 0, false, "abc")
        );
        let mut v = Value::null();
        v.set_text_condition(Some("1".into()), "1");
        assert_eq!(
            (v.tag(), v.float(), v.boolean(), v.text()),
            (ValueTag::Bool, 1.0, true, "1")
        );
    }

    #[test]
    fn null_keeps_text() {
        let mut v = Value::from_text("keep");
        v.set_null();
        assert_eq!(v.tag(), ValueTag::Null);
        assert_eq!(v.text(), "keep");
        assert_eq!(v.float(), 0.0);
        let mut v = Value::from_text("x");
        v.set_float(1.0);
        assert_eq!(v.text(), "");
    }

    #[test]
    fn binding_copy() {
        let mut v = Value::from_float(9.0);
        v.assign_bound(&Value::from_text("1"), Some("1"));
        assert_eq!((v.tag(), v.boolean()), (ValueTag::Bool, true));
        v.assign_bound(&Value::from_text("0"), Some("1"));
        assert!(!v.boolean());
        // Condition ignored for non-text sources.
        v.assign_bound(&Value::from_float(0.25), Some("1"));
        assert_eq!((v.tag(), v.float()), (ValueTag::Float, 0.25));
        // Combo without condition: numeric views are zero (Q8).
        v.assign_bound(&Value::from_text("1"), None);
        assert_eq!((v.tag(), v.float(), v.text()), (ValueTag::Text, 0.0, "1"));
        let mut v = Value::from_text("old");
        v.assign_bound(&Value::null(), None);
        assert_eq!((v.tag(), v.text()), (ValueTag::Null, "old"));
    }

    #[test]
    fn display() {
        assert_eq!(Value::from_float(1.5).to_string(), "1.500000");
        assert_eq!(Value::from_int(-4).to_string(), "-4");
        assert_eq!(Value::from_bool(true).to_string(), "1");
        assert_eq!(
            Value::from_vec3(Vec3::new(1.0, 0.0, 0.0)).to_string(),
            "1.000000, 0.000000, 0.000000"
        );
        assert_eq!(
            Value::from_color(Vec4::ONE).to_string(),
            "1.000000, 1.000000, 1.000000, 1.000000"
        );
        assert_eq!(Value::from_text("t").to_string(), "t");
        assert!(Value::null().to_string().ends_with('0'));
    }
}

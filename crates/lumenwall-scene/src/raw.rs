//! Lenient typed deserialization: one serde pass over the JSON bytes into short-lived structures
//! that borrow strings from the input and record the JSON type of every field instead of failing.
//! Type checks happen afterwards with the JSON path at hand, so a wrong type becomes a precise
//! [`crate::LoadError::Data`] while the only serde errors left are syntax errors.
//!
//! Object keys follow spec 02 §0.3: unknown keys are skipped, the last duplicate wins, and maps
//! are iterated in byte-wise key order ([`RawMap::sorted`]).

use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;

/// JSON type of a value, or [`JsonKind::Absent`] when the key is missing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JsonKind {
    /// Key not present.
    #[default]
    Absent,
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool,
    /// Any number.
    Number,
    /// A string.
    String,
    /// An array.
    Array,
    /// An object.
    Object,
}

impl JsonKind {
    /// Absent or `null` (optional reads treat both alike, §0.3).
    pub fn is_none(self) -> bool {
        matches!(self, Self::Absent | Self::Null)
    }
}

impl fmt::Display for JsonKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Absent => "nothing",
            Self::Null => "null",
            Self::Bool => "a boolean",
            Self::Number => "a number",
            Self::String => "a string",
            Self::Array => "an array",
            Self::Object => "an object",
        })
    }
}

/// Builds a lenient value from the JSON type found where it was expected.
pub trait FromKind {
    /// Value for a JSON value of the wrong type (its content is skipped).
    fn from_kind(kind: JsonKind) -> Self;
}

macro_rules! visit_kinds {
    (scalars) => {
        fn visit_bool<E: ::serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok($crate::raw::FromKind::from_kind(
                $crate::raw::JsonKind::Bool,
            ))
        }
        fn visit_i64<E: ::serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok($crate::raw::FromKind::from_kind(
                $crate::raw::JsonKind::Number,
            ))
        }
        fn visit_u64<E: ::serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok($crate::raw::FromKind::from_kind(
                $crate::raw::JsonKind::Number,
            ))
        }
        fn visit_f64<E: ::serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok($crate::raw::FromKind::from_kind(
                $crate::raw::JsonKind::Number,
            ))
        }
        fn visit_str<E: ::serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok($crate::raw::FromKind::from_kind(
                $crate::raw::JsonKind::String,
            ))
        }
        fn visit_unit<E: ::serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok($crate::raw::FromKind::from_kind(
                $crate::raw::JsonKind::Null,
            ))
        }
        fn visit_none<E: ::serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok($crate::raw::FromKind::from_kind(
                $crate::raw::JsonKind::Null,
            ))
        }
    };
    (seq) => {
        fn visit_seq<A: ::serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            while seq.next_element::<::serde::de::IgnoredAny>()?.is_some() {}
            Ok($crate::raw::FromKind::from_kind(
                $crate::raw::JsonKind::Array,
            ))
        }
    };
    (map) => {
        fn visit_map<A: ::serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            while map
                .next_entry::<::serde::de::IgnoredAny, ::serde::de::IgnoredAny>()?
                .is_some()
            {}
            Ok($crate::raw::FromKind::from_kind(
                $crate::raw::JsonKind::Object,
            ))
        }
    };
}

/// Declares a lenient object: a struct with a `kind` field plus one field per JSON key. A JSON
/// value that is not an object yields the default struct with `kind` set to its type.
macro_rules! raw_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident<$lt:lifetime> {
        $($(#[$fmeta:meta])* $field:ident : $ty:ty = $key:literal),* $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Debug, Default)]
        $vis struct $name<$lt> {
            /// JSON type found ([`JsonKind::Object`] when the fields were read).
            #[allow(dead_code)]
            pub kind: $crate::raw::JsonKind,
            $($(#[$fmeta])* pub $field: $ty,)*
            _lt: ::std::marker::PhantomData<&$lt ()>,
        }

        impl<'de: $lt, $lt> $name<$lt> {
            #[allow(dead_code)]
            pub(crate) fn from_map<A: ::serde::de::MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
                let mut out = Self { kind: $crate::raw::JsonKind::Object, ..Default::default() };
                while let Some(key) = map.next_key::<$crate::raw::Key<'de>>()? {
                    match &*key.0 {
                        $($key => out.$field = map.next_value()?,)*
                        _ => {
                            map.next_value::<::serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(out)
            }
        }

        impl<$lt> $crate::raw::FromKind for $name<$lt> {
            fn from_kind(kind: $crate::raw::JsonKind) -> Self {
                Self { kind, ..Default::default() }
            }
        }

        impl<'de: $lt, $lt> ::serde::Deserialize<'de> for $name<$lt> {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V<$lt>(::std::marker::PhantomData<&$lt ()>);
                impl<'de: $lt, $lt> ::serde::de::Visitor<'de> for V<$lt> {
                    type Value = $name<$lt>;
                    fn expecting(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                        f.write_str("any JSON value")
                    }
                    fn visit_map<A: ::serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                        $name::from_map(map)
                    }
                    $crate::raw::visit_kinds!(scalars);
                    $crate::raw::visit_kinds!(seq);
                }
                d.deserialize_any(V(::std::marker::PhantomData))
            }
        }
    };
}

pub(crate) use visit_kinds;

/// Object key, borrowed from the input when it has no escape sequence.
#[derive(Debug)]
pub struct Key<'a>(pub Cow<'a, str>);

impl<'de: 'a, 'a> Deserialize<'de> for Key<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Key<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Key<'a>, E> {
                Ok(Key(Cow::Borrowed(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Key<'a>, E> {
                Ok(Key(Cow::Owned(v.to_owned())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Key<'a>, E> {
                Ok(Key(Cow::Owned(v)))
            }
        }
        d.deserialize_str(V(PhantomData))
    }
}

/// A JSON scalar with its exact kind; arrays and objects are skipped and only their kind kept.
/// Integers and floats stay distinct (§0.3).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Json<'a> {
    /// Key not present.
    #[default]
    Absent,
    /// `null`.
    Null,
    /// Boolean.
    Bool(bool),
    /// Negative integer written without `.` or exponent.
    Int(i64),
    /// Non-negative integer written without `.` or exponent.
    UInt(u64),
    /// Number written with `.` or an exponent (or too large for 64 bits).
    Float(f64),
    /// String.
    Str(Cow<'a, str>),
    /// Array (content skipped).
    Array,
    /// Object (content skipped).
    Object,
}

impl Json<'_> {
    /// JSON type.
    pub fn kind(&self) -> JsonKind {
        match self {
            Self::Absent => JsonKind::Absent,
            Self::Null => JsonKind::Null,
            Self::Bool(_) => JsonKind::Bool,
            Self::Int(_) | Self::UInt(_) | Self::Float(_) => JsonKind::Number,
            Self::Str(_) => JsonKind::String,
            Self::Array => JsonKind::Array,
            Self::Object => JsonKind::Object,
        }
    }

    /// Absent or `null`.
    pub fn is_none(&self) -> bool {
        matches!(self, Self::Absent | Self::Null)
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Json<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Json<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Json<'a>, E> {
                Ok(Json::Bool(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Json<'a>, E> {
                Ok(if v < 0 {
                    Json::Int(v)
                } else {
                    Json::UInt(v as u64)
                })
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Json<'a>, E> {
                Ok(Json::UInt(v))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Json<'a>, E> {
                Ok(Json::Float(v))
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Json<'a>, E> {
                Ok(Json::Str(Cow::Borrowed(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Json<'a>, E> {
                Ok(Json::Str(Cow::Owned(v.to_owned())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Json<'a>, E> {
                Ok(Json::Str(Cow::Owned(v)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Json<'a>, E> {
                Ok(Json::Null)
            }
            fn visit_none<E: de::Error>(self) -> Result<Json<'a>, E> {
                Ok(Json::Null)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json<'a>, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Json::Array)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json<'a>, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Json::Object)
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// Lenient array.
#[derive(Debug)]
pub struct RawSeq<T> {
    /// JSON type found ([`JsonKind::Array`] when `items` was read).
    pub kind: JsonKind,
    /// Elements in order.
    pub items: Vec<T>,
}

impl<T> Default for RawSeq<T> {
    fn default() -> Self {
        Self {
            kind: JsonKind::Absent,
            items: Vec::new(),
        }
    }
}

impl<T> FromKind for RawSeq<T> {
    fn from_kind(kind: JsonKind) -> Self {
        Self {
            kind,
            items: Vec::new(),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for RawSeq<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = RawSeq<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<RawSeq<T>, A::Error> {
                let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(64));
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(RawSeq {
                    kind: JsonKind::Array,
                    items,
                })
            }
            visit_kinds!(scalars);
            visit_kinds!(map);
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// Lenient object with arbitrary keys, kept in document order until [`RawMap::sorted`].
#[derive(Debug)]
pub struct RawMap<'a, V> {
    /// JSON type found ([`JsonKind::Object`] when `entries` was read).
    pub kind: JsonKind,
    /// Entries in document order, duplicates included.
    pub entries: Vec<(Cow<'a, str>, V)>,
}

impl<V> Default for RawMap<'_, V> {
    fn default() -> Self {
        Self {
            kind: JsonKind::Absent,
            entries: Vec::new(),
        }
    }
}

impl<V> FromKind for RawMap<'_, V> {
    fn from_kind(kind: JsonKind) -> Self {
        Self {
            kind,
            entries: Vec::new(),
        }
    }
}

impl<'a, V> RawMap<'a, V> {
    /// Entries in byte-wise key order, the last duplicate winning (§0.3).
    pub fn sorted(&self) -> Vec<(&str, &V)> {
        let mut v: Vec<(&str, &V)> = self.entries.iter().map(|(k, v)| (&**k, v)).collect();
        // Stable sort keeps document order inside a run of equal keys.
        v.sort_by(|a, b| a.0.cmp(b.0));
        let mut out: Vec<(&str, &V)> = Vec::with_capacity(v.len());
        for e in v {
            match out.last_mut() {
                Some(last) if last.0 == e.0 => *last = e,
                _ => out.push(e),
            }
        }
        out
    }
}

impl<'de: 'a, 'a, V: Deserialize<'de>> Deserialize<'de> for RawMap<'a, V> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Vis<'a, V>(PhantomData<(&'a (), V)>);
        impl<'de: 'a, 'a, V: Deserialize<'de>> Visitor<'de> for Vis<'a, V> {
            type Value = RawMap<'a, V>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawMap<'a, V>, A::Error> {
                let mut entries = Vec::new();
                while let Some(k) = map.next_key::<Key<'de>>()? {
                    let v = map.next_value()?;
                    entries.push((k.0, v));
                }
                Ok(RawMap {
                    kind: JsonKind::Object,
                    entries,
                })
            }
            visit_kinds!(scalars);
            visit_kinds!(seq);
        }
        d.deserialize_any(Vis(PhantomData))
    }
}

/// A setting node (spec 02 §5.1): a scalar, or an object with `value`, `user`, `script`,
/// `scriptproperties`. Public so that other crates (particles) can embed settings in their own
/// typed structures and register them with [`crate::Project::add_setting`].
#[derive(Debug, Default)]
pub enum RawSetting<'a> {
    /// Key not present.
    #[default]
    Absent,
    /// Scalar, `null`, or array (arrays become Null values).
    Scalar(Json<'a>),
    /// Object form.
    Object(Box<RawSettingObject<'a>>),
}

impl RawSetting<'_> {
    /// Absent or `null`.
    pub fn is_none(&self) -> bool {
        matches!(self, Self::Absent | Self::Scalar(Json::Null))
    }
}

raw_struct! {
    /// Object form of a setting.
    pub struct RawSettingObject<'a> {
        /// `value` (required, §5.2-1).
        value: Json<'a> = "value",
        /// `user` binding.
        user: RawUser<'a> = "user",
        /// `script` source.
        script: Json<'a> = "script",
        /// `scriptproperties`.
        script_properties: RawMap<'a, RawSetting<'a>> = "scriptproperties",
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for RawSetting<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = RawSetting<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(RawSetting::Scalar(Json::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(RawSetting::Scalar(if v < 0 {
                    Json::Int(v)
                } else {
                    Json::UInt(v as u64)
                }))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(RawSetting::Scalar(Json::UInt(v)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(RawSetting::Scalar(Json::Float(v)))
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(RawSetting::Scalar(Json::Str(Cow::Borrowed(v))))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(RawSetting::Scalar(Json::Str(Cow::Owned(v.to_owned()))))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(RawSetting::Scalar(Json::Str(Cow::Owned(v))))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawSetting::Scalar(Json::Null))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawSetting::Scalar(Json::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(RawSetting::Scalar(Json::Array))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                Ok(RawSetting::Object(Box::new(RawSettingObject::from_map(
                    map,
                )?)))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

raw_struct! {
    /// Object form of `user`.
    pub struct RawUserObject<'a> {
        /// Property name.
        name: Json<'a> = "name",
        /// Condition text.
        condition: Json<'a> = "condition",
    }
}

/// `user` key of a setting.
#[derive(Debug, Default)]
pub enum RawUser<'a> {
    /// Key not present.
    #[default]
    Absent,
    /// `null`.
    Null,
    /// Property name.
    Name(Cow<'a, str>),
    /// `{ "name": …, "condition": … }`.
    Object(RawUserObject<'a>),
    /// Any other JSON type.
    Other(JsonKind),
}

impl FromKind for RawUser<'_> {
    fn from_kind(kind: JsonKind) -> Self {
        if kind == JsonKind::Null {
            Self::Null
        } else {
            Self::Other(kind)
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for RawUser<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = RawUser<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(RawUser::Name(Cow::Borrowed(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(RawUser::Name(Cow::Owned(v.to_owned())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(RawUser::Name(Cow::Owned(v)))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RawUser::Other(JsonKind::Bool))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(RawUser::Other(JsonKind::Number))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(RawUser::Other(JsonKind::Number))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(RawUser::Other(JsonKind::Number))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawUser::Null)
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawUser::Null)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                Ok(RawUser::Object(RawUserObject::from_map(map)?))
            }
            visit_kinds!(seq);
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// Element of a texture map (§11.1): a name, or anything that is ignored.
#[derive(Debug, Default)]
pub enum RawTexEntry<'a> {
    /// A string, or an object whose `name` is a string.
    Name(Cow<'a, str>),
    /// Ignored element (`null`, number, object without string `name`, …).
    #[default]
    Skip,
}

impl FromKind for RawTexEntry<'_> {
    fn from_kind(_: JsonKind) -> Self {
        Self::Skip
    }
}

raw_struct! {
    struct RawNamed<'a> {
        name: Json<'a> = "name",
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for RawTexEntry<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = RawTexEntry<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(RawTexEntry::Name(Cow::Borrowed(v)))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(RawTexEntry::Name(Cow::Owned(v)))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                Ok(match RawNamed::from_map(map)?.name {
                    Json::Str(s) => RawTexEntry::Name(s),
                    _ => RawTexEntry::Skip,
                })
            }
            // visit_str must override the scalar default to keep the name.
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(RawTexEntry::Name(Cow::Owned(v.to_owned())))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RawTexEntry::Skip)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(RawTexEntry::Skip)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(RawTexEntry::Skip)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(RawTexEntry::Skip)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawTexEntry::Skip)
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawTexEntry::Skip)
            }
            visit_kinds!(seq);
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// `particle` key of a scene object: a path, an inline definition kept as raw JSON text for
/// `lumenwall-particles`, or something else.
#[derive(Debug, Default)]
pub enum RawParticle<'a> {
    /// Key not present.
    #[default]
    Absent,
    /// String.
    Path(Cow<'a, str>),
    /// Object, kept verbatim.
    Inline(&'a RawValue),
    /// Any other JSON type.
    Other(JsonKind),
}

impl<'de: 'a, 'a> Deserialize<'de> for RawParticle<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw: &'de RawValue = Deserialize::deserialize(d)?;
        let text = raw.get();
        Ok(match text.as_bytes().first() {
            Some(b'"') => {
                let k: Key<'de> = serde_json::from_str(text).map_err(de::Error::custom)?;
                RawParticle::Path(k.0)
            }
            Some(b'{') => RawParticle::Inline(raw),
            Some(b'[') => RawParticle::Other(JsonKind::Array),
            Some(b'n') => RawParticle::Other(JsonKind::Null),
            Some(b't' | b'f') => RawParticle::Other(JsonKind::Bool),
            _ => RawParticle::Other(JsonKind::Number),
        })
    }
}

/// `objects` of a scene: an array, or an object whose values are taken in key order (§6.1).
#[derive(Debug, Default)]
pub(crate) struct RawObjects<'a> {
    pub kind: JsonKind,
    pub items: Vec<RawObject<'a>>,
}

impl FromKind for RawObjects<'_> {
    fn from_kind(kind: JsonKind) -> Self {
        Self {
            kind,
            items: Vec::new(),
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for RawObjects<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = RawObjects<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(RawObjects {
                    kind: JsonKind::Array,
                    items,
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries: Vec<(Cow<'de, str>, RawObject<'a>)> = Vec::new();
                while let Some(k) = map.next_key::<Key<'de>>()? {
                    entries.push((k.0, map.next_value()?));
                }
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                let mut items: Vec<RawObject<'a>> = Vec::with_capacity(entries.len());
                let mut last_key: Option<Cow<'de, str>> = None;
                for (k, v) in entries {
                    if last_key.as_ref() == Some(&k) {
                        if let Some(slot) = items.last_mut() {
                            *slot = v;
                        }
                    } else {
                        items.push(v);
                        last_key = Some(k);
                    }
                }
                Ok(RawObjects {
                    kind: JsonKind::Object,
                    items,
                })
            }
            visit_kinds!(scalars);
        }
        d.deserialize_any(V(PhantomData))
    }
}

// ---------------------------------------------------------------------------------------------
// project.json

raw_struct! {
    pub(crate) struct RawProject<'a> {
        ty: Json<'a> = "type",
        title: Json<'a> = "title",
        file: Json<'a> = "file",
        workshopid: Json<'a> = "workshopid",
        general: RawProjectGeneral<'a> = "general",
    }
}

raw_struct! {
    pub(crate) struct RawProjectGeneral<'a> {
        supportsaudioprocessing: Json<'a> = "supportsaudioprocessing",
        properties: RawMap<'a, RawPropertyDef<'a>> = "properties",
    }
}

raw_struct! {
    pub(crate) struct RawPropertyDef<'a> {
        ty: Json<'a> = "type",
        text: Json<'a> = "text",
        // Any JSON is needed here for `textinput` / `usershortcut` (§3.2). Transient.
        value: Option<serde_json::Value> = "value",
        min: Json<'a> = "min",
        max: Json<'a> = "max",
        step: Json<'a> = "step",
        options: RawSeq<RawComboOption<'a>> = "options",
    }
}

raw_struct! {
    pub(crate) struct RawComboOption<'a> {
        label: Json<'a> = "label",
        value: Json<'a> = "value",
    }
}

// ---------------------------------------------------------------------------------------------
// scene.json

raw_struct! {
    pub(crate) struct RawScene<'a> {
        camera: RawCamera<'a> = "camera",
        general: RawSceneGeneral<'a> = "general",
        objects: RawObjects<'a> = "objects",
    }
}

raw_struct! {
    pub(crate) struct RawCamera<'a> {
        center: Json<'a> = "center",
        eye: Json<'a> = "eye",
        up: Json<'a> = "up",
        nearz: RawSetting<'a> = "nearz",
        farz: RawSetting<'a> = "farz",
        fov: RawSetting<'a> = "fov",
        paths: RawSeq<Json<'a>> = "paths",
    }
}

raw_struct! {
    pub(crate) struct RawSceneGeneral<'a> {
        ambientcolor: RawSetting<'a> = "ambientcolor",
        skylightcolor: RawSetting<'a> = "skylightcolor",
        clearcolor: RawSetting<'a> = "clearcolor",
        camerafade: RawSetting<'a> = "camerafade",
        camerapreview: Json<'a> = "camerapreview",
        bloom: RawSetting<'a> = "bloom",
        bloomstrength: RawSetting<'a> = "bloomstrength",
        bloomthreshold: RawSetting<'a> = "bloomthreshold",
        cameraparallax: RawSetting<'a> = "cameraparallax",
        cameraparallaxamount: RawSetting<'a> = "cameraparallaxamount",
        cameraparallaxdelay: RawSetting<'a> = "cameraparallaxdelay",
        cameraparallaxmouseinfluence: RawSetting<'a> = "cameraparallaxmouseinfluence",
        camerashake: RawSetting<'a> = "camerashake",
        camerashakeamplitude: RawSetting<'a> = "camerashakeamplitude",
        camerashakeroughness: RawSetting<'a> = "camerashakeroughness",
        camerashakespeed: RawSetting<'a> = "camerashakespeed",
        nearz: RawSetting<'a> = "nearz",
        farz: RawSetting<'a> = "farz",
        fov: RawSetting<'a> = "fov",
        orthogonalprojection: RawOrtho<'a> = "orthogonalprojection",
        clearenabled: RawSetting<'a> = "clearenabled",
        transparentsorting: Json<'a> = "transparentsorting",
        hdr: Json<'a> = "hdr",
        perspectiveoverridefov: Json<'a> = "perspectiveoverridefov",
    }
}

raw_struct! {
    pub(crate) struct RawOrtho<'a> {
        auto: Json<'a> = "auto",
        width: Json<'a> = "width",
        height: Json<'a> = "height",
    }
}

raw_struct! {
    struct RawDependencyObject<'a> {
        id: Json<'a> = "id",
    }
}

/// Element of an object's `dependencies`: an id, or an object `{ "id", "index", "type" }` as
/// written by Wallpaper Engine for particle links (spec 05 A7.8); only `id` is used.
#[derive(Debug, Default)]
pub(crate) enum RawDependency<'a> {
    /// Scalar (or anything that is not an object).
    Value(Json<'a>),
    /// Object form: its `id`.
    Object(Json<'a>),
    #[default]
    Absent,
}

impl<'de: 'a, 'a> Deserialize<'de> for RawDependency<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = RawDependency<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(RawDependency::Value(Json::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(RawDependency::Value(if v < 0 {
                    Json::Int(v)
                } else {
                    Json::UInt(v as u64)
                }))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(RawDependency::Value(Json::UInt(v)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(RawDependency::Value(Json::Float(v)))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                // Strings are rejected later; the text itself is not needed.
                Ok(RawDependency::Value(Json::Str(Cow::Borrowed(""))))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawDependency::Value(Json::Null))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawDependency::Value(Json::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(RawDependency::Value(Json::Array))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                Ok(RawDependency::Object(
                    RawDependencyObject::from_map(map)?.id,
                ))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

raw_struct! {
    pub(crate) struct RawObject<'a> {
        id: Json<'a> = "id",
        name: Json<'a> = "name",
        dependencies: RawSeq<RawDependency<'a>> = "dependencies",
        parent: Json<'a> = "parent",
        origin: RawSetting<'a> = "origin",
        scale: RawSetting<'a> = "scale",
        angles: RawSetting<'a> = "angles",
        visible: RawSetting<'a> = "visible",
        image: Json<'a> = "image",
        sound: RawSeq<Json<'a>> = "sound",
        particle: RawParticle<'a> = "particle",
        text: RawSetting<'a> = "text",
        light: Json<'a> = "light",
        model: Json<'a> = "model",
        sprite: Json<'a> = "sprite",
        skin: Json<'a> = "skin",
        reflected: Json<'a> = "reflected",
        castshadow: Json<'a> = "castshadow",
        perspective: Json<'a> = "perspective",
        intensity: RawSetting<'a> = "intensity",
        radius: RawSetting<'a> = "radius",
        exponent: RawSetting<'a> = "exponent",
        shape: Json<'a> = "shape",
        solid: Json<'a> = "solid",
        alpha: RawSetting<'a> = "alpha",
        color: RawSetting<'a> = "color",
        horizontalalign: Json<'a> = "horizontalalign",
        alignment: Json<'a> = "alignment",
        verticalalign: Json<'a> = "verticalalign",
        size: RawSetting<'a> = "size",
        parallax_depth: RawSetting<'a> = "parallaxDepth",
        color_blend_mode: RawSetting<'a> = "colorBlendMode",
        brightness: RawSetting<'a> = "brightness",
        effects: RawSeq<RawImageEffect<'a>> = "effects",
        animationlayers: RawSeq<RawAnimationLayer<'a>> = "animationlayers",
        instance: RawInstance<'a> = "instance",
        playbackmode: Json<'a> = "playbackmode",
        font: Json<'a> = "font",
        pointsize: RawSetting<'a> = "pointsize",
        padding: RawSetting<'a> = "padding",
        instanceoverride: Option<serde_json::Value> = "instanceoverride",
    }
}

raw_struct! {
    pub(crate) struct RawImageEffect<'a> {
        file: Json<'a> = "file",
        id: Json<'a> = "id",
        name: Json<'a> = "name",
        visible: RawSetting<'a> = "visible",
        passes: RawSeq<RawPassOverride<'a>> = "passes",
    }
}

raw_struct! {
    pub(crate) struct RawPassOverride<'a> {
        id: Json<'a> = "id",
        combos: RawMap<'a, Json<'a>> = "combos",
        constantshadervalues: RawMap<'a, RawSetting<'a>> = "constantshadervalues",
        textures: RawSeq<RawTexEntry<'a>> = "textures",
        usertextures: RawSeq<RawTexEntry<'a>> = "usertextures",
    }
}

raw_struct! {
    pub(crate) struct RawAnimationLayer<'a> {
        id: Json<'a> = "id",
        rate: RawSetting<'a> = "rate",
        visible: RawSetting<'a> = "visible",
        blend: RawSetting<'a> = "blend",
        animation: RawSetting<'a> = "animation",
    }
}

raw_struct! {
    pub(crate) struct RawInstance<'a> {
        textures: RawSeq<RawTexEntry<'a>> = "textures",
        usertextures: RawSeq<RawTexEntry<'a>> = "usertextures",
    }
}

// ---------------------------------------------------------------------------------------------
// models, materials, effects

raw_struct! {
    pub(crate) struct RawModel<'a> {
        material: Json<'a> = "material",
        solidlayer: Json<'a> = "solidlayer",
        fullscreen: Json<'a> = "fullscreen",
        passthrough: Json<'a> = "passthrough",
        autosize: Json<'a> = "autosize",
        nopadding: Json<'a> = "nopadding",
        width: Json<'a> = "width",
        height: Json<'a> = "height",
        puppet: Json<'a> = "puppet",
    }
}

raw_struct! {
    pub(crate) struct RawMaterial<'a> {
        passes: RawSeq<RawMaterialPass<'a>> = "passes",
    }
}

raw_struct! {
    pub(crate) struct RawMaterialPass<'a> {
        shader: Json<'a> = "shader",
        blending: Json<'a> = "blending",
        cullmode: Json<'a> = "cullmode",
        depthtest: Json<'a> = "depthtest",
        depthwrite: Json<'a> = "depthwrite",
        depthtesting: Json<'a> = "depthtesting",
        depthwriting: Json<'a> = "depthwriting",
        usershadervalues: RawMap<'a, Json<'a>> = "usershadervalues",
        textures: RawSeq<RawTexEntry<'a>> = "textures",
        usertextures: RawSeq<RawTexEntry<'a>> = "usertextures",
        combos: RawMap<'a, Json<'a>> = "combos",
        constantshadervalues: RawMap<'a, RawSetting<'a>> = "constantshadervalues",
    }
}

raw_struct! {
    pub(crate) struct RawEffect<'a> {
        name: Json<'a> = "name",
        description: Json<'a> = "description",
        group: Json<'a> = "group",
        preview: Json<'a> = "preview",
        dependencies: RawSeq<Json<'a>> = "dependencies",
        passes: RawSeq<RawEffectPass<'a>> = "passes",
        fbos: RawSeq<RawFbo<'a>> = "fbos",
    }
}

raw_struct! {
    pub(crate) struct RawEffectPass<'a> {
        material: Json<'a> = "material",
        bind: RawSeq<RawBind<'a>> = "bind",
        command: Json<'a> = "command",
        source: Json<'a> = "source",
        target: Json<'a> = "target",
    }
}

raw_struct! {
    pub(crate) struct RawBind<'a> {
        index: Json<'a> = "index",
        name: Json<'a> = "name",
    }
}

raw_struct! {
    pub(crate) struct RawFbo<'a> {
        name: Json<'a> = "name",
        format: Json<'a> = "format",
        scale: Json<'a> = "scale",
        unique: Json<'a> = "unique",
    }
}

/// Parses a whole document. A UTF-8 byte order mark is skipped.
///
/// Lumenwall: a document that is not valid JSON is parsed again with its `//` and `/* */`
/// comments blanked out (Wallpaper Engine accepts them: `fantasticcar/materials/car/glass.json`,
/// spec 09 §1.11.2). The copy lives in `scratch`; valid documents are never copied.
pub(crate) fn parse_doc<'a, T: Deserialize<'a>>(
    file: &str,
    bytes: &'a [u8],
    scratch: &'a mut Option<Vec<u8>>,
) -> Result<T, crate::LoadError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match serde_json::from_slice(bytes) {
        Ok(v) => Ok(v),
        Err(e) => match strip_comments(bytes) {
            Some(clean) => {
                let clean: &'a [u8] = scratch.insert(clean);
                serde_json::from_slice(clean).map_err(|_| crate::LoadError::syntax(file, &e))
            }
            None => Err(crate::LoadError::syntax(file, &e)),
        },
    }
}

/// `bytes` with every comment outside strings replaced by spaces (line breaks kept), or `None`
/// when there is no comment.
pub(crate) fn strip_comments(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = bytes.to_vec();
    let mut found = false;
    let mut i = 0;
    let mut in_string = false;
    while i < out.len() {
        let c = out[i];
        if in_string {
            match c {
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match (c, out.get(i + 1)) {
            (b'"', _) => in_string = true,
            (b'/', Some(b'/')) => {
                found = true;
                while i < out.len() && out[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
                continue;
            }
            (b'/', Some(b'*')) => {
                found = true;
                let mut j = i + 2;
                while j + 1 < out.len() && !(out[j] == b'*' && out[j + 1] == b'/') {
                    j += 1;
                }
                let end = (j + 2).min(out.len());
                for b in &mut out[i..end] {
                    if *b != b'\n' {
                        *b = b' ';
                    }
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    found.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_blanked_outside_strings() {
        let src = b"{\n\t\"a\": \"x//y\", // c\n\t/* b */ \"b\": 1\n}";
        let out = strip_comments(src).unwrap();
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "{\n\t\"a\": \"x//y\",     \n\t        \"b\": 1\n}"
        );
        assert_eq!(strip_comments(b"{\"a\": \"//\"}"), None);
        let mut scratch = None;
        let m: RawMap<'_, Json<'_>> =
            parse_doc("t", b"{ \"a\": 1 //, \"b\": 2\n }", &mut scratch).unwrap();
        assert_eq!(m.entries.len(), 1);
    }

    #[test]
    fn json_kinds_and_borrowing() {
        let v: RawSeq<Json<'_>> =
            serde_json::from_str(r#"[1, -2, 1.0, 1e3, "a", "b\n", null, true, [1], {"a":1}]"#)
                .unwrap();
        assert_eq!(v.kind, JsonKind::Array);
        assert_eq!(v.items[0], Json::UInt(1));
        assert_eq!(v.items[1], Json::Int(-2));
        assert_eq!(v.items[2], Json::Float(1.0));
        assert_eq!(v.items[3], Json::Float(1000.0));
        assert!(matches!(&v.items[4], Json::Str(Cow::Borrowed("a"))));
        assert!(matches!(&v.items[5], Json::Str(Cow::Owned(s)) if s == "b\n"));
        assert_eq!(v.items[6], Json::Null);
        assert_eq!(v.items[7], Json::Bool(true));
        assert_eq!(v.items[8], Json::Array);
        assert_eq!(v.items[9], Json::Object);
    }

    #[test]
    fn lenient_struct_records_kind_and_last_duplicate_wins() {
        let b: RawBind<'_> =
            serde_json::from_str(r#"{"index": 1, "name": "a", "index": 2, "x": [1,{}]}"#).unwrap();
        assert_eq!(b.kind, JsonKind::Object);
        assert_eq!(b.index, Json::UInt(2));
        let b: RawBind<'_> = serde_json::from_str(r#"[1, 2]"#).unwrap();
        assert_eq!(b.kind, JsonKind::Array);
        assert_eq!(b.index, Json::Absent);
    }

    #[test]
    fn map_sorted_dedups_last() {
        let m: RawMap<'_, Json<'_>> =
            serde_json::from_str(r#"{"b": 1, "a": 2, "b": 3, "B": 4}"#).unwrap();
        let s = m.sorted();
        let keys: Vec<_> = s.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["B", "a", "b"]);
        assert_eq!(*s[2].1, Json::UInt(3));
    }

    #[test]
    fn settings_forms() {
        let s: RawSetting<'_> = serde_json::from_str(r#"{"value": "1 0 0", "user": {"name": "c", "condition": "x"}, "script": "s", "scriptproperties": {"k": 1}}"#).unwrap();
        let RawSetting::Object(o) = s else { panic!() };
        assert!(matches!(o.user, RawUser::Object(_)));
        assert_eq!(o.script_properties.entries.len(), 1);
        let s: RawSetting<'_> = serde_json::from_str("[1,2]").unwrap();
        assert!(matches!(s, RawSetting::Scalar(Json::Array)));
    }

    #[test]
    fn particle_field() {
        let o: RawObject<'_> =
            serde_json::from_str(r#"{"particle": {"maxcount": 5, "emitter": []}}"#).unwrap();
        let RawParticle::Inline(raw) = o.particle else {
            panic!()
        };
        assert_eq!(raw.get(), r#"{"maxcount": 5, "emitter": []}"#);
        let o: RawObject<'_> = serde_json::from_str(r#"{"particle": "particles/a.json"}"#).unwrap();
        assert!(matches!(
            o.particle,
            RawParticle::Path(Cow::Borrowed("particles/a.json"))
        ));
    }

    #[test]
    fn objects_as_map_follow_key_order() {
        let o: RawObjects<'_> =
            serde_json::from_str(r#"{"b": {"id": 2}, "a": {"id": 1}, "b": {"id": 3}}"#).unwrap();
        let ids: Vec<_> = o.items.iter().map(|i| i.id.clone()).collect();
        assert_eq!(ids, [Json::UInt(1), Json::UInt(3)]);
    }
}

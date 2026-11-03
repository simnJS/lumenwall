//! Strict conversions of JSON scalars (spec 02 §0.4, §0.5). Absent and `null` give the default
//! for optional reads (mode O) and an error for required ones (mode R).

use glam::{Vec2, Vec3};

use crate::error::{DataErr, DataError, Expected};
use crate::raw::Json;
use crate::text::vector_components;

pub(crate) fn kind_error(found: crate::raw::JsonKind, expected: Expected) -> DataErr {
    match found {
        crate::raw::JsonKind::Absent => DataErr::new(DataError::Missing),
        found => DataErr::new(DataError::Type { expected, found }),
    }
}

pub(crate) fn type_error(expected: Expected, j: &Json<'_>) -> DataErr {
    match j {
        Json::Absent => DataErr::new(DataError::Missing),
        other => DataErr::new(DataError::Type {
            expected,
            found: other.kind(),
        }),
    }
}

/// Number or boolean to a 32-bit integer: floats truncate toward zero, integers wrap modulo 2³².
pub(crate) fn json_i32(j: &Json<'_>) -> Option<i32> {
    Some(match j {
        Json::Int(i) => *i as i32,
        Json::UInt(u) => *u as i32,
        Json::Float(f) => *f as i32,
        Json::Bool(b) => i32::from(*b),
        _ => return None,
    })
}

/// Number or boolean to a float.
pub(crate) fn json_f32(j: &Json<'_>) -> Option<f32> {
    Some(match j {
        Json::Int(i) => *i as f32,
        Json::UInt(u) => *u as f32,
        Json::Float(f) => *f as f32,
        Json::Bool(b) => f32::from(u8::from(*b)),
        _ => return None,
    })
}

pub(crate) fn req_str<'j>(j: &'j Json<'_>) -> Result<&'j str, DataErr> {
    j.as_str().ok_or_else(|| type_error(Expected::String, j))
}

pub(crate) fn opt_str<'j>(j: &'j Json<'_>, default: &'j str) -> Result<&'j str, DataErr> {
    Ok(opt_str_opt(j)?.unwrap_or(default))
}

pub(crate) fn opt_str_opt<'j>(j: &'j Json<'_>) -> Result<Option<&'j str>, DataErr> {
    match j {
        Json::Absent | Json::Null => Ok(None),
        Json::Str(s) => Ok(Some(s)),
        other => Err(type_error(Expected::String, other)),
    }
}

pub(crate) fn req_i32(j: &Json<'_>) -> Result<i32, DataErr> {
    json_i32(j).ok_or_else(|| type_error(Expected::Integer, j))
}

pub(crate) fn opt_i32(j: &Json<'_>, default: i32) -> Result<i32, DataErr> {
    Ok(opt_i32_opt(j)?.unwrap_or(default))
}

pub(crate) fn opt_i32_opt(j: &Json<'_>) -> Result<Option<i32>, DataErr> {
    if j.is_none() {
        return Ok(None);
    }
    json_i32(j)
        .map(Some)
        .ok_or_else(|| type_error(Expected::Integer, j))
}

pub(crate) fn opt_f32(j: &Json<'_>, default: f32) -> Result<f32, DataErr> {
    if j.is_none() {
        return Ok(default);
    }
    json_f32(j).ok_or_else(|| type_error(Expected::Float, j))
}

pub(crate) fn opt_bool(j: &Json<'_>, default: bool) -> Result<bool, DataErr> {
    match j {
        Json::Absent | Json::Null => Ok(default),
        Json::Bool(b) => Ok(*b),
        other => Err(type_error(Expected::Bool, other)),
    }
}

/// Strict vec3 (§0.6-6): a number fills every component; a string needs exactly 2 spaces.
pub(crate) fn req_vec3(j: &Json<'_>) -> Result<Vec3, DataErr> {
    match j {
        Json::Str(s) => {
            let (n, c) = vector_components(s);
            if n != 3 {
                return Err(DataErr::new(DataError::VectorComponents {
                    expected: "3",
                    found: n,
                }));
            }
            Ok(Vec3::new(c[0], c[1], c[2]))
        }
        other => json_f32(other)
            .map(Vec3::splat)
            .ok_or_else(|| type_error(Expected::Vec3, other)),
    }
}

/// Strict optional vec2 (§0.6-6): a number fills both components; a string needs exactly 1 space.
pub(crate) fn opt_vec2(j: &Json<'_>, default: Vec2) -> Result<Vec2, DataErr> {
    match j {
        Json::Absent | Json::Null => Ok(default),
        Json::Str(s) => {
            let (n, c) = vector_components(s);
            if n != 2 {
                return Err(DataErr::new(DataError::VectorComponents {
                    expected: "2",
                    found: n,
                }));
            }
            Ok(Vec2::new(c[0], c[1]))
        }
        other => json_f32(other)
            .map(Vec2::splat)
            .ok_or_else(|| type_error(Expected::Vec2, other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    #[test]
    fn strict_integers() {
        assert_eq!(json_i32(&Json::Float(-2.9)), Some(-2));
        assert_eq!(json_i32(&Json::UInt(4_294_967_297)), Some(1));
        assert_eq!(json_i32(&Json::Bool(true)), Some(1));
        assert_eq!(json_i32(&Json::Str(Cow::Borrowed("1"))), None);
        assert!(req_i32(&Json::Absent).is_err());
        assert_eq!(opt_i32(&Json::Null, 7).unwrap(), 7);
    }

    #[test]
    fn strict_vectors() {
        assert_eq!(
            req_vec3(&Json::Str("1 2 3".into())).unwrap(),
            Vec3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(req_vec3(&Json::UInt(2)).unwrap(), Vec3::splat(2.0));
        assert!(req_vec3(&Json::Str("1 2".into())).is_err());
        assert!(req_vec3(&Json::Str("1 2 3 4".into())).is_err());
        assert_eq!(
            opt_vec2(&Json::Float(0.5), Vec2::ZERO).unwrap(),
            Vec2::splat(0.5)
        );
        assert!(opt_vec2(&Json::Str("1 2 3".into()), Vec2::ZERO).is_err());
        assert!(opt_bool(&Json::UInt(1), false).is_err());
    }
}

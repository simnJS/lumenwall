//! Uniform annotations (spec 03 §6) and conversion of material constants to
//! the declared uniform type (§7.4).

use serde_json::Value;

use super::RawAnnotation;
use crate::reflect::{UniformMember, UniformType};

/// Typed value of a uniform default or constant, as uploaded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UniformValue {
    Float(f32),
    Int(i32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
}

impl UniformValue {
    pub fn zero(ty: UniformType) -> Option<UniformValue> {
        Some(match ty {
            UniformType::FLOAT => UniformValue::Float(0.0),
            UniformType::INT => UniformValue::Int(0),
            UniformType::VEC2 => UniformValue::Vec2([0.0; 2]),
            UniformType::VEC3 => UniformValue::Vec3([0.0; 3]),
            UniformType::VEC4 => UniformValue::Vec4([0.0; 4]),
            _ => return None,
        })
    }

    /// Writes the value at `member.offset` (little endian, std140).
    /// Values whose type differs from the declared one are ignored (§12.4).
    pub fn write(&self, member: &UniformMember, buf: &mut [u8]) {
        let o = member.offset as usize;
        let put = |buf: &mut [u8], i: usize, b: [u8; 4]| {
            if let Some(dst) = buf.get_mut(o + 4 * i..o + 4 * i + 4) {
                dst.copy_from_slice(&b);
            }
        };
        match (*self, member.ty) {
            (UniformValue::Float(f), UniformType::FLOAT) => put(buf, 0, f.to_le_bytes()),
            (UniformValue::Int(v), UniformType::INT) => put(buf, 0, v.to_le_bytes()),
            (UniformValue::Vec2(v), UniformType::VEC2) => v.iter().enumerate().for_each(|(i, f)| put(buf, i, f.to_le_bytes())),
            (UniformValue::Vec3(v), UniformType::VEC3) => v.iter().enumerate().for_each(|(i, f)| put(buf, i, f.to_le_bytes())),
            (UniformValue::Vec4(v), UniformType::VEC4) => v.iter().enumerate().for_each(|(i, f)| put(buf, i, f.to_le_bytes())),
            _ => {}
        }
    }
}

/// Source value of a material constant (spec 03 §7.2), already resolved
/// (user property or script value applied by the caller).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConstantValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    /// Colour user property (r, g, b, a in 0..1).
    Color([f32; 4]),
    /// String without condition, or null.
    Null,
}

impl ConstantValue {
    /// Conversion table of spec 03 §7.4.
    pub fn to_uniform(self, ty: UniformType) -> Option<UniformValue> {
        let f4: [f32; 4] = match self {
            ConstantValue::Float(f) => [f; 4],
            ConstantValue::Int(i) => [i as f32; 4],
            ConstantValue::Bool(b) => [b as i32 as f32; 4],
            ConstantValue::Vec2([x, y]) => [x, y, 0.0, 0.0],
            ConstantValue::Vec3([x, y, z]) => [x, y, z, 0.0],
            ConstantValue::Vec4(v) | ConstantValue::Color(v) => v,
            ConstantValue::Null => [0.0; 4],
        };
        Some(match ty {
            UniformType::FLOAT => UniformValue::Float(f4[0]),
            UniformType::INT => UniformValue::Int(match self {
                ConstantValue::Int(i) => i,
                // parity: an integer uniform reads a colour as trunc(255 · r).
                ConstantValue::Color(c) => (255.0 * c[0]).trunc() as i32,
                _ => f4[0].trunc() as i32,
            }),
            UniformType::VEC2 => UniformValue::Vec2([f4[0], f4[1]]),
            UniformType::VEC3 => UniformValue::Vec3([f4[0], f4[1], f4[2]]),
            UniformType::VEC4 => UniformValue::Vec4(f4),
            _ => return None,
        })
    }
}

/// Annotated numeric uniform (`float`, `int`, `vecN`).
#[derive(Clone, Debug, PartialEq)]
pub struct UniformParam {
    pub uniform: String,
    pub ty: UniformType,
    /// Material key (`"material"`): the name used by `constantshadervalues`.
    /// Without it the uniform is never registered and keeps 0 (spec 03 §6.2).
    pub material_key: Option<String>,
    /// Default value (§6.3); zero when absent or malformed (see README).
    pub default: UniformValue,
}

/// Annotated sampler.
#[derive(Clone, Debug, PartialEq)]
pub struct SamplerParam {
    pub uniform: String,
    /// Slot from the character after `g_Texture` (§6.4); `None` for other names.
    pub slot: Option<u32>,
    /// Default texture name (`"default"`).
    pub default_texture: Option<String>,
    pub combo: Option<String>,
    pub require: Option<serde_json::Map<String, Value>>,
    pub require_any: bool,
    /// Raw `default` value (for texture combos, §5.3).
    pub raw_default: Option<Value>,
    pub comparison: bool,
}

#[derive(Clone, Debug, Default)]
pub struct UnitParams {
    pub uniforms: Vec<UniformParam>,
    pub samplers: Vec<SamplerParam>,
}

/// Leading decimal integer (C `strtol` style: spaces, sign, digits).
pub fn leading_int(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (sign, digits) = match t.as_bytes().first() {
        Some(b'-') => (-1, &t[1..]),
        Some(b'+') => (1, &t[1..]),
        _ => (1, t),
    };
    let n = digits.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 {
        return None;
    }
    digits[..n].parse::<i64>().ok().map(|v| sign * v)
}

/// Tolerant float parse (numeric prefix, otherwise 0).
pub fn lenient_float(s: &str) -> f32 {
    let t = s.trim_start();
    let b = t.as_bytes();
    let mut end = 0;
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut seen_exp = false;
    while end < b.len() {
        let c = b[end];
        match c {
            b'0'..=b'9' => seen_digit = true,
            b'+' | b'-' if end == 0 || matches!(b[end - 1], b'e' | b'E') => {}
            b'.' if !seen_dot && !seen_exp => seen_dot = true,
            b'e' | b'E' if seen_digit && !seen_exp => seen_exp = true,
            _ => break,
        }
        end += 1;
    }
    while end > 0 {
        if let Ok(v) = t[..end].parse::<f32>() {
            return v;
        }
        end -= 1;
    }
    0.0
}

fn parse_vector(v: &Value, n: usize, exact: bool, warn: &mut dyn FnMut(String)) -> Vec<f32> {
    let Some(s) = v.as_str() else {
        // The reference rejects the object; Lumen broadcasts the number.
        warn(format!("vector default is not a string: {v}"));
        return vec![v.as_f64().unwrap_or(0.0) as f32; n];
    };
    // Components are separated by single spaces; the count is spaces + 1.
    let parts: Vec<&str> = s.split(' ').collect();
    if (exact && parts.len() != n) || parts.len() < n {
        warn(format!("vector default '{s}' does not have {n} components"));
    }
    (0..n).map(|i| parts.get(i).map_or(0.0, |p| lenient_float(p))).collect()
}

fn numeric_default(ty: UniformType, v: Option<&Value>, warn: &mut dyn FnMut(String)) -> UniformValue {
    let Some(v) = v else {
        return UniformValue::zero(ty).expect("numeric type");
    };
    match ty {
        UniformType::FLOAT => UniformValue::Float(match v {
            Value::Number(n) => n.as_f64().unwrap_or(0.0) as f32,
            Value::Bool(b) => *b as i32 as f32,
            // parity: a float default given as a string keeps its integer part ("0.5" → 0).
            Value::String(s) => leading_int(s).unwrap_or_else(|| {
                warn(format!("non-numeric float default '{s}'"));
                0
            }) as f32,
            _ => 0.0,
        }),
        UniformType::INT => UniformValue::Int(match v {
            Value::Number(n) => n.as_f64().unwrap_or(0.0).trunc() as i32,
            Value::Bool(b) => *b as i32,
            Value::String(s) => leading_int(s).unwrap_or_else(|| {
                warn(format!("non-numeric int default '{s}'"));
                0
            }) as i32,
            _ => 0,
        }),
        UniformType::VEC2 => {
            let c = parse_vector(v, 2, true, warn);
            UniformValue::Vec2([c[0], c[1]])
        }
        UniformType::VEC3 => {
            let c = parse_vector(v, 3, true, warn);
            UniformValue::Vec3([c[0], c[1], c[2]])
        }
        _ => {
            let c = parse_vector(v, 4, false, warn);
            UniformValue::Vec4([c[0], c[1], c[2], c[3]])
        }
    }
}

/// Sampler slot: the single character after `g_Texture`, if it is a digit.
pub fn sampler_slot(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("g_Texture")?;
    let c = *rest.as_bytes().first()?;
    c.is_ascii_digit().then(|| (c - b'0') as u32)
}

/// Interprets the raw annotations of a unit.
pub fn interpret(raw: &[&RawAnnotation], warnings: &mut Vec<String>) -> UnitParams {
    let mut p = UnitParams::default();
    for a in raw {
        let mut warn = |m: String| warnings.push(format!("annotation of '{}': {m}", a.name));
        let material_key = a.json.get("material").and_then(Value::as_str).map(str::to_owned);
        match a.glsl_type.as_str() {
            "sampler2D" | "sampler2DComparison" => {
                let comparison = a.glsl_type == "sampler2DComparison";
                let default_texture = a.json.get("default").and_then(Value::as_str).map(str::to_owned);
                if comparison && a.json.get("default").is_none() {
                    warn("sampler2DComparison without default".into());
                }
                p.samplers.push(SamplerParam {
                    uniform: a.name.clone(),
                    slot: sampler_slot(&a.name),
                    default_texture,
                    combo: a.json.get("combo").and_then(Value::as_str).map(str::to_owned),
                    require: a.json.get("require").and_then(Value::as_object).cloned(),
                    require_any: a.json.get("requireany").and_then(Value::as_bool).unwrap_or(false),
                    raw_default: a.json.get("default").cloned(),
                    comparison,
                });
            }
            t => match UniformType::from_glsl_name(t) {
                Some(ty) => {
                    let dv = a.json.get("default");
                    if dv.is_none() && material_key.is_some() {
                        warn("no default value; using zero".into());
                    }
                    let default = numeric_default(ty, dv, &mut warn);
                    p.uniforms.push(UniformParam { uniform: a.name.clone(), ty, material_key, default });
                }
                None => warn(format!("unsupported annotated type '{t}'")),
            },
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflect::Stages;

    fn ann(ty: &str, name: &str, json: &str) -> RawAnnotation {
        RawAnnotation {
            glsl_type: ty.into(),
            name: name.into(),
            json: serde_json::from_str(json).unwrap(),
        }
    }

    #[test]
    fn typed_defaults() {
        let a = [
            ann("float", "a", r#"{"material":"a","default":0.25}"#),
            ann("float", "b", r#"{"material":"b","default":"2.7"}"#),
            ann("int", "c", r#"{"material":"c","default":3.9}"#),
            ann("vec3", "d", r#"{"material":"d","default":"1 0.5 abc"}"#),
            ann("vec4", "e", r#"{"material":"e","default":"1 2 3 4 5"}"#),
            ann("float", "f", r#"{"material":"f","default":true}"#),
            ann("sampler2D", "g_Texture10", r#"{"default":"util/white","combo":"MASK"}"#),
            ann("mat4", "m", r#"{"material":"m"}"#),
        ];
        let refs: Vec<&RawAnnotation> = a.iter().collect();
        let mut w = Vec::new();
        let p = interpret(&refs, &mut w);
        let d: Vec<UniformValue> = p.uniforms.iter().map(|u| u.default).collect();
        assert_eq!(
            d,
            vec![
                UniformValue::Float(0.25),
                UniformValue::Float(2.0),
                UniformValue::Int(3),
                UniformValue::Vec3([1.0, 0.5, 0.0]),
                UniformValue::Vec4([1.0, 2.0, 3.0, 4.0]),
                UniformValue::Float(1.0),
            ]
        );
        assert_eq!(p.samplers[0].slot, Some(1));
        assert_eq!(p.samplers[0].default_texture.as_deref(), Some("util/white"));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn constant_conversion_table() {
        use ConstantValue as C;
        assert_eq!(C::Vec3([1.0, 2.0, 3.0]).to_uniform(UniformType::VEC4), Some(UniformValue::Vec4([1.0, 2.0, 3.0, 0.0])));
        assert_eq!(C::Float(2.7).to_uniform(UniformType::INT), Some(UniformValue::Int(2)));
        assert_eq!(C::Float(-2.7).to_uniform(UniformType::INT), Some(UniformValue::Int(-2)));
        assert_eq!(C::Color([0.5, 0.0, 0.0, 1.0]).to_uniform(UniformType::INT), Some(UniformValue::Int(127)));
        assert_eq!(C::Bool(true).to_uniform(UniformType::VEC2), Some(UniformValue::Vec2([1.0, 1.0])));
        assert_eq!(C::Vec2([3.0, 4.0]).to_uniform(UniformType::FLOAT), Some(UniformValue::Float(3.0)));
        assert_eq!(C::Null.to_uniform(UniformType::VEC3), Some(UniformValue::Vec3([0.0; 3])));
        let m = UniformMember {
            name: "x".into(),
            ty: UniformType::VEC2,
            offset: 4,
            array_len: 0,
            array_stride: 0,
            stages: Stages::default(),
        };
        let mut buf = [0u8; 16];
        UniformValue::Vec2([1.0, 2.0]).write(&m, &mut buf);
        assert_eq!(&buf[4..8], &1.0f32.to_le_bytes());
        UniformValue::Float(9.0).write(&m, &mut buf);
        assert_eq!(&buf[4..8], &1.0f32.to_le_bytes());
    }

    #[test]
    fn number_parsing() {
        assert_eq!(leading_int(" -12abc"), Some(-12));
        assert_eq!(leading_int("util/white"), None);
        assert_eq!(lenient_float("1.5e2x"), 150.0);
        assert_eq!(lenient_float("abc"), 0.0);
        assert_eq!(lenient_float("-0.25"), -0.25);
    }
}

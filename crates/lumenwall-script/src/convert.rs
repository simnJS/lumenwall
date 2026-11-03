//! Conversions between host [`Value`]s and JavaScript values (spec 06 §4).

use rquickjs::{Array, Ctx, Function, Object, Persistent, Result, Value as JsValue};

use crate::hostobj;
use crate::shared::{Shared, Target};
use crate::util::{number, undefined};
use crate::value::{CallbackId, Value};
use crate::vector::{VecData, as_vec, components, new_vec_dyn, owned};

/// Nesting limit for arrays and records crossing the boundary (cycles are cut there).
const MAX_DEPTH: u32 = 16;

/// Host value → JS value (spec 06 §4.1). Vectors become live views on `view` when given,
/// copies otherwise.
pub(crate) fn to_js<'js>(
    ctx: &Ctx<'js>,
    sh: &Shared,
    v: &Value,
    view: Option<Target>,
) -> Result<JsValue<'js>> {
    Ok(match v {
        Value::Undefined => undefined(ctx),
        Value::Null => JsValue::new_null(ctx.clone()),
        Value::Bool(b) => JsValue::new_bool(ctx.clone(), *b),
        Value::Int(i) => JsValue::new_int(ctx.clone(), *i),
        Value::Float(f) => number(ctx, f64::from(*f)),
        Value::String(s) => rquickjs::String::from_str(ctx.clone(), s)?.into_value(),
        Value::Vec2(_) | Value::Vec3(_) | Value::Vec4(_) => {
            let n = v.vector_dim().unwrap_or(4);
            match view {
                Some(t) => new_vec_dyn(ctx, n, VecData::View(t))?,
                None => owned(ctx, n, v.vec4())?,
            }
        }
        Value::Mat3(m) => make_mat(ctx, sh, &m[..])?,
        Value::Mat4(m) => make_mat(ctx, sh, &m[..])?,
        Value::Object(r) => hostobj::wrap(ctx, sh, *r)?,
        Value::Array(items) => {
            let arr = Array::new(ctx.clone())?;
            for (i, item) in items.iter().enumerate() {
                arr.set(i, to_js(ctx, sh, item, None)?)?;
            }
            arr.into_value()
        }
        Value::Record(fields) => {
            let obj = Object::new(ctx.clone())?;
            for (k, item) in fields {
                obj.set(k.as_str(), to_js(ctx, sh, item, None)?)?;
            }
            obj.into_value()
        }
        Value::Callback(id) => {
            let js = sh.js.borrow();
            match js.callbacks.get(id.0 as usize).and_then(Option::as_ref) {
                Some((f, _)) => f.clone().restore(ctx)?.into_value(),
                None => undefined(ctx),
            }
        }
    })
}

fn make_mat<'js>(ctx: &Ctx<'js>, sh: &Shared, m: &[f32]) -> Result<JsValue<'js>> {
    let (m3, m4) = crate::globals::ensure_matrices(ctx, sh)?;
    let ctor = if m.len() == 9 { m3 } else { m4 };
    let arr = Array::new(ctx.clone())?;
    for (i, x) in m.iter().enumerate() {
        arr.set(i, f64::from(*x))?;
    }
    let from: Function = ctor.get("__fromArray")?;
    from.call((arr,))
}

/// Reads `m` of a `Mat3`/`Mat4` instance.
fn mat_of<'js>(ctx: &Ctx<'js>, sh: &Shared, obj: &Object<'js>) -> Result<Option<Value>> {
    let (m3, m4) = {
        let js = sh.js.borrow();
        (js.mat3.clone(), js.mat4.clone())
    };
    for (ctor, n) in [(m3, 9usize), (m4, 16)] {
        let Some(ctor) = ctor else { continue };
        let ctor = ctor.restore(ctx)?;
        if obj.is_instance_of(&ctor) {
            let arr: Array = obj.get("m")?;
            let mut out = [0.0f32; 16];
            for (i, slot) in out.iter_mut().enumerate().take(n) {
                *slot = arr.get::<Option<f64>>(i)?.unwrap_or(0.0) as f32;
            }
            return Ok(Some(if n == 9 {
                let mut m = [0.0; 9];
                m.copy_from_slice(&out[..9]);
                Value::Mat3(m)
            } else {
                Value::Mat4(out)
            }));
        }
    }
    Ok(None)
}

/// Retains a JS function for the host.
pub(crate) fn register_callback<'js>(ctx: &Ctx<'js>, sh: &Shared, f: Function<'js>) -> CallbackId {
    let mut js = sh.js.borrow_mut();
    let entry = Some((Persistent::save(ctx, f), sh.current.get()));
    if let Some(i) = js.callbacks.iter().position(Option::is_none) {
        js.callbacks[i] = entry;
        CallbackId(i as u32)
    } else {
        js.callbacks.push(entry);
        CallbackId((js.callbacks.len() - 1) as u32)
    }
}

/// JS value → host value for setters and method arguments.
pub(crate) fn from_js<'js>(ctx: &Ctx<'js>, sh: &Shared, v: JsValue<'js>) -> Result<Value> {
    from_js_depth(ctx, sh, v, 0)
}

fn from_js_depth<'js>(ctx: &Ctx<'js>, sh: &Shared, v: JsValue<'js>, depth: u32) -> Result<Value> {
    if let Some(i) = v.as_int() {
        return Ok(Value::Int(i));
    }
    if let Some(f) = v.as_float() {
        return Ok(Value::Float(f as f32));
    }
    if let Some(b) = v.as_bool() {
        return Ok(Value::Bool(b));
    }
    if v.is_null() {
        return Ok(Value::Null);
    }
    if let Some(s) = v.as_string() {
        return Ok(Value::String(s.to_string()?));
    }
    if !v.is_object() {
        return Ok(Value::Undefined);
    }
    if let Some((n, d)) = as_vec(sh, &v) {
        return Ok(Value::vector(n, components(ctx, sh, n, d)?));
    }
    if let Some(r) = hostobj::as_object_ref(ctx, sh, &v)? {
        return Ok(Value::Object(r));
    }
    if let Some(f) = v.as_function() {
        return Ok(Value::Callback(register_callback(ctx, sh, f.clone())));
    }
    if depth >= MAX_DEPTH {
        return Ok(Value::Undefined);
    }
    if let Some(arr) = v.as_array() {
        let mut items = Vec::with_capacity(arr.len());
        for item in arr.iter::<JsValue>() {
            items.push(from_js_depth(ctx, sh, item?, depth + 1)?);
        }
        return Ok(Value::Array(items));
    }
    let obj = v.as_object().expect("checked is_object");
    if let Some(m) = mat_of(ctx, sh, obj)? {
        return Ok(m);
    }
    let mut fields = Vec::new();
    for prop in obj.props::<String, JsValue>() {
        let (k, item) = prop?;
        fields.push((k, from_js_depth(ctx, sh, item, depth + 1)?));
    }
    Ok(Value::Record(fields))
}

/// Outcome of converting an `init`/`update` return value (spec 06 §4.2).
pub(crate) enum Returned {
    /// Leave the property untouched.
    Keep,
    /// Write this value.
    Set(Value),
    /// An object without numeric `x`/`y`: logged, property untouched.
    Invalid,
}

/// Converts the value returned by `init`/`update` (spec 06 §4.2).
pub(crate) fn from_return<'js>(ctx: &Ctx<'js>, sh: &Shared, v: JsValue<'js>) -> Result<Returned> {
    // Integer-tagged numbers become `Int`, others `Float` (spec 06 §4.2 rules 3 and 5).
    if let Some(i) = v.as_int() {
        return Ok(Returned::Set(Value::Int(i)));
    }
    if let Some(f) = v.as_float() {
        return Ok(Returned::Set(Value::Float(f as f32)));
    }
    if let Some(b) = v.as_bool() {
        return Ok(Returned::Set(Value::Bool(b)));
    }
    if let Some(s) = v.as_string() {
        return Ok(Returned::Set(Value::String(s.to_string()?)));
    }
    if v.is_null() {
        return Ok(Returned::Set(Value::Null));
    }
    // we-doc: "Return nothing to leave the property unmodified" (the reference resets the
    // value to Null, spec 06 §4.2 rule 2 / Q-M1-3).
    let Some(obj) = v.as_object() else {
        return Ok(Returned::Keep);
    };
    if let Some((n, d)) = as_vec(sh, &v) {
        return Ok(Returned::Set(Value::vector(n, components(ctx, sh, n, d)?)));
    }
    let num = |k: &str| -> Result<Option<f32>> {
        Ok(obj.get::<_, JsValue>(k)?.as_number().map(|x| x as f32))
    };
    let (Some(x), Some(y)) = (num("x")?, num("y")?) else {
        return Ok(Returned::Invalid);
    };
    Ok(Returned::Set(match num("z")? {
        None => Value::Vec2([x, y]),
        Some(z) => match num("w")? {
            None => Value::Vec3([x, y, z]),
            Some(w) => Value::Vec4([x, y, z, w]),
        },
    }))
}

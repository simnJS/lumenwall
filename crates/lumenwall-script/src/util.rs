//! Small helpers around rquickjs: raw native functions, errors, number formatting.

use std::fmt::Write as _;

use rquickjs::function::{IntoJsFunc, ParamRequirement, Params};
use rquickjs::object::{Accessor, Property};
use rquickjs::{Coerced, Ctx, Exception, Function, Object, Result, Value as JsValue};

/// Marker for [`RawFn`].
pub(crate) struct RawMarker;

/// A native function receiving the raw call parameters (`this`, argument count, values)
/// without any argument conversion or allocation by rquickjs.
pub(crate) struct RawFn<F>(pub F);

impl<'js, F> IntoJsFunc<'js, RawMarker> for RawFn<F>
where
    F: for<'a> Fn(Params<'a, 'js>) -> Result<JsValue<'js>> + 'js,
{
    fn param_requirements() -> ParamRequirement {
        ParamRequirement::any()
    }

    fn call<'a>(&self, params: Params<'a, 'js>) -> Result<JsValue<'js>> {
        (self.0)(params)
    }
}

/// Creates a named native function with a declared `length`.
pub(crate) fn func<'js, F>(ctx: &Ctx<'js>, name: &str, length: usize, f: F) -> Result<Function<'js>>
where
    F: for<'a> Fn(Params<'a, 'js>) -> Result<JsValue<'js>> + 'js,
{
    Function::new(ctx.clone(), RawFn(f))?
        .with_name(name)?
        .with_length(length)
}

/// Defines a method (writable, configurable, non-enumerable, like built-ins).
pub(crate) fn method<'js, F>(obj: &Object<'js>, name: &str, length: usize, f: F) -> Result<()>
where
    F: for<'a> Fn(Params<'a, 'js>) -> Result<JsValue<'js>> + 'js,
{
    let f = func(obj.ctx(), name, length, f)?;
    obj.prop(name, Property::from(f).writable().configurable())
}

/// Defines a getter with a setter that throws (`TypeError`), non-configurable.
pub(crate) fn readonly_getter<'js, F>(obj: &Object<'js>, name: &'static str, get: F) -> Result<()>
where
    F: for<'a> Fn(Params<'a, 'js>) -> Result<JsValue<'js>> + 'js,
{
    let setter = move |p: Params<'_, 'js>| -> Result<JsValue<'js>> {
        Err(type_error(p.ctx(), &format!("'{name}' is read-only")))
    };
    obj.prop(name, Accessor::new(RawFn(get), RawFn(setter)).enumerable())
}

/// Throws a `TypeError` with `msg`.
pub(crate) fn type_error(ctx: &Ctx<'_>, msg: &str) -> rquickjs::Error {
    Exception::throw_type(ctx, msg)
}

/// Throws a plain `Error` with `msg`.
pub(crate) fn plain_error(ctx: &Ctx<'_>, msg: &str) -> rquickjs::Error {
    Exception::throw_message(ctx, msg)
}

/// `undefined`.
pub(crate) fn undefined<'js>(ctx: &Ctx<'js>) -> JsValue<'js> {
    JsValue::new_undefined(ctx.clone())
}

/// JS `ToString`, with the fallback of spec 06 §6.5 when the conversion throws.
pub(crate) fn to_display_string<'js>(ctx: &Ctx<'js>, v: JsValue<'js>) -> String {
    if let Some(s) = v.as_string() {
        return s.to_string().unwrap_or_default();
    }
    coerce_string(ctx, v).unwrap_or_else(|| "[object]".to_owned())
}

/// JS `ToString`; `None` when the conversion throws (the exception is consumed so it cannot
/// resurface later, spec 06 §11).
pub(crate) fn coerce_string<'js>(ctx: &Ctx<'js>, v: JsValue<'js>) -> Option<String> {
    use rquickjs::FromJs;
    match Coerced::<String>::from_js(ctx, v) {
        Ok(s) => Some(s.0),
        Err(_) => {
            let _ = ctx.catch();
            None
        }
    }
}

/// Human-readable description of a caught exception value.
pub(crate) fn describe_exception<'js>(ctx: &Ctx<'js>, v: JsValue<'js>) -> String {
    if let Some(exc) = v.as_exception() {
        let mut out = String::new();
        let name: Option<String> = exc.get("name").ok();
        if let Some(name) = name {
            out.push_str(&name);
            out.push_str(": ");
        }
        out.push_str(&exc.message().unwrap_or_default());
        if let Some(stack) = exc.stack().filter(|s| !s.is_empty()) {
            out.push('\n');
            out.push_str(stack.trim_end());
        }
        return out;
    }
    format!("uncaught {}", to_display_string(ctx, v))
}

/// Appends `x` formatted like C `printf("%f")` (6 decimals; `nan`, `inf`, `-inf`).
pub(crate) fn push_fixed6(out: &mut String, x: f64) {
    if x.is_nan() {
        out.push_str("nan");
    } else if x.is_infinite() {
        out.push_str(if x > 0.0 { "inf" } else { "-inf" });
    } else {
        let _ = write!(out, "{x:.6}");
    }
}

/// JS number (rquickjs tags integral values that fit `i32` as integers).
pub(crate) fn number<'js>(ctx: &Ctx<'js>, x: f64) -> JsValue<'js> {
    JsValue::new_number(ctx.clone(), x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed6_matches_printf() {
        let mut s = String::new();
        for x in [
            1.5,
            -0.0,
            0.1f32 as f64,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e20,
        ] {
            push_fixed6(&mut s, x);
            s.push('|');
        }
        assert_eq!(
            s,
            "1.500000|-0.000000|0.100000|nan|inf|-inf|100000000000000000000.000000|"
        );
    }
}

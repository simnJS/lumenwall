//! Native modules `WEMath`, `WEColor`, `WEVector` (spec 06 §7, WE `lib.sceneScript.d.ts`).

use std::f64::consts::PI;

use rquickjs::function::Params;
use rquickjs::loader::{BuiltinResolver, ModuleLoader};
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::{Coerced, Ctx, FromJs, Object, Result, Value as JsValue};

use crate::util::{func, number, type_error};
use crate::vector::owned;

type P<'a, 'js> = Params<'a, 'js>;

/// Module names accepted by `import` (exact, case-sensitive; spec 06 §7.1).
pub(crate) const NAMES: [&str; 3] = ["WEMath", "WEColor", "WEVector"];

pub(crate) fn resolver() -> BuiltinResolver {
    NAMES
        .iter()
        .fold(BuiltinResolver::default(), |r, n| r.with_module(*n))
}

pub(crate) fn loader() -> ModuleLoader {
    ModuleLoader::default()
        .with_module("WEMath", WeMath)
        .with_module("WEColor", WeColor)
        .with_module("WEVector", WeVector)
}

fn numbers<const K: usize>(p: &P<'_, '_>, what: &str) -> Result<[f64; K]> {
    if p.len() != K {
        return Err(type_error(p.ctx(), &format!("{what} expects {K} numbers")));
    }
    let mut out = [0.0; K];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = p
            .arg(i)
            .and_then(|v| v.as_number())
            .ok_or_else(|| type_error(p.ctx(), &format!("{what} expects {K} numbers")))?;
    }
    Ok(out)
}

pub(crate) struct WeMath;

impl ModuleDef for WeMath {
    fn declare(decl: &Declarations<'_>) -> Result<()> {
        for name in ["smoothStep", "mix", "deg2rad", "rad2deg"] {
            decl.declare(name)?;
        }
        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> Result<()> {
        exports.export(
            "smoothStep",
            func(ctx, "smoothStep", 3, |p: P<'_, 'js>| {
                let [e0, e1, x] = numbers::<3>(&p, "smoothStep")?;
                // NaN propagates when e0 == e1 (clamp keeps NaN).
                let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
                Ok(number(p.ctx(), t * t * (3.0 - 2.0 * t)))
            })?,
        )?;
        // parity: `mix.length` is 1 (spec 06 §7.2).
        exports.export(
            "mix",
            func(ctx, "mix", 1, |p: P<'_, 'js>| {
                let [a, b, t] = numbers::<3>(&p, "mix")?;
                Ok(number(p.ctx(), a * (1.0 - t) + b * t))
            })?,
        )?;
        exports.export("deg2rad", PI / 180.0)?;
        exports.export("rad2deg", 180.0 / PI)?;
        Ok(())
    }
}

fn color_arg<'js>(p: &P<'_, 'js>, what: &str) -> Result<Object<'js>> {
    if p.len() != 1 {
        return Err(type_error(p.ctx(), &format!("{what} expects one color")));
    }
    p.arg(0)
        .and_then(|v| v.into_object())
        .ok_or_else(|| type_error(p.ctx(), &format!("{what} expects a color object")))
}

/// `ToNumber` of `obj[k]` (`undefined` → NaN).
fn to_number<'js>(ctx: &Ctx<'js>, obj: &Object<'js>, k: &str) -> Result<f64> {
    let v: JsValue = obj.get(k)?;
    Ok(Coerced::<f64>::from_js(ctx, v)?.0)
}

/// Strict number read: non-numbers throw (spec 06 §7.3 `normalizeColor`/`expandColor`).
fn strict_number<'js>(ctx: &Ctx<'js>, obj: &Object<'js>, k: &str, what: &str) -> Result<f64> {
    let v: JsValue = obj.get(k)?;
    v.as_number()
        .ok_or_else(|| type_error(ctx, &format!("{what}: component '{k}' must be a number")))
}

/// RGB → HSV with the hue in degrees, following spec 06 §7.3 to the bit.
pub(crate) fn rgb2hsv_degrees(x: f64, y: f64, z: f64) -> [f32; 3] {
    let (xf, yf, zf) = (x as f32, y as f32, z as f32);
    // f32::max/min return the other operand when one is NaN, as the spec requires.
    let max = xf.max(yf).max(zf);
    let min = xf.min(yf).min(zf);
    let v = max;
    if max == 0.0 || max - min == 0.0 {
        return [0.0, 0.0, v];
    }
    let s = (max - min) / max;
    let (m64, d) = (f64::from(max), f64::from(max) - f64::from(min));
    // parity: the f32 maximum is compared with the unrounded components.
    let mut h = if m64 == x {
        60.0 * (y - z) / d
    } else if m64 == y {
        60.0 * (z - x) / d + 120.0
    } else {
        60.0 * (x - y) / d + 240.0
    };
    if h < 0.0 {
        h += 360.0;
    }
    [h as f32, s, v]
}

/// HSV → RGB with the hue in degrees (spec 06 §7.3).
pub(crate) fn hsv2rgb_degrees(h: f64, s: f64, v: f64) -> [f32; 3] {
    let k = (h / 60.0).trunc() as i64;
    // Truncated remainder: −1 mod 6 = −1.
    let i = k % 6;
    let f = h / 60.0 - i as f64;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        5 => (v, p, q),
        _ => (0.0, 0.0, 0.0),
    };
    [r as f32, g as f32, b as f32]
}

pub(crate) struct WeColor;

impl ModuleDef for WeColor {
    fn declare(decl: &Declarations<'_>) -> Result<()> {
        for name in ["rgb2hsv", "hsv2rgb", "normalizeColor", "expandColor"] {
            decl.declare(name)?;
        }
        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> Result<()> {
        // we-doc: "normalized HSV" — the hue is returned in [0, 1) (the spec's reference
        // computation works in degrees, see README "Deviations").
        exports.export(
            "rgb2hsv",
            func(ctx, "rgb2hsv", 1, |p: P<'_, 'js>| {
                let ctx = p.ctx();
                let c = color_arg(&p, "rgb2hsv")?;
                let [h, s, v] = rgb2hsv_degrees(
                    to_number(ctx, &c, "x")?,
                    to_number(ctx, &c, "y")?,
                    to_number(ctx, &c, "z")?,
                );
                owned(ctx, 3, [(f64::from(h) / 360.0) as f32, s, v, 0.0])
            })?,
        )?;
        exports.export(
            "hsv2rgb",
            func(ctx, "hsv2rgb", 1, |p: P<'_, 'js>| {
                let ctx = p.ctx();
                let c = color_arg(&p, "hsv2rgb")?;
                let h = to_number(ctx, &c, "x")? * 360.0;
                let [r, g, b] =
                    hsv2rgb_degrees(h, to_number(ctx, &c, "y")?, to_number(ctx, &c, "z")?);
                owned(ctx, 3, [r, g, b, 0.0])
            })?,
        )?;
        for (name, factor) in [("normalizeColor", 1.0 / 255.0), ("expandColor", 255.0)] {
            exports.export(
                name,
                func(ctx, name, 1, move |p: P<'_, 'js>| {
                    let ctx = p.ctx();
                    let c = color_arg(&p, name)?;
                    let mut out = [0.0f32; 4];
                    for (slot, k) in out.iter_mut().zip(["x", "y", "z"]) {
                        let x = strict_number(ctx, &c, k, name)?;
                        *slot = if factor > 1.0 {
                            (x * factor) as f32
                        } else {
                            (x / 255.0) as f32
                        };
                    }
                    owned(ctx, 3, out)
                })?,
            )?;
        }
        Ok(())
    }
}

pub(crate) struct WeVector;

impl ModuleDef for WeVector {
    fn declare(decl: &Declarations<'_>) -> Result<()> {
        decl.declare("angleVector2")?;
        decl.declare("vectorAngle2")?;
        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> Result<()> {
        exports.export(
            "angleVector2",
            func(ctx, "angleVector2", 1, |p: P<'_, 'js>| {
                let ctx = p.ctx();
                let angle = match p.arg(0) {
                    Some(v) => Coerced::<f64>::from_js(ctx, v)?.0,
                    None => {
                        return Err(type_error(ctx, "angleVector2 expects an angle in degrees"));
                    }
                };
                let (s, c) = (angle * PI / 180.0).sin_cos();
                owned(ctx, 2, [c as f32, s as f32, 0.0, 0.0])
            })?,
        )?;
        exports.export(
            "vectorAngle2",
            func(ctx, "vectorAngle2", 1, |p: P<'_, 'js>| {
                let ctx = p.ctx();
                let Some(dir) = p.arg(0).and_then(|v| v.into_object()) else {
                    return Err(type_error(ctx, "vectorAngle2 expects a Vec2"));
                };
                let (x, y) = (to_number(ctx, &dir, "x")?, to_number(ctx, &dir, "y")?);
                Ok(number(ctx, y.atan2(x) * 180.0 / PI))
            })?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb2hsv_reference_cases() {
        assert_eq!(rgb2hsv_degrees(1.0, 0.0, 0.0), [0.0, 1.0, 1.0]);
        assert_eq!(rgb2hsv_degrees(0.0, 1.0, 0.0), [120.0, 1.0, 1.0]);
        assert_eq!(rgb2hsv_degrees(0.0, 0.0, 1.0), [240.0, 1.0, 1.0]);
        assert_eq!(rgb2hsv_degrees(1.0, 0.0, 1.0), [300.0, 1.0, 1.0]);
        assert_eq!(rgb2hsv_degrees(0.0, 0.0, 0.0), [0.0, 0.0, 0.0]);
        assert_eq!(rgb2hsv_degrees(0.5, 0.5, 0.5), [0.0, 0.0, 0.5]);
        // NaN components are ignored by max/min.
        assert_eq!(rgb2hsv_degrees(f64::NAN, 0.0, 0.0)[2], 0.0);
    }

    #[test]
    fn hsv2rgb_reference_cases() {
        assert_eq!(hsv2rgb_degrees(0.0, 1.0, 1.0), [1.0, 0.0, 0.0]);
        assert_eq!(hsv2rgb_degrees(120.0, 1.0, 1.0), [0.0, 1.0, 0.0]);
        assert_eq!(hsv2rgb_degrees(240.0, 1.0, 1.0), [0.0, 0.0, 1.0]);
        assert_eq!(hsv2rgb_degrees(60.0, 1.0, 0.5), [0.5, 0.5, 0.0]);
        // Hue in (-60, 0): k = 0, so i = 0 with a negative f.
        assert_eq!(hsv2rgb_degrees(-30.0, 1.0, 1.0), [1.0, -0.5, 0.0]);
        // Truncated remainder is negative → black.
        assert_eq!(hsv2rgb_degrees(-90.0, 1.0, 1.0), [0.0, 0.0, 0.0]);
        // h in [360, 420): i = 0 and f in [6, 7).
        assert_eq!(hsv2rgb_degrees(360.0, 0.5, 1.0), [1.0, 3.5, 0.5]);
        // parity: the f32 maximum (0.100000001…) differs from the f64 component 0.1, so
        // neither "max == x" nor "max == y" matches and the last branch gives 270°.
        assert_eq!(rgb2hsv_degrees(0.1, 0.05, 0.0)[0], 270.0);
    }
}

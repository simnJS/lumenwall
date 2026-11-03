//! `Vec2`, `Vec3`, `Vec4` native classes (spec 06 §5, WE `lib.sceneScript.d.ts`).
//!
//! One Rust type per dimension (`JsVec<N>`), components in `f32`. An instance either owns
//! its components (a copy) or is a live view on a host value (spec 06 §4.3). Component
//! accessors live on the prototype, so instances have no own properties.

use std::f64::consts::PI;
use std::rc::Rc;

use rquickjs::class::{JsClass, Trace, Tracer, Writable};
use rquickjs::function::Params;
use rquickjs::object::{Accessor, Property};
use rquickjs::{
    Class, Coerced, Constructor, Ctx, FromJs, JsLifetime, Object, Result, Value as JsValue, qjs,
};

use crate::shared::{Shared, Target};
use crate::util::{RawFn, method, number, push_fixed6, type_error, undefined};
use crate::value::Value;

/// Component names.
pub(crate) const COMPONENTS: [&str; 4] = ["x", "y", "z", "w"];

/// we-doc: `equals` "checks if one vector is equal (with epsilon)"; the epsilon is not
/// documented, 1e-5 absolute per component is used.
pub(crate) const EQUALS_EPSILON: f32 = 1e-5;

/// Storage behind a vector object.
#[derive(Clone, Copy, Debug)]
pub(crate) enum VecData {
    Owned([f32; 4]),
    View(Target),
}

/// Native vector class of dimension `N` (2, 3 or 4).
pub(crate) struct JsVec<const N: usize> {
    pub d: VecData,
}

// SAFETY: `JsVec` holds no JS values, so it has no lifetime to change.
unsafe impl<'js, const N: usize> JsLifetime<'js> for JsVec<N> {
    type Changed<'to> = JsVec<N>;
}

impl<'js, const N: usize> Trace<'js> for JsVec<N> {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

impl<'js, const N: usize> JsClass<'js> for JsVec<N> {
    const NAME: &'static str = match N {
        2 => "Vec2",
        3 => "Vec3",
        _ => "Vec4",
    };
    type Mutable = Writable;

    fn prototype(ctx: &Ctx<'js>) -> Result<Option<Object<'js>>> {
        Object::new(ctx.clone()).map(Some)
    }

    fn constructor(_ctx: &Ctx<'js>) -> Result<Option<Constructor<'js>>> {
        Ok(None)
    }
}

fn mask<const N: usize>(mut c: [f32; 4]) -> [f32; 4] {
    for v in c.iter_mut().skip(N) {
        *v = 0.0;
    }
    c
}

/// Projection of a host value to an `n`-component vector (spec 06 §5.2).
pub(crate) fn project(v: &Value, n: usize) -> [f32; 4] {
    let mut c = v.vec4();
    for x in c.iter_mut().skip(n) {
        *x = 0.0;
    }
    c
}

/// Creates a vector object of dimension `N`.
pub(crate) fn new_vec<'js, const N: usize>(ctx: &Ctx<'js>, d: VecData) -> Result<JsValue<'js>> {
    let d = match d {
        VecData::Owned(c) => VecData::Owned(mask::<N>(c)),
        v => v,
    };
    let obj = Class::instance(ctx.clone(), JsVec::<N> { d })?;
    // parity: assigning a non-component name throws instead of creating a property
    // (spec 06 §5.2, strict-mode modules).
    // SAFETY: `obj` is a live object of this context.
    unsafe { qjs::JS_PreventExtensions(ctx.as_raw().as_ptr(), obj.as_raw()) };
    Ok(obj.into_value())
}

/// Creates a vector object of runtime dimension `n` (2..=4).
pub(crate) fn new_vec_dyn<'js>(ctx: &Ctx<'js>, n: usize, d: VecData) -> Result<JsValue<'js>> {
    match n {
        2 => new_vec::<2>(ctx, d),
        3 => new_vec::<3>(ctx, d),
        _ => new_vec::<4>(ctx, d),
    }
}

/// Owned vector copy.
pub(crate) fn owned<'js>(ctx: &Ctx<'js>, n: usize, c: [f32; 4]) -> Result<JsValue<'js>> {
    new_vec_dyn(ctx, n, VecData::Owned(c))
}

/// Whether `v` is an instance of one of our native classes.
pub(crate) fn is_native(shared: &Shared, v: &JsValue<'_>) -> bool {
    v.is_object()
        // SAFETY: reading the class id of a live value has no side effect.
        && unsafe { qjs::JS_GetClassID(v.as_raw()) } == shared.class_id.get()
}

/// Dimension and storage of a vector object.
pub(crate) fn as_vec(shared: &Shared, v: &JsValue<'_>) -> Option<(usize, VecData)> {
    if !is_native(shared, v) {
        return None;
    }
    let obj = v.as_object()?;
    if let Some(c) = Class::<JsVec<2>>::from_object(obj) {
        return Some((2, c.borrow().d));
    }
    if let Some(c) = Class::<JsVec<3>>::from_object(obj) {
        return Some((3, c.borrow().d));
    }
    if let Some(c) = Class::<JsVec<4>>::from_object(obj) {
        return Some((4, c.borrow().d));
    }
    None
}

/// Current components of a vector with dimension `n`.
pub(crate) fn components(ctx: &Ctx<'_>, shared: &Shared, n: usize, d: VecData) -> Result<[f32; 4]> {
    match d {
        VecData::Owned(c) => Ok(c),
        VecData::View(t) => match shared.read_target(ctx, t)? {
            Some(v) => Ok(project(&v, n)),
            None => Err(type_error(ctx, "vector target no longer exists")),
        },
    }
}

fn this_class<'js, const N: usize>(
    shared: &Shared,
    p: &Params<'_, 'js>,
) -> Result<Class<'js, JsVec<N>>> {
    let this = p.this();
    if is_native(shared, &this)
        && let Some(c) = this.as_object().and_then(Class::<JsVec<N>>::from_object)
    {
        return Ok(c);
    }
    Err(type_error(
        p.ctx(),
        &format!("'this' is not a {}", <JsVec<N> as JsClass>::NAME),
    ))
}

fn this_vec<const N: usize>(shared: &Shared, p: &Params<'_, '_>) -> Result<[f32; 4]> {
    let c = this_class::<N>(shared, p)?;
    let d = c.borrow().d;
    components(p.ctx(), shared, N, d)
}

/// Argument conversion to an `N`-vector (spec 06 §5.3).
pub(crate) fn arg_vec<const N: usize>(
    ctx: &Ctx<'_>,
    shared: &Shared,
    v: Option<JsValue<'_>>,
) -> Result<[f32; 4]> {
    let Some(v) = v else {
        return Err(type_error(ctx, "missing vector argument"));
    };
    if let Some(x) = v.as_number() {
        return Ok(mask::<N>([x as f32; 4]));
    }
    if let Some((d, data)) = as_vec(shared, &v) {
        if d > N {
            return Err(type_error(ctx, "vector argument has too many components"));
        }
        return components(ctx, shared, d, data);
    }
    if let Some(obj) = v.as_object() {
        let num = |k: &str| -> Result<Option<f64>> { Ok(obj.get::<_, JsValue>(k)?.as_number()) };
        let (Some(x), Some(y)) = (num("x")?, num("y")?) else {
            return Err(type_error(ctx, "vector argument needs numeric x and y"));
        };
        let z = num("z")?;
        let w = num("w")?;
        if (N == 2 && z.is_some()) || (N <= 3 && w.is_some()) {
            return Err(type_error(ctx, "vector argument has too many components"));
        }
        let c = [
            x as f32,
            y as f32,
            z.unwrap_or(0.0) as f32,
            w.unwrap_or(0.0) as f32,
        ];
        return Ok(mask::<N>(c));
    }
    Err(type_error(ctx, "expected a number or a vector"))
}

/// Constructor argument (WE: number, string, any vector, `{x, y, …}`).
fn ctor_single<const N: usize>(ctx: &Ctx<'_>, shared: &Shared, v: JsValue<'_>) -> Result<[f32; 4]> {
    if let Some(x) = v.as_number() {
        return Ok(mask::<N>([x as f32; 4]));
    }
    if let Some(s) = v.as_string() {
        // we-doc: "parses the first numbers in the string".
        let s = s.to_string()?;
        let mut c = [0.0; 4];
        for (slot, tok) in c
            .iter_mut()
            .zip(
                s.split(|ch: char| ch.is_whitespace() || ch == ',')
                    .filter(|t| !t.is_empty()),
            )
            .take(N)
        {
            *slot = tok.parse::<f32>().unwrap_or(0.0);
        }
        return Ok(c);
    }
    if let Some((d, data)) = as_vec(shared, &v) {
        // we-doc: `new Vec2(vec3)` takes x and y.
        return Ok(mask::<N>(components(ctx, shared, d, data)?));
    }
    if let Some(obj) = v.as_object() {
        let num = |k: &str| -> Result<Option<f64>> { Ok(obj.get::<_, JsValue>(k)?.as_number()) };
        let (Some(x), Some(y)) = (num("x")?, num("y")?) else {
            return Err(type_error(ctx, "vector argument needs numeric x and y"));
        };
        let c = [
            x as f32,
            y as f32,
            num("z")?.unwrap_or(0.0) as f32,
            num("w")?.unwrap_or(0.0) as f32,
        ];
        return Ok(mask::<N>(c));
    }
    Err(type_error(ctx, "invalid vector constructor argument"))
}

fn dot<const N: usize>(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    (0..N).map(|i| a[i] * b[i]).sum()
}

fn map<const N: usize>(a: &[f32; 4], f: impl Fn(f32) -> f32) -> [f32; 4] {
    let mut r = [0.0; 4];
    for i in 0..N {
        r[i] = f(a[i]);
    }
    r
}

fn zip<const N: usize>(a: &[f32; 4], b: &[f32; 4], f: impl Fn(f32, f32) -> f32) -> [f32; 4] {
    let mut r = [0.0; 4];
    for i in 0..N {
        r[i] = f(a[i], b[i]);
    }
    r
}

fn deg(rad: f64) -> f64 {
    rad * 180.0 / PI
}

fn rad(deg: f64) -> f64 {
    deg * PI / 180.0
}

/// Text form of a vector (spec 06 §5.6; we-doc: components separated by one space so WE
/// can parse it back).
pub(crate) fn vec_to_string(
    ctx: &Ctx<'_>,
    shared: &Shared,
    n: usize,
    d: VecData,
) -> Result<String> {
    let mut out = String::new();
    let push_vec = |out: &mut String, c: &[f32]| {
        for (i, x) in c.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            push_fixed6(out, f64::from(*x));
        }
    };
    match d {
        VecData::Owned(c) => push_vec(&mut out, &c[..n]),
        VecData::View(t) => match shared.read_target(ctx, t)? {
            Some(Value::Vec2(c)) => push_vec(&mut out, &c),
            Some(Value::Vec3(c)) => push_vec(&mut out, &c),
            Some(Value::Vec4(c)) => push_vec(&mut out, &c),
            Some(Value::Float(f)) => push_fixed6(&mut out, f64::from(f)),
            Some(Value::Int(i)) => out.push_str(&i.to_string()),
            Some(Value::Bool(b)) => out.push(if b { '1' } else { '0' }),
            Some(Value::String(s)) => out.push_str(&s),
            _ => {}
        },
    }
    Ok(out)
}

type P<'a, 'js> = Params<'a, 'js>;

/// Installs the three vector classes and their global constructors.
pub(crate) fn install<'js>(
    ctx: &Ctx<'js>,
    shared: &Rc<Shared>,
    globals: &Object<'js>,
) -> Result<()> {
    install_dim::<2>(ctx, shared, globals)?;
    install_dim::<3>(ctx, shared, globals)?;
    install_dim::<4>(ctx, shared, globals)?;
    Ok(())
}

fn install_dim<'js, const N: usize>(
    ctx: &Ctx<'js>,
    shared: &Rc<Shared>,
    globals: &Object<'js>,
) -> Result<()> {
    let proto = Class::<JsVec<N>>::prototype(ctx)?.ok_or(rquickjs::Error::Unknown)?;

    for (i, name) in COMPONENTS.iter().enumerate().take(N) {
        let sh = shared.clone();
        let get = move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
            let c = this_vec::<N>(&sh, &p)?;
            Ok(number(p.ctx(), f64::from(c[i])))
        };
        let sh = shared.clone();
        let set = move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
            let ctx = p.ctx().clone();
            let cls = this_class::<N>(&sh, &p)?;
            let Some(x) = p.arg(0).and_then(|v| v.as_number()) else {
                return Err(type_error(&ctx, "vector components must be numbers"));
            };
            let d = cls.borrow().d;
            match d {
                VecData::Owned(mut c) => {
                    c[i] = x as f32;
                    cls.borrow_mut().d = VecData::Owned(c);
                }
                VecData::View(t) => {
                    let Some(cur) = sh.read_target(&ctx, t)? else {
                        return Err(type_error(&ctx, "vector target no longer exists"));
                    };
                    let mut c = project(&cur, N);
                    c[i] = x as f32;
                    sh.write_target(&ctx, t, &Value::vector(N, c))?;
                }
            }
            Ok(undefined(&ctx))
        };
        proto.prop(*name, Accessor::new(RawFn(get), RawFn(set)).configurable())?;
    }

    macro_rules! unary {
        ($name:literal, |$a:ident| $body:expr) => {{
            let sh = shared.clone();
            method(
                &proto,
                $name,
                0,
                move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                    let $a = this_vec::<N>(&sh, &p)?;
                    owned(p.ctx(), N, $body)
                },
            )?;
        }};
    }
    macro_rules! binary {
        ($name:literal, |$a:ident, $b:ident| $body:expr) => {{
            let sh = shared.clone();
            method(
                &proto,
                $name,
                1,
                move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                    let $a = this_vec::<N>(&sh, &p)?;
                    if p.len() != 1 {
                        // parity: wrong arity yields undefined (spec 06 §5.5, Q-VEC-2).
                        return Ok(undefined(p.ctx()));
                    }
                    let $b = arg_vec::<N>(p.ctx(), &sh, p.arg(0))?;
                    owned(p.ctx(), N, $body)
                },
            )?;
        }};
    }
    macro_rules! with_arg {
        ($name:literal, $len:literal, |$p:ident, $a:ident, $b:ident| $body:expr) => {{
            let sh = shared.clone();
            method(
                &proto,
                $name,
                $len,
                move |$p: P<'_, 'js>| -> Result<JsValue<'js>> {
                    let $a = this_vec::<N>(&sh, &$p)?;
                    let $b = arg_vec::<N>($p.ctx(), &sh, $p.arg(0))?;
                    $body
                },
            )?;
        }};
    }

    unary!("copy", |a| a);
    unary!("negate", |a| map::<N>(&a, |x| -x));
    unary!("normalize", |a| {
        let inv = 1.0 / dot::<N>(&a, &a).sqrt();
        map::<N>(&a, |x| x * inv)
    });
    unary!("abs", |a| map::<N>(&a, f32::abs));
    unary!("sign", |a| map::<N>(&a, |x| if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }));
    // Half away from zero (spec 06 §5.5), which is `f32::round`.
    unary!("round", |a| map::<N>(&a, f32::round));
    unary!("floor", |a| map::<N>(&a, f32::floor));
    unary!("ceil", |a| map::<N>(&a, f32::ceil));
    unary!("fract", |a| map::<N>(&a, |x| x - x.floor()));

    binary!("add", |a, b| zip::<N>(&a, &b, |x, y| x + y));
    binary!("subtract", |a, b| zip::<N>(&a, &b, |x, y| x - y));
    binary!("multiply", |a, b| zip::<N>(&a, &b, |x, y| x * y));
    binary!("divide", |a, b| zip::<N>(&a, &b, |x, y| x / y));
    binary!("min", |a, b| zip::<N>(&a, &b, f32::min));
    binary!("max", |a, b| zip::<N>(&a, &b, f32::max));

    {
        let sh = shared.clone();
        method(
            &proto,
            "dot",
            1,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                if p.len() != 1 {
                    return Ok(undefined(p.ctx()));
                }
                let b = arg_vec::<N>(p.ctx(), &sh, p.arg(0))?;
                Ok(number(p.ctx(), f64::from(dot::<N>(&a, &b))))
            },
        )?;
    }
    {
        let sh = shared.clone();
        method(
            &proto,
            "length",
            0,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                Ok(number(p.ctx(), f64::from(dot::<N>(&a, &a).sqrt())))
            },
        )?;
        let sh = shared.clone();
        method(
            &proto,
            "lengthSqr",
            0,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                Ok(number(p.ctx(), f64::from(dot::<N>(&a, &a))))
            },
        )?;
        let sh = shared.clone();
        method(
            &proto,
            "isFinite",
            0,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                Ok(JsValue::new_bool(
                    p.ctx().clone(),
                    a[..N].iter().all(|x| x.is_finite()),
                ))
            },
        )?;
    }
    with_arg!("distance", 1, |p, a, b| {
        let d = zip::<N>(&a, &b, |x, y| x - y);
        Ok(number(p.ctx(), f64::from(dot::<N>(&d, &d).sqrt())))
    });
    with_arg!("distanceSqr", 1, |p, a, b| {
        let d = zip::<N>(&a, &b, |x, y| x - y);
        Ok(number(p.ctx(), f64::from(dot::<N>(&d, &d))))
    });
    with_arg!("reflect", 1, |p, a, n| {
        let k = 2.0 * dot::<N>(&a, &n);
        owned(p.ctx(), N, zip::<N>(&a, &n, |x, y| x - k * y))
    });
    with_arg!("project", 1, |p, a, b| {
        let k = dot::<N>(&a, &b) / dot::<N>(&b, &b);
        owned(p.ctx(), N, map::<N>(&b, |y| y * k))
    });
    // GLSL semantics: x - y * floor(x / y).
    with_arg!("mod", 1, |p, a, b| owned(
        p.ctx(),
        N,
        zip::<N>(&a, &b, |x, y| x - y * (x / y).floor())
    ));
    with_arg!("step", 1, |p, a, e| owned(
        p.ctx(),
        N,
        zip::<N>(&a, &e, |x, edge| if x < edge { 0.0 } else { 1.0 })
    ));
    {
        let sh = shared.clone();
        method(
            &proto,
            "clamp",
            2,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                let lo = arg_vec::<N>(p.ctx(), &sh, p.arg(0))?;
                let hi = arg_vec::<N>(p.ctx(), &sh, p.arg(1))?;
                let mut r = [0.0; 4];
                for i in 0..N {
                    r[i] = a[i].max(lo[i]).min(hi[i]);
                }
                owned(p.ctx(), N, r)
            },
        )?;
        let sh = shared.clone();
        method(
            &proto,
            "smoothStep",
            2,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                let e0 = arg_vec::<N>(p.ctx(), &sh, p.arg(0))?;
                let e1 = arg_vec::<N>(p.ctx(), &sh, p.arg(1))?;
                let mut r = [0.0; 4];
                for i in 0..N {
                    let t = ((a[i] - e0[i]) / (e1[i] - e0[i])).clamp(0.0, 1.0);
                    r[i] = t * t * (3.0 - 2.0 * t);
                }
                owned(p.ctx(), N, r)
            },
        )?;
        let sh = shared.clone();
        method(
            &proto,
            "mix",
            2,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                if p.len() != 2 {
                    return Err(type_error(p.ctx(), "mix expects 2 arguments"));
                }
                let b = arg_vec::<N>(p.ctx(), &sh, p.arg(0))?;
                let t = p.arg(1).expect("checked arity");
                // we-doc: the amount may also be a vector (per component).
                let t: [f64; 4] = match t.as_number() {
                    Some(x) => [x; 4],
                    None if as_vec(&sh, &t).is_some() => {
                        arg_vec::<N>(p.ctx(), &sh, Some(t))?.map(f64::from)
                    }
                    None => return Err(type_error(p.ctx(), "mix amount must be a number")),
                };
                let mut r = [0.0; 4];
                for i in 0..N {
                    r[i] = (f64::from(a[i]) * (1.0 - t[i]) + f64::from(b[i]) * t[i]) as f32;
                }
                owned(p.ctx(), N, r)
            },
        )?;
        let sh = shared.clone();
        method(
            &proto,
            "equals",
            1,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                let Some(o) = p.arg(0) else {
                    return Ok(JsValue::new_bool(p.ctx().clone(), false));
                };
                let Some((d, data)) = as_vec(&sh, &o).filter(|(d, _)| *d == N) else {
                    return Err(type_error(
                        p.ctx(),
                        "equals expects a vector of the same dimension",
                    ));
                };
                let b = components(p.ctx(), &sh, d, data)?;
                let eq = (0..N).all(|i| (a[i] - b[i]).abs() <= EQUALS_EPSILON);
                Ok(JsValue::new_bool(p.ctx().clone(), eq))
            },
        )?;
        let sh = shared.clone();
        method(
            &proto,
            "toString",
            0,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let cls = this_class::<N>(&sh, &p)?;
                let d = cls.borrow().d;
                let s = vec_to_string(p.ctx(), &sh, N, d)?;
                rquickjs::String::from_str(p.ctx().clone(), &s).map(|s| s.into_value())
            },
        )?;
    }

    if N == 2 {
        unary!("perpendicular", |a| [-a[1], a[0], 0.0, 0.0]);
        let sh = shared.clone();
        method(
            &proto,
            "angle",
            0,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                Ok(number(p.ctx(), deg(f64::from(a[1]).atan2(f64::from(a[0])))))
            },
        )?;
        with_arg!("angleBetween", 1, |p, a, b| {
            let cross = f64::from(a[0]) * f64::from(b[1]) - f64::from(a[1]) * f64::from(b[0]);
            let d = f64::from(a[0]) * f64::from(b[0]) + f64::from(a[1]) * f64::from(b[1]);
            Ok(number(p.ctx(), f64::from(deg(cross.atan2(d)) as f32)))
        });
        let sh = shared.clone();
        method(
            &proto,
            "rotate",
            1,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?;
                let Some(angle) = p.arg(0).and_then(|v| v.as_number()) else {
                    return Err(type_error(p.ctx(), "rotate expects an angle in degrees"));
                };
                let (s, c) = rad(angle).sin_cos();
                let (x, y) = (f64::from(a[0]), f64::from(a[1]));
                owned(
                    p.ctx(),
                    N,
                    [(x * c - y * s) as f32, (x * s + y * c) as f32, 0.0, 0.0],
                )
            },
        )?;
    }

    if N == 3 {
        binary!("cross", |a, b| [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
            0.0
        ]);
        with_arg!("angleBetween", 1, |p, a, b| {
            let la = dot::<N>(&a, &a).sqrt();
            let lb = dot::<N>(&b, &b).sqrt();
            let c = f64::from(dot::<N>(&a, &b) / (la * lb)).clamp(-1.0, 1.0);
            Ok(number(p.ctx(), f64::from(deg(c.acos()) as f32)))
        });
        let sh = shared.clone();
        method(
            &proto,
            "refract",
            2,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let i = this_vec::<N>(&sh, &p)?;
                let n = arg_vec::<N>(p.ctx(), &sh, p.arg(0))?;
                let Some(eta) = p.arg(1).and_then(|v| v.as_number()) else {
                    return Err(type_error(p.ctx(), "refract expects a numeric eta"));
                };
                let eta = eta as f32;
                let d = dot::<N>(&n, &i);
                let k = 1.0 - eta * eta * (1.0 - d * d);
                let r = if k < 0.0 {
                    [0.0; 4]
                } else {
                    let s = eta * d + k.sqrt();
                    zip::<N>(&i, &n, |x, y| eta * x - s * y)
                };
                owned(p.ctx(), N, r)
            },
        )?;
        // Spherical coordinates: theta = polar angle from +Y, phi = azimuth around Y measured
        // from +Z toward +X, both in degrees (WE documents the axes, not the azimuth origin).
        let sh = shared.clone();
        method(
            &proto,
            "toSpherical",
            0,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let a = this_vec::<N>(&sh, &p)?.map(f64::from);
                let r = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
                let theta = if r > 0.0 {
                    (a[1] / r).clamp(-1.0, 1.0).acos()
                } else {
                    0.0
                };
                let phi = a[0].atan2(a[2]);
                owned(
                    p.ctx(),
                    3,
                    [r as f32, deg(theta) as f32, deg(phi) as f32, 0.0],
                )
            },
        )?;
    }

    let sh = shared.clone();
    let ctor = Constructor::new_class::<JsVec<N>, _, _>(
        ctx.clone(),
        RawFn(move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
            let ctx = p.ctx().clone();
            let c = match p.len() {
                // we-doc: `new Vec2()` is the zero vector.
                0 => [0.0; 4],
                1 => ctor_single::<N>(&ctx, &sh, p.arg(0).expect("one argument"))?,
                // we-doc: one argument per component (missing = 0, others ToNumber).
                len => {
                    let mut c = [0.0; 4];
                    for (i, slot) in c.iter_mut().enumerate().take(N.min(len)) {
                        let v = p.arg(i).expect("argument in range");
                        *slot = Coerced::<f64>::from_js(&ctx, v)?.0 as f32;
                    }
                    c
                }
            };
            new_vec::<N>(&ctx, VecData::Owned(c))
        }),
    )?;
    ctor.set_length(N)?;

    if N == 3 {
        let from_spherical = crate::util::func(
            ctx,
            "fromSpherical",
            3,
            move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let ctx = p.ctx().clone();
                let mut a = [0.0f64; 3];
                for (i, slot) in a.iter_mut().enumerate() {
                    *slot = match p.arg(i) {
                        Some(v) => Coerced::<f64>::from_js(&ctx, v)?.0,
                        None => 0.0,
                    };
                }
                let (r, theta, phi) = (a[0], rad(a[1]), rad(a[2]));
                let c = [
                    (r * theta.sin() * phi.sin()) as f32,
                    (r * theta.cos()) as f32,
                    (r * theta.sin() * phi.cos()) as f32,
                    0.0,
                ];
                owned(&ctx, 3, c)
            },
        )?;
        ctor.prop(
            "fromSpherical",
            Property::from(from_spherical).writable().configurable(),
        )?;
    }

    globals.prop(
        <JsVec<N> as JsClass>::NAME,
        Property::from(ctor).writable().configurable(),
    )?;
    Ok(())
}

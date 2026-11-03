//! Global objects: `engine`, `input`, `thisScene`, `console`, `shared`, `localStorage`,
//! `MediaPlaybackEvent`, `renderContext`, plus the natives used by the JS prelude.

use std::rc::Rc;
use std::time::{Duration, Instant};

use rquickjs::function::Params;
use rquickjs::object::{Accessor, Property};
use rquickjs::{
    Array, Coerced, Ctx, FromJs, Function, Object, Persistent, Result, TypedArray, Value as JsValue,
};

use crate::convert::to_js;
use crate::host::LogLevel;
use crate::hostobj::{self, host_call, layer_arg, layer_object};
use crate::shared::{AudioRegistration, Shared, Target, Timer};
use crate::storage::StorageFile;
use crate::util::{
    RawFn, func, method, number, readonly_getter, to_display_string, type_error, undefined,
};
use crate::value::{ObjectRef, Value};
use crate::vector::{as_vec, components, owned};

type P<'a, 'js> = Params<'a, 'js>;

fn bool_value<'js>(ctx: &Ctx<'js>, b: bool) -> JsValue<'js> {
    JsValue::new_bool(ctx.clone(), b)
}

/// Defines a global the way spec 06 §3.3 requires (enumerable, read-only, non-configurable).
pub(crate) fn define_fixed<'js>(
    globals: &Object<'js>,
    name: &str,
    v: impl rquickjs::IntoJs<'js>,
) -> Result<()> {
    globals.prop(name, Property::from(v).enumerable())
}

/// `engine` (spec 06 §6.1, WE `IEngine`).
pub(crate) fn engine<'js>(
    ctx: &Ctx<'js>,
    sh: &Rc<Shared>,
    user_props: Object<'js>,
) -> Result<Object<'js>> {
    let e = Object::new_proto(ctx.clone(), None)?;
    let s = sh.clone();
    readonly_getter(&e, "frametime", move |p: P<'_, 'js>| {
        Ok(number(p.ctx(), f64::from(s.frame.get().clock.frametime)))
    })?;
    let s = sh.clone();
    readonly_getter(&e, "runtime", move |p: P<'_, 'js>| {
        Ok(number(p.ctx(), f64::from(s.frame.get().clock.runtime)))
    })?;
    let s = sh.clone();
    readonly_getter(&e, "timeOfDay", move |p: P<'_, 'js>| {
        Ok(number(p.ctx(), f64::from(s.frame.get().clock.time_of_day)))
    })?;
    let s = sh.clone();
    readonly_getter(&e, "screenResolution", move |p: P<'_, 'js>| {
        let [w, h] = s.frame.get().display.screen_resolution;
        owned(p.ctx(), 2, [w, h, 0.0, 0.0])
    })?;
    let s = sh.clone();
    readonly_getter(&e, "canvasSize", move |p: P<'_, 'js>| {
        let [w, h] = s.frame.get().display.canvas_size;
        owned(p.ctx(), 2, [w, h, 0.0, 0.0])
    })?;
    e.prop("userProperties", Property::from(user_props).enumerable())?;
    for res in [16, 32, 64] {
        e.prop(
            format!("AUDIO_RESOLUTION_{res}"),
            Property::from(res).enumerable(),
        )?;
    }

    let s = sh.clone();
    method(&e, "isRunningInEditor", 0, move |p: P<'_, 'js>| {
        Ok(bool_value(p.ctx(), s.frame.get().display.editor))
    })?;
    let s = sh.clone();
    method(&e, "isPortrait", 0, move |p: P<'_, 'js>| {
        let [w, h] = s.frame.get().display.screen_resolution;
        Ok(bool_value(p.ctx(), h > w))
    })?;
    let s = sh.clone();
    method(&e, "isLandscape", 0, move |p: P<'_, 'js>| {
        let [w, h] = s.frame.get().display.screen_resolution;
        Ok(bool_value(p.ctx(), w >= h))
    })?;
    method(&e, "isDesktopDevice", 0, |p: P<'_, 'js>| {
        Ok(bool_value(p.ctx(), true))
    })?;
    method(&e, "isMobileDevice", 0, |p: P<'_, 'js>| {
        Ok(bool_value(p.ctx(), false))
    })?;
    let s = sh.clone();
    method(&e, "isWallpaper", 0, move |p: P<'_, 'js>| {
        Ok(bool_value(p.ctx(), !s.frame.get().display.screensaver))
    })?;
    let s = sh.clone();
    method(&e, "isScreensaver", 0, move |p: P<'_, 'js>| {
        Ok(bool_value(p.ctx(), s.frame.get().display.screensaver))
    })?;

    // we-doc: returns whether a shortcut ran (the reference returns undefined).
    let s = sh.clone();
    method(&e, "openUserShortcut", 1, move |p: P<'_, 'js>| {
        let name = p
            .arg(0)
            .map(|v| to_display_string(p.ctx(), v))
            .unwrap_or_default();
        let ran = s
            .host
            .with(|h| h.open_user_shortcut(&name))
            .unwrap_or(false);
        Ok(bool_value(p.ctx(), ran))
    })?;

    let s = sh.clone();
    method(&e, "registerAsset", 2, move |p: P<'_, 'js>| {
        let ctx = p.ctx();
        let path = p
            .arg(0)
            .map(|v| to_display_string(ctx, v))
            .unwrap_or_default();
        let precache = p.arg(1).and_then(|v| v.as_bool()).unwrap_or(false);
        match s.host.with(|h| h.register_asset(&path, precache)).flatten() {
            Some(id) => hostobj::wrap(ctx, &s, ObjectRef::Asset(id)),
            None => Ok(undefined(ctx)),
        }
    })?;

    let s = sh.clone();
    method(&e, "registerAudioBuffers", 1, move |p: P<'_, 'js>| {
        register_audio(&s, &p)
    })?;

    for interval in [false, true] {
        let s = sh.clone();
        let name = if interval {
            "setInterval"
        } else {
            "setTimeout"
        };
        method(&e, name, 1, move |p: P<'_, 'js>| {
            set_timer(&s, &p, interval)
        })?;
    }
    Ok(e)
}

fn register_audio<'js>(sh: &Rc<Shared>, p: &P<'_, 'js>) -> Result<JsValue<'js>> {
    let ctx = p.ctx();
    let res = p.arg(0).and_then(|v| v.as_number()).map(|n| n as i64);
    let Some(res @ (16 | 32 | 64)) = res else {
        return Err(type_error(
            ctx,
            "registerAudioBuffers expects engine.AUDIO_RESOLUTION_16, _32 or _64",
        ));
    };
    let res = res as usize;
    if let Some(r) = sh.js.borrow().audio.iter().find(|r| r.resolution == res) {
        return Ok(r.buffers.clone().restore(ctx)?.into_value());
    }
    let obj = Object::new(ctx.clone())?;
    let mut arrays = Vec::with_capacity(3);
    for name in ["left", "right", "average"] {
        let arr = TypedArray::<f32>::new(ctx.clone(), vec![0.0f32; res])?;
        obj.set(name, arr.clone())?;
        arrays.push(Persistent::save(ctx, arr.into_object()));
    }
    let average = arrays.pop().expect("three arrays");
    let right = arrays.pop().expect("three arrays");
    let left = arrays.pop().expect("three arrays");
    sh.js.borrow_mut().audio.push(AudioRegistration {
        resolution: res,
        buffers: Persistent::save(ctx, obj.clone()),
        left,
        right,
        average,
    });
    Ok(obj.into_value())
}

/// Copies this frame's spectrum into every registered `AudioBuffers` (spec: WE
/// `AudioBuffers`, updated every frame).
pub(crate) fn fill_audio(ctx: &Ctx<'_>, sh: &Shared, scratch: &mut Vec<f32>) -> Result<()> {
    let count = sh.js.borrow().audio.len();
    for i in 0..count {
        let (res, arrays) = {
            let js = sh.js.borrow();
            let r = &js.audio[i];
            (
                r.resolution,
                [r.left.clone(), r.right.clone(), r.average.clone()],
            )
        };
        scratch.clear();
        scratch.resize(res * 3, 0.0);
        let (lr, avg) = scratch.split_at_mut(res * 2);
        let (left, right) = lr.split_at_mut(res);
        if !sh
            .host
            .with(|h| h.audio_spectrum(left, right))
            .unwrap_or(false)
        {
            left.fill(0.0);
            right.fill(0.0);
        }
        for i in 0..res {
            avg[i] = (left[i] + right[i]) * 0.5;
        }
        for (arr, data) in arrays.into_iter().zip([&*left, &*right, &*avg]) {
            let arr = TypedArray::<f32>::from_object(arr.restore(ctx)?)?;
            if let Some(raw) = arr.as_raw() {
                let bytes = raw.len().min(data.len() * 4);
                // SAFETY: `raw` points to the live backing store of `arr` (no JS runs
                // between `as_raw` and this copy) and at most `raw.len()` bytes are written.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        data.as_ptr().cast::<u8>(),
                        raw.as_ptr().cast::<u8>(),
                        bytes,
                    );
                }
            }
        }
    }
    Ok(())
}

/// `engine.setTimeout` / `engine.setInterval` (spec 06 §10).
fn set_timer<'js>(sh: &Rc<Shared>, p: &P<'_, 'js>, interval: bool) -> Result<JsValue<'js>> {
    let ctx = p.ctx();
    let Some(callback) = p.arg(0).and_then(|v| v.into_function()) else {
        return Err(type_error(ctx, "timer callback must be a function"));
    };
    let delay = match p.arg(1) {
        Some(v) => Coerced::<i32>::from_js(ctx, v)?.0,
        None => 0,
    };
    // Réf: a negative delay counts as 0.
    let period = Duration::from_millis(u64::from(delay.max(0).unsigned_abs()));
    let now = sh.host.with(|h| h.now()).unwrap_or_else(Instant::now);
    let timer = Timer {
        due: now + period,
        period,
        callback: Persistent::save(ctx, callback),
        owner: sh.current.get(),
    };
    let id = {
        let mut t = sh.timers.borrow_mut();
        if interval {
            t.next_interval += 1;
            let id = t.next_interval;
            t.intervals.insert(id, timer);
            id
        } else {
            t.next_timeout += 1;
            let id = t.next_timeout;
            t.timeouts.insert(id, timer);
            id
        }
    };
    // we-doc: the returned function cancels this timer (any arguments are ignored).
    let s = sh.clone();
    let cancel = func(ctx, "cancel", 2, move |p: P<'_, 'js>| {
        let removed = {
            let mut t = s.timers.borrow_mut();
            if interval {
                t.intervals.remove(&id)
            } else {
                t.timeouts.remove(&id)
            }
        };
        drop(removed);
        Ok(undefined(p.ctx()))
    })?;
    Ok(cancel.into_value())
}

/// `input` (spec 06 §6.2, WE `IInput`).
pub(crate) fn input<'js>(ctx: &Ctx<'js>, sh: &Rc<Shared>) -> Result<Object<'js>> {
    let o = Object::new_proto(ctx.clone(), None)?;
    let s = sh.clone();
    // we-doc: screen position in pixels (the reference returns normalized scene UVs).
    readonly_getter(&o, "cursorScreenPosition", move |p: P<'_, 'js>| {
        let [x, y] = s.frame.get().cursor.screen_position;
        owned(p.ctx(), 2, [x, y, 0.0, 0.0])
    })?;
    let s = sh.clone();
    readonly_getter(&o, "cursorWorldPosition", move |p: P<'_, 'js>| {
        let [x, y, z] = s.frame.get().cursor.world_position;
        owned(p.ctx(), 3, [x, y, z, 0.0])
    })?;
    let s = sh.clone();
    readonly_getter(&o, "cursorLeftDown", move |p: P<'_, 'js>| {
        Ok(bool_value(p.ctx(), s.frame.get().cursor.left_down))
    })?;
    Ok(o)
}

/// `console` (spec 06 §6.5): arguments concatenated without separator.
pub(crate) fn console<'js>(ctx: &Ctx<'js>, sh: &Rc<Shared>) -> Result<Object<'js>> {
    let o = Object::new_proto(ctx.clone(), None)?;
    for (name, level) in [("log", LogLevel::Info), ("error", LogLevel::Error)] {
        let s = sh.clone();
        method(&o, name, 0, move |p: P<'_, 'js>| {
            if p.is_empty() {
                return Ok(undefined(p.ctx()));
            }
            // parity: no separator between arguments (Q-CON-1).
            let mut line = String::new();
            for i in 0..p.len() {
                line.push_str(&to_display_string(
                    p.ctx(),
                    p.arg(i).expect("argument in range"),
                ));
            }
            s.log(level, &line);
            Ok(undefined(p.ctx()))
        })?;
    }
    Ok(o)
}

#[derive(Clone, Copy)]
enum SceneConv {
    Bool,
    Int,
    Float,
    Color,
}

/// `thisScene` members: (JS name, host member actually read, conversion).
const SCENE_PROPS: [(&str, &str, SceneConv); 19] = [
    ("bloom", "bloom", SceneConv::Bool),
    // parity: bloom strength and threshold are truncated to integers (spec 06 §6.3).
    ("bloomstrength", "bloomstrength", SceneConv::Int),
    ("bloomthreshold", "bloomthreshold", SceneConv::Int),
    // parity: `clearenabled` reads the bloom flag (spec 06 §6.3, Q-SC-1).
    ("clearenabled", "bloom", SceneConv::Bool),
    ("clearcolor", "clearcolor", SceneConv::Color),
    ("ambientcolor", "ambientcolor", SceneConv::Color),
    // parity: `skylightcolor` reads the ambient color (spec 06 §6.3, Q-SC-1).
    ("skylightcolor", "ambientcolor", SceneConv::Color),
    ("fov", "fov", SceneConv::Float),
    ("nearz", "nearz", SceneConv::Float),
    ("farz", "farz", SceneConv::Float),
    ("camerafade", "camerafade", SceneConv::Bool),
    ("camerashake", "camerashake", SceneConv::Bool),
    ("camerashakespeed", "camerashakespeed", SceneConv::Float),
    (
        "camerashakeamplitude",
        "camerashakeamplitude",
        SceneConv::Float,
    ),
    (
        "camerashakeroughness",
        "camerashakeroughness",
        SceneConv::Float,
    ),
    ("cameraparallax", "cameraparallax", SceneConv::Bool),
    (
        "cameraparallaxamount",
        "cameraparallaxamount",
        SceneConv::Float,
    ),
    (
        "cameraparallaxdelay",
        "cameraparallaxdelay",
        SceneConv::Float,
    ),
    (
        "cameraparallaxmouseinfluence",
        "cameraparallaxmouseinfluence",
        SceneConv::Float,
    ),
];

/// `thisScene` (spec 06 §6.3, WE `IScene`).
pub(crate) fn scene<'js>(ctx: &Ctx<'js>, sh: &Rc<Shared>) -> Result<Object<'js>> {
    let o = Object::new_proto(ctx.clone(), None)?;
    for (js_name, host_name, conv) in SCENE_PROPS {
        let s = sh.clone();
        let get = move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
            let ctx = p.ctx();
            let Some(v) = s.host_get(ctx, ObjectRef::Scene, host_name)? else {
                return Ok(undefined(ctx));
            };
            match conv {
                SceneConv::Bool => Ok(bool_value(ctx, v.bool())),
                SceneConv::Int => Ok(JsValue::new_int(ctx.clone(), v.int())),
                SceneConv::Float => Ok(number(ctx, f64::from(v.float()))),
                SceneConv::Color => crate::vector::new_vec_dyn(
                    ctx,
                    3,
                    crate::vector::VecData::View(Target::Member {
                        obj: ObjectRef::Scene,
                        name: host_name,
                    }),
                ),
            }
        };
        let s = sh.clone();
        // we-doc: IScene members are writable (the reference throws).
        let set = move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
            if let Some(v) = p.arg(0) {
                let v = crate::convert::from_js(p.ctx(), &s, v)?;
                s.host_set(p.ctx(), ObjectRef::Scene, js_name, &v)?;
            }
            Ok(undefined(p.ctx()))
        };
        o.prop(js_name, Accessor::new(RawFn(get), RawFn(set)).enumerable())?;
    }

    let s = sh.clone();
    method(&o, "getLayer", 1, move |p: P<'_, 'js>| {
        let ctx = p.ctx();
        if p.len() != 1 {
            return Err(type_error(ctx, "getLayer expects exactly one argument"));
        }
        let arg = p.arg(0).expect("one argument");
        if arg.is_number() {
            let id = Coerced::<i32>::from_js(ctx, arg)?.0;
            return if s.host.with(|h| h.layer_exists(id)).unwrap_or(false) {
                Ok(layer_object(ctx, &s, id)?.into_value())
            } else {
                Ok(undefined(ctx))
            };
        }
        if let Some(name) = arg.as_string() {
            let name = name.to_string()?;
            return match s.host.with(|h| h.find_layer(&name)).flatten() {
                Some(id) => Ok(layer_object(ctx, &s, id)?.into_value()),
                // lumenwall: an unknown name gives null (the reference throws, spec 06 §6.3), so
                // scripts probing for an optional layer keep running.
                None => Ok(JsValue::new_null(ctx.clone())),
            };
        }
        Err(type_error(ctx, "getLayer expects a layer name or id"))
    })?;
    let s = sh.clone();
    method(&o, "getLayerByID", 1, move |p: P<'_, 'js>| {
        let ctx = p.ctx();
        let id = match p.arg(0) {
            Some(v) if v.is_number() => v.as_number().map(|n| n as i32),
            Some(v) => v
                .as_string()
                .and_then(|s| s.to_string().ok())
                .and_then(|s| s.trim().parse().ok()),
            None => None,
        };
        match id.filter(|id| s.host.with(|h| h.layer_exists(*id)).unwrap_or(false)) {
            Some(id) => Ok(layer_object(ctx, &s, id)?.into_value()),
            None => Ok(undefined(ctx)),
        }
    })?;
    let s = sh.clone();
    method(&o, "getLayerCount", 0, move |p: P<'_, 'js>| {
        let n = s.host.with(|h| h.layer_count()).unwrap_or(0);
        Ok(number(p.ctx(), n as f64))
    })?;
    let s = sh.clone();
    method(&o, "enumerateLayers", 0, move |p: P<'_, 'js>| {
        let ctx = p.ctx();
        let ids: Vec<_> = s
            .host
            .with(|h| (0..h.layer_count()).filter_map(|i| h.layer_at(i)).collect())
            .unwrap_or_default();
        let arr = Array::new(ctx.clone())?;
        for (i, id) in ids.into_iter().enumerate() {
            arr.set(i, layer_object(ctx, &s, id)?)?;
        }
        Ok(arr.into_value())
    })?;
    let s = sh.clone();
    method(&o, "getLayerIndex", 1, move |p: P<'_, 'js>| {
        let ctx = p.ctx();
        let id = match p.arg(0) {
            Some(v) => layer_arg(ctx, &s, &v)?,
            None => None,
        };
        let index = id.and_then(|id| {
            s.host
                .with(|h| (0..h.layer_count()).position(|i| h.layer_at(i) == Some(id)))
                .flatten()
        });
        Ok(number(ctx, index.map_or(-1.0, |i| i as f64)))
    })?;
    for (name, neutral_false) in [
        ("destroyLayer", true),
        ("createLayer", false),
        ("sortLayer", true),
        ("getInitialLayerConfig", false),
        ("getCameraTransforms", false),
        ("setCameraTransforms", false),
        ("getAnimation", false),
        ("createModelData", false),
        ("destroyModelData", false),
    ] {
        let s = sh.clone();
        method(&o, name, 1, move |p: P<'_, 'js>| {
            match host_call(&s, &p, ObjectRef::Scene, name)? {
                Some(v) => Ok(v),
                None if neutral_false => Ok(bool_value(p.ctx(), false)),
                None => Ok(undefined(p.ctx())),
            }
        })?;
    }
    Ok(o)
}

/// Native half of `createScriptProperties().finish()` (spec 06 §6.9): builds the
/// `scriptProperties` object of the script being evaluated or run.
pub(crate) fn finish_script_properties<'js>(
    ctx: &Ctx<'js>,
    sh: &Rc<Shared>,
) -> Result<Function<'js>> {
    let s = sh.clone();
    func(ctx, "finishScriptProperties", 1, move |p: P<'_, 'js>| {
        let ctx = p.ctx();
        let Some(id) = s.current.get() else {
            return Ok(undefined(ctx));
        };
        let names = s.scripts.borrow()[id.0 as usize].prop_names.clone();
        let obj = Object::new_proto(ctx.clone(), None)?;
        for (i, name) in names.iter().enumerate() {
            let index = i as u16;
            let sg = s.clone();
            let get = move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                let t = Target::ScriptProp { script: id, index };
                match sg.read_target(p.ctx(), t)? {
                    Some(v) => to_js(p.ctx(), &sg, &v, Some(t)),
                    None => Ok(undefined(p.ctx())),
                }
            };
            let set = |p: P<'_, 'js>| -> Result<JsValue<'js>> {
                Err(type_error(p.ctx(), "scriptProperties are read-only"))
            };
            obj.prop(
                name.as_str(),
                Accessor::new(RawFn(get), RawFn(set)).enumerable(),
            )?;
        }
        if let Some(defaults) = p.arg(0).and_then(|v| v.into_object()) {
            for prop in defaults.props::<String, JsValue>() {
                let (k, v) = prop?;
                if !names.contains(&k) {
                    obj.prop(k.as_str(), Property::from(v).enumerable())?;
                }
            }
        }
        Ok(obj.into_value())
    })
}

const VEC_MARKER: &str = "__lumenVec";

fn store_for<'a>(sh: &'a Shared, location: Option<JsValue<'_>>) -> &'a StorageFile {
    match location.and_then(|v| v.as_string().and_then(|s| s.to_string().ok())) {
        Some(l) if l == "global" => &sh.global_storage,
        _ => &sh.screen_storage,
    }
}

/// Serializes a value for `localStorage` (JSON; vectors tagged so they come back as
/// vectors).
fn serialize<'js>(ctx: &Ctx<'js>, sh: &Rc<Shared>, v: JsValue<'js>) -> Result<Option<String>> {
    let s = sh.clone();
    let replacer = func(ctx, "replacer", 2, move |p: P<'_, 'js>| {
        let ctx = p.ctx();
        let v = p.arg(1).unwrap_or_else(|| undefined(ctx));
        if let Some((n, d)) = as_vec(&s, &v) {
            let c = components(ctx, &s, n, d)?;
            let marked = Object::new(ctx.clone())?;
            let arr = Array::new(ctx.clone())?;
            for (i, x) in c.iter().take(n).enumerate() {
                arr.set(i, f64::from(*x))?;
            }
            marked.set(VEC_MARKER, arr)?;
            return Ok(marked.into_value());
        }
        Ok(v)
    })?;
    ctx.json_stringify_replacer(v, replacer)?
        .map(|s| s.to_string())
        .transpose()
}

fn revive<'js>(ctx: &Ctx<'js>, v: JsValue<'js>, depth: u32) -> Result<JsValue<'js>> {
    if depth > 32 {
        return Ok(v);
    }
    let Some(obj) = v.as_object() else {
        return Ok(v);
    };
    if let Some(arr) = obj.get::<_, Option<Array>>(VEC_MARKER)? {
        let mut c = [0.0f32; 4];
        let n = arr.len().clamp(2, 4);
        for (i, slot) in c.iter_mut().enumerate().take(n) {
            *slot = arr.get::<Option<f64>>(i)?.unwrap_or(0.0) as f32;
        }
        return owned(ctx, n, c);
    }
    let keys: Vec<String> = obj.keys::<String>().collect::<Result<_>>()?;
    for k in keys {
        let item: JsValue = obj.get(k.as_str())?;
        if item.is_object() {
            obj.set(k.as_str(), revive(ctx, item, depth + 1)?)?;
        }
    }
    Ok(v)
}

/// `localStorage` (WE `ILocalStorage`; the reference keeps an in-memory string map).
pub(crate) fn local_storage<'js>(ctx: &Ctx<'js>, sh: &Rc<Shared>) -> Result<Object<'js>> {
    let o = Object::new(ctx.clone())?;
    o.prop("LOCATION_GLOBAL", Property::from("global").enumerable())?;
    o.prop("LOCATION_SCREEN", Property::from("screen").enumerable())?;

    let s = sh.clone();
    method(&o, "set", 2, move |p: P<'_, 'js>| {
        let ctx = p.ctx();
        let key = p
            .arg(0)
            .map(|v| to_display_string(ctx, v))
            .unwrap_or_else(|| "undefined".into());
        let store = store_for(&s, p.arg(2));
        let value = p.arg(1).unwrap_or_else(|| undefined(ctx));
        match serialize(ctx, &s, value)? {
            Some(json) => {
                if store.set(&key, json).is_err() {
                    s.warn(&format!("localStorage quota exceeded, '{key}' not stored"));
                }
            }
            None => {
                store.delete(&key);
            }
        }
        Ok(undefined(ctx))
    })?;
    let s = sh.clone();
    method(&o, "get", 1, move |p: P<'_, 'js>| {
        let ctx = p.ctx();
        let key = p
            .arg(0)
            .map(|v| to_display_string(ctx, v))
            .unwrap_or_else(|| "undefined".into());
        let Some(json) = store_for(&s, p.arg(1)).get(&key) else {
            // parity: a missing key reads as null.
            return Ok(JsValue::new_null(ctx.clone()));
        };
        match ctx.json_parse(json.as_str()) {
            Ok(v) if json.contains(VEC_MARKER) => revive(ctx, v, 0),
            Ok(v) => Ok(v),
            Err(_) => {
                let _ = ctx.catch();
                Ok(JsValue::new_null(ctx.clone()))
            }
        }
    })?;
    // `delete` is the WE name; parity: the reference calls it `remove`.
    for name in ["delete", "remove"] {
        let s = sh.clone();
        method(&o, name, 1, move |p: P<'_, 'js>| {
            let ctx = p.ctx();
            let key = p
                .arg(0)
                .map(|v| to_display_string(ctx, v))
                .unwrap_or_else(|| "undefined".into());
            let removed = store_for(&s, p.arg(1)).delete(&key);
            Ok(bool_value(ctx, removed))
        })?;
    }
    let s = sh.clone();
    method(&o, "clear", 0, move |p: P<'_, 'js>| {
        store_for(&s, p.arg(0)).clear();
        Ok(undefined(p.ctx()))
    })?;
    Ok(o)
}

/// `MediaPlaybackEvent` constants (spec 06 §6.8).
pub(crate) fn media_playback_constants<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
    let o = Object::new(ctx.clone())?;
    o.set("PLAYBACK_STOPPED", 0)?;
    o.set("PLAYBACK_PLAYING", 1)?;
    o.set("PLAYBACK_PAUSED", 2)?;
    Ok(o)
}

/// Native helpers handed to the prelude.
pub(crate) fn prelude_natives<'js>(ctx: &Ctx<'js>, sh: &Rc<Shared>) -> Result<Object<'js>> {
    let o = Object::new(ctx.clone())?;
    o.set("finishScriptProperties", finish_script_properties(ctx, sh)?)?;
    Ok(o)
}

/// `Mat3` and `Mat4` constructors, compiling `matrix.js` on first use (it is the largest
/// piece of JS in the runtime and most wallpapers never touch matrices).
pub(crate) fn ensure_matrices<'js>(
    ctx: &Ctx<'js>,
    sh: &Shared,
) -> Result<(Function<'js>, Function<'js>)> {
    let loaded = {
        let js = sh.js.borrow();
        js.mat3.clone().zip(js.mat4.clone())
    };
    if let Some((m3, m4)) = loaded {
        return Ok((m3.restore(ctx)?, m4.restore(ctx)?));
    }
    let globals = ctx.globals();
    let natives = Object::new(ctx.clone())?;
    for name in ["Vec2", "Vec3", "Vec4"] {
        natives.set(name, globals.get::<_, JsValue>(name)?)?;
    }
    natives.set(
        "fmt",
        func(ctx, "fmt", 1, |p: P<'_, 'js>| {
            let x = p.arg(0).and_then(|v| v.as_number()).unwrap_or(f64::NAN);
            let mut s = String::new();
            crate::util::push_fixed6(&mut s, f64::from(x as f32));
            rquickjs::String::from_str(p.ctx().clone(), &s).map(|s| s.into_value())
        })?,
    )?;
    let factory: Function = ctx.eval(include_str!("matrix.js"))?;
    let exports: Object = factory.call((natives,))?;
    let m3: Function = exports.get("Mat3")?;
    let m4: Function = exports.get("Mat4")?;
    globals.prop("Mat3", Property::from(m3.clone()).writable().configurable())?;
    globals.prop("Mat4", Property::from(m4.clone()).writable().configurable())?;
    let mut js = sh.js.borrow_mut();
    js.mat3 = Some(Persistent::save(ctx, m3.clone()));
    js.mat4 = Some(Persistent::save(ctx, m4.clone()));
    Ok((m3, m4))
}

/// `thisObject` (WE): the object the running script is bound to.
pub(crate) fn install_this_object<'js>(globals: &Object<'js>, sh: &Rc<Shared>) -> Result<()> {
    let s = sh.clone();
    let get = move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
        let obj = s
            .current
            .get()
            .and_then(|id| s.scripts.borrow()[id.0 as usize].object_obj.clone());
        match obj {
            Some(o) => Ok(o.restore(p.ctx())?.into_value()),
            None => Ok(undefined(p.ctx())),
        }
    };
    let set = |p: P<'_, 'js>| -> Result<JsValue<'js>> { Ok(undefined(p.ctx())) };
    globals.prop(
        "thisObject",
        Accessor::new(RawFn(get), RawFn(set))
            .enumerable()
            .configurable(),
    )
}

/// Lazy `Mat3`/`Mat4` globals.
pub(crate) fn install_matrix_globals<'js>(globals: &Object<'js>, sh: &Rc<Shared>) -> Result<()> {
    for (i, name) in ["Mat3", "Mat4"].into_iter().enumerate() {
        let s = sh.clone();
        let get = move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
            let (m3, m4) = ensure_matrices(p.ctx(), &s)?;
            Ok(if i == 0 { m3 } else { m4 }.into_value())
        };
        let set = move |p: P<'_, 'js>| -> Result<JsValue<'js>> {
            let v = p.arg(0).unwrap_or_else(|| undefined(p.ctx()));
            p.ctx()
                .globals()
                .prop(name, Property::from(v).writable().configurable())?;
            Ok(undefined(p.ctx()))
        };
        globals.prop(name, Accessor::new(RawFn(get), RawFn(set)).configurable())?;
    }
    Ok(())
}

/// JS object holding `pairs` (user properties, general settings).
pub(crate) fn record<'js>(
    ctx: &Ctx<'js>,
    sh: &Shared,
    pairs: &[(String, Value)],
) -> Result<Object<'js>> {
    let o = Object::new(ctx.clone())?;
    for (k, v) in pairs {
        o.set(k.as_str(), to_js(ctx, sh, v, None)?)?;
    }
    Ok(o)
}

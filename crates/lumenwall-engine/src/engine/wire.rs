//! Messages between the daemon and its engine process: one JSON object per line on the
//! child's stdin (daemon → engine) and stdout (engine → daemon). Nothing here is sent per
//! frame: plans, controls, monitor reports on change, counters on request, log records.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use lumenwall_render::{ClampMode, QualitySettings, ScalingMode};
use serde_json::{Map, Value, json};

use crate::monitor::{
    Change, EngineReport, FrameCounters, OutputCounters, OutputReport, PauseCause, PropertyValue,
    TargetReport, TargetState,
};
use crate::plan::{
    AudioPlan, DebugPlan, EnginePlan, HwDecode, Layer, OutputMode, PausePlan, Stopped, TargetPlan,
};

/// What the engine process starts with.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Init {
    pub plan: EnginePlan,
    pub user_paused: bool,
    pub sampling: bool,
    /// Most verbose level the daemon's log filter lets through (0 off, 1 error … 5 trace).
    pub log_level: u8,
}

/// Daemon → engine process.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ToHost {
    Init(Box<Init>),
    Reload {
        id: u64,
        plan: Box<EnginePlan>,
    },
    Fps(u32),
    Properties(BTreeMap<String, String>),
    Paused(bool),
    Sampling(bool),
    /// Asks for the frame counters (answered by [`FromHost::Counters`] with the same id).
    Counters(u64),
    Stop(Option<i32>),
}

/// Engine process → daemon.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum FromHost {
    /// Answer to [`Init`]: the wallpapers are loaded and shown, or why not.
    Started(Result<(), String>),
    /// Answer to [`ToHost::Reload`].
    Done {
        id: u64,
        result: Result<(), String>,
    },
    Report {
        change: Change,
        report: EngineReport,
    },
    Counters {
        id: u64,
        counters: FrameCounters,
    },
    Log {
        level: u8,
        target: String,
        message: String,
    },
    /// The engine loop ended (asked to, or its output driver closed it).
    Stopped(Stopped),
    /// The engine loop failed.
    Failed(String),
}

type R<T> = Result<T, String>;

fn field<'a>(v: &'a Value, key: &str) -> R<&'a Value> {
    v.get(key).ok_or_else(|| format!("missing `{key}`"))
}

fn as_str<'a>(v: &'a Value, key: &str) -> R<&'a str> {
    field(v, key)?
        .as_str()
        .ok_or_else(|| format!("`{key}` is not a string"))
}

fn as_u64(v: &Value, key: &str) -> R<u64> {
    field(v, key)?
        .as_u64()
        .ok_or_else(|| format!("`{key}` is not an unsigned integer"))
}

fn as_u32(v: &Value, key: &str) -> R<u32> {
    u32::try_from(as_u64(v, key)?).map_err(|e| format!("`{key}`: {e}"))
}

fn as_i32(v: &Value, key: &str) -> R<i32> {
    let n = field(v, key)?
        .as_i64()
        .ok_or_else(|| format!("`{key}` is not an integer"))?;
    i32::try_from(n).map_err(|e| format!("`{key}`: {e}"))
}

fn as_f64(v: &Value, key: &str) -> R<f64> {
    field(v, key)?
        .as_f64()
        .ok_or_else(|| format!("`{key}` is not a number"))
}

fn as_bool(v: &Value, key: &str) -> R<bool> {
    field(v, key)?
        .as_bool()
        .ok_or_else(|| format!("`{key}` is not a boolean"))
}

fn as_array<'a>(v: &'a Value, key: &str) -> R<&'a Vec<Value>> {
    field(v, key)?
        .as_array()
        .ok_or_else(|| format!("`{key}` is not an array"))
}

fn opt<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key).filter(|x| !x.is_null())
}

fn strings(v: &Value, key: &str) -> R<Vec<String>> {
    as_array(v, key)?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_owned)
                .ok_or("not a string".to_owned())
        })
        .collect()
}

fn u32s(v: &Value, key: &str) -> R<Vec<u32>> {
    as_array(v, key)?
        .iter()
        .map(|n| {
            n.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("not a u32".to_owned())
        })
        .collect()
}

/// Paths as strings when they are valid Unicode, as their native units otherwise (Windows
/// UTF-16 with lone surrogates, Unix bytes), so no folder name is lost.
fn path_json(p: &std::path::Path) -> Value {
    match p.to_str() {
        Some(s) => Value::String(s.to_owned()),
        None => {
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                json!({ "wide": p.as_os_str().encode_wide().collect::<Vec<u16>>() })
            }
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                json!({ "bytes": p.as_os_str().as_bytes() })
            }
            #[cfg(not(any(windows, unix)))]
            {
                Value::String(p.to_string_lossy().into_owned())
            }
        }
    }
}

fn quality_from(v: &Value) -> R<QualitySettings> {
    Ok(QualitySettings {
        msaa: as_str(v, "msaa")?.parse()?,
        texture_resolution: as_str(v, "texture_resolution")?.parse()?,
        hdr: as_str(v, "hdr")?.parse()?,
        native_resolution: as_bool(v, "native_resolution")?,
    })
}

fn path_from(v: &Value) -> R<PathBuf> {
    if let Some(s) = v.as_str() {
        return Ok(PathBuf::from(s));
    }
    #[cfg(windows)]
    if let Some(units) = v.get("wide").and_then(Value::as_array) {
        use std::os::windows::ffi::OsStringExt;
        let units: Vec<u16> = units
            .iter()
            .map(|u| u.as_u64().and_then(|u| u16::try_from(u).ok()))
            .collect::<Option<_>>()
            .ok_or("bad UTF-16 path")?;
        return Ok(PathBuf::from(OsString::from_wide(&units)));
    }
    #[cfg(unix)]
    if let Some(bytes) = v.get("bytes").and_then(Value::as_array) {
        use std::os::unix::ffi::OsStringExt;
        let bytes: Vec<u8> = bytes
            .iter()
            .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
            .collect::<Option<_>>()
            .ok_or("bad byte path")?;
        return Ok(PathBuf::from(OsString::from_vec(bytes)));
    }
    Err("bad path".into())
}

fn path_field(v: &Value, key: &str) -> R<PathBuf> {
    path_from(field(v, key)?)
}

fn scaling_name(s: ScalingMode) -> &'static str {
    match s {
        ScalingMode::Stretch => "stretch",
        ScalingMode::Fill => "fill",
        ScalingMode::Fit => "fit",
        ScalingMode::Default => "default",
    }
}

fn scaling_from(s: &str) -> R<ScalingMode> {
    Ok(match s {
        "stretch" => ScalingMode::Stretch,
        "fill" => ScalingMode::Fill,
        "fit" => ScalingMode::Fit,
        "default" => ScalingMode::Default,
        _ => return Err(format!("unknown scaling `{s}`")),
    })
}

fn clamp_name(c: ClampMode) -> &'static str {
    match c {
        ClampMode::Clamp => "clamp",
        ClampMode::Border => "border",
        ClampMode::Repeat => "repeat",
    }
}

fn clamp_from(s: &str) -> R<ClampMode> {
    Ok(match s {
        "clamp" => ClampMode::Clamp,
        "border" => ClampMode::Border,
        "repeat" => ClampMode::Repeat,
        _ => return Err(format!("unknown clamp `{s}`")),
    })
}

fn duration_json(d: Duration) -> Value {
    json!(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

fn duration_from(v: &Value, key: &str) -> R<Duration> {
    Ok(Duration::from_nanos(as_u64(v, key)?))
}

pub(super) fn plan_json(p: &EnginePlan) -> Value {
    let mode = match p.mode {
        OutputMode::Window => json!("window"),
        OutputMode::Desktop => json!("desktop"),
        OutputMode::ExplicitWindow {
            x,
            y,
            width,
            height,
        } => json!({ "x": x, "y": y, "width": width, "height": height }),
    };
    let targets: Vec<Value> = p
        .targets
        .iter()
        .map(|t| {
            json!({
                "key": t.key,
                "outputs": t.outputs,
                "wallpaper": path_json(&t.wallpaper),
                "scaling": scaling_name(t.scaling),
                "clamp": clamp_name(t.clamp),
            })
        })
        .collect();
    json!({
        "mode": mode,
        "targets": targets,
        "layer": match p.layer {
            Layer::Background => "background",
            Layer::Bottom => "bottom",
            Layer::Top => "top",
            Layer::Overlay => "overlay",
        },
        "fps": p.fps,
        "audio": {
            "silent": p.audio.silent,
            "volume": p.audio.volume,
            "automute": p.audio.automute,
            "processing": p.audio.processing,
        },
        "hwdec": match p.hwdec {
            HwDecode::Auto => "auto",
            HwDecode::NvdecCopy => "nvdec-copy",
            HwDecode::No => "no",
        },
        "pause": {
            "enabled": p.pause.enabled,
            "only_active": p.pause.only_active,
            "ignore_app_ids": p.pause.ignore_app_ids,
        },
        "gpu_release": p.gpu_release.map(duration_json),
        "mouse": p.mouse,
        "parallax": p.parallax,
        "particles": p.particles,
        "quality": {
            "msaa": p.quality.msaa.to_string(),
            "texture_resolution": p.quality.texture_resolution.to_string(),
            "hdr": p.quality.hdr.to_string(),
            "native_resolution": p.quality.native_resolution,
        },
        "assets_dir": p.assets_dir.as_deref().map(path_json),
        "properties": p.properties,
        "screenshot": p.screenshot.as_ref().map(|(path, delay)| json!({
            "path": path_json(path),
            "delay": delay,
        })),
        "list_properties": p.list_properties,
        "dump_structure": p.dump_structure,
        "debug": {
            "base_only": p.debug.base_only,
            "no_solid_final": p.debug.no_solid_final,
            "pass_log": p.debug.pass_log,
            "only_object": p.debug.only_object,
            "skip_objects": p.debug.skip_objects,
            "skip_effects": p.debug.skip_effects,
        },
    })
}

pub(super) fn plan_from(v: &Value) -> R<EnginePlan> {
    let mode = match field(v, "mode")? {
        Value::String(s) if s == "window" => OutputMode::Window,
        Value::String(s) if s == "desktop" => OutputMode::Desktop,
        m @ Value::Object(_) => OutputMode::ExplicitWindow {
            x: as_i32(m, "x")?,
            y: as_i32(m, "y")?,
            width: as_u32(m, "width")?,
            height: as_u32(m, "height")?,
        },
        other => return Err(format!("unknown mode {other}")),
    };
    let targets = as_array(v, "targets")?
        .iter()
        .map(|t| {
            Ok(TargetPlan {
                key: as_str(t, "key")?.to_owned(),
                outputs: strings(t, "outputs")?,
                wallpaper: path_field(t, "wallpaper")?,
                scaling: scaling_from(as_str(t, "scaling")?)?,
                clamp: clamp_from(as_str(t, "clamp")?)?,
            })
        })
        .collect::<R<Vec<_>>>()?;
    let audio = field(v, "audio")?;
    let pause = field(v, "pause")?;
    let debug = field(v, "debug")?;
    let properties = field(v, "properties")?
        .as_object()
        .ok_or("`properties` is not an object")?
        .iter()
        .map(|(k, x)| {
            x.as_str()
                .map(|s| (k.clone(), s.to_owned()))
                .ok_or_else(|| format!("property `{k}` is not a string"))
        })
        .collect::<R<BTreeMap<_, _>>>()?;
    Ok(EnginePlan {
        mode,
        targets,
        layer: match as_str(v, "layer")? {
            "background" => Layer::Background,
            "bottom" => Layer::Bottom,
            "top" => Layer::Top,
            "overlay" => Layer::Overlay,
            other => return Err(format!("unknown layer `{other}`")),
        },
        fps: as_u32(v, "fps")?,
        audio: AudioPlan {
            silent: as_bool(audio, "silent")?,
            volume: u8::try_from(as_u64(audio, "volume")?).map_err(|e| e.to_string())?,
            automute: as_bool(audio, "automute")?,
            processing: as_bool(audio, "processing")?,
        },
        hwdec: match as_str(v, "hwdec")? {
            "auto" => HwDecode::Auto,
            "nvdec-copy" => HwDecode::NvdecCopy,
            "no" => HwDecode::No,
            other => return Err(format!("unknown hwdec `{other}`")),
        },
        pause: PausePlan {
            enabled: as_bool(pause, "enabled")?,
            only_active: as_bool(pause, "only_active")?,
            ignore_app_ids: strings(pause, "ignore_app_ids")?,
        },
        gpu_release: match opt(v, "gpu_release") {
            Some(_) => Some(duration_from(v, "gpu_release")?),
            None => None,
        },
        mouse: as_bool(v, "mouse")?,
        parallax: as_bool(v, "parallax")?,
        particles: as_bool(v, "particles")?,
        quality: quality_from(field(v, "quality")?)?,
        assets_dir: opt(v, "assets_dir").map(path_from).transpose()?,
        properties,
        screenshot: match opt(v, "screenshot") {
            Some(s) => Some((path_field(s, "path")?, as_u32(s, "delay")?)),
            None => None,
        },
        list_properties: as_bool(v, "list_properties")?,
        dump_structure: as_bool(v, "dump_structure")?,
        debug: DebugPlan {
            base_only: as_bool(debug, "base_only")?,
            no_solid_final: as_bool(debug, "no_solid_final")?,
            pass_log: as_bool(debug, "pass_log")?,
            only_object: match opt(debug, "only_object") {
                Some(_) => Some(as_u32(debug, "only_object")?),
                None => None,
            },
            skip_objects: u32s(debug, "skip_objects")?,
            skip_effects: u32s(debug, "skip_effects")?,
        },
    })
}

fn change_name(c: Change) -> &'static str {
    match c {
        Change::Outputs => "outputs",
        Change::State => "state",
        Change::Properties => "properties",
    }
}

fn change_from(s: &str) -> R<Change> {
    Ok(match s {
        "outputs" => Change::Outputs,
        "state" => Change::State,
        "properties" => Change::Properties,
        _ => return Err(format!("unknown change `{s}`")),
    })
}

pub(super) fn report_json(r: &EngineReport) -> Value {
    let outputs: Vec<Value> = r
        .outputs
        .iter()
        .map(|o| {
            json!({
                "name": o.name,
                "description": o.description,
                "position": [o.position.0, o.position.1],
                "size": [o.size.0, o.size.1],
                "scale": o.scale,
                "refresh_mhz": o.refresh_mhz,
                "shown": o.shown,
                "target": o.target,
                "paused": o.paused.map(|p| match p {
                    PauseCause::Locked => "locked",
                    PauseCause::Fullscreen => "fullscreen",
                    PauseCause::Hidden => "hidden",
                }),
            })
        })
        .collect();
    let targets: Vec<Value> = r
        .targets
        .iter()
        .map(|t| {
            let properties: Vec<Value> = t
                .properties
                .iter()
                .map(|(k, v)| {
                    let v = match v {
                        PropertyValue::Bool(b) => json!({ "bool": b }),
                        PropertyValue::Number(n) => json!({ "number": n }),
                        PropertyValue::Color(c) => json!({ "color": c }),
                        PropertyValue::Text(s) => json!({ "text": s }),
                    };
                    json!([k, v])
                })
                .collect();
            json!({
                "key": t.key,
                "outputs": t.outputs,
                "wallpaper": path_json(&t.wallpaper),
                "title": t.title,
                "scaling": scaling_name(t.scaling),
                "state": match &t.state {
                    TargetState::Loading => json!("loading"),
                    TargetState::Running => json!("running"),
                    TargetState::Error(e) => json!({ "error": e }),
                },
                "properties": properties,
            })
        })
        .collect();
    json!({
        "active": r.active,
        "user_paused": r.user_paused,
        "outputs": outputs,
        "targets": targets,
    })
}

fn pair<T>(v: &Value, key: &str, f: impl Fn(&Value) -> Option<T>) -> R<(T, T)> {
    let a = as_array(v, key)?;
    match a.as_slice() {
        [x, y] => Ok((
            f(x).ok_or(format!("bad `{key}`"))?,
            f(y).ok_or(format!("bad `{key}`"))?,
        )),
        _ => Err(format!("`{key}` is not a pair")),
    }
}

pub(super) fn report_from(v: &Value) -> R<EngineReport> {
    let outputs = as_array(v, "outputs")?
        .iter()
        .map(|o| {
            Ok(OutputReport {
                name: as_str(o, "name")?.to_owned(),
                description: opt(o, "description")
                    .map(|d| d.as_str().map(str::to_owned).ok_or("bad description"))
                    .transpose()?,
                position: pair(o, "position", |x| {
                    x.as_i64().and_then(|n| i32::try_from(n).ok())
                })?,
                size: pair(o, "size", |x| {
                    x.as_u64().and_then(|n| u32::try_from(n).ok())
                })?,
                scale: as_f64(o, "scale")?,
                refresh_mhz: match opt(o, "refresh_mhz") {
                    Some(_) => Some(as_u32(o, "refresh_mhz")?),
                    None => None,
                },
                shown: as_bool(o, "shown")?,
                target: match opt(o, "target") {
                    Some(_) => Some(as_u64(o, "target")? as usize),
                    None => None,
                },
                paused: match opt(o, "paused").and_then(Value::as_str) {
                    None => None,
                    Some("locked") => Some(PauseCause::Locked),
                    Some("fullscreen") => Some(PauseCause::Fullscreen),
                    Some("hidden") => Some(PauseCause::Hidden),
                    Some(other) => return Err(format!("unknown pause `{other}`")),
                },
            })
        })
        .collect::<R<Vec<_>>>()?;
    let targets = as_array(v, "targets")?
        .iter()
        .map(|t| {
            let properties = as_array(t, "properties")?
                .iter()
                .map(|p| {
                    let [k, v] = p.as_array().map(Vec::as_slice).unwrap_or_default() else {
                        return Err("bad property".to_owned());
                    };
                    let k = k.as_str().ok_or("bad property name")?.to_owned();
                    let value = if let Some(b) = v.get("bool").and_then(Value::as_bool) {
                        PropertyValue::Bool(b)
                    } else if let Some(n) = v.get("number").and_then(Value::as_f64) {
                        PropertyValue::Number(n)
                    } else if let Some(c) = v.get("color").and_then(Value::as_array) {
                        let c: Vec<f32> = c
                            .iter()
                            .filter_map(Value::as_f64)
                            .map(|x| x as f32)
                            .collect();
                        PropertyValue::Color(c.try_into().map_err(|_| "bad color")?)
                    } else if let Some(s) = v.get("text").and_then(Value::as_str) {
                        PropertyValue::Text(s.to_owned())
                    } else {
                        return Err(format!("bad value of property `{k}`"));
                    };
                    Ok((k, value))
                })
                .collect::<R<Vec<_>>>()?;
            Ok(TargetReport {
                key: as_str(t, "key")?.to_owned(),
                outputs: strings(t, "outputs")?,
                wallpaper: path_field(t, "wallpaper")?,
                title: as_str(t, "title")?.to_owned(),
                scaling: scaling_from(as_str(t, "scaling")?)?,
                state: match field(t, "state")? {
                    Value::String(s) if s == "loading" => TargetState::Loading,
                    Value::String(s) if s == "running" => TargetState::Running,
                    s => TargetState::Error(as_str(s, "error")?.to_owned()),
                },
                properties,
            })
        })
        .collect::<R<Vec<_>>>()?;
    Ok(EngineReport {
        active: as_bool(v, "active")?,
        user_paused: as_bool(v, "user_paused")?,
        outputs,
        targets,
    })
}

fn counters_json(c: &FrameCounters) -> Value {
    let outputs: Vec<Value> = c
        .outputs
        .iter()
        .map(|o| json!({ "name": o.name, "frames": o.frames, "cpu": duration_json(o.cpu) }))
        .collect();
    json!({
        "elapsed": duration_json(c.elapsed),
        "outputs": outputs,
        "gpu": duration_json(c.gpu),
        "gpu_samples": c.gpu_samples,
        "gpu_memory": c.gpu_memory,
    })
}

fn counters_from(v: &Value) -> R<FrameCounters> {
    Ok(FrameCounters {
        elapsed: duration_from(v, "elapsed")?,
        outputs: as_array(v, "outputs")?
            .iter()
            .map(|o| {
                Ok(OutputCounters {
                    name: as_str(o, "name")?.to_owned(),
                    frames: as_u32(o, "frames")?,
                    cpu: duration_from(o, "cpu")?,
                })
            })
            .collect::<R<Vec<_>>>()?,
        gpu: duration_from(v, "gpu")?,
        gpu_samples: as_u32(v, "gpu_samples")?,
        gpu_memory: as_u64(v, "gpu_memory")?,
    })
}

fn result_json(r: &Result<(), String>) -> Value {
    match r {
        Ok(()) => Value::Null,
        Err(e) => Value::String(e.clone()),
    }
}

fn result_from(v: &Value) -> Result<(), String> {
    match v.as_str() {
        Some(e) => Err(e.to_owned()),
        None => Ok(()),
    }
}

/// One message as a line (with its `\n`).
fn line(kind: &str, body: Value) -> String {
    let mut m = Map::new();
    m.insert(kind.to_owned(), body);
    let mut s = Value::Object(m).to_string();
    s.push('\n');
    s
}

fn split(text: &str) -> R<(String, Value)> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Value::Object(m) = v else {
        return Err("not an object".into());
    };
    let mut it = m.into_iter();
    match (it.next(), it.next()) {
        (Some(kv), None) => Ok(kv),
        _ => Err("expected exactly one key".into()),
    }
}

impl ToHost {
    pub(super) fn encode(&self) -> String {
        match self {
            Self::Init(i) => line(
                "init",
                json!({
                    "plan": plan_json(&i.plan),
                    "user_paused": i.user_paused,
                    "sampling": i.sampling,
                    "log_level": i.log_level,
                }),
            ),
            Self::Reload { id, plan } => {
                line("reload", json!({ "id": id, "plan": plan_json(plan) }))
            }
            Self::Fps(f) => line("fps", json!(f)),
            Self::Properties(p) => line("properties", json!(p)),
            Self::Paused(b) => line("paused", json!(b)),
            Self::Sampling(b) => line("sampling", json!(b)),
            Self::Counters(id) => line("counters", json!(id)),
            Self::Stop(sig) => line("stop", json!(sig)),
        }
    }

    pub(super) fn decode(text: &str) -> R<Self> {
        let (kind, v) = split(text)?;
        Ok(match kind.as_str() {
            "init" => Self::Init(Box::new(Init {
                plan: plan_from(field(&v, "plan")?)?,
                user_paused: as_bool(&v, "user_paused")?,
                sampling: as_bool(&v, "sampling")?,
                log_level: u8::try_from(as_u64(&v, "log_level")?).map_err(|e| e.to_string())?,
            })),
            "reload" => Self::Reload {
                id: as_u64(&v, "id")?,
                plan: Box::new(plan_from(field(&v, "plan")?)?),
            },
            "fps" => Self::Fps(
                v.as_u64()
                    .and_then(|f| u32::try_from(f).ok())
                    .ok_or("bad fps")?,
            ),
            "properties" => Self::Properties(
                v.as_object()
                    .ok_or("bad properties")?
                    .iter()
                    .map(|(k, x)| x.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect::<Option<_>>()
                    .ok_or("bad property value")?,
            ),
            "paused" => Self::Paused(v.as_bool().ok_or("bad paused")?),
            "sampling" => Self::Sampling(v.as_bool().ok_or("bad sampling")?),
            "counters" => Self::Counters(v.as_u64().ok_or("bad counters id")?),
            "stop" => Self::Stop(match v {
                Value::Null => None,
                n => Some(
                    n.as_i64()
                        .and_then(|n| i32::try_from(n).ok())
                        .ok_or("bad signal")?,
                ),
            }),
            other => return Err(format!("unknown message `{other}`")),
        })
    }
}

impl FromHost {
    pub(super) fn encode(&self) -> String {
        match self {
            Self::Started(r) => line("started", result_json(r)),
            Self::Done { id, result } => {
                line("done", json!({ "id": id, "error": result_json(result) }))
            }
            Self::Report { change, report } => line(
                "report",
                json!({ "change": change_name(*change), "report": report_json(report) }),
            ),
            Self::Counters { id, counters } => line(
                "counters",
                json!({ "id": id, "counters": counters_json(counters) }),
            ),
            Self::Log {
                level,
                target,
                message,
            } => line(
                "log",
                json!({ "level": level, "target": target, "message": message }),
            ),
            Self::Stopped(s) => line(
                "stopped",
                match s {
                    Stopped::Signal(n) => json!({ "signal": n }),
                    Stopped::Requested => json!("requested"),
                    Stopped::Driver => json!("driver"),
                },
            ),
            Self::Failed(e) => line("failed", json!(e)),
        }
    }

    pub(super) fn decode(text: &str) -> R<Self> {
        let (kind, v) = split(text)?;
        Ok(match kind.as_str() {
            "started" => Self::Started(result_from(&v)),
            "done" => Self::Done {
                id: as_u64(&v, "id")?,
                result: result_from(field(&v, "error")?),
            },
            "report" => Self::Report {
                change: change_from(as_str(&v, "change")?)?,
                report: report_from(field(&v, "report")?)?,
            },
            "counters" => Self::Counters {
                id: as_u64(&v, "id")?,
                counters: counters_from(field(&v, "counters")?)?,
            },
            "log" => Self::Log {
                level: u8::try_from(as_u64(&v, "level")?).map_err(|e| e.to_string())?,
                target: as_str(&v, "target")?.to_owned(),
                message: as_str(&v, "message")?.to_owned(),
            },
            "stopped" => Self::Stopped(match &v {
                Value::String(s) if s == "requested" => Stopped::Requested,
                Value::String(s) if s == "driver" => Stopped::Driver,
                other => Stopped::Signal(as_i32(other, "signal")?),
            }),
            "failed" => Self::Failed(v.as_str().ok_or("bad failure")?.to_owned()),
            other => return Err(format!("unknown message `{other}`")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenwall_render::{HdrMode, Msaa, TextureResolution};

    fn full_plan() -> EnginePlan {
        let mut properties = BTreeMap::new();
        properties.insert("schemecolor".to_owned(), "0.1 0.2 0.3".to_owned());
        properties.insert("é ünïcode".to_owned(), "\"quoted\"\n".to_owned());
        EnginePlan {
            mode: OutputMode::ExplicitWindow {
                x: -2560,
                y: 0,
                width: 480,
                height: 270,
            },
            targets: vec![
                TargetPlan {
                    key: "DISPLAY2".into(),
                    outputs: vec!["DISPLAY2".into()],
                    wallpaper: PathBuf::from(r"C:\Program Files (x86)\Steam\431960\3018340632"),
                    scaling: ScalingMode::Fit,
                    clamp: ClampMode::Border,
                },
                TargetPlan {
                    key: "span:A,B".into(),
                    outputs: vec!["A".into(), "B".into()],
                    wallpaper: PathBuf::from("/w/x"),
                    scaling: ScalingMode::Default,
                    clamp: ClampMode::Repeat,
                },
            ],
            layer: Layer::Overlay,
            fps: 144,
            audio: AudioPlan {
                silent: true,
                volume: 128,
                automute: false,
                processing: false,
            },
            hwdec: HwDecode::NvdecCopy,
            pause: PausePlan {
                enabled: false,
                only_active: true,
                ignore_app_ids: vec!["steam".into()],
            },
            gpu_release: Some(Duration::from_millis(1500)),
            mouse: false,
            parallax: false,
            particles: false,
            quality: QualitySettings {
                msaa: Msaa::X4,
                texture_resolution: TextureResolution::Half,
                hdr: HdrMode::Scene,
                native_resolution: true,
            },
            assets_dir: Some(PathBuf::from("assets")),
            properties,
            screenshot: Some((PathBuf::from("shot.png"), 5)),
            list_properties: true,
            dump_structure: true,
            debug: DebugPlan {
                base_only: true,
                no_solid_final: true,
                pass_log: true,
                only_object: Some(7),
                skip_objects: vec![1, 2],
                skip_effects: vec![3],
            },
        }
    }

    #[test]
    fn plans_round_trip() {
        for plan in [EnginePlan::default(), full_plan()] {
            let msg = ToHost::Reload {
                id: 9,
                plan: Box::new(plan),
            };
            let line = msg.encode();
            assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
            assert_eq!(ToHost::decode(&line).unwrap(), msg);
        }
        let init = ToHost::Init(Box::new(Init {
            plan: full_plan(),
            user_paused: true,
            sampling: true,
            log_level: 4,
        }));
        assert_eq!(ToHost::decode(&init.encode()).unwrap(), init);
    }

    #[cfg(windows)]
    #[test]
    fn paths_that_are_not_unicode_survive() {
        use std::os::windows::ffi::OsStringExt;
        let odd = PathBuf::from(OsString::from_wide(&[0x43, 0x3a, 0x5c, 0xd800, 0x78]));
        assert!(odd.to_str().is_none());
        assert_eq!(path_from(&path_json(&odd)).unwrap(), odd);
    }

    #[test]
    fn controls_round_trip() {
        let mut props = BTreeMap::new();
        props.insert("a".to_owned(), "1".to_owned());
        for msg in [
            ToHost::Fps(30),
            ToHost::Properties(props),
            ToHost::Paused(true),
            ToHost::Sampling(false),
            ToHost::Counters(3),
            ToHost::Stop(None),
            ToHost::Stop(Some(15)),
        ] {
            assert_eq!(ToHost::decode(&msg.encode()).unwrap(), msg);
        }
    }

    #[test]
    fn engine_messages_round_trip() {
        let report = EngineReport {
            active: true,
            user_paused: false,
            outputs: vec![OutputReport {
                name: "DISPLAY1".into(),
                description: Some("GBT273C".into()),
                position: (-2560, 0),
                size: (2560, 1440),
                scale: 1.25,
                refresh_mhz: Some(143_999),
                shown: true,
                target: Some(0),
                paused: Some(PauseCause::Fullscreen),
            }],
            targets: vec![TargetReport {
                key: "default".into(),
                outputs: vec![],
                wallpaper: PathBuf::from("/w/3463520581"),
                title: "Kirito x Asuna".into(),
                scaling: ScalingMode::Fill,
                state: TargetState::Error("boom".into()),
                properties: vec![
                    ("b".into(), PropertyValue::Bool(true)),
                    ("n".into(), PropertyValue::Number(0.5)),
                    ("c".into(), PropertyValue::Color([0.25, 0.5, 1.0])),
                    ("t".into(), PropertyValue::Text("x".into())),
                ],
            }],
        };
        let counters = FrameCounters {
            elapsed: Duration::from_millis(1000),
            outputs: vec![OutputCounters {
                name: "DISPLAY1".into(),
                frames: 30,
                cpu: Duration::from_micros(21_000),
            }],
            gpu: Duration::from_micros(9_000),
            gpu_samples: 30,
            gpu_memory: 123 << 20,
        };
        for msg in [
            FromHost::Started(Ok(())),
            FromHost::Started(Err("no output".into())),
            FromHost::Done {
                id: 4,
                result: Err("refused".into()),
            },
            FromHost::Report {
                change: Change::Outputs,
                report,
            },
            FromHost::Counters { id: 2, counters },
            FromHost::Log {
                level: 2,
                target: "wgpu_hal::vulkan".into(),
                message: "a\nb".into(),
            },
            FromHost::Stopped(Stopped::Signal(2)),
            FromHost::Stopped(Stopped::Driver),
            FromHost::Stopped(Stopped::Requested),
            FromHost::Failed("lost".into()),
        ] {
            assert_eq!(FromHost::decode(&msg.encode()).unwrap(), msg);
        }
        assert!(FromHost::decode("{}").is_err());
        assert!(FromHost::decode("{\"nope\": 1}").is_err());
        assert!(ToHost::decode("not json").is_err());
    }
}

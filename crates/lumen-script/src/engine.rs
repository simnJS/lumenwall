//! [`ScriptEngine`]: one QuickJS runtime and context per wallpaper.

use std::cell::Ref;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;
use std::time::{Duration, Instant};

use rquickjs::context::EvalOptions;
use rquickjs::function::{Args, This};
use rquickjs::object::Property;
use rquickjs::{
    Atom, Context, Ctx, Function, Module, Object, Persistent, Runtime, Value as JsValue,
};

use crate::convert::{Returned, from_return, to_js};
use crate::globals::{self, define_fixed};
use crate::host::{LogLevel, SceneHost};
use crate::hostobj::{self, layer_object, new_layer_object};
use crate::modules;
use crate::shared::{Event, Frame, Kind, Script, Shared, Target};
use crate::storage::StorageFile;
use crate::text;
use crate::util::{describe_exception, undefined};
use crate::value::{
    CallbackId, CursorEvent, CursorEventKind, LayerId, MediaProperties, MediaSnapshot,
    MediaThumbnail, ObjectRef, PlaybackState, Value,
};
use crate::vector::{VecData, new_vec_dyn, owned};

/// Identifies a loaded script.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScriptId(pub(crate) u32);

/// Execution and memory limits of one runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptLimits {
    /// JavaScript time allowed per [`ScriptEngine::tick`] (timers, updates, jobs). The
    /// script running when it runs out is interrupted and disabled; the rest of the frame's
    /// script work is skipped.
    pub tick_budget: Duration,
    /// Time allowed per event dispatch and per text-layer tick.
    pub call_budget: Duration,
    /// Time allowed to load one script (evaluation, `init`, first `update`).
    pub load_budget: Duration,
    /// Heap limit of the runtime, in bytes.
    pub memory_limit: usize,
    /// JavaScript stack limit, in bytes.
    pub max_stack_size: usize,
    /// Promise jobs run per tick at most.
    pub max_jobs_per_tick: usize,
    /// Delay before `localStorage` changes are written to disk.
    pub storage_flush_delay: Duration,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        ScriptLimits {
            tick_budget: Duration::from_millis(100),
            call_budget: Duration::from_millis(100),
            load_budget: Duration::from_secs(1),
            memory_limit: 64 * 1024 * 1024,
            max_stack_size: 512 * 1024,
            max_jobs_per_tick: 10_000,
            storage_flush_delay: Duration::from_secs(2),
        }
    }
}

/// Construction parameters of a [`ScriptEngine`].
#[derive(Clone)]
pub struct ScriptConfig {
    pub limits: ScriptLimits,
    /// `localStorage` area for `LOCATION_SCREEN` (default: in memory).
    pub screen_storage: StorageFile,
    /// `localStorage` area for `LOCATION_GLOBAL`, shared by every screen of the wallpaper.
    pub global_storage: StorageFile,
    /// Current user property values (`engine.userProperties`, initial
    /// `applyUserProperties`). Colors should be `Value::Vec3`.
    pub user_properties: Vec<(String, Value)>,
    /// General settings for the initial `applyGeneralSettings` (e.g. `language`).
    pub general_settings: Vec<(String, Value)>,
}

impl Default for ScriptConfig {
    fn default() -> Self {
        ScriptConfig {
            limits: ScriptLimits::default(),
            screen_storage: StorageFile::in_memory(),
            global_storage: StorageFile::in_memory(),
            user_properties: Vec::new(),
            general_settings: Vec::new(),
        }
    }
}

/// A property script (M1): an ES module bound to one scene value.
#[derive(Clone, Copy, Debug)]
pub struct PropertyScript<'a> {
    /// Owner layer (`thisLayer`).
    pub layer: LayerId,
    /// Member of `object` the script is bound to, read and written through
    /// [`SceneHost::get`]/[`SceneHost::set`]. Also names the script: `<property>_<layer>`.
    pub property: &'a str,
    /// Bound object (`thisObject`); `None` means `ObjectRef::Layer(layer)`.
    pub object: Option<ObjectRef>,
    /// Full module source.
    pub source: &'a str,
    /// Host key passed back to [`SceneHost::script_property`].
    pub binding: u64,
    /// Names present in the value's `scriptproperties` JSON.
    pub script_properties: &'a [&'a str],
}

/// A dynamic text layer script (M2).
#[derive(Clone, Copy, Debug)]
pub struct TextScript<'a> {
    pub layer: LayerId,
    pub source: &'a str,
    /// Text when the layer is created (`thisLayer.text`).
    pub initial_text: &'a str,
    pub binding: u64,
    pub script_properties: &'a [&'a str],
}

/// Why a script could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptError {
    /// A script with the same `<property>_<layer>` key exists (spec 06 §1.2: the first wins).
    Duplicate(String),
    /// Evaluation failed; the script is disabled and its value left untouched.
    Failed { key: String, message: String },
    /// The JavaScript runtime could not be set up.
    Runtime(String),
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptError::Duplicate(k) => write!(f, "script '{k}' is already loaded"),
            ScriptError::Failed { key, message } => write!(f, "script '{key}' failed: {message}"),
            ScriptError::Runtime(m) => write!(f, "script runtime error: {m}"),
        }
    }
}

impl std::error::Error for ScriptError {}

/// Debug counters (docs/PERFORMANCE.md "Measure").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScriptStats {
    /// Scripts ever loaded (including disabled ones).
    pub scripts: usize,
    /// Scripts still running.
    pub active: usize,
    /// Scripts whose `update` runs every tick.
    pub per_frame: usize,
    pub timers: usize,
    /// Wall time of the last [`ScriptEngine::tick`].
    pub last_tick: Duration,
    /// Bytes allocated by the runtime.
    pub memory_used: u64,
}

/// Payload of a dispatched event.
enum Payload<'a> {
    Vec2([f32; 2]),
    Record(&'a [(String, Value)]),
    Cursor(&'a CursorEvent),
    Status(bool),
    Properties(&'a MediaProperties),
    Playback(PlaybackState),
    Timeline(f64, f64),
    Thumbnail(&'a MediaThumbnail),
}

/// SceneScript runtime of one wallpaper (spec 06 §3.1: one runtime and one context per
/// scene instance, everything on the calling thread).
///
/// No JavaScript runs unless a script needs it: [`ScriptEngine::tick`] returns
/// immediately when there are no per-frame updates, timers, audio buffers or pending jobs.
pub struct ScriptEngine {
    // Field order matters: the context goes before the runtime.
    ctx: Context,
    rt: Runtime,
    shared: Rc<Shared>,
    m1_keys: BTreeMap<String, ScriptId>,
    update_order: Vec<ScriptId>,
    audio_scratch: Vec<f32>,
    due_scratch: Vec<u32>,
    last_tick: Duration,
}

fn arm(sh: &Shared, budget: Duration) {
    sh.interrupted.set(false);
    sh.deadline.set(Some(Instant::now() + budget));
}

fn disarm(sh: &Shared) {
    sh.deadline.set(None);
    sh.current.set(None);
}

/// Message of a failed call; consumes the pending exception.
fn error_message(ctx: &Ctx<'_>, sh: &Shared, e: rquickjs::Error) -> String {
    let caught = matches!(e, rquickjs::Error::Exception).then(|| ctx.catch());
    if sh.interrupted.get() {
        return "execution budget exceeded".to_owned();
    }
    match caught {
        Some(v) => describe_exception(ctx, v),
        None => e.to_string(),
    }
}

/// Consecutive `update` errors that disable a script.
const MAX_UPDATE_ERRORS: u8 = 3;
/// Non-fatal `update` errors logged per script.
const MAX_ERROR_LOGS: u8 = 10;

/// lumen: an `update` error is logged and the script keeps running; it is disabled after
/// [`MAX_UPDATE_ERRORS`] consecutive errors, or at once for a budget overrun or out-of-memory
/// (those would otherwise stall every frame). Returns whether the script was disabled.
fn update_failed(sh: &Shared, id: ScriptId, msg: &str) -> bool {
    let fatal = sh.interrupted.get() || msg.contains("out of memory");
    let (key, count, log) = {
        let mut scripts = sh.scripts.borrow_mut();
        let s = &mut scripts[id.0 as usize];
        s.update_errors = s.update_errors.saturating_add(1);
        let log = s.error_logs < MAX_ERROR_LOGS;
        s.error_logs = s.error_logs.saturating_add(1);
        (s.key.clone(), s.update_errors, log)
    };
    if fatal || count >= MAX_UPDATE_ERRORS {
        deactivate(sh, id, "update", Some(msg));
        return true;
    }
    if log {
        sh.log(
            LogLevel::Error,
            &format!("script '{key}': update error ({count}/{MAX_UPDATE_ERRORS}): {msg}"),
        );
        if sh.scripts.borrow()[id.0 as usize].error_logs == MAX_ERROR_LOGS {
            sh.log(
                LogLevel::Error,
                &format!("script '{key}': further update errors are not logged"),
            );
        }
    }
    false
}

/// Ends an `update` error streak.
fn update_succeeded(sh: &Shared, id: ScriptId) {
    let mut scripts = sh.scripts.borrow_mut();
    let s = &mut scripts[id.0 as usize];
    if s.update_errors != 0 {
        s.update_errors = 0;
    }
}

/// Disables a script: releases its JS handles and timers, logs `msg` when given.
fn deactivate(sh: &Shared, id: ScriptId, what: &str, msg: Option<&str>) {
    let key = {
        let mut scripts = sh.scripts.borrow_mut();
        let s = &mut scripts[id.0 as usize];
        if !s.active {
            return;
        }
        s.active = false;
        s.release();
        s.key.clone()
    };
    sh.timers.borrow_mut().remove_owned_by(id);
    for slot in sh.js.borrow_mut().callbacks.iter_mut() {
        if slot.as_ref().is_some_and(|(_, owner)| *owner == Some(id)) {
            *slot = None;
        }
    }
    if let Some(msg) = msg {
        sh.log(
            LogLevel::Error,
            &format!("script '{key}' disabled ({what}): {msg}"),
        );
    }
}

/// Points `thisLayer` at the script's layer and marks the script current (`thisObject`
/// is a getter reading the current script, so it costs nothing per call).
fn enter_script<'js>(
    ctx: &Ctx<'js>,
    sh: &Shared,
    globals: &Object<'js>,
    atoms: &Atoms<'js>,
    id: Option<ScriptId>,
) -> rquickjs::Result<()> {
    sh.current.set(id);
    let Some(id) = id else { return Ok(()) };
    let layer = sh.scripts.borrow()[id.0 as usize].layer_obj.clone();
    if let Some(l) = layer {
        globals.set(atoms.this_layer.clone(), l.restore(ctx)?)?;
    }
    Ok(())
}

struct Atoms<'js> {
    this_layer: Atom<'js>,
}

impl<'js> Atoms<'js> {
    fn new(ctx: &Ctx<'js>) -> rquickjs::Result<Self> {
        Ok(Atoms {
            this_layer: Atom::from_str(ctx.clone(), "thisLayer")?,
        })
    }
}

/// `value` argument of `init`/`update`: the bound value, vectors as a cached live view.
fn bound_arg<'js>(ctx: &Ctx<'js>, sh: &Shared, id: ScriptId) -> rquickjs::Result<JsValue<'js>> {
    let Some(v) = sh.read_target(ctx, Target::Bound(id))? else {
        return Ok(undefined(ctx));
    };
    let Some(n) = v.vector_dim() else {
        return to_js(ctx, sh, &v, None);
    };
    let cached = sh.scripts.borrow()[id.0 as usize]
        .view
        .as_ref()
        .filter(|(d, _)| *d == n)
        .map(|(_, p)| p.clone());
    if let Some(p) = cached {
        return Ok(p.restore(ctx)?.into_value());
    }
    let view = new_vec_dyn(ctx, n, VecData::View(Target::Bound(id)))?;
    if let Some(obj) = view.as_object() {
        sh.scripts.borrow_mut()[id.0 as usize].view = Some((n, Persistent::save(ctx, obj.clone())));
    }
    Ok(view)
}

/// Applies an `init`/`update` result to the bound value (spec 06 §4.2).
fn apply_return<'js>(
    ctx: &Ctx<'js>,
    sh: &Shared,
    id: ScriptId,
    r: JsValue<'js>,
) -> Result<(), String> {
    match from_return(ctx, sh, r).map_err(|e| error_message(ctx, sh, e))? {
        Returned::Keep => Ok(()),
        Returned::Set(v) => sh
            .write_target(ctx, Target::Bound(id), &v)
            .map_err(|e| error_message(ctx, sh, e)),
        Returned::Invalid => {
            let key = sh.scripts.borrow()[id.0 as usize].key.clone();
            sh.warn(&format!(
                "script '{key}': returned object has no numeric x/y, value unchanged"
            ));
            Ok(())
        }
    }
}

/// Calls the script's `init`/`update` with its bound value and applies the result.
fn call_bound<'js>(ctx: &Ctx<'js>, sh: &Shared, id: ScriptId, ev: Event) -> Result<(), String> {
    let (f, this) = {
        let scripts = sh.scripts.borrow();
        let s = &scripts[id.0 as usize];
        if !s.active {
            return Ok(());
        }
        match (s.handler(ev), &s.this) {
            (Some(f), Some(t)) => (f.clone(), t.clone()),
            _ => return Ok(()),
        }
    };
    let run = || -> rquickjs::Result<JsValue<'js>> {
        let f = f.restore(ctx)?;
        let this = this.restore(ctx)?;
        let arg = bound_arg(ctx, sh, id)?;
        f.call((This(this), arg))
    };
    let r = run().map_err(|e| error_message(ctx, sh, e))?;
    apply_return(ctx, sh, id, r)
}

/// Calls event handler `ev` of script `id` with `args`.
fn call_event<'js>(
    ctx: &Ctx<'js>,
    sh: &Shared,
    id: ScriptId,
    ev: Event,
    args: &[JsValue<'js>],
) -> Result<(), String> {
    let (f, this) = {
        let scripts = sh.scripts.borrow();
        let s = &scripts[id.0 as usize];
        if !s.active {
            return Ok(());
        }
        match (s.handler(ev), &s.this) {
            (Some(f), Some(t)) => (f.clone(), t.clone()),
            _ => return Ok(()),
        }
    };
    let run = || -> rquickjs::Result<JsValue<'js>> {
        let f = f.restore(ctx)?;
        let mut a = Args::new(ctx.clone(), args.len());
        a.this(this.restore(ctx)?)?;
        for v in args {
            a.push_arg(v.clone())?;
        }
        f.call_arg(a)
    };
    run().map(drop).map_err(|e| error_message(ctx, sh, e))
}

fn drain_jobs(ctx: &Ctx<'_>, sh: &Shared) {
    sh.current.set(None);
    let mut n = 0;
    while n < sh.limits.max_jobs_per_tick && !sh.interrupted.get() && ctx.execute_pending_job() {
        n += 1;
    }
    if n == sh.limits.max_jobs_per_tick {
        sh.warn("promise job limit reached for this frame");
    }
}

fn vec3<'js>(ctx: &Ctx<'js>, c: [f32; 3]) -> rquickjs::Result<JsValue<'js>> {
    owned(ctx, 3, [c[0], c[1], c[2], 0.0])
}

fn payload<'js>(ctx: &Ctx<'js>, sh: &Shared, p: &Payload<'_>) -> rquickjs::Result<JsValue<'js>> {
    let o = Object::new(ctx.clone())?;
    match p {
        Payload::Vec2([x, y]) => return owned(ctx, 2, [*x, *y, 0.0, 0.0]),
        Payload::Record(pairs) => return Ok(globals::record(ctx, sh, pairs)?.into_value()),
        Payload::Cursor(c) => {
            o.set("worldPosition", vec3(ctx, c.world_position)?)?;
            o.set("localPosition", vec3(ctx, c.local_position)?)?;
            if let Some(h) = &c.hit_box {
                o.set("hitBox", h.as_str())?;
            }
        }
        Payload::Status(enabled) => o.set("enabled", *enabled)?,
        Payload::Properties(m) => {
            o.set("title", m.title.as_str())?;
            o.set("artist", m.artist.as_str())?;
            o.set("subTitle", m.sub_title.as_str())?;
            o.set("albumTitle", m.album_title.as_str())?;
            o.set("albumArtist", m.album_artist.as_str())?;
            o.set("genres", m.genres.as_str())?;
            o.set("contentType", m.content_type.as_str())?;
        }
        Payload::Playback(state) => o.set("state", *state as i32)?,
        // we-doc: seconds (the reference forwards raw MPRIS microseconds).
        Payload::Timeline(position, duration) => {
            o.set("position", *position)?;
            o.set("duration", *duration)?;
        }
        Payload::Thumbnail(t) => {
            o.set("hasThumbnail", t.has_thumbnail)?;
            o.set("primaryColor", vec3(ctx, t.primary_color)?)?;
            o.set("secondaryColor", vec3(ctx, t.secondary_color)?)?;
            o.set("tertiaryColor", vec3(ctx, t.tertiary_color)?)?;
            o.set("textColor", vec3(ctx, t.text_color)?)?;
            o.set("highContrastColor", vec3(ctx, t.high_contrast_color)?)?;
        }
    }
    Ok(o.into_value())
}

impl ScriptEngine {
    /// Creates the runtime, context, native classes, globals and modules.
    pub fn new(config: ScriptConfig) -> Result<Self, ScriptError> {
        let rt_err = |e: rquickjs::Error| ScriptError::Runtime(e.to_string());
        let rt = Runtime::new().map_err(rt_err)?;
        rt.set_memory_limit(config.limits.memory_limit);
        rt.set_max_stack_size(config.limits.max_stack_size);
        let shared = Rc::new(Shared::new(
            config.limits,
            config.screen_storage,
            config.global_storage,
            config.user_properties,
            config.general_settings,
        ));

        let sh = shared.clone();
        rt.set_interrupt_handler(Some(Box::new(move || match sh.deadline.get() {
            Some(d) if Instant::now() >= d => {
                sh.interrupted.set(true);
                true
            }
            _ => false,
        })));
        let sh = shared.clone();
        rt.set_host_promise_rejection_tracker(Some(Box::new(
            move |ctx, _promise, reason, handled| {
                if !handled && !sh.loading.get() {
                    let msg = describe_exception(&ctx, reason);
                    sh.warn(&format!("unhandled promise rejection: {msg}"));
                }
            },
        )));
        rt.set_loader(modules::resolver(), modules::loader());

        let ctx = Context::full(&rt).map_err(rt_err)?;
        ctx.with(|ctx| Self::install(&ctx, &shared))
            .map_err(rt_err)?;

        Ok(ScriptEngine {
            ctx,
            rt,
            shared,
            m1_keys: BTreeMap::new(),
            update_order: Vec::new(),
            audio_scratch: Vec::new(),
            due_scratch: Vec::new(),
            last_tick: Duration::ZERO,
        })
    }

    fn install(ctx: &Ctx<'_>, sh: &Rc<Shared>) -> rquickjs::Result<()> {
        let globals = ctx.globals();
        crate::vector::install(ctx, sh, &globals)?;
        let probe = owned(ctx, 2, [0.0; 4])?;
        // SAFETY: reading the class id of a live object.
        sh.class_id
            .set(unsafe { rquickjs::qjs::JS_GetClassID(probe.as_raw()) });
        hostobj::install(ctx, sh)?;

        let user_props = globals::record(ctx, sh, &sh.user_props.borrow())?;
        let engine = globals::engine(ctx, sh, user_props.clone())?;
        let scene = globals::scene(ctx, sh)?;
        define_fixed(&globals, "engine", engine.clone())?;
        define_fixed(&globals, "input", globals::input(ctx, sh)?)?;
        define_fixed(&globals, "thisScene", scene.clone())?;
        define_fixed(&globals, "console", globals::console(ctx, sh)?)?;
        define_fixed(&globals, "shared", Object::new(ctx.clone())?)?;
        globals.set("localStorage", globals::local_storage(ctx, sh)?)?;
        globals.set(
            "MediaPlaybackEvent",
            globals::media_playback_constants(ctx)?,
        )?;
        // we-doc: `renderContext` global (IRenderContext has no documented member).
        globals.set("renderContext", Object::new(ctx.clone())?)?;
        globals.set("thisLayer", undefined(ctx))?;
        globals::install_this_object(&globals, sh)?;

        globals::install_matrix_globals(&globals, sh)?;
        let prelude: Function = ctx.eval(include_str!("prelude.js"))?;
        let exports: Object = prelude.call((globals::prelude_natives(ctx, sh)?,))?;
        let csp: Function = exports.get("createScriptProperties")?;
        define_fixed(&globals, "createScriptProperties", csp)?;

        let m2_time = Object::new(ctx.clone())?;
        m2_time.set("time", 0.0)?;
        m2_time.set("dt", 0.0)?;
        m2_time.set("fps", 60.0)?;

        let mut js = sh.js.borrow_mut();
        js.user_props = Some(Persistent::save(ctx, user_props));
        js.engine = Some(Persistent::save(ctx, engine));
        js.scene = Some(Persistent::save(ctx, scene));
        js.m2_ctx = Some(Persistent::save(ctx, m2_time));
        Ok(())
    }

    fn push_script(&mut self, script: Script) -> ScriptId {
        let mut scripts = self.shared.scripts.borrow_mut();
        scripts.push(script);
        ScriptId((scripts.len() - 1) as u32)
    }

    fn snapshot(&self) {
        if let Some(frame) = self.shared.host.with(|h| Frame {
            clock: h.clock(),
            cursor: h.cursor(),
            display: h.display(),
        }) {
            self.shared.frame.set(frame);
        }
    }

    /// Loads a property script (spec 06 §8.1): evaluates the module, then calls `init`,
    /// `applyUserProperties` and `applyGeneralSettings` when exported. Its first `update`
    /// runs on the next [`ScriptEngine::tick`].
    pub fn load_property_script(
        &mut self,
        host: &mut dyn SceneHost,
        desc: &PropertyScript<'_>,
    ) -> Result<ScriptId, ScriptError> {
        let key = format!("{}_{}", desc.property, desc.layer);
        if self.m1_keys.contains_key(&key) {
            return Err(ScriptError::Duplicate(key));
        }
        let id = self.push_script(Script::new(
            key.clone(),
            Kind::Property,
            desc.layer,
            desc.object.unwrap_or(ObjectRef::Layer(desc.layer)),
            desc.property,
            desc.binding,
            desc.script_properties,
        ));
        self.m1_keys.insert(key.clone(), id);
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        self.snapshot();
        let outcome = self
            .ctx
            .with(|ctx| load_module(&ctx, &sh, id, &key, desc.source));
        match outcome {
            Ok(has_update) => {
                if has_update && sh.scripts.borrow()[id.0 as usize].active {
                    let scripts = sh.scripts.borrow();
                    let pos = self
                        .update_order
                        .partition_point(|other| scripts[other.0 as usize].key < key);
                    self.update_order.insert(pos, id);
                }
                Ok(id)
            }
            Err(message) => Err(ScriptError::Failed { key, message }),
        }
    }

    /// Creates a dynamic text layer script (spec 06 §9.1–9.4).
    pub fn create_text_script(
        &mut self,
        host: &mut dyn SceneHost,
        desc: &TextScript<'_>,
    ) -> Result<ScriptId, ScriptError> {
        let key = format!("text_{}", desc.layer);
        let mut script = Script::new(
            key.clone(),
            Kind::Text,
            desc.layer,
            ObjectRef::Layer(desc.layer),
            "text",
            desc.binding,
            desc.script_properties,
        );
        // Spec 06 §9.1: the first glyph texture shows the initial text, or a space.
        script.text = if desc.initial_text.is_empty() {
            " ".to_owned()
        } else {
            desc.initial_text.to_owned()
        };
        let id = self.push_script(script);
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        self.snapshot();
        let outcome = self.ctx.with(|ctx| create_text(&ctx, &sh, id, desc));
        outcome
            .map(|()| id)
            .map_err(|message| ScriptError::Failed { key, message })
    }

    /// Runs one frame of script work (spec 06 §8.2, §12): timers (intervals then
    /// timeouts), `update` of every property script in key order, then pending promise
    /// jobs. Does nothing, and calls no JavaScript, when there is no such work.
    pub fn tick(&mut self, host: &mut dyn SceneHost) {
        let started = Instant::now();
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        self.snapshot();
        let has_work = !self.update_order.is_empty()
            || !sh.timers.borrow().is_empty()
            || !sh.js.borrow().audio.is_empty()
            || self.rt.is_job_pending();
        if has_work {
            let order = &mut self.update_order;
            let audio = &mut self.audio_scratch;
            let due = &mut self.due_scratch;
            self.ctx.with(|ctx| {
                arm(&sh, sh.limits.tick_budget);
                if let Err(e) = globals::fill_audio(&ctx, &sh, audio) {
                    let msg = error_message(&ctx, &sh, e);
                    sh.warn(&format!("audio buffers: {msg}"));
                }
                if let Err(e) = run_timers(&ctx, &sh, due) {
                    let msg = error_message(&ctx, &sh, e);
                    sh.warn(&format!("timers: {msg}"));
                }
                if !sh.interrupted.get()
                    && let Err(e) = run_updates(&ctx, &sh, order)
                {
                    let msg = error_message(&ctx, &sh, e);
                    sh.warn(&format!("updates: {msg}"));
                }
                if sh.interrupted.get() {
                    sh.warn("frame script budget exhausted; remaining script work skipped");
                } else {
                    drain_jobs(&ctx, &sh);
                }
                disarm(&sh);
            });
        }
        sh.screen_storage
            .flush_if_due(started, sh.limits.storage_flush_delay);
        sh.global_storage
            .flush_if_due(started, sh.limits.storage_flush_delay);
        self.last_tick = started.elapsed();
    }

    /// Ticks a text layer (spec 06 §9.5–9.6). Call it while drawing the layer, only when
    /// it is visible. Returns `true` when the text changed; read it with
    /// [`ScriptEngine::text`].
    pub fn tick_text(&mut self, host: &mut dyn SceneHost, id: ScriptId) -> bool {
        {
            let scripts = self.shared.scripts.borrow();
            match scripts.get(id.0 as usize) {
                Some(s) if s.kind == Kind::Text && s.active => {}
                _ => return false,
            }
        }
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        let clock = sh.host.with(|h| h.clock()).unwrap_or_default();
        self.ctx.with(|ctx| tick_text_layer(&ctx, &sh, id, clock))
    }

    /// Current text of a text layer script (initial text until it changes).
    pub fn text(&self, id: ScriptId) -> Option<Ref<'_, str>> {
        Ref::filter_map(self.shared.scripts.borrow(), |scripts| {
            scripts
                .get(id.0 as usize)
                .filter(|s| s.kind == Kind::Text)
                .map(|s| s.text.as_str())
        })
        .ok()
    }

    fn dispatch(
        &mut self,
        host: &mut dyn SceneHost,
        ev: Event,
        layer: Option<LayerId>,
        data: &Payload<'_>,
    ) {
        let targets: Vec<ScriptId> = {
            let scripts = self.shared.scripts.borrow();
            self.m1_keys
                .values()
                .copied()
                .filter(|id| {
                    let s = &scripts[id.0 as usize];
                    s.active && s.handler(ev).is_some() && layer.is_none_or(|l| s.layer == l)
                })
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        self.ctx.with(|ctx| {
            arm(&sh, sh.limits.call_budget);
            let run = || -> rquickjs::Result<()> {
                let atoms = Atoms::new(&ctx)?;
                let globals = ctx.globals();
                let arg = payload(&ctx, &sh, data)?;
                for id in targets {
                    if sh.interrupted.get() {
                        break;
                    }
                    enter_script(&ctx, &sh, &globals, &atoms, Some(id))?;
                    if let Err(msg) = call_event(&ctx, &sh, id, ev, std::slice::from_ref(&arg)) {
                        deactivate(&sh, id, ev.name(), Some(&msg));
                    }
                }
                Ok(())
            };
            if let Err(e) = run() {
                let msg = error_message(&ctx, &sh, e);
                sh.warn(&format!("{}: {msg}", ev.name()));
            }
            drain_jobs(&ctx, &sh);
            disarm(&sh);
        });
        self.prune_update_order();
    }

    fn prune_update_order(&mut self) {
        let scripts = self.shared.scripts.borrow();
        self.update_order.retain(|id| scripts[id.0 as usize].active);
    }

    /// `resizeScreen(size)` on every script exporting it (WE event; not sent at creation).
    pub fn resize_screen(&mut self, host: &mut dyn SceneHost, size: [f32; 2]) {
        self.dispatch(host, Event::ResizeScreen, None, &Payload::Vec2(size));
    }

    /// Updates user properties (`engine.userProperties`, live) and calls
    /// `applyUserProperties(changed)` with only the changed entries (WE event).
    pub fn apply_user_properties(&mut self, host: &mut dyn SceneHost, changed: &[(String, Value)]) {
        {
            let mut props = self.shared.user_props.borrow_mut();
            for (k, v) in changed {
                match props.iter_mut().find(|(name, _)| name == k) {
                    Some(slot) => slot.1 = v.clone(),
                    None => props.push((k.clone(), v.clone())),
                }
            }
        }
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        self.ctx.with(|ctx| {
            let update = || -> rquickjs::Result<()> {
                let obj = sh.js.borrow().user_props.clone();
                if let Some(obj) = obj {
                    let obj = obj.restore(&ctx)?;
                    for (k, v) in changed {
                        obj.set(k.as_str(), to_js(&ctx, &sh, v, None)?)?;
                    }
                }
                Ok(())
            };
            if let Err(e) = update() {
                let msg = error_message(&ctx, &sh, e);
                sh.warn(&format!("engine.userProperties: {msg}"));
            }
        });
        drop(_host);
        self.dispatch(
            host,
            Event::ApplyUserProperties,
            None,
            &Payload::Record(changed),
        );
    }

    /// Calls `applyGeneralSettings(changed)` (WE event; currently `language`).
    pub fn apply_general_settings(
        &mut self,
        host: &mut dyn SceneHost,
        changed: &[(String, Value)],
    ) {
        {
            let mut settings = self.shared.general_settings.borrow_mut();
            for (k, v) in changed {
                match settings.iter_mut().find(|(name, _)| name == k) {
                    Some(slot) => slot.1 = v.clone(),
                    None => settings.push((k.clone(), v.clone())),
                }
            }
        }
        self.dispatch(
            host,
            Event::ApplyGeneralSettings,
            None,
            &Payload::Record(changed),
        );
    }

    /// Dispatches a cursor event. With `layer`, only scripts of that layer receive it
    /// (the host does the hit testing); with `None`, every script exporting the handler.
    pub fn cursor_event(
        &mut self,
        host: &mut dyn SceneHost,
        kind: CursorEventKind,
        layer: Option<LayerId>,
        event: &CursorEvent,
    ) {
        let ev = match kind {
            CursorEventKind::Enter => Event::CursorEnter,
            CursorEventKind::Leave => Event::CursorLeave,
            CursorEventKind::Move => Event::CursorMove,
            CursorEventKind::Down => Event::CursorDown,
            CursorEventKind::Up => Event::CursorUp,
            CursorEventKind::Click => Event::CursorClick,
        };
        self.dispatch(host, ev, layer, &Payload::Cursor(event));
    }

    /// `mediaStatusChanged({enabled})`.
    pub fn media_status_changed(&mut self, host: &mut dyn SceneHost, enabled: bool) {
        self.dispatch(
            host,
            Event::MediaStatusChanged,
            None,
            &Payload::Status(enabled),
        );
    }

    /// `mediaPropertiesChanged(event)`.
    pub fn media_properties_changed(
        &mut self,
        host: &mut dyn SceneHost,
        properties: &MediaProperties,
    ) {
        self.dispatch(
            host,
            Event::MediaPropertiesChanged,
            None,
            &Payload::Properties(properties),
        );
    }

    /// `mediaPlaybackChanged({state})`.
    pub fn media_playback_changed(&mut self, host: &mut dyn SceneHost, state: PlaybackState) {
        self.dispatch(
            host,
            Event::MediaPlaybackChanged,
            None,
            &Payload::Playback(state),
        );
    }

    /// `mediaTimelineChanged({position, duration})`, in seconds.
    pub fn media_timeline_changed(
        &mut self,
        host: &mut dyn SceneHost,
        position: f64,
        duration: f64,
    ) {
        self.dispatch(
            host,
            Event::MediaTimelineChanged,
            None,
            &Payload::Timeline(position, duration),
        );
    }

    /// `mediaThumbnailChanged(event)`.
    pub fn media_thumbnail_changed(
        &mut self,
        host: &mut dyn SceneHost,
        thumbnail: &MediaThumbnail,
    ) {
        self.dispatch(
            host,
            Event::MediaThumbnailChanged,
            None,
            &Payload::Thumbnail(thumbnail),
        );
    }

    /// Spec 06 §8.4: on any metadata or artwork change, the four media events are built
    /// once and, script by script in key order, `mediaPropertiesChanged`,
    /// `mediaPlaybackChanged`, `mediaTimelineChanged`, `mediaThumbnailChanged` are called
    /// with the same objects.
    pub fn media_changed(&mut self, host: &mut dyn SceneHost, media: &MediaSnapshot) {
        const EVENTS: [Event; 4] = [
            Event::MediaPropertiesChanged,
            Event::MediaPlaybackChanged,
            Event::MediaTimelineChanged,
            Event::MediaThumbnailChanged,
        ];
        let targets: Vec<ScriptId> = {
            let scripts = self.shared.scripts.borrow();
            self.m1_keys
                .values()
                .copied()
                .filter(|id| {
                    let s = &scripts[id.0 as usize];
                    s.active && EVENTS.iter().any(|e| s.handler(*e).is_some())
                })
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        self.ctx.with(|ctx| {
            arm(&sh, sh.limits.call_budget);
            let run = || -> rquickjs::Result<()> {
                let atoms = Atoms::new(&ctx)?;
                let globals = ctx.globals();
                let args = [
                    payload(&ctx, &sh, &Payload::Properties(&media.properties))?,
                    payload(&ctx, &sh, &Payload::Playback(media.state))?,
                    payload(
                        &ctx,
                        &sh,
                        &Payload::Timeline(media.position, media.duration),
                    )?,
                    payload(&ctx, &sh, &Payload::Thumbnail(&media.thumbnail))?,
                ];
                for id in targets {
                    enter_script(&ctx, &sh, &globals, &atoms, Some(id))?;
                    for (ev, arg) in EVENTS.iter().zip(&args) {
                        if sh.interrupted.get() {
                            return Ok(());
                        }
                        if let Err(msg) = call_event(&ctx, &sh, id, *ev, std::slice::from_ref(arg))
                        {
                            deactivate(&sh, id, ev.name(), Some(&msg));
                            break;
                        }
                    }
                }
                Ok(())
            };
            if let Err(e) = run() {
                let msg = error_message(&ctx, &sh, e);
                sh.warn(&format!("media events: {msg}"));
            }
            drain_jobs(&ctx, &sh);
            disarm(&sh);
        });
        self.prune_update_order();
    }

    /// Calls a JS function retained for the host (`Value::Callback`).
    pub fn invoke_callback(&mut self, host: &mut dyn SceneHost, id: CallbackId, args: &[Value]) {
        let entry = self
            .shared
            .js
            .borrow()
            .callbacks
            .get(id.0 as usize)
            .and_then(Option::as_ref)
            .map(|(f, owner)| (f.clone(), *owner));
        let Some((f, owner)) = entry else { return };
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        self.ctx.with(|ctx| {
            arm(&sh, sh.limits.call_budget);
            let run = || -> rquickjs::Result<JsValue<'_>> {
                let atoms = Atoms::new(&ctx)?;
                enter_script(&ctx, &sh, &ctx.globals(), &atoms, owner)?;
                let f = f.restore(&ctx)?;
                let mut a = Args::new(ctx.clone(), args.len());
                for v in args {
                    a.push_arg(to_js(&ctx, &sh, v, None)?)?;
                }
                f.call_arg(a)
            };
            if let Err(e) = run() {
                let msg = error_message(&ctx, &sh, e);
                match owner {
                    Some(o) => deactivate(&sh, o, "callback", Some(&msg)),
                    None => sh.warn(&format!("callback failed: {msg}")),
                }
            }
            drain_jobs(&ctx, &sh);
            disarm(&sh);
        });
        self.prune_update_order();
    }

    /// Releases a callback retained for the host.
    pub fn release_callback(&mut self, id: CallbackId) {
        let sh = &self.shared;
        self.ctx.with(|_| {
            if let Some(slot) = sh.js.borrow_mut().callbacks.get_mut(id.0 as usize) {
                *slot = None;
            }
        });
    }

    /// A layer was destroyed: calls `destroy()` of its scripts, then drops them (WE event).
    pub fn remove_layer(&mut self, host: &mut dyn SceneHost, layer: LayerId) {
        let ids: Vec<ScriptId> = self
            .shared
            .scripts
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.layer == layer && s.active)
            .map(|(i, _)| ScriptId(i as u32))
            .collect();
        self.destroy_scripts(host, &ids);
        let sh = self.shared.clone();
        self.m1_keys
            .retain(|_, id| sh.scripts.borrow()[id.0 as usize].layer != layer);
        self.ctx.with(|_| {
            sh.js.borrow_mut().layer_cache.remove(&layer);
        });
    }

    /// Calls `destroy()` of one script and drops it (spec 06 §9.7 for text layers).
    pub fn destroy_script(&mut self, host: &mut dyn SceneHost, id: ScriptId) {
        self.destroy_scripts(host, &[id]);
    }

    fn destroy_scripts(&mut self, host: &mut dyn SceneHost, ids: &[ScriptId]) {
        if ids.is_empty() {
            return;
        }
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        self.ctx.with(|ctx| {
            arm(&sh, sh.limits.call_budget);
            let atoms = Atoms::new(&ctx);
            for &id in ids {
                if sh
                    .scripts
                    .borrow()
                    .get(id.0 as usize)
                    .is_none_or(|s| !s.active)
                {
                    continue;
                }
                if !sh.interrupted.get()
                    && let Ok(atoms) = &atoms
                {
                    let r = enter_script(&ctx, &sh, &ctx.globals(), atoms, Some(id))
                        .map_err(|e| error_message(&ctx, &sh, e))
                        .and_then(|()| call_event(&ctx, &sh, id, Event::Destroy, &[]));
                    if let Err(msg) = r {
                        let key = sh.scripts.borrow()[id.0 as usize].key.clone();
                        sh.log(
                            LogLevel::Error,
                            &format!("script '{key}': destroy failed: {msg}"),
                        );
                    }
                }
                deactivate(&sh, id, "destroyed", None);
            }
            drain_jobs(&ctx, &sh);
            disarm(&sh);
        });
        self.prune_update_order();
    }

    /// Calls `destroy()` on every script and writes `localStorage` (WE: "just before the
    /// object gets destroyed"). The engine is inert afterwards.
    pub fn shutdown(&mut self, host: &mut dyn SceneHost) {
        let mut ids: Vec<ScriptId> = self.m1_keys.values().copied().collect();
        let scripts = self.shared.scripts.borrow().len();
        ids.extend(
            (0..scripts as u32)
                .map(ScriptId)
                .filter(|id| self.shared.scripts.borrow()[id.0 as usize].kind == Kind::Text),
        );
        self.destroy_scripts(host, &ids);
        self.flush_storage();
    }

    /// Writes pending `localStorage` changes now.
    pub fn flush_storage(&self) {
        for store in [&self.shared.screen_storage, &self.shared.global_storage] {
            if let Err(e) = store.flush() {
                self.shared.warn(&format!("cannot write localStorage: {e}"));
            }
        }
    }

    /// Whether [`ScriptEngine::tick`] has script work to do.
    pub fn has_work(&self) -> bool {
        !self.update_order.is_empty()
            || !self.shared.timers.borrow().is_empty()
            || !self.shared.js.borrow().audio.is_empty()
            || self.rt.is_job_pending()
    }

    /// Whether a script is still running (not disabled by an error or destroyed).
    pub fn is_active(&self, id: ScriptId) -> bool {
        self.shared
            .scripts
            .borrow()
            .get(id.0 as usize)
            .is_some_and(|s| s.active)
    }

    /// Debug counters.
    pub fn stats(&self) -> ScriptStats {
        let scripts = self.shared.scripts.borrow();
        let t = self.shared.timers.borrow();
        ScriptStats {
            scripts: scripts.len(),
            active: scripts.iter().filter(|s| s.active).count(),
            per_frame: self.update_order.len(),
            timers: t.timeouts.len() + t.intervals.len(),
            last_tick: self.last_tick,
            memory_used: u64::try_from(self.rt.memory_usage().memory_used_size).unwrap_or(0),
        }
    }

    /// Evaluates a classic script in the global scope (diagnostics and tests). Returns
    /// the result converted to a host value, or the error message.
    pub fn eval(&mut self, host: &mut dyn SceneHost, source: &str) -> Result<Value, String> {
        let sh = self.shared.clone();
        let _host = sh.host.enter(host);
        self.snapshot();
        self.ctx.with(|ctx| {
            arm(&sh, sh.limits.call_budget);
            let r = ctx
                .eval_with_options::<JsValue, _>(source, sloppy("eval".to_owned()))
                .and_then(|v| crate::convert::from_js(&ctx, &sh, v))
                .map_err(|e| error_message(&ctx, &sh, e));
            drain_jobs(&ctx, &sh);
            disarm(&sh);
            r
        })
    }
}

impl Drop for ScriptEngine {
    fn drop(&mut self) {
        // Every JS handle held outside the runtime must go before it.
        let sh = &self.shared;
        self.ctx.with(|_| sh.release_js());
    }
}

fn load_module(
    ctx: &Ctx<'_>,
    sh: &Rc<Shared>,
    id: ScriptId,
    key: &str,
    source: &str,
) -> Result<bool, String> {
    arm(sh, sh.limits.load_budget);
    let result = load_module_inner(ctx, sh, id, key, source);
    drain_jobs(ctx, sh);
    disarm(sh);
    if let Err(msg) = &result {
        deactivate(sh, id, "load", Some(msg));
    }
    result
}

fn load_module_inner(
    ctx: &Ctx<'_>,
    sh: &Rc<Shared>,
    id: ScriptId,
    key: &str,
    source: &str,
) -> Result<bool, String> {
    let err = |e| error_message(ctx, sh, e);
    let globals = ctx.globals();
    let atoms = Atoms::new(ctx).map_err(err)?;
    let (layer, object) = {
        let s = &sh.scripts.borrow()[id.0 as usize];
        (s.layer, s.object)
    };
    let layer_obj = layer_object(ctx, sh, layer).map_err(err)?;
    let object_obj = if object == ObjectRef::Layer(layer) {
        layer_obj.clone()
    } else {
        hostobj::wrap(ctx, sh, object)
            .map_err(err)?
            .into_object()
            .unwrap_or_else(|| layer_obj.clone())
    };
    {
        let mut scripts = sh.scripts.borrow_mut();
        let s = &mut scripts[id.0 as usize];
        s.layer_obj = Some(Persistent::save(ctx, layer_obj));
        s.object_obj = Some(Persistent::save(ctx, object_obj));
    }
    enter_script(ctx, sh, &globals, &atoms, Some(id)).map_err(err)?;

    // Synchronous module evaluation: declare, evaluate, settle the evaluation promise
    // (only top-level await needs jobs), then read the namespace.
    sh.loading.set(true);
    let evaluated = (|| -> rquickjs::Result<Object<'_>> {
        let module = Module::declare(ctx.clone(), key, source)?;
        let (module, promise) = module.eval()?;
        promise.finish::<()>()?;
        module.namespace()
    })();
    sh.loading.set(false);
    let ns = evaluated.map_err(|e| match e {
        rquickjs::Error::WouldBlock => "top-level await did not settle during loading".to_owned(),
        e => error_message(ctx, sh, e),
    })?;

    let mut handlers: [Option<Persistent<Function<'static>>>; 17] = Default::default();
    for ev in Event::ALL {
        let v: JsValue = ns.get(ev.name()).map_err(err)?;
        if let Some(f) = v.into_function() {
            handlers[ev as usize] = Some(Persistent::save(ctx, f));
        }
    }
    let has = |e: Event| handlers[e as usize].is_some();
    let (has_init, has_update) = (has(Event::Init), has(Event::Update));
    let (has_props, has_settings) = (
        has(Event::ApplyUserProperties),
        has(Event::ApplyGeneralSettings),
    );
    {
        let mut scripts = sh.scripts.borrow_mut();
        let s = &mut scripts[id.0 as usize];
        s.this = Some(Persistent::save(ctx, ns));
        s.handlers = handlers;
    }

    // we-doc: `init(value)` once after creation; its result is applied like update's.
    if has_init {
        call_bound(ctx, sh, id, Event::Init)?;
    }
    // we-doc: applyUserProperties / applyGeneralSettings are called once at load.
    if has_props {
        let all = sh.user_props.borrow().clone();
        let arg = payload(ctx, sh, &Payload::Record(&all)).map_err(err)?;
        call_event(ctx, sh, id, Event::ApplyUserProperties, &[arg])?;
    }
    if has_settings {
        let all = sh.general_settings.borrow().clone();
        let arg = payload(ctx, sh, &Payload::Record(&all)).map_err(err)?;
        call_event(ctx, sh, id, Event::ApplyGeneralSettings, &[arg])?;
    }
    // lumen: the first `update` runs on the next tick, not at load (spec 06 §8.1 step 5,
    // Q-M1-2), so it sees every object of the scene.
    Ok(has_update)
}

fn run_timers(ctx: &Ctx<'_>, sh: &Shared, due: &mut Vec<u32>) -> rquickjs::Result<()> {
    if sh.timers.borrow().is_empty() {
        return Ok(());
    }
    let now = sh.host.with(|h| h.now()).unwrap_or_else(Instant::now);
    let globals = ctx.globals();
    let atoms = Atoms::new(ctx)?;
    // Spec 06 §10: intervals first, then timeouts, each by increasing id. Timers created
    // during this pass are not in `due`, so they cannot fire before the next tick.
    for interval in [true, false] {
        due.clear();
        {
            let t = sh.timers.borrow();
            let map = if interval { &t.intervals } else { &t.timeouts };
            due.extend(
                map.iter()
                    .filter(|(_, timer)| timer.due <= now)
                    .map(|(id, _)| *id),
            );
        }
        for &tid in due.iter() {
            if sh.interrupted.get() {
                return Ok(());
            }
            let (callback, owner) = {
                let mut t = sh.timers.borrow_mut();
                if interval {
                    match t.intervals.get_mut(&tid) {
                        // No catch-up: at most one call per tick.
                        Some(timer) => {
                            timer.due = now + timer.period;
                            (timer.callback.clone(), timer.owner)
                        }
                        None => continue,
                    }
                } else {
                    match t.timeouts.remove(&tid) {
                        Some(timer) => (timer.callback, timer.owner),
                        None => continue,
                    }
                }
            };
            if let Some(o) = owner
                && !sh.scripts.borrow()[o.0 as usize].active
            {
                continue;
            }
            enter_script(ctx, sh, &globals, &atoms, owner)?;
            let r = callback
                .restore(ctx)
                .and_then(|f| f.call::<_, JsValue>((This(JsValue::new_null(ctx.clone())),)));
            if let Err(e) = r {
                let msg = error_message(ctx, sh, e);
                match owner {
                    Some(o) => deactivate(sh, o, "timer callback", Some(&msg)),
                    None => sh.warn(&format!("timer callback failed: {msg}")),
                }
            }
        }
    }
    sh.current.set(None);
    Ok(())
}

fn run_updates(ctx: &Ctx<'_>, sh: &Shared, order: &mut Vec<ScriptId>) -> rquickjs::Result<()> {
    if order.is_empty() {
        return Ok(());
    }
    let globals = ctx.globals();
    let atoms = Atoms::new(ctx)?;
    let mut failed = false;
    for &id in order.iter() {
        if sh.interrupted.get() {
            break;
        }
        enter_script(ctx, sh, &globals, &atoms, Some(id))?;
        match call_bound(ctx, sh, id, Event::Update) {
            Ok(()) => update_succeeded(sh, id),
            Err(msg) => failed |= update_failed(sh, id, &msg),
        }
    }
    sh.current.set(None);
    if failed {
        let scripts = sh.scripts.borrow();
        order.retain(|id| scripts[id.0 as usize].active);
    }
    Ok(())
}

fn create_text(
    ctx: &Ctx<'_>,
    sh: &Rc<Shared>,
    id: ScriptId,
    desc: &TextScript<'_>,
) -> Result<(), String> {
    arm(sh, sh.limits.load_budget);
    let result = create_text_inner(ctx, sh, id, desc);
    drain_jobs(ctx, sh);
    disarm(sh);
    if let Err(msg) = &result {
        // Spec 06 §9.4: creation errors are logged and the text stays at its initial value.
        deactivate(sh, id, "text layer creation", Some(msg));
    }
    result
}

fn create_text_inner(
    ctx: &Ctx<'_>,
    sh: &Rc<Shared>,
    id: ScriptId,
    desc: &TextScript<'_>,
) -> Result<(), String> {
    let err = |e| error_message(ctx, sh, e);
    let globals = ctx.globals();
    let atoms = Atoms::new(ctx).map_err(err)?;
    let real_layer = layer_object(ctx, sh, desc.layer).map_err(err)?;
    {
        let mut scripts = sh.scripts.borrow_mut();
        let s = &mut scripts[id.0 as usize];
        s.layer_obj = Some(Persistent::save(ctx, real_layer.clone()));
        s.object_obj = Some(Persistent::save(ctx, real_layer));
    }
    enter_script(ctx, sh, &globals, &atoms, Some(id)).map_err(err)?;

    let build = || -> rquickjs::Result<(Object<'_>, Object<'_>)> {
        let wrapper = text::wrapper_source(&text::preprocess(desc.source));
        // Spec 06 §9.3: classic, non-strict global code.
        let f: Function = ctx.eval_with_options(wrapper, sloppy(format!("text_{}", desc.layer)))?;
        // Spec 06 §9.3: props from the scriptproperties; vectors stay live, scalars frozen.
        let props = Object::new(ctx.clone())?;
        for (i, name) in desc.script_properties.iter().enumerate() {
            let t = Target::ScriptProp {
                script: id,
                index: i as u16,
            };
            if let Some(v) = sh.read_target(ctx, t)? {
                props.set(*name, to_js(ctx, sh, &v, Some(t))?)?;
            }
        }
        let local_layer = new_layer_object(ctx, desc.layer)?;
        local_layer.prop(
            "text",
            Property::from(desc.initial_text)
                .writable()
                .enumerable()
                .configurable(),
        )?;
        let (scene, engine, time) = {
            let js = sh.js.borrow();
            (js.scene.clone(), js.engine.clone(), js.m2_ctx.clone())
        };
        let restore = |p: Option<Persistent<Object<'static>>>| -> rquickjs::Result<JsValue<'_>> {
            p.map(|p| p.restore(ctx).map(|o| o.into_value()))
                .unwrap_or_else(|| Ok(undefined(ctx)))
        };
        let record: Object = f.call((
            props,
            local_layer.clone(),
            restore(scene)?,
            restore(engine)?,
            restore(time)?,
        ))?;
        Ok((record, local_layer))
    };
    let (record, local_layer) = build().map_err(err)?;
    let mut handlers: [Option<Persistent<Function<'static>>>; 17] = Default::default();
    for ev in [Event::Init, Event::Update, Event::Destroy] {
        let v: JsValue = record.get(ev.name()).map_err(err)?;
        if let Some(f) = v.into_function() {
            handlers[ev as usize] = Some(Persistent::save(ctx, f));
        }
    }
    let mut scripts = sh.scripts.borrow_mut();
    let s = &mut scripts[id.0 as usize];
    s.this = Some(Persistent::save(ctx, record));
    s.handlers = handlers;
    s.text_layer = Some(Persistent::save(ctx, local_layer));
    Ok(())
}

fn tick_text_layer(ctx: &Ctx<'_>, sh: &Shared, id: ScriptId, clock: crate::host::Clock) -> bool {
    // Spec 06 §9.5 step 1: shared M2 time context.
    let dt = clock.frametime;
    let fps = if dt > 1e-6 { 1.0 / dt } else { 60.0 };
    let (time, dt, fps) = (f64::from(clock.runtime), f64::from(dt), f64::from(fps));
    let (record, layer, init, update, initialized) = {
        let mut scripts = sh.scripts.borrow_mut();
        let s = &mut scripts[id.0 as usize];
        let initialized = s.initialized;
        s.initialized = true;
        (
            s.this.clone(),
            s.text_layer.clone(),
            s.handler(Event::Init).cloned(),
            s.handler(Event::Update).cloned(),
            initialized,
        )
    };
    let (Some(record), Some(layer)) = (record, layer) else {
        return false;
    };
    arm(sh, sh.limits.call_budget);
    sh.current.set(Some(id));
    let init_failed = std::cell::Cell::new(false);
    let run = || -> rquickjs::Result<bool> {
        let m2 = sh.js.borrow().m2_ctx.clone();
        if let Some(m2) = m2 {
            let m2 = m2.restore(ctx)?;
            m2.set("time", time)?;
            m2.set("dt", dt)?;
            m2.set("fps", fps)?;
        }
        let record = record.restore(ctx)?;
        let layer = layer.restore(ctx)?;
        if !initialized && let Some(init) = init {
            // An `init` error disables the layer at once.
            init_failed.set(true);
            init.restore(ctx)?
                .call::<_, JsValue>((This(record.clone()),))?;
            init_failed.set(false);
        }
        if let Some(update) = update {
            let current: JsValue = layer.get("text")?;
            let r: JsValue = update.restore(ctx)?.call((This(record), current))?;
            if r.is_string() {
                layer.set("text", r)?;
            }
        }
        // Spec 06 §9.6: undefined/null → "", ToString otherwise, "" → " ".
        let v: JsValue = layer.get("text")?;
        let mut scripts = sh.scripts.borrow_mut();
        let last = &mut scripts[id.0 as usize].text;
        let changed = if v.is_undefined() || v.is_null() {
            replace_text(last, " ")
        } else if let Some(s) = v.as_string() {
            let s = s.clone().to_cstring()?;
            replace_text(
                last,
                if s.as_str().is_empty() {
                    " "
                } else {
                    s.as_str()
                },
            )
        } else {
            drop(scripts);
            let s = crate::util::to_display_string(ctx, v);
            let mut scripts = sh.scripts.borrow_mut();
            replace_text(
                &mut scripts[id.0 as usize].text,
                if s.is_empty() { " " } else { &s },
            )
        };
        Ok(changed)
    };
    let changed = match run() {
        Ok(changed) => {
            update_succeeded(sh, id);
            changed
        }
        Err(e) => {
            let msg = error_message(ctx, sh, e);
            if init_failed.get() {
                deactivate(sh, id, "text init", Some(&msg));
            } else {
                update_failed(sh, id, &msg);
            }
            false
        }
    };
    drain_jobs(ctx, sh);
    disarm(sh);
    changed
}

/// Options for classic (non-module, non-strict) global code.
fn sloppy(filename: String) -> EvalOptions {
    let mut options = EvalOptions::default();
    options.strict = false;
    options.filename = Some(filename);
    options
}

fn replace_text(last: &mut String, new: &str) -> bool {
    if last == new {
        return false;
    }
    last.clear();
    last.push_str(new);
    true
}

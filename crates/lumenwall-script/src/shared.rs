//! State shared between the engine and its native functions.
//!
//! Native closures capture an `Rc<Shared>`. `Shared` holds JS handles (`Persistent`) only
//! inside its `RefCell`s, which the engine empties before the runtime is freed, so no
//! closure ever releases a JS value after its runtime.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use rquickjs::{Ctx, Function, Object, Persistent};

use crate::host::{Clock, Cursor, Display, LogLevel, SceneHost};
use crate::storage::StorageFile;
use crate::value::{LayerId, ObjectRef, Value};
use crate::{ScriptId, ScriptLimits};

/// The host reference installed for the duration of an engine entry point.
pub(crate) struct HostSlot {
    ptr: Cell<Option<NonNull<dyn SceneHost + 'static>>>,
    busy: Cell<bool>,
}

/// Keeps the host installed; restores the previous one on drop.
pub(crate) struct HostGuard<'s, 'h> {
    slot: &'s HostSlot,
    prev: Option<NonNull<dyn SceneHost + 'static>>,
    _host: PhantomData<&'h mut dyn SceneHost>,
}

impl HostSlot {
    fn new() -> Self {
        HostSlot {
            ptr: Cell::new(None),
            busy: Cell::new(false),
        }
    }

    /// Installs `host` until the returned guard is dropped. The guard borrows `host`, so
    /// the erased pointer can never outlive it.
    pub(crate) fn enter<'s, 'h>(&'s self, host: &'h mut (dyn SceneHost + 'h)) -> HostGuard<'s, 'h> {
        let raw: NonNull<dyn SceneHost + 'h> = NonNull::from(host);
        // SAFETY: only the trait-object lifetime bound is erased. The pointer is removed
        // from the slot when the guard (which holds the `'h` borrow) is dropped, and it is
        // only dereferenced through `with`, inside that window.
        let raw: NonNull<dyn SceneHost + 'static> = unsafe { std::mem::transmute(raw) };
        let prev = self.ptr.replace(Some(raw));
        HostGuard {
            slot: self,
            prev,
            _host: PhantomData,
        }
    }

    /// Runs `f` with the installed host. `None` when no host is installed or when called
    /// re-entrantly from inside another host call.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut dyn SceneHost) -> R) -> Option<R> {
        let ptr = self.ptr.get()?;
        if self.busy.replace(true) {
            return None;
        }
        struct Reset<'a>(&'a Cell<bool>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _reset = Reset(&self.busy);
        // SAFETY: the pointer is valid while a `HostGuard` exists (see `enter`), and the
        // `busy` flag guarantees this is the only live reference derived from it.
        Some(f(unsafe { &mut *ptr.as_ptr() }))
    }
}

impl Drop for HostGuard<'_, '_> {
    fn drop(&mut self) {
        self.slot.ptr.set(self.prev);
    }
}

/// Per-frame snapshot of host state read by `engine` and `input` getters.
#[derive(Clone, Copy, Default)]
pub(crate) struct Frame {
    pub clock: Clock,
    pub cursor: Cursor,
    pub display: Display,
}

/// Where a live vector view reads and writes its value (spec 06 §4.3).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Target {
    /// A member of a host object (`thisLayer.origin`, `thisScene.clearcolor`…).
    Member { obj: ObjectRef, name: &'static str },
    /// The value a property script is bound to.
    Bound(ScriptId),
    /// A script property (`scriptProperties.foo`).
    ScriptProp { script: ScriptId, index: u16 },
}

/// Event functions a script may export (`IComponent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Init,
    Update,
    Destroy,
    ResizeScreen,
    ApplyUserProperties,
    ApplyGeneralSettings,
    CursorEnter,
    CursorLeave,
    CursorMove,
    CursorDown,
    CursorUp,
    CursorClick,
    MediaStatusChanged,
    MediaPlaybackChanged,
    MediaPropertiesChanged,
    MediaThumbnailChanged,
    MediaTimelineChanged,
}

impl Event {
    pub(crate) const ALL: [Event; 17] = [
        Event::Init,
        Event::Update,
        Event::Destroy,
        Event::ResizeScreen,
        Event::ApplyUserProperties,
        Event::ApplyGeneralSettings,
        Event::CursorEnter,
        Event::CursorLeave,
        Event::CursorMove,
        Event::CursorDown,
        Event::CursorUp,
        Event::CursorClick,
        Event::MediaStatusChanged,
        Event::MediaPlaybackChanged,
        Event::MediaPropertiesChanged,
        Event::MediaThumbnailChanged,
        Event::MediaTimelineChanged,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Event::Init => "init",
            Event::Update => "update",
            Event::Destroy => "destroy",
            Event::ResizeScreen => "resizeScreen",
            Event::ApplyUserProperties => "applyUserProperties",
            Event::ApplyGeneralSettings => "applyGeneralSettings",
            Event::CursorEnter => "cursorEnter",
            Event::CursorLeave => "cursorLeave",
            Event::CursorMove => "cursorMove",
            Event::CursorDown => "cursorDown",
            Event::CursorUp => "cursorUp",
            Event::CursorClick => "cursorClick",
            Event::MediaStatusChanged => "mediaStatusChanged",
            Event::MediaPlaybackChanged => "mediaPlaybackChanged",
            Event::MediaPropertiesChanged => "mediaPropertiesChanged",
            Event::MediaThumbnailChanged => "mediaThumbnailChanged",
            Event::MediaTimelineChanged => "mediaTimelineChanged",
        }
    }
}

/// Which mechanism a script belongs to (spec 06 §0.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// M1: ES module bound to a property.
    Property,
    /// M2: dynamic text layer.
    Text,
}

/// One loaded script.
pub(crate) struct Script {
    pub key: String,
    pub kind: Kind,
    pub layer: LayerId,
    pub object: ObjectRef,
    pub property: String,
    pub binding: u64,
    pub prop_names: Vec<String>,
    pub active: bool,
    /// M2: `init` already ran.
    pub initialized: bool,
    /// Consecutive failed `update` calls.
    pub update_errors: u8,
    /// Non-fatal `update` errors logged so far.
    pub error_logs: u8,
    /// M2: last text handed to the host.
    pub text: String,
    /// M1: module namespace (`this` of event calls). M2: the layer record.
    pub this: Option<Persistent<Object<'static>>>,
    pub handlers: [Option<Persistent<Function<'static>>>; 17],
    /// `thisLayer` while this script runs.
    pub layer_obj: Option<Persistent<Object<'static>>>,
    /// `thisObject` while this script runs.
    pub object_obj: Option<Persistent<Object<'static>>>,
    /// Cached live view passed to `update(value)`, with its dimension.
    pub view: Option<(usize, Persistent<Object<'static>>)>,
    /// M2: the layer's local `thisLayer` (holds the `text` data property).
    pub text_layer: Option<Persistent<Object<'static>>>,
}

impl Script {
    pub(crate) fn new(
        key: String,
        kind: Kind,
        layer: LayerId,
        object: ObjectRef,
        property: &str,
        binding: u64,
        prop_names: &[&str],
    ) -> Self {
        Script {
            key,
            kind,
            layer,
            object,
            property: property.to_owned(),
            binding,
            prop_names: prop_names.iter().map(|s| (*s).to_owned()).collect(),
            active: true,
            initialized: false,
            update_errors: 0,
            error_logs: 0,
            text: String::new(),
            this: None,
            handlers: Default::default(),
            layer_obj: None,
            object_obj: None,
            view: None,
            text_layer: None,
        }
    }

    pub(crate) fn handler(&self, e: Event) -> Option<&Persistent<Function<'static>>> {
        self.handlers[e as usize].as_ref()
    }

    /// Drops every JS handle held for this script.
    pub(crate) fn release(&mut self) {
        self.this = None;
        self.handlers = Default::default();
        self.layer_obj = None;
        self.object_obj = None;
        self.view = None;
        self.text_layer = None;
    }
}

pub(crate) struct Timer {
    pub due: Instant,
    pub period: Duration,
    pub callback: Persistent<Function<'static>>,
    pub owner: Option<ScriptId>,
}

#[derive(Default)]
pub(crate) struct Timers {
    pub next_timeout: u32,
    pub next_interval: u32,
    pub timeouts: BTreeMap<u32, Timer>,
    pub intervals: BTreeMap<u32, Timer>,
}

impl Timers {
    pub(crate) fn is_empty(&self) -> bool {
        self.timeouts.is_empty() && self.intervals.is_empty()
    }

    pub(crate) fn remove_owned_by(&mut self, owner: ScriptId) {
        self.timeouts.retain(|_, t| t.owner != Some(owner));
        self.intervals.retain(|_, t| t.owner != Some(owner));
    }
}

/// Float32Arrays returned by `engine.registerAudioBuffers` for one resolution.
pub(crate) struct AudioRegistration {
    pub resolution: usize,
    pub buffers: Persistent<Object<'static>>,
    pub left: Persistent<Object<'static>>,
    pub right: Persistent<Object<'static>>,
    pub average: Persistent<Object<'static>>,
}

/// JS handles that are not tied to one script.
#[derive(Default)]
pub(crate) struct JsHandles {
    pub layer_cache: HashMap<LayerId, Persistent<Object<'static>>>,
    pub callbacks: Vec<Option<(Persistent<Function<'static>>, Option<ScriptId>)>>,
    pub audio: Vec<AudioRegistration>,
    pub user_props: Option<Persistent<Object<'static>>>,
    pub scene: Option<Persistent<Object<'static>>>,
    pub engine: Option<Persistent<Object<'static>>>,
    pub m2_ctx: Option<Persistent<Object<'static>>>,
    pub mat3: Option<Persistent<Function<'static>>>,
    pub mat4: Option<Persistent<Function<'static>>>,
}

pub(crate) struct Shared {
    pub host: HostSlot,
    pub frame: Cell<Frame>,
    /// Script whose code is running (module evaluation, events, its timers).
    pub current: Cell<Option<ScriptId>>,
    pub scripts: RefCell<Vec<Script>>,
    pub timers: RefCell<Timers>,
    pub js: RefCell<JsHandles>,
    pub screen_storage: StorageFile,
    pub global_storage: StorageFile,
    pub user_props: RefCell<Vec<(String, Value)>>,
    pub general_settings: RefCell<Vec<(String, Value)>>,
    pub deadline: Cell<Option<Instant>>,
    pub interrupted: Cell<bool>,
    /// rquickjs class id shared by all Rust classes (fast "is native class" test).
    pub class_id: Cell<u32>,
    pub limits: ScriptLimits,
    pub warnings: Cell<u32>,
    /// A module is being evaluated (its rejection is reported as a load failure).
    pub loading: Cell<bool>,
}

/// Logged warnings are capped so a misbehaving script cannot flood the log.
const MAX_WARNINGS: u32 = 64;

impl Shared {
    pub(crate) fn new(
        limits: ScriptLimits,
        screen_storage: StorageFile,
        global_storage: StorageFile,
        user_props: Vec<(String, Value)>,
        general_settings: Vec<(String, Value)>,
    ) -> Self {
        Shared {
            host: HostSlot::new(),
            frame: Cell::new(Frame::default()),
            current: Cell::new(None),
            scripts: RefCell::new(Vec::new()),
            timers: RefCell::new(Timers::default()),
            js: RefCell::new(JsHandles::default()),
            screen_storage,
            global_storage,
            user_props: RefCell::new(user_props),
            general_settings: RefCell::new(general_settings),
            deadline: Cell::new(None),
            interrupted: Cell::new(false),
            class_id: Cell::new(0),
            limits,
            warnings: Cell::new(0),
            loading: Cell::new(false),
        }
    }

    /// Logs through the host when one is installed, else through `tracing`.
    pub(crate) fn log(&self, level: LogLevel, msg: &str) {
        if self.host.with(|h| h.log(level, msg)).is_none() {
            match level {
                LogLevel::Info => tracing::info!(target: "lumenwall_script::console", "{msg}"),
                LogLevel::Warn => tracing::warn!(target: "lumenwall_script", "{msg}"),
                LogLevel::Error => tracing::error!(target: "lumenwall_script", "{msg}"),
            }
        }
    }

    /// Rate-limited warning.
    pub(crate) fn warn(&self, msg: &str) {
        let n = self.warnings.get();
        if n < MAX_WARNINGS {
            self.warnings.set(n + 1);
            self.log(LogLevel::Warn, msg);
            if n + 1 == MAX_WARNINGS {
                self.log(LogLevel::Warn, "further script warnings suppressed");
            }
        }
    }

    /// Host member read; error when no host is installed.
    pub(crate) fn host_get(
        &self,
        ctx: &Ctx<'_>,
        obj: ObjectRef,
        name: &str,
    ) -> rquickjs::Result<Option<Value>> {
        self.host
            .with(|h| h.get(obj, name))
            .ok_or_else(|| no_host(ctx))
    }

    pub(crate) fn host_set(
        &self,
        ctx: &Ctx<'_>,
        obj: ObjectRef,
        name: &str,
        v: &Value,
    ) -> rquickjs::Result<bool> {
        self.host
            .with(|h| h.set(obj, name, v))
            .ok_or_else(|| no_host(ctx))
    }

    /// Reads the value behind a live view.
    pub(crate) fn read_target(&self, ctx: &Ctx<'_>, t: Target) -> rquickjs::Result<Option<Value>> {
        match t {
            Target::Member { obj, name } => self.host_get(ctx, obj, name),
            Target::Bound(id) => {
                let scripts = self.scripts.borrow();
                let s = &scripts[id.0 as usize];
                self.host
                    .with(|h| h.get(s.object, &s.property))
                    .ok_or_else(|| no_host(ctx))
            }
            Target::ScriptProp { script, index } => {
                let scripts = self.scripts.borrow();
                let s = &scripts[script.0 as usize];
                let name = &s.prop_names[usize::from(index)];
                self.host
                    .with(|h| h.script_property(s.binding, name))
                    .ok_or_else(|| no_host(ctx))
            }
        }
    }

    /// Writes the value behind a live view.
    pub(crate) fn write_target(&self, ctx: &Ctx<'_>, t: Target, v: &Value) -> rquickjs::Result<()> {
        let done = match t {
            Target::Member { obj, name } => self.host.with(|h| {
                h.set(obj, name, v);
            }),
            Target::Bound(id) => {
                let scripts = self.scripts.borrow();
                let s = &scripts[id.0 as usize];
                self.host.with(|h| {
                    h.set(s.object, &s.property, v);
                })
            }
            Target::ScriptProp { script, index } => {
                let scripts = self.scripts.borrow();
                let s = &scripts[script.0 as usize];
                let name = &s.prop_names[usize::from(index)];
                self.host.with(|h| {
                    h.set_script_property(s.binding, name, v);
                })
            }
        };
        done.ok_or_else(|| no_host(ctx))
    }

    /// Drops every JS handle (called before the runtime is freed).
    pub(crate) fn release_js(&self) {
        for s in self.scripts.borrow_mut().iter_mut() {
            s.release();
        }
        *self.timers.borrow_mut() = Timers::default();
        *self.js.borrow_mut() = JsHandles::default();
    }
}

fn no_host(ctx: &Ctx<'_>) -> rquickjs::Error {
    crate::util::plain_error(ctx, "scene host unavailable")
}

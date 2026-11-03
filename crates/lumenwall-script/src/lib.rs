//! SceneScript JavaScript runtime (spec 06, Wallpaper Engine `lib.sceneScript.d.ts`).
//!
//! One [`ScriptEngine`] per wallpaper instance owns a QuickJS-ng runtime and context
//! (via `rquickjs`). The scene engine implements [`SceneHost`] and lends it to every
//! entry point:
//!
//! - [`ScriptEngine::load_property_script`] — property scripts (ES modules exporting
//!   `update(value)` and the other `IComponent` events);
//! - [`ScriptEngine::create_text_script`] / [`ScriptEngine::tick_text`] — dynamic text
//!   layers;
//! - [`ScriptEngine::tick`] — once per frame: timers, `update` calls, promise jobs;
//! - event methods (`resize_screen`, `apply_user_properties`, `cursor_event`, `media_*`…).
//!
//! An exhausted time budget, an out-of-memory condition, a load or event error, or three
//! consecutive `update` errors disable the faulty script with a log line; an isolated
//! `update` error is logged and the script keeps running. Nothing panics.

mod convert;
mod engine;
mod globals;
mod host;
mod hostobj;
pub mod mock;
mod modules;
mod shared;
mod storage;
mod text;
mod util;
mod value;
mod vector;

pub use engine::{
    PropertyScript, ScriptConfig, ScriptEngine, ScriptError, ScriptId, ScriptLimits, ScriptStats,
    TextScript,
};
pub use host::{Clock, Cursor, Display, LogLevel, SceneHost};
pub use storage::{STORAGE_QUOTA, StorageFile};
pub use value::{
    CallbackId, CursorEvent, CursorEventKind, LayerId, MediaProperties, MediaSnapshot,
    MediaThumbnail, ObjectRef, PlaybackState, Value,
};

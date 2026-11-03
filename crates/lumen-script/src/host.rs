//! The interface the scene engine implements for scripts.

use std::time::Instant;

use crate::value::{LayerId, ObjectRef, Value};

/// Scene clock for the current frame (spec 06 §6.1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Clock {
    /// `t_g`: render time in seconds (`engine.runtime`). Does not advance while paused.
    pub runtime: f32,
    /// `t_g − t_g_prev` computed in `f32` (`engine.frametime`).
    pub frametime: f32,
    /// `(local_hour·60 + local_minute) / 1440` (`engine.timeOfDay`).
    pub time_of_day: f32,
}

/// Cursor state for the current frame (`input`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cursor {
    /// `input.cursorScreenPosition`, in screen pixels (WE documentation).
    pub screen_position: [f32; 2],
    /// `input.cursorWorldPosition` (only x and y are meaningful, as in WE).
    pub world_position: [f32; 3],
    /// `input.cursorLeftDown`.
    pub left_down: bool,
}

/// Display and environment information (`engine.*` queries).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    /// `engine.screenResolution`, pixels.
    pub screen_resolution: [f32; 2],
    /// `engine.canvasSize`, pixels (2D scenes).
    pub canvas_size: [f32; 2],
    /// `engine.isScreensaver()`; `engine.isWallpaper()` is its negation.
    pub screensaver: bool,
    /// `engine.isRunningInEditor()`.
    pub editor: bool,
}

impl Default for Display {
    fn default() -> Self {
        Display {
            screen_resolution: [1920.0, 1080.0],
            canvas_size: [1920.0, 1080.0],
            screensaver: false,
            editor: false,
        }
    }
}

/// Severity of a message logged on behalf of scripts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    /// `console.log`.
    Info,
    /// Recoverable engine conditions (quota exceeded, frame budget exhausted…).
    Warn,
    /// `console.error` and script failures.
    Error,
}

/// What the scene engine provides to scripts.
///
/// The engine passes `&mut dyn SceneHost` to every [`crate::ScriptEngine`] entry point that
/// runs JavaScript; the reference is only used for the duration of that call. Host methods
/// are never re-entered: no JavaScript runs while one of them executes.
///
/// Object members are addressed by name, using the Wallpaper Engine spelling
/// (`origin`, `alpha`, `pointsize`, `bloomstrength`…). The script crate applies the
/// reference quirks itself (for example `thisScene.clearenabled` reads `bloom`), so the
/// host only answers for true names.
pub trait SceneHost {
    /// Clock for the current frame. Read once per [`crate::ScriptEngine::tick`].
    fn clock(&self) -> Clock;

    /// Monotonic time used by `engine.setTimeout` / `engine.setInterval` (spec 06 §10).
    fn now(&self) -> Instant {
        Instant::now()
    }

    /// Cursor for the current frame. Read once per tick.
    fn cursor(&self) -> Cursor {
        Cursor::default()
    }

    /// Display information. Read once per tick.
    fn display(&self) -> Display {
        Display::default()
    }

    /// Fills `left` and `right` (both of length 16, 32 or 64) with the audio spectrum
    /// for this frame. Called once per tick and resolution, only when a script called
    /// `engine.registerAudioBuffers`. Returns `false` when no audio is available (buffers
    /// are then zeroed).
    fn audio_spectrum(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let _ = (left, right);
        false
    }

    /// Reads member `name` of `obj`. `None` means "no such member" (`undefined` in JS).
    fn get(&self, obj: ObjectRef, name: &str) -> Option<Value>;

    /// Writes member `name` of `obj`; the value has already been converted following
    /// spec 06 §4.2. Returns `false` when the member does not exist or is read-only
    /// (the write is then silently ignored).
    fn set(&mut self, obj: ObjectRef, name: &str, value: &Value) -> bool {
        let _ = (obj, name, value);
        false
    }

    /// Calls method `method` of `obj` (`thisLayer.play()`, `thisScene.createLayer(…)`…).
    /// `None` means the method is not supported; scripts then get the documented neutral
    /// result (`undefined`, `false`, `0`, an empty array…).
    fn call(&mut self, obj: ObjectRef, method: &str, args: &[Value]) -> Option<Value> {
        let _ = (obj, method, args);
        None
    }

    /// Whether `id` is a created, scriptable object (spec 06 §6.3).
    fn layer_exists(&self, id: LayerId) -> bool;

    /// First scriptable object named `name`, in render order (spec 06 §6.3).
    fn find_layer(&self, name: &str) -> Option<LayerId>;

    /// Number of layers in render order (`thisScene.getLayerCount()`).
    fn layer_count(&self) -> usize;

    /// Layer at `index` in render order (`thisScene.enumerateLayers()`).
    fn layer_at(&self, index: usize) -> Option<LayerId>;

    /// Current value of script property `name` of the script registered with `binding`
    /// (the `scriptproperties` JSON of the scripted value, spec 06 §6.9).
    fn script_property(&self, binding: u64, name: &str) -> Option<Value> {
        let _ = (binding, name);
        None
    }

    /// Writes a script property (component writes through a live vector view).
    fn set_script_property(&mut self, binding: u64, name: &str, value: &Value) -> bool {
        let _ = (binding, name, value);
        false
    }

    /// `engine.registerAsset(file, precache)`: returns a host asset id.
    fn register_asset(&mut self, path: &str, precache: bool) -> Option<u32> {
        let _ = (path, precache);
        None
    }

    /// `engine.openUserShortcut(name)`.
    fn open_user_shortcut(&mut self, name: &str) -> bool {
        let _ = name;
        false
    }

    /// Receives `console.log` / `console.error` output and script failures.
    fn log(&mut self, level: LogLevel, message: &str) {
        match level {
            LogLevel::Info => tracing::info!(target: "lumen_script::console", "{message}"),
            LogLevel::Warn => tracing::warn!(target: "lumen_script", "{message}"),
            LogLevel::Error => tracing::error!(target: "lumen_script", "{message}"),
        }
    }
}

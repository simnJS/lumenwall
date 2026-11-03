//! What the engine runs, independent of the command-line crate (which depends on this one and
//! converts its `ExecutionPlan` into an [`EnginePlan`]).

use std::collections::BTreeMap;
use std::path::PathBuf;

use lumen_render::{ClampMode, ScalingMode};

/// Output mode (spec 07 §4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    /// Resizable 640×480 window.
    Window,
    /// Borderless window at a fixed geometry (`-w XxYxWxH`).
    ExplicitWindow {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    /// One wallpaper surface per output (Wayland layer-shell, X11 desktop windows, Windows).
    Desktop,
}

/// One renderer: a wallpaper on the window, an output or a span group.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetPlan {
    /// `default` for the window, the output name, or `span:A,B`.
    pub key: String,
    /// Output names (empty for the window).
    pub outputs: Vec<String>,
    /// Resolved wallpaper folder.
    pub wallpaper: PathBuf,
    pub scaling: ScalingMode,
    pub clamp: ClampMode,
}

/// Audio settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioPlan {
    pub silent: bool,
    /// 0..=128.
    pub volume: u8,
    pub automute: bool,
    /// Spectrum capture for audio-reactive wallpapers.
    pub processing: bool,
}

impl Default for AudioPlan {
    fn default() -> Self {
        Self {
            silent: false,
            volume: 15,
            automute: true,
            processing: true,
        }
    }
}

/// Fullscreen pause settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PausePlan {
    pub enabled: bool,
    pub only_active: bool,
    pub ignore_app_ids: Vec<String>,
}

impl Default for PausePlan {
    fn default() -> Self {
        Self {
            enabled: true,
            only_active: false,
            ignore_app_ids: Vec::new(),
        }
    }
}

/// `--render-debug` switches (spec 07 §20).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugPlan {
    pub base_only: bool,
    pub no_solid_final: bool,
    pub pass_log: bool,
    pub only_object: Option<u32>,
    pub skip_objects: Vec<u32>,
    pub skip_effects: Vec<u32>,
}

/// Wayland layer (ignored elsewhere).
pub use lumen_platform::Layer;

/// `--hwdec`: video decoder choice (spec 07 §14.1).
pub use lumen_video::HwDecode;

/// Everything the engine needs for a run.
#[derive(Clone, Debug, PartialEq)]
pub struct EnginePlan {
    pub mode: OutputMode,
    pub targets: Vec<TargetPlan>,
    pub layer: Layer,
    /// Frame-rate cap, at least 1.
    pub fps: u32,
    pub audio: AudioPlan,
    /// Video decoding (`--hwdec`).
    pub hwdec: HwDecode,
    pub pause: PausePlan,
    pub mouse: bool,
    pub parallax: bool,
    pub particles: bool,
    /// `--assets-dir`.
    pub assets_dir: Option<PathBuf>,
    /// `--set-property` overrides by exact key.
    pub properties: BTreeMap<String, String>,
    /// Screenshot after `delay` rendered frames.
    pub screenshot: Option<(PathBuf, u32)>,
    pub list_properties: bool,
    pub dump_structure: bool,
    pub debug: DebugPlan,
}

impl Default for EnginePlan {
    fn default() -> Self {
        Self {
            mode: OutputMode::Window,
            targets: Vec::new(),
            layer: Layer::Bottom,
            fps: 30,
            audio: AudioPlan::default(),
            hwdec: HwDecode::Auto,
            pause: PausePlan::default(),
            mouse: true,
            parallax: true,
            particles: true,
            assets_dir: None,
            properties: BTreeMap::new(),
            screenshot: None,
            list_properties: false,
            dump_structure: false,
            debug: DebugPlan::default(),
        }
    }
}

/// Messages from the application to a running engine.
#[derive(Clone, Debug)]
pub enum EngineControl {
    /// Leave the loop (signal number, or `None` for any other reason).
    Stop(Option<i32>),
    /// New frame-rate cap.
    SetFps(u32),
    /// Property overrides changed (configuration reload): re-applied to every wallpaper.
    SetProperties(BTreeMap<String, String>),
}

/// Why [`crate::Engine::run`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// [`EngineControl::Stop`] with a signal number.
    Signal(i32),
    /// Stop requested by the application without a signal.
    Requested,
    /// Window closed or display connection lost.
    Driver,
}

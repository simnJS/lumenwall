//! What the engine runs, independent of the command-line crate (which depends on this one and
//! converts its `ExecutionPlan` into an [`EnginePlan`]).

use std::collections::BTreeMap;
use std::path::PathBuf;

use lumenwall_render::{ClampMode, QualitySettings, ScalingMode};

use crate::scene::EngineSettings;

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
pub use lumenwall_platform::Layer;

/// `--hwdec`: video decoder choice (spec 07 §14.1).
pub use lumenwall_video::HwDecode;

/// Default [`EnginePlan::gpu_release`]: 5 minutes.
pub const DEFAULT_GPU_RELEASE: std::time::Duration = std::time::Duration::from_secs(300);

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
    /// Once every output has been paused this long, the GPU device, the surfaces' swap chains
    /// and every GPU resource are released, and rebuilt when an output resumes. `None`: never.
    pub gpu_release: Option<std::time::Duration>,
    pub mouse: bool,
    pub parallax: bool,
    pub particles: bool,
    /// Quality settings (`--msaa`, `--texture-resolution`, `--hdr`, `--native-resolution`).
    /// The development environment variables (`LUMENWALL_MSAA`, ...) override them setting by
    /// setting, see [`EnginePlan::engine_settings`].
    pub quality: QualitySettings,
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

impl EnginePlan {
    /// The settings scenes are built with: the plan's quality, with each development variable
    /// that is set and valid (`LUMENWALL_MSAA`, `LUMENWALL_TEXTURE_RESOLUTION`,
    /// `LUMENWALL_HDR`, `LUMENWALL_FULL_SIZE_CHAINS`) overriding its setting.
    #[must_use]
    pub fn engine_settings(&self) -> EngineSettings {
        self.engine_settings_with(|name| std::env::var(name).ok())
    }

    /// [`Self::engine_settings`] over any variable lookup.
    #[must_use]
    pub fn engine_settings_with(&self, get: impl Fn(&str) -> Option<String>) -> EngineSettings {
        quality_with_overrides(self.quality, get)
    }
}

/// `quality` as engine settings, each development variable that is set and valid overriding
/// its setting.
#[must_use]
pub fn quality_with_overrides(
    mut q: QualitySettings,
    get: impl Fn(&str) -> Option<String>,
) -> EngineSettings {
    if let Some(v) = get("LUMENWALL_MSAA").and_then(|v| v.parse().ok()) {
        q.msaa = v;
    }
    if let Some(v) = get("LUMENWALL_TEXTURE_RESOLUTION").and_then(|v| v.parse().ok()) {
        q.texture_resolution = v;
    }
    if let Some(v) = get("LUMENWALL_HDR").and_then(|v| v.parse().ok()) {
        q.hdr = v;
    }
    if let Some(v) = get("LUMENWALL_FULL_SIZE_CHAINS") {
        q.native_resolution = v != "0";
    }
    EngineSettings { quality: q }
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
            gpu_release: Some(DEFAULT_GPU_RELEASE),
            mouse: true,
            parallax: true,
            particles: true,
            quality: QualitySettings::default(),
            assets_dir: None,
            properties: BTreeMap::new(),
            screenshot: None,
            list_properties: false,
            dump_structure: false,
            debug: DebugPlan::default(),
        }
    }
}

/// Receives the outcome of an [`EngineControl::Reload`] (`Err`: nothing changed).
pub type ReloadDone = std::sync::mpsc::SyncSender<Result<(), String>>;

/// Messages from the application to a running engine.
#[derive(Clone, Debug)]
pub enum EngineControl {
    /// Leave the loop (signal number, or `None` for any other reason).
    Stop(Option<i32>),
    /// New frame-rate cap.
    SetFps(u32),
    /// The complete set of property overrides (configuration reload, live preview): every
    /// wallpaper takes these values, and its own defaults for the properties left out.
    SetProperties(BTreeMap<String, String>),
    /// Pause (`true`) or resume every output, whatever the platform says.
    SetPaused(bool),
    /// Switch to a new plan without restarting: wallpapers are loaded or released, outputs
    /// rebound, audio, fps, pause and surfaces updated, only where the plan differs. Either
    /// the whole plan applies, or `done` receives the error and the engine is unchanged.
    /// `assets_dir`, `hwdec`, input, particles and debug switches apply to the wallpapers
    /// loaded afterwards (a change of quality settings rebuilds every wallpaper); `screenshot`, `list_properties` and `dump_structure` are ignored.
    Reload {
        plan: Box<EnginePlan>,
        done: Option<ReloadDone>,
    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use lumenwall_render::{HdrMode, Msaa, TextureResolution};

    #[test]
    fn the_default_plan_asks_for_the_cheapest_quality() {
        let settings = EnginePlan::default().engine_settings_with(|_| None);
        assert!(settings.quality.is_default());
    }

    #[test]
    fn plan_quality_is_used_and_development_variables_override_it() {
        let plan = EnginePlan {
            quality: QualitySettings {
                msaa: Msaa::X4,
                texture_resolution: TextureResolution::Half,
                hdr: HdrMode::Scene,
                native_resolution: true,
            },
            ..EnginePlan::default()
        };
        assert_eq!(plan.engine_settings_with(|_| None).quality, plan.quality);
        let q = plan
            .engine_settings_with(|name| match name {
                "LUMENWALL_MSAA" => Some("x2".into()),
                "LUMENWALL_TEXTURE_RESOLUTION" => Some("bogus".into()),
                "LUMENWALL_FULL_SIZE_CHAINS" => Some("0".into()),
                _ => None,
            })
            .quality;
        assert_eq!(q.msaa, Msaa::X2);
        // An unreadable variable keeps the plan's value.
        assert_eq!(q.texture_resolution, TextureResolution::Half);
        assert_eq!(q.hdr, HdrMode::Scene);
        assert!(!q.native_resolution);
    }
}

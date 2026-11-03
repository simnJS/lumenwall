//! Integration layer of Lumen: wires the asset VFS, the scene model, the shader translator, the
//! renderer, the particle simulation, audio and the platform surfaces into one engine.
//!
//! - [`Engine`]: load / start / run, driven by the `lumen` application (which adapts its
//!   `Engine` trait to this type: the application crate depends on this one).
//! - [`WallpaperResolver`]: Workshop id or path → wallpaper folder (Steam discovery).
//! - [`render_headless`]: off-screen rendering of a wallpaper to PNG files.
//! - [`ShaderProvider`]: WE shader + combos → render-ready shader (lumen-shader, or WGSL
//!   stand-ins).
//! - [`TextSink`]: receives the text that SceneScript computes for text layers.

mod assets;
mod audio;
mod bloom;
mod engine;
mod headless;
mod particles;
pub mod plan;
mod scene;
mod script;
pub mod shaders;
mod texture;
mod wallpaper;

use std::path::PathBuf;

pub use assets::{SceneSource, VfsBytes, WallpaperResolver, open_vfs};
pub use engine::{Engine, EngineWaker};
pub use headless::{HeadlessOptions, HeadlessReport, render_headless};
pub use plan::{
    AudioPlan, DebugPlan, EngineControl, EnginePlan, OutputMode, PausePlan, Stopped, TargetPlan,
};
pub use scene::{FrameState, SceneOptions, SceneRuntime, SceneStats, ScreenState, build_scene};
pub use script::TextSink;
pub use shaders::{ShaderProvider, default_provider};
pub use texture::{LoadedTexture, TextureCache};
pub use wallpaper::Wallpaper;

/// Script types used by [`SceneRuntime::media_changed`] and [`SceneRuntime::script_stats`].
pub use lumen_script::{MediaSnapshot, ScriptStats};

/// Output composition modes (spec 04 §21), re-exported for the application.
pub use lumen_render::{ClampMode, ScalingMode};

/// Per-user cache folder of Lumen (pipeline and shader caches).
pub fn cache_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Lumen").join("cache"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|d| d.join("lumen"))
    }
}

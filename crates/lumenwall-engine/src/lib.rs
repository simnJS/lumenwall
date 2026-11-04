//! Integration layer of Lumenwall: wires the asset VFS, the scene model, the shader translator, the
//! renderer, the particle simulation, audio and the platform surfaces into one engine.
//!
//! - [`Engine`]: load / start / run, driven by the `lumenwall` application (which adapts its
//!   `Engine` trait to this type: the application crate depends on this one).
//! - [`WallpaperResolver`]: Workshop id or path → wallpaper folder (Steam discovery).
//! - [`render_headless`]: off-screen rendering of a wallpaper to PNG files.
//! - [`ShaderProvider`]: WE shader + combos → render-ready shader (lumenwall-shader, or WGSL
//!   stand-ins).
//! - [`TextSink`]: receives the text that SceneScript computes for text layers.
//! - [`EngineMonitor`]: outputs, wallpapers, properties and frame counters for the
//!   application (IPC daemon), read without waking the engine.

mod assets;
mod audio;
mod bloom;
mod clock;
mod engine;
mod headless;
mod model3d;
pub mod monitor;
mod particles;
pub mod plan;
pub mod puppet;
mod scene;
mod script;
pub mod shaders;
pub mod text;
mod texture;
mod video;
mod vulkan_driver;
mod wallpaper;
mod web;

use std::path::PathBuf;

pub use assets::{SceneSource, VfsBytes, WallpaperResolver, open_vfs};
pub use engine::{Engine, EngineWaker, run_engine_host};
pub use headless::{HeadlessOptions, HeadlessReport, render_headless};
pub use monitor::{EngineMonitor, EngineReport};
pub use plan::{
    AudioPlan, DebugPlan, EngineControl, EnginePlan, OutputMode, PausePlan, ReloadDone, Stopped,
    TargetPlan,
};
pub use scene::{
    EngineSettings, FrameState, LayerStats, SceneOptions, SceneRuntime, SceneStats, ScreenState,
    build_scene, build_scene_with,
};
pub use script::TextSink;
pub use shaders::{ShaderProvider, default_provider};
pub use texture::{LoadedTexture, TextureCache};
pub use vulkan_driver::restrict_vulkan_loader;
pub use wallpaper::Wallpaper;

/// Script types used by [`SceneRuntime::media_changed`] and [`SceneRuntime::script_stats`].
pub use lumenwall_script::{MediaSnapshot, ScriptStats};

/// Output composition modes (spec 04 §21), re-exported for the application.
pub use lumenwall_render::{
    ClampMode, HdrMode, Msaa, QualitySettings, ScalingMode, TextureResolution,
};

/// Per-user cache folder of Lumenwall (pipeline and shader caches).
pub fn cache_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Lumenwall").join("cache"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|d| d.join("lumenwall"))
    }
}

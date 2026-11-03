//! Execution plan: everything the engine needs to know, computed from the command line and
//! the configuration file without touching the OS (the wallpaper lookups go through
//! [`WallpaperResolver`] and [`PlaylistCatalog`]).
//!
//! Merge rules (command line first):
//! - Layout: the command line's `-w`/`-r`/`--screen-span` replace the file's
//!   `[window]`/`[[screen]]`/`[[span]]` entirely; otherwise the file's layout is used.
//! - Default content: the command line's positional/`--bg`/untargeted `--playlist` replace
//!   the file's `wallpaper`/`playlist` together.
//! - Scaling and clamp: per-target value, then the command line's global value, then the
//!   file's global value, then the default (§4.3).
//! - Scalars: command line, then file, then the defaults of spec 07 §2.4. Flags such as
//!   `--silent` can only force their setting; `--volume` turns `silent` off.
//! - Properties: the file's table, then `--set-property` (later wins).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::cli::{CliArgs, CliMode, TargetSettings};
use crate::config::Config;
use crate::services::{PlaylistCatalog, WallpaperResolver};
use crate::settings::{
    Clamp, DEFAULT_FPS, DEFAULT_SCREENSHOT_DELAY, DEFAULT_VOLUME, HwDecode, Layer, RenderDebug,
    Scaling, ScreenshotFormat, WallpaperRef, WallpaperSource, WindowGeometry, clamp_volume,
    validate_fps, validate_screenshot_delay,
};

/// Identifies a render target: the window (`default` viewport), an output, or a span group.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TargetKey {
    Window,
    Screen(String),
    /// Synthetic key `span:A,B` (§2.3).
    Span(String),
}

impl fmt::Display for TargetKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Window => f.write_str("default"),
            Self::Screen(name) | Self::Span(name) => f.write_str(name),
        }
    }
}

/// A wallpaper reference and the folder it resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedWallpaper {
    pub reference: WallpaperRef,
    pub path: PathBuf,
}

/// One renderer: what it shows and how (§4.3, §4.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetPlan {
    pub key: TargetKey,
    /// Output names: none for the window, one for a screen, the group in order for a span.
    pub outputs: Vec<String>,
    pub wallpaper: ResolvedWallpaper,
    /// Playlist that drives this target (§17).
    pub playlist: Option<String>,
    pub scaling: Scaling,
    pub clamp: Clamp,
}

/// Output mode (§4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    /// Resizable 640×480 window.
    Window,
    /// Borderless, fixed, always-on-top window.
    ExplicitWindow(WindowGeometry),
    /// One surface per requested output.
    Desktop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioPlan {
    /// `--silent`: no sound from wallpapers.
    pub silent: bool,
    /// 0..=128.
    pub volume: u8,
    /// Mute while another application plays (§12).
    pub automute: bool,
    /// Spectrum capture for audio-reactive wallpapers (§13).
    pub processing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FullscreenPausePlan {
    pub enabled: bool,
    pub only_active: bool,
    /// Non-empty, case-insensitive app_id substrings.
    pub ignore_app_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputPlan {
    pub mouse: bool,
    pub parallax: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenshotPlan {
    pub path: PathBuf,
    pub format: ScreenshotFormat,
    /// Frames rendered before the capture, 0..=5 (§18.2).
    pub delay_frames: u32,
}

/// The complete, OS-independent description of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub mode: OutputMode,
    /// Window mode: exactly one target. Desktop mode: screens sorted by name, then span
    /// groups sorted by key (§4.3: outputs are visited in alphabetical order).
    pub targets: Vec<TargetPlan>,
    pub layer: Layer,
    /// Frame-rate cap, at least 1.
    pub fps: u32,
    pub hwdec: HwDecode,
    pub audio: AudioPlan,
    pub fullscreen_pause: FullscreenPausePlan,
    pub input: InputPlan,
    pub particles: bool,
    pub assets_dir: Option<PathBuf>,
    /// Property overrides by exact key (§3.6).
    pub properties: BTreeMap<String, String>,
    pub screenshot: Option<ScreenshotPlan>,
    pub list_properties: bool,
    pub dump_structure: bool,
    pub render_debug: RenderDebug,
}

impl ExecutionPlan {
    /// Minimum frame duration (§8.2).
    pub fn frame_interval(&self) -> Duration {
        Duration::from_secs(1) / self.fps.max(1)
    }

    pub fn target(&self, key: &TargetKey) -> Option<&TargetPlan> {
        self.targets.iter().find(|t| &t.key == key)
    }

    /// Every requested output name, span members included, for the "no requested output
    /// found" error (§4.3).
    pub fn requested_outputs(&self) -> Vec<&str> {
        self.targets
            .iter()
            .flat_map(|t| t.outputs.iter().map(String::as_str))
            .collect()
    }
}

impl fmt::Display for ExecutionPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.mode {
            OutputMode::Window => writeln!(f, "mode: window")?,
            OutputMode::ExplicitWindow(g) => writeln!(f, "mode: window {g}")?,
            OutputMode::Desktop => writeln!(f, "mode: desktop")?,
        }
        for target in &self.targets {
            write!(
                f,
                "{}: {} ({}, {}",
                target.key,
                target.wallpaper.path.display(),
                target.scaling,
                target.clamp
            )?;
            if let Some(playlist) = &target.playlist {
                write!(f, ", playlist {playlist}")?;
            }
            writeln!(f, ")")?;
        }
        write!(
            f,
            "fps {}, volume {}{}, automute {}, audio processing {}, fullscreen pause {}",
            self.fps,
            self.audio.volume,
            if self.audio.silent { " (silent)" } else { "" },
            self.audio.automute,
            self.audio.processing,
            self.fullscreen_pause.enabled
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// Message from the resolver or the playlist catalog, shown as is.
    #[error("{message}")]
    Resolve {
        reference: WallpaperRef,
        message: String,
    },
    #[error("at least one wallpaper must be specified")]
    NoWallpaper,
    #[error("invalid screenshot path '{path}': {reason}")]
    Screenshot { path: PathBuf, reason: String },
}

/// Lookups used while building a plan.
#[derive(Clone, Copy)]
pub struct PlanServices<'a> {
    pub resolver: &'a dyn WallpaperResolver,
    pub playlists: &'a dyn PlaylistCatalog,
}

struct Draft {
    key: TargetKey,
    outputs: Vec<String>,
    wallpaper: Option<WallpaperRef>,
    playlist: Option<String>,
    scaling: Scaling,
    clamp: Clamp,
}

/// Builds the plan from the command line and the optional configuration file.
pub fn build(
    cli: &CliArgs,
    config: Option<&Config>,
    services: PlanServices<'_>,
) -> Result<ExecutionPlan, PlanError> {
    let no_config = Config::default();
    let file = config.unwrap_or(&no_config);
    let layout = &cli.layout;
    let source = |path: &PathBuf| WallpaperRef::Source(WallpaperSource::classify(path.clone()));

    // Default content: command line or file, as a unit.
    let (default_wallpaper, window_playlist) = if layout.has_default_content() {
        (
            layout.default_wallpaper.clone(),
            layout.window_playlist.clone(),
        )
    } else {
        let first = file.playlist.clone().map(WallpaperRef::PlaylistFirst);
        (
            file.wallpaper.as_ref().map(source).or(first),
            file.playlist.clone(),
        )
    };

    let global_scaling = layout.global_scaling.or(file.scaling).unwrap_or_default();
    let global_clamp = layout.global_clamp.or(file.clamp).unwrap_or_default();
    let window = || Draft {
        key: TargetKey::Window,
        outputs: Vec::new(),
        wallpaper: window_playlist
            .clone()
            .map(WallpaperRef::PlaylistFirst)
            .or(default_wallpaper.clone()),
        playlist: window_playlist.clone(),
        scaling: global_scaling,
        clamp: global_clamp,
    };
    // A command-line target already holds the command-line global value it copied (§2.3).
    let cli_target = |key: TargetKey, outputs: Vec<String>, s: &TargetSettings| Draft {
        key,
        outputs,
        wallpaper: s.wallpaper.clone().or(default_wallpaper.clone()),
        playlist: s.playlist.clone(),
        scaling: s.scaling.or(file.scaling).unwrap_or_default(),
        clamp: s.clamp.or(file.clamp).unwrap_or_default(),
    };
    let file_target = |key: TargetKey,
                       outputs: Vec<String>,
                       wallpaper: &Option<PathBuf>,
                       playlist: &Option<String>,
                       scaling: Option<Scaling>,
                       clamp: Option<Clamp>| Draft {
        key,
        outputs,
        wallpaper: wallpaper
            .as_ref()
            .map(source)
            .or(playlist.clone().map(WallpaperRef::PlaylistFirst))
            .or(default_wallpaper.clone()),
        playlist: playlist.clone(),
        scaling: scaling
            .or(layout.global_scaling)
            .or(file.scaling)
            .unwrap_or_default(),
        clamp: clamp
            .or(layout.global_clamp)
            .or(file.clamp)
            .unwrap_or_default(),
    };

    let (mode, mut screens, mut spans) = match layout.mode {
        CliMode::Window(geometry) => (OutputMode::ExplicitWindow(geometry), vec![window()], vec![]),
        CliMode::Desktop => (
            OutputMode::Desktop,
            layout
                .screens
                .iter()
                .map(|s| {
                    cli_target(
                        TargetKey::Screen(s.name.clone()),
                        vec![s.name.clone()],
                        &s.settings,
                    )
                })
                .collect(),
            layout
                .spans
                .iter()
                .map(|s| {
                    cli_target(
                        TargetKey::Span(s.key.clone()),
                        s.outputs.clone(),
                        &s.settings,
                    )
                })
                .collect(),
        ),
        CliMode::Unset => match &file.window {
            Some(w) => {
                let geometry = WindowGeometry {
                    x: w.x,
                    y: w.y,
                    width: w.width,
                    height: w.height,
                };
                (OutputMode::ExplicitWindow(geometry), vec![window()], vec![])
            }
            None if file.screens.is_empty() && file.spans.is_empty() => {
                (OutputMode::Window, vec![window()], vec![])
            }
            None => (
                OutputMode::Desktop,
                file.screens
                    .iter()
                    .map(|s| {
                        let key = TargetKey::Screen(s.name.clone());
                        file_target(
                            key,
                            vec![s.name.clone()],
                            &s.wallpaper,
                            &s.playlist,
                            s.scaling,
                            s.clamp,
                        )
                    })
                    .collect(),
                file.spans
                    .iter()
                    .map(|s| {
                        let key = TargetKey::Span(s.key());
                        file_target(
                            key,
                            s.outputs(),
                            &s.wallpaper,
                            &s.playlist,
                            s.scaling,
                            s.clamp,
                        )
                    })
                    .collect(),
            ),
        },
    };
    screens.sort_by(|a: &Draft, b: &Draft| a.key.cmp(&b.key));
    spans.sort_by(|a: &Draft, b: &Draft| a.key.cmp(&b.key));

    // Resolve every wallpaper named on the command line (a typo is fatal even when a later
    // option overrides it), then those the targets use. Each reference is looked up once.
    let mut resolved: HashMap<WallpaperRef, PathBuf> = HashMap::new();
    let mut resolve = |reference: &WallpaperRef| -> Result<PathBuf, PlanError> {
        if let Some(path) = resolved.get(reference) {
            return Ok(path.clone());
        }
        let path = match reference {
            WallpaperRef::Source(source) => services.resolver.resolve(source),
            WallpaperRef::PlaylistFirst(name) => services.playlists.first_wallpaper(name),
        }
        .map_err(|e| PlanError::Resolve {
            reference: reference.clone(),
            message: e.to_string(),
        })?;
        resolved.insert(reference.clone(), path.clone());
        Ok(path)
    };
    for reference in &cli.mentioned {
        resolve(reference)?;
    }
    let mut targets = Vec::with_capacity(screens.len() + spans.len());
    for draft in screens.into_iter().chain(spans) {
        let reference = draft.wallpaper.ok_or(PlanError::NoWallpaper)?;
        let path = resolve(&reference)?;
        targets.push(TargetPlan {
            key: draft.key,
            outputs: draft.outputs,
            wallpaper: ResolvedWallpaper { reference, path },
            playlist: draft.playlist,
            scaling: draft.scaling,
            clamp: draft.clamp,
        });
    }

    let s = &cli.settings;
    let file_volume = file.volume.map(clamp_volume).unwrap_or(DEFAULT_VOLUME);
    let (silent, volume) = match s.volume {
        Some(volume) => (false, volume),
        None if s.silent => (true, file_volume),
        None => (file.silent.unwrap_or(false), file_volume),
    };

    let mut properties: BTreeMap<String, String> = file
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), v.to_text()))
        .collect();
    properties.extend(s.properties.iter().cloned());

    let screenshot = match s.screenshot.as_ref().or(file.screenshot.as_ref()) {
        None => None,
        Some(path) => {
            let format =
                ScreenshotFormat::from_path(path).map_err(|reason| PlanError::Screenshot {
                    path: path.clone(),
                    reason,
                })?;
            let delay_frames = s
                .screenshot_delay
                .or_else(|| {
                    file.screenshot_delay
                        .and_then(|d| validate_screenshot_delay(d).ok())
                })
                .unwrap_or(DEFAULT_SCREENSHOT_DELAY);
            Some(ScreenshotPlan {
                path: path.clone(),
                format,
                delay_frames,
            })
        }
    };

    let ignore_app_ids = if s.fullscreen_pause_ignore_appids.is_empty() {
        file.fullscreen_pause_ignore_appids
            .iter()
            .flatten()
            .filter(|p| !p.is_empty())
            .cloned()
            .collect()
    } else {
        s.fullscreen_pause_ignore_appids.clone()
    };

    Ok(ExecutionPlan {
        mode,
        targets,
        layer: s.layer.or(file.layer).unwrap_or_default(),
        fps: s
            .fps
            .or_else(|| file.fps.and_then(|f| validate_fps(f).ok()))
            .unwrap_or(DEFAULT_FPS),
        hwdec: s.hwdec.or(file.hwdec).unwrap_or_default(),
        audio: AudioPlan {
            silent,
            volume,
            automute: !s.no_automute && file.automute.unwrap_or(true),
            processing: !s.no_audio_processing && file.audio_processing.unwrap_or(true),
        },
        fullscreen_pause: FullscreenPausePlan {
            enabled: !s.no_fullscreen_pause && file.fullscreen_pause.unwrap_or(true),
            only_active: s.fullscreen_pause_only_active
                || file.fullscreen_pause_only_active.unwrap_or(false),
            ignore_app_ids,
        },
        input: InputPlan {
            mouse: !s.disable_mouse && file.mouse.unwrap_or(true),
            parallax: !s.disable_parallax && file.parallax.unwrap_or(true),
        },
        particles: !s.disable_particles && file.particles.unwrap_or(true),
        assets_dir: s.assets_dir.clone().or_else(|| file.assets_dir.clone()),
        properties,
        screenshot,
        list_properties: s.list_properties,
        dump_structure: s.dump_structure,
        render_debug: s.render_debug.clone(),
    })
}

/// What differs between two plans, so a reload only redoes the affected work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanChange {
    /// Window ↔ desktop or new window geometry: every output is recreated. Per-target
    /// changes are not listed in that case.
    OutputMode,
    TargetAdded(TargetKey),
    TargetRemoved(TargetKey),
    /// New wallpaper, playlist or span members: the target's renderer is rebuilt.
    TargetContent(TargetKey),
    /// Only scaling or clamp changed: recompute UVs and the sampler.
    TargetScaling(TargetKey),
    Layer,
    Fps,
    HwDecode,
    Audio,
    FullscreenPause,
    Input,
    Particles,
    AssetsDir,
    Properties,
    Screenshot,
}

/// Lists the changes from `old` to `new`; empty when the plans are equivalent.
pub fn diff(old: &ExecutionPlan, new: &ExecutionPlan) -> Vec<PlanChange> {
    let mut changes = Vec::new();
    if old.mode != new.mode {
        changes.push(PlanChange::OutputMode);
    } else {
        for target in &old.targets {
            if new.target(&target.key).is_none() {
                changes.push(PlanChange::TargetRemoved(target.key.clone()));
            }
        }
        for target in &new.targets {
            let key = target.key.clone();
            match old.target(&target.key) {
                None => changes.push(PlanChange::TargetAdded(key)),
                Some(before) => {
                    if before.wallpaper != target.wallpaper
                        || before.playlist != target.playlist
                        || before.outputs != target.outputs
                    {
                        changes.push(PlanChange::TargetContent(key));
                    } else if before.scaling != target.scaling || before.clamp != target.clamp {
                        changes.push(PlanChange::TargetScaling(key));
                    }
                }
            }
        }
    }
    let globals = [
        (old.layer != new.layer, PlanChange::Layer),
        (old.fps != new.fps, PlanChange::Fps),
        (old.hwdec != new.hwdec, PlanChange::HwDecode),
        (old.audio != new.audio, PlanChange::Audio),
        (
            old.fullscreen_pause != new.fullscreen_pause,
            PlanChange::FullscreenPause,
        ),
        (old.input != new.input, PlanChange::Input),
        (old.particles != new.particles, PlanChange::Particles),
        (old.assets_dir != new.assets_dir, PlanChange::AssetsDir),
        (old.properties != new.properties, PlanChange::Properties),
        (old.screenshot != new.screenshot, PlanChange::Screenshot),
    ];
    changes.extend(
        globals
            .into_iter()
            .filter(|(changed, _)| *changed)
            .map(|(_, change)| change),
    );
    changes
}

#[cfg(test)]
mod tests;

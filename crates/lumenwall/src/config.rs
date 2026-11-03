//! Optional TOML configuration file (Lumenwall extension, not in the reference).
//!
//! Location: `<config dir>/lumenwall/config.toml` (`~/.config/lumenwall/config.toml` on Linux,
//! `%APPDATA%\Lumenwall\config.toml` on Windows), or `--config FILE`. The file is merged with the
//! command line by [`crate::plan::build`], the command line taking precedence. It is watched
//! and reloaded while running (see [`crate::watch`]).
//!
//! ```toml
//! wallpaper = "2317494988"        # default wallpaper (Workshop ID or path)
//! fps = 30
//! volume = 64                     # 0..=128 like --volume; 64 is the UI's 50 %
//! scaling = "fill"
//! fullscreen-pause-ignore-appids = ["mpv"]
//! gpu-release-delay = 300         # seconds paused before the GPU is released; 0 = never
//!
//! [properties]
//! bloom = true                    # booleans become "1"/"0"
//!
//! [wallpaper-audio."2317494988"]  # per wallpaper: Workshop ID or folder path
//! volume = 40                     # percent of the global volume, 0..=100
//! muted = false
//!
//! [[screen]]
//! name = "HDMI-1"
//! wallpaper = "./my_wallpaper"
//! clamp = "border"
//!
//! [[span]]
//! screens = ["DP-1", "DP-2"]
//! playlist = "evening"
//!
//! [log]
//! level = "info,lumen=debug"
//! file = true
//! ```

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cli::ConfigChoice;
use crate::settings::{
    Clamp, HwDecode, Layer, Scaling, validate_fps, validate_gpu_release_delay,
    validate_screenshot_delay,
};

/// Configuration file contents. Every field is optional; unknown keys are errors.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Default wallpaper: Workshop ID or path, like the positional argument.
    pub wallpaper: Option<PathBuf>,
    /// Playlist of the window (like `--playlist` before any `-r`).
    pub playlist: Option<String>,
    pub fps: Option<i64>,
    pub volume: Option<i64>,
    pub silent: Option<bool>,
    pub automute: Option<bool>,
    pub audio_processing: Option<bool>,
    pub fullscreen_pause: Option<bool>,
    pub fullscreen_pause_only_active: Option<bool>,
    pub fullscreen_pause_ignore_appids: Option<Vec<String>>,
    /// Seconds every output stays paused before the GPU device and its memory are released
    /// (0 = never; default 300).
    pub gpu_release_delay: Option<i64>,
    /// Default scaling of the window and of the outputs without their own value.
    pub scaling: Option<Scaling>,
    pub clamp: Option<Clamp>,
    pub layer: Option<Layer>,
    pub hwdec: Option<HwDecode>,
    pub mouse: Option<bool>,
    pub parallax: Option<bool>,
    pub particles: Option<bool>,
    pub assets_dir: Option<PathBuf>,
    pub screenshot: Option<PathBuf>,
    pub screenshot_delay: Option<i64>,
    /// User property overrides (`--set-property`), merged key by key with the command line.
    pub properties: BTreeMap<String, PropertyValue>,
    /// Sound level per wallpaper, keyed by Workshop ID or folder path
    /// (`lumenwall_ipc::wallpaper_key`), applied on top of `volume`.
    pub wallpaper_audio: BTreeMap<String, WallpaperAudioConfig>,
    /// Explicit window mode (like `--window`).
    pub window: Option<WindowConfig>,
    /// Desktop outputs (like `-r`).
    #[serde(rename = "screen")]
    pub screens: Vec<ScreenConfig>,
    /// Span groups (like `--screen-span`).
    #[serde(rename = "span")]
    pub spans: Vec<SpanConfig>,
    pub log: LogConfig,
}

/// A property value; TOML scalars are converted to the text the command line would pass.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum PropertyValue {
    Bool(bool),
    Integer(i64),
    Float(f64),
    Text(String),
}

impl PropertyValue {
    /// `true`/`false` become `"1"`/`"0"` (`--set-property key` means `"1"`).
    pub fn to_text(&self) -> String {
        match self {
            Self::Bool(b) => if *b { "1" } else { "0" }.to_owned(),
            Self::Integer(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Text(s) => s.clone(),
        }
    }
}

/// `[wallpaper-audio."<id or path>"]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WallpaperAudioConfig {
    /// Percent of the global volume, 0..=100 (clamped); 100 when absent.
    pub volume: Option<i64>,
    pub muted: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowConfig {
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenConfig {
    pub name: String,
    pub wallpaper: Option<PathBuf>,
    pub playlist: Option<String>,
    pub scaling: Option<Scaling>,
    pub clamp: Option<Clamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpanConfig {
    pub screens: Vec<String>,
    pub wallpaper: Option<PathBuf>,
    pub playlist: Option<String>,
    pub scaling: Option<Scaling>,
    pub clamp: Option<Clamp>,
}

impl SpanConfig {
    /// Output names without empty items, as for `--screen-span`.
    pub fn outputs(&self) -> Vec<String> {
        self.screens
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect()
    }

    /// Synthetic key, as `--screen-span` builds it.
    pub fn key(&self) -> String {
        format!("span:{}", self.outputs().join(","))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct LogConfig {
    /// `tracing` filter directives; `LUMENWALL_LOG`/`RUST_LOG` take precedence.
    pub level: Option<String>,
    /// Write a daily-rotated log file (default: on for Windows, off elsewhere).
    pub file: Option<bool>,
    pub directory: Option<PathBuf>,
    /// Number of rotated files kept (default 7).
    pub max_files: Option<usize>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read the configuration file {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid configuration file {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

impl Config {
    /// Parses and validates TOML text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        let empty_path = |p: &Option<PathBuf>| p.as_ref().is_some_and(|p| p.as_os_str().is_empty());
        let empty_text = |s: &Option<String>| s.as_ref().is_some_and(String::is_empty);
        if empty_path(&self.wallpaper) || empty_text(&self.playlist) {
            return Err("'wallpaper' and 'playlist' cannot be empty".into());
        }
        if let Some(fps) = self.fps {
            validate_fps(fps).map_err(|e| format!("fps: {e}"))?;
        }
        if let Some(delay) = self.screenshot_delay {
            validate_screenshot_delay(delay).map_err(|e| format!("screenshot-delay: {e}"))?;
        }
        if let Some(delay) = self.gpu_release_delay {
            validate_gpu_release_delay(delay).map_err(|e| format!("gpu-release-delay: {e}"))?;
        }
        if let Some(window) = &self.window {
            if !self.screens.is_empty() || !self.spans.is_empty() {
                return Err("[window] cannot be combined with [[screen]] or [[span]]".into());
            }
            if window.width <= 0 || window.height <= 0 {
                return Err("[window]: width and height must be positive".into());
            }
        }
        if self.properties.keys().any(String::is_empty) {
            return Err("[properties]: keys cannot be empty".into());
        }
        if self.wallpaper_audio.keys().any(|k| k.trim().is_empty()) {
            return Err("[wallpaper-audio]: keys cannot be empty".into());
        }
        let mut seen: Vec<&str> = Vec::new();
        for screen in &self.screens {
            if screen.name.is_empty() {
                return Err("[[screen]]: 'name' cannot be empty".into());
            }
            if seen.contains(&screen.name.as_str()) {
                return Err(format!(
                    "output '{}' is configured more than once",
                    screen.name
                ));
            }
            if empty_path(&screen.wallpaper) || empty_text(&screen.playlist) {
                return Err(format!(
                    "[[screen]] '{}': empty wallpaper or playlist",
                    screen.name
                ));
            }
            seen.push(&screen.name);
        }
        for span in &self.spans {
            let outputs: Vec<&str> = span
                .screens
                .iter()
                .filter(|s| !s.is_empty())
                .map(String::as_str)
                .collect();
            if outputs.len() < 2 {
                return Err("[[span]]: 'screens' needs at least two output names".into());
            }
            for output in outputs {
                if seen.contains(&output) {
                    return Err(format!("output '{output}' is configured more than once"));
                }
                seen.push(output);
            }
            if empty_path(&span.wallpaper) || empty_text(&span.playlist) {
                return Err("[[span]]: empty wallpaper or playlist".into());
            }
        }
        if let Some(level) = &self.log.level {
            tracing_subscriber::EnvFilter::try_new(level).map_err(|e| format!("log.level: {e}"))?;
        }
        Ok(())
    }

    /// Whether the file chooses the output layout (window or desktop outputs).
    pub fn has_layout(&self) -> bool {
        self.window.is_some() || !self.screens.is_empty() || !self.spans.is_empty()
    }

    /// Whether the file chooses the default content.
    pub fn has_default_content(&self) -> bool {
        self.wallpaper.is_some() || self.playlist.is_some()
    }
}

/// Folder name under the platform configuration directory.
const APP_DIR: &str = if cfg!(any(windows, target_os = "macos")) {
    "Lumenwall"
} else {
    "lumenwall"
};

/// `<config dir>/lumenwall/config.toml`, if the platform has a configuration directory.
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join(APP_DIR).join("config.toml"))
}

/// A configuration file read from disk.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadedConfig {
    /// File to watch for changes (also set when the default file does not exist yet).
    pub path: Option<PathBuf>,
    /// Raw contents, to ignore change notifications that do not change anything.
    pub bytes: Option<Vec<u8>>,
    pub config: Option<Config>,
}

/// Reads the configuration selected by the command line. A missing default file is not an
/// error; a missing `--config` file is.
pub fn load(choice: &ConfigChoice) -> Result<LoadedConfig, ConfigError> {
    let (path, required) = match choice {
        ConfigChoice::Disabled => return Ok(LoadedConfig::default()),
        ConfigChoice::Default => match default_path() {
            Some(path) => (path, false),
            None => return Ok(LoadedConfig::default()),
        },
        ConfigChoice::File(path) => (path.clone(), true),
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let config = parse_bytes(&path, &bytes)?;
            Ok(LoadedConfig {
                path: Some(path),
                bytes: Some(bytes),
                config: Some(config),
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound && !required => Ok(LoadedConfig {
            path: Some(path),
            bytes: None,
            config: None,
        }),
        Err(source) => Err(ConfigError::Io { path, source }),
    }
}

/// Parses file contents, reporting errors against `path`.
pub fn parse_bytes(path: &Path, bytes: &[u8]) -> Result<Config, ConfigError> {
    let invalid = |message: String| ConfigError::Invalid {
        path: path.to_owned(),
        message,
    };
    let text = std::str::from_utf8(bytes).map_err(|e| invalid(e.to_string()))?;
    Config::parse(text).map_err(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn full_file() {
        let config = Config::parse(
            r#"
            wallpaper = "2317494988"
            playlist = "evening"
            fps = 60
            volume = 300
            silent = false
            automute = false
            audio-processing = false
            fullscreen-pause = false
            fullscreen-pause-only-active = true
            fullscreen-pause-ignore-appids = ["mpv", ""]
            scaling = "fill"
            clamp = "border"
            layer = "top"
            hwdec = "nvdec-copy"
            mouse = false
            parallax = false
            particles = false
            assets-dir = "/we/assets"
            screenshot = "/tmp/shot.png"
            screenshot-delay = 9

            [properties]
            bloom = true
            off = false
            count = 3
            ratio = 0.5
            color = "1 0 0"

            [wallpaper-audio."2317494988"]
            volume = 40
            [wallpaper-audio."/home/me/my wallpaper"]
            muted = true

            [[screen]]
            name = "HDMI-1"
            wallpaper = "./w"
            scaling = "fit"

            [[screen]]
            name = "eDP-1"
            playlist = "day"

            [[span]]
            screens = ["DP-1", "", "DP-2"]
            clamp = "repeat"

            [log]
            level = "debug"
            file = true
            directory = "/tmp/logs"
            max-files = 3
            "#,
        )
        .unwrap();
        assert_eq!(config.fps, Some(60));
        assert_eq!(config.scaling, Some(Scaling::Fill));
        assert_eq!(config.hwdec, Some(HwDecode::NvdecCopy));
        assert_eq!(config.properties["bloom"].to_text(), "1");
        assert_eq!(config.properties["off"].to_text(), "0");
        assert_eq!(config.properties["count"].to_text(), "3");
        assert_eq!(config.properties["ratio"].to_text(), "0.5");
        assert_eq!(config.properties["color"].to_text(), "1 0 0");
        assert_eq!(
            config.wallpaper_audio["2317494988"],
            WallpaperAudioConfig {
                volume: Some(40),
                muted: None
            }
        );
        assert_eq!(
            config.wallpaper_audio["/home/me/my wallpaper"].muted,
            Some(true)
        );
        assert_eq!(config.screens.len(), 2);
        assert_eq!(config.screens[0].scaling, Some(Scaling::Fit));
        assert_eq!(config.spans[0].outputs(), ["DP-1", "DP-2"]);
        assert_eq!(config.spans[0].key(), "span:DP-1,DP-2");
        assert_eq!(config.log.max_files, Some(3));
        assert!(config.has_layout());
        assert!(config.has_default_content());
    }

    #[test]
    fn rejects_invalid_files() {
        for (text, needle) in [
            ("fsp = 30", "unknown field"),
            ("fps = 0", "fps"),
            ("fps = \"x\"", "invalid type"),
            ("screenshot-delay = -1", "screenshot-delay"),
            ("gpu-release-delay = -5", "gpu-release-delay"),
            ("gpu-release-delay = 5000000000", "gpu-release-delay"),
            ("scaling = \"Fill\"", "unknown variant"),
            ("wallpaper = \"\"", "cannot be empty"),
            ("[window]\nwidth = 0\nheight = 10", "positive"),
            (
                "[window]\nwidth = 10\nheight = 10\n[[screen]]\nname = \"A\"",
                "cannot be combined",
            ),
            ("[[screen]]\nname = \"\"", "cannot be empty"),
            (
                "[[screen]]\nname = \"A\"\n[[screen]]\nname = \"A\"",
                "more than once",
            ),
            (
                "[[screen]]\nname = \"A\"\n[[span]]\nscreens = [\"A\", \"B\"]",
                "more than once",
            ),
            ("[[span]]\nscreens = [\"A\", \"\"]", "at least two"),
            ("[wallpaper-audio.\" \"]\nvolume = 5", "cannot be empty"),
            ("[wallpaper-audio.\"1\"]\nloud = 5", "unknown field"),
            ("[[span]]\nscreens = [\"A\", \"A\"]", "more than once"),
            (
                "[[span]]\nscreens = [\"A\", \"B\"]\n[[span]]\nscreens = [\"B\", \"C\"]",
                "more than once",
            ),
            ("[properties]\n\"\" = 1", "keys cannot be empty"),
            ("[log]\nlevel = \"lumen=notalevel\"", "log.level"),
            ("[[screen]]\nname = \"A\"\nbg = \"x\"", "unknown field"),
        ] {
            let err = Config::parse(text).unwrap_err();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn loading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        // A missing explicit file is an error.
        let err = load(&ConfigChoice::File(path.clone())).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));

        std::fs::write(&path, "fps = 12").unwrap();
        let loaded = load(&ConfigChoice::File(path.clone())).unwrap();
        assert_eq!(loaded.config.unwrap().fps, Some(12));
        assert_eq!(loaded.path.as_deref(), Some(path.as_path()));
        assert_eq!(loaded.bytes.as_deref(), Some(&b"fps = 12"[..]));

        std::fs::write(&path, "fps = -3").unwrap();
        let err = load(&ConfigChoice::File(path.clone())).unwrap_err();
        assert!(err.to_string().contains("config.toml"), "{err}");

        std::fs::write(&path, b"\xff").unwrap();
        assert!(load(&ConfigChoice::File(path)).is_err());

        assert_eq!(
            load(&ConfigChoice::Disabled).unwrap(),
            LoadedConfig::default()
        );
    }

    #[test]
    fn default_path_is_under_the_config_dir() {
        if let Some(path) = default_path() {
            assert!(path.ends_with(Path::new(APP_DIR).join("config.toml")));
        }
    }
}

//! Preferences that belong to the app, not the daemon (language, library folders,
//! preview animation, sort). Stored as JSON in the user config directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LangPref {
    #[default]
    System,
    En,
    Fr,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMode {
    /// Animate every visible card.
    Visible,
    /// Animate only the card under the pointer (and the details panel).
    #[default]
    Hover,
    /// Still previews only.
    Never,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePref {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    Name,
    Recent,
    Kind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub language: LangPref,
    pub preview_mode: PreviewMode,
    pub theme: ThemePref,
    pub sort: SortKey,
    /// Extra folders scanned for wallpapers, besides the Workshop.
    pub folders: Vec<PathBuf>,
    pub scaling: lumenwall_ipc::Scaling,
    /// Window size in logical pixels, restored at start.
    pub window: Option<[f32; 2]>,
    /// Screens last chosen for "Apply" (empty: all).
    pub targets: Vec<String>,
    /// Library filters, restored at start.
    pub filter_kind: Option<crate::catalog::Kind>,
    pub filter_tag: Option<String>,
    pub filter_res: Option<crate::catalog::ResClass>,
    /// Workshop results beyond "Everyone" content (explicit opt-in).
    pub workshop_questionable: bool,
    pub workshop_mature: bool,
    /// End of the previous session (Unix seconds), for "New" badges.
    pub last_session: Option<u64>,
    /// Automatic update checks and the pre-release channel.
    pub updates: crate::updater::Prefs,
    /// Answers to the installation dialog.
    pub setup: crate::setup::Prefs,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            language: LangPref::System,
            preview_mode: PreviewMode::Hover,
            theme: ThemePref::System,
            sort: SortKey::Name,
            folders: Vec::new(),
            scaling: lumenwall_ipc::Scaling::Fill,
            window: None,
            targets: Vec::new(),
            filter_kind: None,
            filter_tag: None,
            filter_res: None,
            workshop_questionable: false,
            workshop_mature: false,
            last_session: None,
            updates: crate::updater::Prefs::default(),
            setup: crate::setup::Prefs::default(),
        }
    }
}

pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("lumenwall"))
}

pub fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("lumenwall"))
}

impl UiConfig {
    fn path() -> Option<PathBuf> {
        config_dir().map(|d| d.join("ui.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("{}: {e}; using defaults", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            tracing::warn!("cannot save {}: {e}", path.display());
        }
    }
}

/// Start with the session: a desktop entry on Linux, a `Run` registry value on Windows
/// (the same ones the installation dialog and the Windows installer write).
/// It launches `lumenwall-ui --background`, which starts the daemon and the tray.
pub mod autostart {
    #[cfg(not(windows))]
    fn entry() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("autostart/lumenwall.desktop"))
    }

    #[cfg(not(windows))]
    pub fn is_enabled() -> bool {
        entry().is_some_and(|p| {
            std::fs::read_to_string(p).is_ok_and(|t| !t.lines().any(|l| l.trim() == "Hidden=true"))
        })
    }

    #[cfg(not(windows))]
    pub fn set(enabled: bool) -> std::io::Result<()> {
        let path = entry().ok_or(std::io::ErrorKind::NotFound)?;
        if !enabled {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let exe = std::env::current_exe()?;
        crate::setup::linux::write_autostart(&path, &exe).map_err(std::io::Error::other)
    }

    #[cfg(windows)]
    use crate::setup::windows::{RUN_KEY, RUN_VALUE};

    #[cfg(windows)]
    pub fn is_enabled() -> bool {
        crate::platform::registry_string(true, RUN_KEY, RUN_VALUE).is_some()
    }

    #[cfg(windows)]
    pub fn set(enabled: bool) -> std::io::Result<()> {
        if enabled {
            let exe = std::env::current_exe()?;
            let cmd = crate::setup::windows::run_command(&exe);
            crate::platform::registry_set_run(RUN_KEY, RUN_VALUE, Some(&cmd))
        } else {
            crate::platform::registry_set_run(RUN_KEY, RUN_VALUE, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_and_tolerates_old_files() {
        let cfg = UiConfig {
            language: LangPref::Fr,
            folders: vec!["/x".into()],
            ..Default::default()
        };
        let back: UiConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back, cfg);
        let old: UiConfig = serde_json::from_str(r#"{"language":"en","unknown":1}"#).unwrap();
        assert_eq!(old.language, LangPref::En);
        assert_eq!(old.preview_mode, PreviewMode::Hover);
    }
}

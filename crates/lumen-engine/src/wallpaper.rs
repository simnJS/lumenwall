//! A loaded wallpaper: its VFS and its project (`project.json`, scene model, properties).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lumen_assets::Vfs;
use lumen_scene::{LoadContext, Project, ProjectKind, PropertyOverrides};
use serde_json::Value as Json;

use crate::assets::{SceneSource, open_vfs};

/// A wallpaper read from disk, before any GPU work.
pub struct Wallpaper {
    pub path: PathBuf,
    pub vfs: Arc<Vfs>,
    pub project: Project,
}

impl Wallpaper {
    /// Mounts the VFS and reads `project.json` and the scene, with the property overrides
    /// applied before the scene is read (spec 02 Q18).
    pub fn load(
        path: &Path,
        assets: &Path,
        overrides: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let vfs = Arc::new(open_vfs(path, assets).map_err(|e| format!("{}: {e}", path.display()))?);
        let mut o = PropertyOverrides::new();
        for (k, v) in overrides {
            o.set(k.clone(), v.clone());
        }
        let project = LoadContext::new(o)
            .load(&SceneSource(&vfs))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            vfs,
            project,
        })
    }

    pub fn kind(&self) -> ProjectKind {
        self.project.kind
    }

    /// Fails for wallpaper types this build cannot render.
    pub fn check_supported(&self) -> Result<(), String> {
        match self.project.kind {
            ProjectKind::Scene => Ok(()),
            ProjectKind::Video => Err(format!(
                "'{}' is a video wallpaper: video wallpapers are not integrated yet (lumen-video)",
                self.project.title
            )),
            ProjectKind::Web => Err(format!(
                "'{}' is a web wallpaper: web wallpapers are not integrated yet (lumen-web)",
                self.project.title
            )),
        }
    }

    /// The objects of `scene.json` as written (sound and particle objects read extra keys).
    pub fn raw_objects(&self) -> Vec<Json> {
        let Ok(bytes) = self.vfs.read(&self.project.file) else {
            return Vec::new();
        };
        serde_json::from_slice::<Json>(crate::particles::strip_bom(&bytes))
            .ok()
            .and_then(|j| j.get("objects").and_then(Json::as_array).cloned())
            .unwrap_or_default()
    }

    /// Applies new property overrides; returns the names that changed.
    pub fn set_properties(&mut self, overrides: &BTreeMap<String, String>) -> Vec<String> {
        let mut changed = Vec::new();
        for (name, text) in overrides {
            let Some(id) = self.project.property(name) else {
                continue;
            };
            let before = self.project.properties.get(id).value().clone();
            match self.project.set_property_text(name, text) {
                Ok(_) => {
                    if *self.project.properties.get(id).value() != before {
                        changed.push(name.clone());
                    }
                }
                Err(e) => tracing::warn!("property {name}: {e}"),
            }
        }
        changed
    }
}

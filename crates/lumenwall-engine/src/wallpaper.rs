//! A loaded wallpaper: its VFS and its project (`project.json`, scene model, properties).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lumenwall_assets::Vfs;
use lumenwall_scene::{LoadContext, Project, ProjectKind, PropertyKind, PropertyOverrides};
use serde_json::Value as Json;

use crate::assets::{SceneSource, open_vfs};
use crate::monitor::PropertyValue;

/// A wallpaper read from disk, before any GPU work.
pub struct Wallpaper {
    pub path: PathBuf,
    pub vfs: Arc<Vfs>,
    pub project: Project,
    /// `project.json` value of each property, as override text, to go back to it when an
    /// override is removed.
    defaults: BTreeMap<String, String>,
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
        let defaults = vfs
            .read("project.json")
            .ok()
            .map(|bytes| property_defaults(&bytes))
            .unwrap_or_default();
        Ok(Self {
            path: path.to_path_buf(),
            vfs,
            project,
            defaults,
        })
    }

    pub fn kind(&self) -> ProjectKind {
        self.project.kind
    }

    /// Fails for wallpaper types this build cannot render.
    pub fn check_supported(&self) -> Result<(), String> {
        match self.project.kind {
            ProjectKind::Scene => Ok(()),
            ProjectKind::Video if lumenwall_video::is_supported() => self.video_path().map(|_| ()),
            ProjectKind::Video => Err(format!(
                "'{}' is a video wallpaper: this build has no video support (lumenwall-video without FFmpeg)",
                self.project.title
            )),
            ProjectKind::Web => match crate::web::unsupported_reason() {
                None => Ok(()),
                Some(reason) => Err(format!(
                    "'{}' is a web wallpaper: {reason}",
                    self.project.title
                )),
            },
        }
    }

    /// Video file of a `video` wallpaper on disk (spec 07 §14.1: `file` resolved through the
    /// wallpaper's file space).
    pub fn video_path(&self) -> Result<PathBuf, String> {
        self.vfs.physical_path(&self.project.file).map_err(|e| {
            format!(
                "'{}': video file {}: {e}",
                self.project.title, self.project.file
            )
        })
    }

    /// The objects of `scene.json` as written (sound and particle objects read extra keys).
    pub fn raw_objects(&self) -> Vec<Json> {
        if self.project.kind != ProjectKind::Scene {
            return Vec::new();
        }
        let Ok(bytes) = self.vfs.read(&self.project.file) else {
            return Vec::new();
        };
        serde_json::from_slice::<Json>(crate::particles::strip_bom(&bytes))
            .ok()
            .and_then(|j| j.get("objects").and_then(Json::as_array).cloned())
            .unwrap_or_default()
    }

    /// Applies the complete set of property overrides: listed properties take their override,
    /// the others their `project.json` value. Returns the names that changed.
    pub fn set_properties(&mut self, overrides: &BTreeMap<String, String>) -> Vec<String> {
        let mut changed = Vec::new();
        let names: Vec<String> = self
            .project
            .properties
            .iter()
            .map(|(_, p)| p.name().to_owned())
            .collect();
        for name in names {
            let Some(text) = overrides.get(&name).or_else(|| self.defaults.get(&name)) else {
                continue;
            };
            let Some(id) = self.project.property(&name) else {
                continue;
            };
            let before = self.project.properties.get(id).value().clone();
            match self.project.set_property_text(&name, text) {
                Ok(_) => {
                    if *self.project.properties.get(id).value() != before {
                        changed.push(name);
                    }
                }
                Err(e) => tracing::warn!("property {name}: {e}"),
            }
        }
        changed
    }

    /// Current user property values (decorative `text` labels left out).
    pub fn property_values(&self) -> Vec<(String, PropertyValue)> {
        self.project
            .properties
            .iter()
            .filter_map(|(_, p)| {
                let v = p.value();
                let value = match p.kind() {
                    PropertyKind::Text => return None,
                    PropertyKind::Bool => PropertyValue::Bool(v.boolean()),
                    PropertyKind::Slider { .. } => PropertyValue::Number(f64::from(v.float())),
                    PropertyKind::Color => {
                        let c = v.vec4();
                        PropertyValue::Color([c.x, c.y, c.z])
                    }
                    // Combo values are stored as text; numeric options travel as numbers, like
                    // in project.json.
                    PropertyKind::Combo { .. } => match v.text().parse::<f64>() {
                        Ok(n) if n.is_finite() => PropertyValue::Number(n),
                        _ => PropertyValue::Text(v.text().to_owned()),
                    },
                    _ => PropertyValue::Text(v.text().to_owned()),
                };
                Some((p.name().to_owned(), value))
            })
            .collect()
    }
}

/// `general.properties.<name>.value` of `project.json`, as override text.
fn property_defaults(project_json: &[u8]) -> BTreeMap<String, String> {
    let Ok(json) = serde_json::from_slice::<Json>(crate::particles::strip_bom(project_json)) else {
        return BTreeMap::new();
    };
    let Some(props) = json
        .get("general")
        .and_then(|g| g.get("properties"))
        .and_then(Json::as_object)
    else {
        return BTreeMap::new();
    };
    props
        .iter()
        .filter_map(|(name, def)| {
            let text = match def.get("value")? {
                Json::Bool(b) => if *b { "1" } else { "0" }.to_owned(),
                Json::Number(n) => n.to_string(),
                Json::String(s) => s.clone(),
                _ => return None,
            };
            Some((name.clone(), text))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_as_override_text() {
        let d = property_defaults(
            br#"{"general":{"properties":{
                "rain":{"type":"bool","value":true},
                "speed":{"type":"slider","value":0.5},
                "mode":{"type":"combo","value":2},
                "tint":{"type":"color","value":"0.5 0.25 1"},
                "label":{"type":"text","text":"x"}
            }}}"#,
        );
        assert_eq!(d["rain"], "1");
        assert_eq!(d["speed"], "0.5");
        assert_eq!(d["mode"], "2");
        assert_eq!(d["tint"], "0.5 0.25 1");
        assert!(!d.contains_key("label"));
        assert!(property_defaults(b"not json").is_empty());
    }
}

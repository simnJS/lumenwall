//! `project.json` (spec 02 §2) and the loading sequence (§1.1).

use serde::Deserialize as _;

use crate::asset::AssetSource;
use crate::convert::{opt_bool, req_str};
use crate::error::{At, DataErr, DataError, Fail, LoadError, NotAWallpaper, Warning, Warnings};
use crate::intern::Interner;
use crate::load::{Arenas, Loader};
use crate::property::{
    PropId, Properties, PropertyOverrides, PropertyUpdateError, Subscriber, parse_properties,
};
use crate::raw::{Json, JsonKind, RawProject, RawSetting, parse_doc};
use crate::scene::Scene;
use crate::setting::{Binder, SettingId, Settings};
use crate::value::Value;

/// Path of the project file at the VFS root.
pub const PROJECT_FILE: &str = "project.json";

/// `type` of a project (compared case-insensitively).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectKind {
    /// `scene`: `file` is a `scene.json`.
    Scene,
    /// `video`: `file` is a video path.
    Video,
    /// `web`: `file` is an HTML page.
    Web,
}

/// A loaded wallpaper project.
#[derive(Debug)]
pub struct Project {
    /// Project type.
    pub kind: ProjectKind,
    /// `title`.
    pub title: String,
    /// `file`: main file.
    pub file: String,
    /// `workshopid`, or a synthetic negative id (§2.2).
    pub workshop_id: String,
    /// `general.supportsaudioprocessing`.
    pub supports_audio_processing: bool,
    /// User properties.
    pub properties: Properties,
    /// Every setting of the scene.
    pub settings: Settings,
    /// Interned names and paths.
    pub strings: Interner,
    /// The scene, for [`ProjectKind::Scene`].
    pub scene: Option<Scene>,
    pub(crate) warnings: Warnings,
}

/// State shared by the projects loaded by one process: the synthetic id counter and the
/// command-line property overrides, re-applied to each project (§3.5).
#[derive(Clone, Debug, Default)]
pub struct LoadContext {
    last_id: i32,
    /// `--set-property` overrides.
    pub overrides: PropertyOverrides,
}

impl LoadContext {
    /// A context with these overrides.
    pub fn new(overrides: PropertyOverrides) -> Self {
        Self {
            last_id: 0,
            overrides,
        }
    }

    /// Loads `project.json` and its main file from `src` (§1.1 steps 2–5).
    ///
    /// Overrides are applied right after the properties are parsed, before the scene is read,
    /// so every setting, snapshots included, sees the overridden values (Q18).
    pub fn load(&mut self, src: &dyn AssetSource) -> Result<Project, LoadError> {
        let bytes = src.read(PROJECT_FILE).map_err(|source| LoadError::Asset {
            path: PROJECT_FILE.into(),
            source,
        })?;
        let mut scratch = None;
        let raw: RawProject<'_> = parse_doc(PROJECT_FILE, &bytes, &mut scratch)?;
        // The counter moves for every project, with or without `workshopid` (§2.2).
        self.last_id = self.last_id.wrapping_sub(1);
        let kind = project_kind(&raw)?;
        let mut warnings = Warnings::default();
        let mut project = project_doc(&raw, kind, self.last_id, &mut warnings)
            .map_err(|e| e.into_load(PROJECT_FILE))?;
        self.overrides
            .apply(&mut project.properties, &mut warnings, PROJECT_FILE);
        project.warnings = warnings;
        if project.kind == ProjectKind::Scene {
            let file = project.file.clone();
            let mut arenas = Arenas::default();
            let mut loader = Loader {
                src,
                b: Binder {
                    settings: &mut project.settings,
                    props: &mut project.properties,
                    strings: &mut project.strings,
                },
                warnings: &mut project.warnings,
                arenas: &mut arenas,
                model_states: false,
                files: std::collections::HashMap::new(),
            };
            project.scene = Some(loader.load_scene(&file)?);
        }
        Ok(project)
    }
}

/// Loads a project with a fresh [`LoadContext`].
pub fn load_project(
    src: &dyn AssetSource,
    overrides: PropertyOverrides,
) -> Result<Project, LoadError> {
    LoadContext::new(overrides).load(src)
}

/// Extensions of the video files Wallpaper Engine plays as video wallpapers.
const VIDEO_EXTENSIONS: [&str; 8] = ["mp4", "webm", "m4v", "mov", "avi", "mkv", "wmv", "ogv"];

/// Project type (spec 02 §2.1), with what the spec leaves out (Lumenwall, parity rule 4):
///
/// - Workshop assets (`"category": "Asset"`, or `file` = `assets.json` without `type`) and
///   editor templates (`templateoptions` without `type`) are Wallpaper Engine content but not
///   wallpapers:
///   [`LoadError::NotAWallpaper`] instead of a parsing error;
/// - without `type`, the type follows the extension of `file`, as Wallpaper Engine does for
///   its own projects (`.json` scene, video files, `.html` web, `.exe` application);
/// - `application` (a Windows program) is [`LoadError::Unsupported`].
fn project_kind(raw: &RawProject<'_>) -> Result<ProjectKind, LoadError> {
    if raw.kind != JsonKind::Object {
        return Err(
            crate::convert::kind_error(raw.kind, crate::error::Expected::Object)
                .into_load(PROJECT_FILE),
        );
    }
    let file = match &raw.file {
        Json::Str(s) => Some(&**s),
        _ => None,
    };
    let file_name = file.map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f));
    let asset = matches!(&raw.category, Json::Str(c) if c.eq_ignore_ascii_case("asset"))
        || (matches!(raw.ty, Json::Absent | Json::Null)
            && file_name.is_some_and(|f| f.eq_ignore_ascii_case("assets.json")));
    if asset {
        return Err(LoadError::NotAWallpaper(NotAWallpaper::WorkshopAsset));
    }
    // A wallpaper made from a template has its own `type`; the template itself has none.
    if raw.templateoptions != Json::Absent && matches!(raw.ty, Json::Absent | Json::Null) {
        return Err(LoadError::NotAWallpaper(NotAWallpaper::Template));
    }
    let application = || LoadError::Unsupported {
        kind: "application",
        file: file.unwrap_or_default().to_owned(),
    };
    let ty = match &raw.ty {
        Json::Absent | Json::Null => None,
        other => Some(
            req_str(other)
                .at("type")
                .map_err(|e| e.into_load(PROJECT_FILE))?,
        ),
    };
    if let Some(ty) = ty {
        return if ty.eq_ignore_ascii_case("scene") {
            Ok(ProjectKind::Scene)
        } else if ty.eq_ignore_ascii_case("video") {
            Ok(ProjectKind::Video)
        } else if ty.eq_ignore_ascii_case("web") {
            Ok(ProjectKind::Web)
        } else if ty.eq_ignore_ascii_case("application") {
            Err(application())
        } else {
            Err(key_error(DataError::ProjectType(ty.to_owned()), "type"))
        };
    }
    let file = req_str(&raw.file)
        .at("file")
        .map_err(|e| e.into_load(PROJECT_FILE))?;
    let ext = file_name
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "json" => Ok(ProjectKind::Scene),
        "html" | "htm" => Ok(ProjectKind::Web),
        "exe" => Err(application()),
        e if VIDEO_EXTENSIONS.contains(&e) => Ok(ProjectKind::Video),
        _ => Err(key_error(DataError::ProjectFile(file.to_owned()), "file")),
    }
}

/// `error` at `$.<key>` of `project.json`.
fn key_error(error: DataError, key: &'static str) -> LoadError {
    DataErr::new(error).at(key).into_load(PROJECT_FILE)
}

fn project_doc(
    raw: &RawProject<'_>,
    kind: ProjectKind,
    synthetic_id: i32,
    warnings: &mut Warnings,
) -> Result<Project, DataErr> {
    let title = req_str(&raw.title).at("title")?.to_owned();
    let file = req_str(&raw.file).at("file")?.to_owned();
    let workshop_id = match &raw.workshopid {
        Json::Absent | Json::Null => synthetic_id.to_string(),
        Json::Str(s) => s.to_string(),
        // Lumenwall keeps the full 64-bit id (Q7).
        Json::UInt(u) => u.to_string(),
        Json::Int(i) => i.to_string(),
        Json::Float(f) => (*f as i64).to_string(),
        other => {
            warnings.push(
                PROJECT_FILE,
                format!("workshopid ignored, found {}", other.kind()),
            );
            synthetic_id.to_string()
        }
    };
    let general = &raw.general;
    let (supports_audio_processing, properties) = match general.kind {
        JsonKind::Absent | JsonKind::Null => (false, Properties::default()),
        JsonKind::Object => {
            let audio = opt_bool(&general.supportsaudioprocessing, false)
                .at("supportsaudioprocessing")
                .at("general")?;
            let props = match general.properties.kind {
                JsonKind::Absent | JsonKind::Null => Properties::default(),
                JsonKind::Object => parse_properties(&general.properties, warnings, PROJECT_FILE)
                    .at("properties")
                    .at("general")?,
                found => {
                    return Err(DataErr::new(DataError::Type {
                        expected: crate::error::Expected::Object,
                        found,
                    }))
                    .at("properties")
                    .at("general");
                }
            };
            (audio, props)
        }
        found => {
            return Err(DataErr::new(DataError::Type {
                expected: crate::error::Expected::Object,
                found,
            }))
            .at("general");
        }
    };
    Ok(Project {
        kind,
        title,
        file,
        workshop_id,
        supports_audio_processing,
        properties,
        settings: Settings::default(),
        strings: Interner::new(),
        scene: None,
        warnings: Warnings::default(),
    })
}

impl Project {
    /// Warnings collected while loading and building.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings.0
    }

    /// Current value of a setting.
    #[inline]
    pub fn value(&self, id: SettingId) -> &Value {
        self.settings.value(id)
    }

    /// Property by exact name.
    pub fn property(&self, name: &str) -> Option<PropId> {
        self.properties.find(name)
    }

    /// Updates a property from a text (§3.3, source "user") and propagates it to the bound
    /// settings. Returns the updated settings.
    pub fn set_property_text(
        &mut self,
        name: &str,
        text: &str,
    ) -> Result<&[Subscriber], PropertyUpdateError> {
        let id = self
            .properties
            .find(name)
            .ok_or_else(|| PropertyUpdateError::Unknown(name.to_owned()))?;
        let v = self.properties.value_from_text(id, text)?;
        Ok(self
            .properties
            .set_value(id, v, &mut self.settings, &self.strings))
    }

    /// Replaces a property value and propagates it. Returns the updated settings.
    pub fn set_property_value(&mut self, id: PropId, value: Value) -> &[Subscriber] {
        self.properties
            .set_value(id, value, &mut self.settings, &self.strings)
    }

    /// Applies command-line overrides to an already loaded project and propagates them.
    pub fn apply_overrides(&mut self, overrides: &PropertyOverrides) {
        let ids: Vec<PropId> = self.properties.iter().map(|(id, _)| id).collect();
        for id in ids {
            let name = self.properties.get(id).name();
            let Some(text) = overrides.get(name) else {
                continue;
            };
            tracing::info!("applying override to property {name}");
            match self.properties.value_from_text(id, text) {
                Ok(v) => {
                    self.properties
                        .set_value(id, v, &mut self.settings, &self.strings);
                }
                Err(e) => self
                    .warnings
                    .push(PROJECT_FILE, format!("property override ignored: {e}")),
            }
        }
    }

    /// The `--list-properties` output (§15.1).
    pub fn list_properties(&self) -> String {
        self.properties.listing()
    }

    /// Registers a setting parsed by another crate (e.g. particle initializers), binding it to
    /// the project's properties (§5.2). `default` selects mode L (`Some`) or L! (`None`); `file`
    /// only names the document in errors.
    pub fn add_setting(
        &mut self,
        file: &str,
        raw: &RawSetting<'_>,
        default: Option<Value>,
        color: bool,
    ) -> Result<SettingId, LoadError> {
        let mut b = Binder {
            settings: &mut self.settings,
            props: &mut self.properties,
            strings: &mut self.strings,
        };
        b.setting(raw, default, color)
            .map_err(|e| Fail::Data(e).into_load(file))
    }

    /// [`Project::add_setting`] from a `serde_json::Value` (e.g. keys of `instanceoverride`).
    pub fn add_setting_json(
        &mut self,
        file: &str,
        json: &serde_json::Value,
        default: Option<Value>,
        color: bool,
    ) -> Result<SettingId, LoadError> {
        let raw = RawSetting::deserialize(json).map_err(|e| LoadError::syntax(file, &e))?;
        self.add_setting(file, &raw, default, color)
    }
}

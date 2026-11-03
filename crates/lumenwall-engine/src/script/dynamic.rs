//! Layers created and destroyed by scripts: Wallpaper Engine `IScene.createLayer`,
//! `destroyLayer`, `sortLayer`, `getInitialLayerConfig` and `IEngine.registerAsset` (public
//! SceneScript reference), on the scene-model side.
//!
//! A created layer is a `lumenwall-scene` object added after build
//! ([`lumenwall_scene::Project::add_object`]): it has the settings, materials, effects and
//! user-property bindings of an object read from `scene.json`, so every member scripts read or
//! write works as for the file's layers. Objects are pooled per configuration: `destroyLayer`
//! parks the object and the next `createLayer` of the same asset takes it back with its initial
//! setting values restored, so neither the scene model nor the GPU resources grow with the
//! number of layers a wallpaper creates over time. `registerAsset` prepares one object at once
//! (its GPU resources are created with the scene), so the first `createLayer` in the frame
//! loop creates nothing.
//!
//! The GPU side and the draw order follow in the scene update, after the scripts ran
//! ([`Structure`]).

use std::collections::HashMap;
use std::sync::Arc;

use lumenwall_assets::Vfs;
use lumenwall_scene::{Alignment, BuildOptions, ObjectIndex, ObjectKind, Value as SValue};
use lumenwall_script::{LayerId, ObjectRef, Value};
use serde_json::{Map, Value as Json};

use super::{HostView, new_layer};
use crate::assets::SceneSource;

/// File name given to created objects in warnings.
const CREATED_FILE: &str = "<createLayer>";

/// What scripts changed in the layer list since the scene update last looked.
#[derive(Debug, Default)]
pub(crate) struct Structure {
    /// Objects added to the scene model, without GPU resources yet (pooled or live).
    pub created: Vec<ObjectIndex>,
    /// Pooled objects taken back by `createLayer`: their runtime state starts again.
    pub reused: Vec<ObjectIndex>,
    /// Objects whose layer was destroyed.
    pub removed: Vec<ObjectIndex>,
    /// Ids of the destroyed layers (their scripts get `destroy()`).
    pub removed_ids: Vec<LayerId>,
    /// The draw order changed (layers created, destroyed or sorted).
    pub reordered: bool,
}

impl Structure {
    fn is_empty(&self) -> bool {
        self.created.is_empty()
            && self.reused.is_empty()
            && self.removed.is_empty()
            && self.removed_ids.is_empty()
            && !self.reordered
    }
}

/// One layer configuration (registered asset, path or configuration object) and its pool.
struct Template {
    /// The `scene.json` object configuration, without `id`.
    config: Json,
    /// Name of created layers without a `name` (the asset path).
    label: String,
    /// Setting values and anchor of a new object (from the first one created).
    initial: Option<(Box<[SValue]>, Alignment)>,
    /// Parked objects, ready for the next `createLayer`.
    free: Vec<ObjectIndex>,
    /// The configuration failed once: no further attempt.
    broken: bool,
}

/// A created object.
struct Created {
    template: usize,
    /// First of its contiguous settings.
    settings: usize,
    /// Its entry in the host's layer list (kept while parked, reused with it).
    slot: Option<usize>,
}

/// Layers created by scripts: assets, templates, pools.
pub(crate) struct Dynamic {
    vfs: Arc<Vfs>,
    /// `__workshopId` exports of the scene's scripts: Workshop scripts name assets relative to
    /// their own folder (`models/bar.json` for `models/workshop/<id>/bar.json`; Wallpaper
    /// Engine's editor warns that removing the line breaks "asset references").
    workshop_ids: Vec<String>,
    /// `scene.json` objects by id, as written (`getInitialLayerConfig`).
    raw: HashMap<i64, Json>,
    build: BuildOptions,
    /// Asset handle → template.
    assets: Vec<usize>,
    templates: Vec<Template>,
    by_key: HashMap<String, usize>,
    created: HashMap<ObjectIndex, Created>,
    next_id: i32,
    pending: Structure,
    /// Objects added to the scene model, and the time it took (statistics).
    pub(crate) added: u32,
    pub(crate) add_time: std::time::Duration,
    /// `createLayer` calls answered from a pool.
    pub(crate) reused: u32,
}

impl Dynamic {
    pub(crate) fn new(
        vfs: Arc<Vfs>,
        workshop_ids: Vec<String>,
        raw: HashMap<i64, Json>,
        build: BuildOptions,
    ) -> Self {
        Dynamic {
            vfs,
            workshop_ids,
            raw,
            build,
            assets: Vec::new(),
            templates: Vec::new(),
            by_key: HashMap::new(),
            created: HashMap::new(),
            next_id: 1,
            pending: Structure::default(),
            added: 0,
            add_time: std::time::Duration::ZERO,
            reused: 0,
        }
    }

    /// Created layers get ids above every id of the file.
    pub(super) fn first_id(&mut self, max_file_id: i32) {
        self.next_id = self.next_id.max(max_file_id.saturating_add(1));
    }

    pub(super) fn take_structure(&mut self) -> Option<Structure> {
        (!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending))
    }

    pub(super) fn take_removed_ids(&mut self) -> Vec<LayerId> {
        std::mem::take(&mut self.pending.removed_ids)
    }

    pub(super) fn slot_of(&self, idx: ObjectIndex) -> Option<usize> {
        self.created.get(&idx).and_then(|c| c.slot)
    }

    /// Configuration a created object was made from (`getInitialLayerConfig`, particle
    /// respawn).
    pub(crate) fn config_of(&self, idx: ObjectIndex) -> Option<&Json> {
        self.created
            .get(&idx)
            .map(|c| &self.templates[c.template].config)
    }

    fn alloc_id(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    /// An asset path as written by a script, resolved in the VFS (exact path first, then under
    /// `<first folder>/workshop/<id>/` for each Workshop id of the scene's scripts).
    fn resolve(&self, path: &str) -> String {
        let p = path.trim().replace('\\', "/");
        let p = p.trim_start_matches('/');
        if self.vfs.exists(p) {
            return p.to_owned();
        }
        if let Some((head, rest)) = p.split_once('/') {
            for id in &self.workshop_ids {
                let c = format!("{head}/workshop/{id}/{rest}");
                if self.vfs.exists(&c) {
                    return c;
                }
            }
        }
        p.to_owned()
    }

    fn template(&mut self, config: Json, label: String) -> usize {
        let key = config.to_string();
        if let Some(&t) = self.by_key.get(&key) {
            return t;
        }
        self.templates.push(Template {
            config,
            label,
            initial: None,
            free: Vec::new(),
            broken: false,
        });
        let t = self.templates.len() - 1;
        self.by_key.insert(key, t);
        t
    }

    /// Template of an asset path (`registerAsset`, `createLayer('models/x.json')`): image
    /// models, particle systems and sounds (WE `registerAsset` documentation).
    fn template_for_path(&mut self, path: &str) -> Option<usize> {
        let path = self.resolve(path);
        let lower = path.to_ascii_lowercase();
        let config = if lower.starts_with("particles/") && lower.ends_with(".json") {
            serde_json::json!({ "particle": path })
        } else if lower.ends_with(".json") {
            serde_json::json!({ "image": path })
        } else if [".mp3", ".wav", ".ogg", ".flac"]
            .iter()
            .any(|e| lower.ends_with(e))
        {
            serde_json::json!({ "sound": [path], "playbackmode": "single", "startsilent": true })
        } else {
            tracing::warn!(
                "createLayer: '{path}' is not a layer asset (image model, particle system or sound)"
            );
            return None;
        };
        Some(self.template(config, path))
    }

    /// Template of a configuration object (`createLayer({ image: …, origin: … })`, or the
    /// result of `getInitialLayerConfig`).
    fn template_for_config(&mut self, fields: &[(String, Value)]) -> Option<usize> {
        let mut map = Map::new();
        for (k, v) in fields {
            map.insert(k.clone(), to_json(v));
        }
        map.remove("id");
        for key in ["image", "particle", "font"] {
            if let Some(Json::String(p)) = map.get(key) {
                let resolved = self.resolve(p);
                map.insert(key.to_owned(), Json::String(resolved));
            }
        }
        let label = ["image", "particle", "text"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Json::as_str))
            .unwrap_or("layer")
            .to_owned();
        Some(self.template(Json::Object(map), label))
    }

    /// The JSON of a new object of template `t` with id `id`.
    fn object_json(&self, t: usize, id: i32) -> Vec<u8> {
        let tpl = &self.templates[t];
        let mut config = match &tpl.config {
            Json::Object(m) => m.clone(),
            _ => Map::new(),
        };
        config.insert("id".into(), Json::from(id));
        config
            .entry("name")
            .or_insert_with(|| Json::String(tpl.label.clone()));
        Json::Object(config).to_string().into_bytes()
    }
}

/// Script value → JSON of a `scene.json` object (vectors as `"x y z"`).
fn to_json(v: &Value) -> Json {
    let vector = |c: &[f32]| {
        Json::String(
            c.iter()
                .map(|x| format!("{x}"))
                .collect::<Vec<_>>()
                .join(" "),
        )
    };
    match v {
        Value::Undefined | Value::Null | Value::Object(_) | Value::Callback(_) => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::from(*i),
        Value::Float(f) => Json::from(f64::from(*f)),
        Value::String(s) => Json::String(s.clone()),
        Value::Vec2(c) => vector(c),
        Value::Vec3(c) => vector(c),
        Value::Vec4(c) => vector(c),
        Value::Mat3(m) => m.iter().map(|x| Json::from(f64::from(*x))).collect(),
        Value::Mat4(m) => m.iter().map(|x| Json::from(f64::from(*x))).collect(),
        Value::Array(items) => items.iter().map(to_json).collect(),
        Value::Record(fields) => Json::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), to_json(v)))
                .collect(),
        ),
    }
}

/// JSON → script value (`getInitialLayerConfig`).
fn from_json(j: &Json) -> Value {
    match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match n.as_i64().and_then(|i| i32::try_from(i).ok()) {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(0.0) as f32),
        },
        Json::String(s) => Value::String(s.clone()),
        Json::Array(a) => Value::Array(a.iter().map(from_json).collect()),
        Json::Object(m) => {
            Value::Record(m.iter().map(|(k, v)| (k.clone(), from_json(v))).collect())
        }
    }
}

impl HostView<'_> {
    /// `engine.registerAsset(file)`: an asset handle, with one object prepared at once.
    pub(super) fn register(&mut self, path: &str) -> Option<u32> {
        let d = self.data.dynamic.as_mut()?;
        let t = d.template_for_path(path)?;
        let handle = match d.assets.iter().position(|&a| a == t) {
            Some(h) => h,
            None => {
                d.assets.push(t);
                d.assets.len() - 1
            }
        };
        let tpl = &d.templates[t];
        if tpl.initial.is_none() && !tpl.broken {
            // Precache: created with the scene (or at the next update), parked until used.
            if let Some(idx) = self.new_object(t)
                && let Some(d) = self.data.dynamic.as_mut()
            {
                d.templates[t].free.push(idx);
            }
        }
        u32::try_from(handle).ok()
    }

    /// Adds an object of template `t` to the scene model (not listed yet).
    fn new_object(&mut self, t: usize) -> Option<ObjectIndex> {
        let d = self.data.dynamic.as_mut()?;
        if d.templates[t].broken {
            return None;
        }
        let id = d.alloc_id();
        let json = d.object_json(t, id);
        let t0 = std::time::Instant::now();
        let added = match self
            .project
            .add_object(&SceneSource(&d.vfs), CREATED_FILE, &json)
        {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("createLayer '{}': {e}", d.templates[t].label);
                d.templates[t].broken = true;
                return None;
            }
        };
        if let Some(why) = self.project.set_up_error(added.index, &d.build) {
            tracing::warn!("createLayer '{}': not created: {why}", d.templates[t].label);
            d.templates[t].broken = true;
            return None;
        }
        let tpl = &mut d.templates[t];
        if tpl.initial.is_none() {
            let values = self
                .project
                .settings
                .values_in(added.settings.clone())
                .into();
            let alignment = self
                .project
                .scene
                .as_ref()
                .map(|s| &s.object(added.index).kind)
                .and_then(|k| match k {
                    ObjectKind::Image(i) => Some(i.alignment),
                    _ => None,
                })
                .unwrap_or_default();
            tpl.initial = Some((values, alignment));
        }
        d.created.insert(
            added.index,
            Created {
                template: t,
                settings: added.settings.start,
                slot: None,
            },
        );
        d.pending.created.push(added.index);
        d.added += 1;
        d.add_time += t0.elapsed();
        Some(added.index)
    }

    /// `thisScene.createLayer(asset | path | configuration)`: a new layer drawn on top.
    pub(super) fn create_layer(&mut self, arg: &Value) -> Option<Value> {
        let d = self.data.dynamic.as_mut()?;
        let t = match arg {
            Value::Object(ObjectRef::Asset(a)) => *d.assets.get(*a as usize)?,
            Value::String(path) => d.template_for_path(path)?,
            Value::Record(fields) => d.template_for_config(fields)?,
            _ => return None,
        };
        let (idx, id) = match d.templates[t].free.pop() {
            Some(idx) => {
                // Reuse: a new id (stale references to the destroyed layer stay dead), the
                // initial values back.
                let id = d.alloc_id();
                let c = &d.created[&idx];
                if let Some((values, alignment)) = &d.templates[c.template].initial {
                    self.project.settings.restore(c.settings, values);
                    if let Some(scene) = self.project.scene.as_mut() {
                        let o = &mut scene.objects[idx.index()];
                        o.id = id;
                        if let ObjectKind::Image(i) = &mut o.kind {
                            i.alignment = *alignment;
                        }
                    }
                }
                d.pending.reused.push(idx);
                d.reused += 1;
                (idx, id)
            }
            None => {
                let idx = self.new_object(t)?;
                let id = self.project.scene.as_ref()?.object(idx).id;
                (idx, id)
            }
        };
        let d = self.data.dynamic.as_mut()?;
        let obj = self.project.scene.as_ref()?.object(idx);
        let mut layer = new_layer(obj, idx, None)?;
        let slot = match d.created.get(&idx).and_then(|c| c.slot) {
            Some(s) => {
                layer.size = self.data.layers[s].size;
                self.data.layers[s] = layer;
                s
            }
            None => {
                self.data.layers.push(layer);
                let s = self.data.layers.len() - 1;
                if let Some(c) = d.created.get_mut(&idx) {
                    c.slot = Some(s);
                }
                s
            }
        };
        self.data.by_id.insert(id, slot);
        self.data.order.push(slot);
        d.pending.reordered = true;
        // Restored values reach the renderer like a script write.
        self.data.changes.transforms = true;
        self.data.changes.uniforms = true;
        Some(Value::Object(ObjectRef::Layer(id)))
    }

    /// Layer named by a script argument: layer object, id (else index) or name.
    fn layer_ref(&self, arg: &Value) -> Option<LayerId> {
        match arg {
            Value::Object(ObjectRef::Layer(id)) => Some(*id),
            Value::Int(_) | Value::Float(_) => {
                let n = arg.int();
                if self.data.by_id.contains_key(&n) {
                    Some(n)
                } else {
                    let i = usize::try_from(n).ok()?;
                    self.data.order.get(i).map(|&s| self.data.layers[s].id)
                }
            }
            Value::String(name) => {
                use lumenwall_script::SceneHost;
                self.find_layer(name)
            }
            _ => None,
        }
    }

    /// `thisScene.destroyLayer(layer)`: the layer leaves the scene (its object is parked for
    /// reuse when a script created it).
    pub(super) fn destroy_layer(&mut self, arg: &Value) -> bool {
        let Some(id) = self.layer_ref(arg) else {
            return false;
        };
        let Some(slot) = self.data.by_id.remove(&id) else {
            return false;
        };
        self.data.order.retain(|&i| i != slot);
        let idx = self.data.layers[slot].object;
        if let Some(d) = self.data.dynamic.as_mut() {
            if let Some(c) = d.created.get(&idx) {
                d.templates[c.template].free.push(idx);
            }
            d.pending.removed.push(idx);
            d.pending.removed_ids.push(id);
            d.pending.reordered = true;
        }
        true
    }

    /// `thisScene.sortLayer(layer, index)`: moves the layer to `index` of the layer list.
    pub(super) fn sort_layer(&mut self, arg: &Value, index: i32) -> bool {
        let Some(slot) = self
            .layer_ref(arg)
            .and_then(|id| self.data.by_id.get(&id).copied())
        else {
            return false;
        };
        let Some(pos) = self.data.order.iter().position(|&i| i == slot) else {
            return false;
        };
        self.data.order.remove(pos);
        let to = usize::try_from(index)
            .unwrap_or(0)
            .min(self.data.order.len());
        self.data.order.insert(to, slot);
        if to != pos
            && let Some(d) = self.data.dynamic.as_mut()
        {
            d.pending.reordered = true;
        }
        true
    }

    /// `thisScene.getInitialLayerConfig(layer)`: the object as written in `scene.json`, or
    /// the configuration a created layer came from.
    pub(super) fn initial_layer_config(&self, arg: &Value) -> Option<Value> {
        let id = self.layer_ref(arg)?;
        let slot = *self.data.by_id.get(&id)?;
        let idx = self.data.layers[slot].object;
        let d = self.data.dynamic.as_ref()?;
        let json = match d.config_of(idx) {
            Some(c) => c,
            None => d.raw.get(&i64::from(id))?,
        };
        Some(from_json(json))
    }
}

/// `__workshopId` values exported by script sources (`export let __workshopId = '123';`).
pub(crate) fn workshop_ids<'a>(sources: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for src in sources {
        let mut rest = src;
        while let Some(i) = rest.find("__workshopId") {
            rest = &rest[i + "__workshopId".len()..];
            let Some(q) = rest.find(['\'', '"', '`']) else {
                break;
            };
            let quote = rest.as_bytes()[q] as char;
            let after = &rest[q + 1..];
            let Some(end) = after.find(quote) else {
                break;
            };
            let id = &after[..end];
            if !id.is_empty()
                && id.chars().all(|c| c.is_ascii_alphanumeric())
                && !ids.iter().any(|x| x == id)
            {
                ids.push(id.to_owned());
            }
            rest = &after[end..];
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workshop_ids_from_sources() {
        let ids = workshop_ids(
            [
                "'use strict';\nexport let __workshopId = '2652493753';\nlet a = 1;",
                "export var __workshopId = \"42\"; export let __workshopId2 = '2652493753';",
                "no id here",
            ]
            .into_iter(),
        );
        assert_eq!(ids, vec!["2652493753".to_owned(), "42".to_owned()]);
    }

    #[test]
    fn json_round_trip_of_configurations() {
        let v = Value::Record(vec![
            ("image".into(), Value::String("models/a.json".into())),
            ("origin".into(), Value::Vec3([1.0, 2.5, 0.0])),
            ("visible".into(), Value::Bool(false)),
            ("alpha".into(), Value::Float(0.5)),
        ]);
        let j = to_json(&v);
        assert_eq!(j["origin"], Json::String("1 2.5 0".into()));
        assert_eq!(j["visible"], Json::Bool(false));
        let back = from_json(&j);
        let Value::Record(fields) = back else {
            panic!("record expected");
        };
        assert!(fields.contains(&("alpha".into(), Value::Float(0.5))));
        assert!(fields.contains(&("image".into(), Value::String("models/a.json".into()))));
    }
}

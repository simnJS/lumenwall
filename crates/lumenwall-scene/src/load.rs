//! Conversion of `scene.json`, models, materials and effects from the lenient raw form into the
//! model (spec 02 §6–§11), with JSON paths on errors.

use std::borrow::Cow;
use std::collections::HashMap;

use serde::Deserialize as _;

use glam::{Vec2, Vec3, Vec4};

use crate::asset::{AssetError, AssetSource};
use crate::convert::{
    json_i32, kind_error, opt_bool, opt_f32, opt_i32, opt_i32_opt, opt_str, opt_str_opt, opt_vec2,
    req_i32, req_str, req_vec3, type_error,
};
use crate::error::{At, DataErr, DataError, Expected, Fail, LoadError, Warnings};
use crate::intern::Sym;
use crate::raw::{
    Json, JsonKind, RawDependency, RawEffect, RawEffectPass, RawImageEffect, RawMap, RawMaterial,
    RawMaterialPass, RawModel, RawObject, RawParticle, RawPassOverride, RawScene, RawSeq,
    RawSetting, RawTexEntry, parse_doc,
};
use crate::scene::*;
use crate::setting::{Binder, SettingId};
use crate::value::Value;

/// Arenas filled while loading.
#[derive(Debug, Default)]
pub(crate) struct Arenas {
    pub models: Vec<Model>,
    pub materials: Vec<Material>,
    pub effects: Vec<Effect>,
}

pub(crate) struct Loader<'s, 'p> {
    pub src: &'s dyn AssetSource,
    pub b: Binder<'p>,
    pub warnings: &'p mut Warnings,
    pub arenas: &'p mut Arenas,
    /// Materials loaded now belong to 3D models or sprites: their passes default to depth test
    /// and write enabled and back-face culling (spec 09 §1.11.2).
    pub model_states: bool,
    /// Files already read during this load, by path: each reference still builds a fresh copy
    /// (§8–§10), but the bytes come from here (an effect applied to 64 layers is read once).
    /// `Err(None)`: not found; `Err(Some(message))`: unreadable.
    pub files: HashMap<String, Result<Cow<'s, [u8]>, Option<String>>>,
}

fn require_object(kind: JsonKind) -> Result<(), DataErr> {
    if kind == JsonKind::Object {
        Ok(())
    } else {
        Err(kind_error(kind, Expected::Object))
    }
}

impl<'s> Loader<'s, '_> {
    fn intern(&mut self, s: &str) -> Sym {
        self.b.strings.intern(s)
    }

    fn warn(&mut self, file: &str, message: impl Into<String>) {
        self.warnings.push(file, message);
    }

    /// Reads a referenced file; a missing or unreadable file is a warning (Q31: fatal error
    /// fixed, the referencing object degrades instead of failing the wallpaper).
    fn read_optional(&mut self, from: &str, path: &str) -> Option<Cow<'s, [u8]>> {
        let read = match self.files.get(path) {
            Some(r) => r.clone(),
            None => {
                let src = self.src;
                let r = match src.read(path) {
                    Ok(b) => Ok(b),
                    Err(AssetError::NotFound) => Err(None),
                    Err(e) => Err(Some(e.to_string())),
                };
                self.files.insert(path.to_owned(), r.clone());
                r
            }
        };
        match read {
            Ok(b) => Some(b),
            Err(None) => {
                self.warn(from, format!("referenced file `{path}` not found"));
                None
            }
            Err(Some(e)) => {
                self.warn(from, format!("referenced file `{path}` unreadable: {e}"));
                None
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // scene.json

    pub fn load_scene(&mut self, file: &str) -> Result<Scene, LoadError> {
        let src = self.src;
        let bytes = src.read(file).map_err(|source| LoadError::Asset {
            path: file.to_owned(),
            source,
        })?;
        let mut scratch = None;
        let raw: RawScene<'_> = parse_doc(file, &bytes, &mut scratch)?;
        self.scene_doc(file, raw).map_err(|e| e.into_load(file))
    }

    fn scene_doc(&mut self, file: &str, raw: RawScene<'_>) -> Result<Scene, Fail> {
        require_object(raw.kind)?;
        let cam = &raw.camera;
        let gen_ = &raw.general;
        require_object(cam.kind).at("camera")?;
        require_object(gen_.kind).at("general")?;
        // Lumenwall also reads nearz/farz/fov from `general`, where Wallpaper Engine writes them
        // (Q5); `camera` keeps priority.
        let nearz_raw = if cam.nearz.is_none() {
            &gen_.nearz
        } else {
            &cam.nearz
        };
        let farz_raw = if cam.farz.is_none() {
            &gen_.farz
        } else {
            &cam.farz
        };
        let fov_raw = if cam.fov.is_none() {
            &gen_.fov
        } else {
            &cam.fov
        };
        // Spec 09 §1.2: a perspective scene (no `orthogonalprojection`) defaults to the planes of
        // Wallpaper Engine's 3D templates instead of the 2D ones.
        let perspective = gen_.orthogonalprojection.kind.is_none();
        let (near_default, far_default) = if perspective {
            (0.01, 10000.0)
        } else {
            (0.0, 1000.0)
        };
        let camera = Camera {
            center: req_vec3(&cam.center).at("center").at("camera")?,
            eye: req_vec3(&cam.eye).at("eye").at("camera")?,
            up: req_vec3(&cam.up).at("up").at("camera")?,
            nearz: self
                .b
                .setting(nearz_raw, Some(Value::from_float(near_default)), false)
                .at("nearz")
                .at("camera")?,
            farz: self
                .b
                .setting(farz_raw, Some(Value::from_float(far_default)), false)
                .at("farz")
                .at("camera")?,
            fov: self
                .b
                .setting(fov_raw, Some(Value::from_float(50.0)), false)
                .at("fov")
                .at("camera")?,
        };
        let mut camera_paths = Vec::new();
        if cam.paths.kind == JsonKind::Array {
            for (i, p) in cam.paths.items.iter().enumerate() {
                let p = req_str(p).at_idx(i).at("paths").at("camera")?;
                camera_paths.push(self.intern(p));
            }
        }
        let general = self.scene_general(gen_).at("general")?;

        let objects_raw = raw.objects;
        match objects_raw.kind {
            JsonKind::Array | JsonKind::Object => {}
            k => return Err(Fail::from(kind_error(k, Expected::Array))).at("objects"),
        }
        let mut objects = Vec::with_capacity(objects_raw.items.len());
        let mut without_id = Vec::new();
        for (i, o) in objects_raw.items.into_iter().enumerate() {
            if o.id == Json::Absent {
                without_id.push(i);
            }
            let obj = self.object(file, o).at_idx(i).at("objects")?;
            objects.push(obj);
        }
        // Spec 09 §1.8: objects of old projects have no `id` at all. Each gets its own synthetic
        // id `-1000 - rank`, skipping ids of the file, so that all of them are created (the
        // degraded mode of spec 02 §7.2 would merge them into one object of id -1).
        if !without_id.is_empty() {
            let mut taken: std::collections::HashSet<i32> = objects
                .iter()
                .enumerate()
                .filter(|(i, _)| !without_id.contains(i))
                .map(|(_, o)| o.id)
                .collect();
            for i in without_id {
                let mut id = -1000 - i as i32;
                while taken.contains(&id) {
                    id -= 1;
                }
                taken.insert(id);
                objects[i].id = id;
            }
        }
        let mut by_id = HashMap::with_capacity(objects.len());
        for (i, o) in objects.iter().enumerate() {
            by_id.entry(o.id).or_insert(ObjectIndex(i as u32));
        }
        let file_objects = objects.len();
        Ok(Scene {
            camera,
            camera_paths,
            general,
            objects,
            arenas: std::mem::take(self.arenas),
            names: Names::new(self.b.strings),
            by_id,
            file_objects,
            bloom: None,
            util_tint: None,
            util_passthrough: None,
        })
    }

    fn scene_general(
        &mut self,
        g: &crate::raw::RawSceneGeneral<'_>,
    ) -> Result<SceneGeneral, DataErr> {
        let f = |v: f32| Some(Value::from_float(v));
        let b = |v: bool| Some(Value::from_bool(v));
        let ortho = &g.orthogonalprojection;
        // Spec 09 §3.10: Wallpaper Engine's bloom defaults when `bloom` is set without strength
        // or threshold. Applied to perspective scenes only; 2D scenes keep spec 02 Q21.
        let (bloom_strength, bloom_threshold) = if ortho.kind.is_none() {
            (2.0, 0.65)
        } else {
            (0.0, 0.0)
        };
        // Lumenwall: a missing or null projection is a perspective scene instead of a load
        // failure (Q6).
        let orthogonal_projection = match ortho.kind {
            JsonKind::Absent | JsonKind::Null => None,
            JsonKind::Object => {
                let auto = opt_bool(&ortho.auto, false)
                    .at("auto")
                    .at("orthogonalprojection")?;
                let (width, height) = if auto {
                    (0, 0)
                } else {
                    (
                        req_i32(&ortho.width)
                            .at("width")
                            .at("orthogonalprojection")?,
                        req_i32(&ortho.height)
                            .at("height")
                            .at("orthogonalprojection")?,
                    )
                };
                Some(OrthogonalProjection {
                    auto,
                    width,
                    height,
                })
            }
            found => {
                return Err(DataErr::new(DataError::Type {
                    expected: Expected::Object,
                    found,
                }))
                .at("orthogonalprojection");
            }
        };
        Ok(SceneGeneral {
            ambient_color: self
                .b
                .setting(&g.ambientcolor, Some(Value::from_vec3(Vec3::ZERO)), false)
                .at("ambientcolor")?,
            skylight_color: self
                .b
                .setting(&g.skylightcolor, Some(Value::from_vec3(Vec3::ZERO)), false)
                .at("skylightcolor")?,
            clear_color: self
                .b
                .setting(&g.clearcolor, Some(Value::from_vec3(Vec3::ONE)), false)
                .at("clearcolor")?,
            camera_fade: self
                .b
                .setting(&g.camerafade, b(false), false)
                .at("camerafade")?,
            camera_preview: opt_bool(&g.camerapreview, false).at("camerapreview")?,
            bloom: self.b.setting(&g.bloom, b(false), false).at("bloom")?,
            bloom_strength: self
                .b
                .setting(&g.bloomstrength, f(bloom_strength), false)
                .at("bloomstrength")?,
            bloom_threshold: self
                .b
                .setting(&g.bloomthreshold, f(bloom_threshold), false)
                .at("bloomthreshold")?,
            camera_parallax: self
                .b
                .setting(&g.cameraparallax, b(false), false)
                .at("cameraparallax")?,
            camera_parallax_amount: self
                .b
                .setting(&g.cameraparallaxamount, f(1.0), false)
                .at("cameraparallaxamount")?,
            camera_parallax_delay: self
                .b
                .setting(&g.cameraparallaxdelay, f(0.0), false)
                .at("cameraparallaxdelay")?,
            camera_parallax_mouse_influence: self
                .b
                .setting(&g.cameraparallaxmouseinfluence, f(1.0), false)
                .at("cameraparallaxmouseinfluence")?,
            camera_shake: self
                .b
                .setting(&g.camerashake, b(false), false)
                .at("camerashake")?,
            camera_shake_amplitude: self
                .b
                .setting(&g.camerashakeamplitude, f(0.0), false)
                .at("camerashakeamplitude")?,
            camera_shake_roughness: self
                .b
                .setting(&g.camerashakeroughness, f(0.0), false)
                .at("camerashakeroughness")?,
            camera_shake_speed: self
                .b
                .setting(&g.camerashakespeed, f(0.0), false)
                .at("camerashakespeed")?,
            orthogonal_projection,
            // `null` counts as true (spec 09 §1.2).
            clear_enabled: self
                .b
                .setting(&g.clearenabled, b(true), false)
                .at("clearenabled")?,
            transparent_sorting: match &g.transparentsorting {
                Json::Absent | Json::Null => None,
                j => Some(opt_bool(j, false).at("transparentsorting")?),
            },
            hdr: opt_bool(&g.hdr, false).at("hdr")?,
            perspective_override_fov: match &g.perspectiveoverridefov {
                Json::Absent | Json::Null => None,
                j => Some(opt_f32(j, 0.0).at("perspectiveoverridefov")?),
            },
        })
    }

    // -----------------------------------------------------------------------------------------
    // objects (§7)

    pub(crate) fn object(&mut self, file: &str, mut raw: RawObject<'_>) -> Result<Object, Fail> {
        require_object(raw.kind)?;
        let (id, name) = match (req_i32(&raw.id), req_str(&raw.name)) {
            (Ok(id), Ok(name)) => (id, name.to_owned()),
            // Spec 09 §1.8: objects of old projects have neither `id` nor `name`; the caller
            // gives them a synthetic id.
            _ if raw.id == Json::Absent && raw.name.kind().is_none() => (-1, String::new()),
            _ if raw.id == Json::Absent => (-1, opt_str(&raw.name, "").unwrap_or("").to_owned()),
            _ => {
                // §7.2 degraded mode.
                let id = match &raw.id {
                    j @ (Json::Int(_) | Json::UInt(_) | Json::Float(_)) => {
                        json_i32(j).unwrap_or(-1)
                    }
                    _ => -1,
                };
                let name = match &raw.name {
                    Json::Str(s) => s.to_string(),
                    j @ (Json::Int(_) | Json::UInt(_) | Json::Float(_)) => {
                        json_i32(j).unwrap_or(0).to_string()
                    }
                    _ => "unknown".to_owned(),
                };
                self.warn(
                    file,
                    format!("object `id`/`name` unreadable, using id {id} and name {name:?}"),
                );
                (id, name)
            }
        };
        let dependencies: Box<[i32]> = if raw.dependencies.kind == JsonKind::Array {
            let mut d = Vec::with_capacity(raw.dependencies.items.len());
            for (i, dep) in raw.dependencies.items.iter().enumerate() {
                // Lumenwall also accepts `{ "id", "index", "type" }` objects, using their `id`
                // (spec 05 A7.8).
                let id = match dep {
                    RawDependency::Object(id) => req_i32(id).at("id"),
                    RawDependency::Value(j) => req_i32(j),
                    RawDependency::Absent => req_i32(&Json::Absent),
                };
                d.push(id.at_idx(i).at("dependencies")?);
            }
            d.into()
        } else {
            Box::default()
        };
        let parent = opt_i32_opt(&raw.parent).at("parent")?;
        let origin = self
            .b
            .setting(&raw.origin, Some(Value::from_vec3(Vec3::ZERO)), false)
            .at("origin")?;
        let scale = self
            .b
            .setting(&raw.scale, Some(Value::from_vec3(Vec3::ONE)), false)
            .at("scale")?;
        let angles = self
            .b
            .setting(&raw.angles, Some(Value::from_vec3(Vec3::ZERO)), false)
            .at("angles")?;
        let visible = self
            .b
            .setting(&raw.visible, Some(Value::from_bool(true)), false)
            .at("visible")?;
        let name = self.intern(&name);

        let particle_source = match &raw.particle {
            RawParticle::Inline(r) => Some(ParticleSource::Inline((*r).to_owned())),
            RawParticle::Path(p) if !p.is_empty() && p != "None" => {
                Some(ParticleSource::File(self.intern(p)))
            }
            _ => None,
        };
        let kind = if let Json::Str(path) = &raw.image {
            let path = path.to_string();
            ObjectKind::Image(Box::new(self.image(file, &raw, &path)?))
        } else if raw.sound.kind == JsonKind::Array {
            let mut files = Vec::with_capacity(raw.sound.items.len());
            for (i, j) in raw.sound.items.iter().enumerate() {
                let p = req_str(j).at_idx(i).at("sound")?;
                files.push(self.intern(p));
            }
            let mode = opt_str_opt(&raw.playbackmode).at("playbackmode")?;
            ObjectKind::Sound(SoundObject {
                files,
                looping: mode == Some("loop"),
            })
        } else if let Some(source) = particle_source {
            ObjectKind::Particle(Box::new(self.particle(source, &mut raw)?))
        } else if !matches!(raw.text, RawSetting::Absent) {
            ObjectKind::Text(Box::new(self.text(file, &raw)?))
        } else if let Some(path) = non_empty(&raw.model) {
            let path = path.to_owned();
            ObjectKind::Model3d(Box::new(self.model_object(&raw, &path)?))
        } else if let Some(kind) = non_empty(&raw.light) {
            match LightKind::parse(kind) {
                Some(kind) => ObjectKind::Light(Box::new(self.light(&raw, kind)?)),
                None => {
                    self.warn(
                        file,
                        format!("object {id}: unknown light type {kind:?}, ignored"),
                    );
                    ObjectKind::Generic(GenericKind::Light)
                }
            }
        } else if let Some(path) = non_empty(&raw.sprite) {
            let path = path.to_owned();
            ObjectKind::Sprite(Box::new(self.sprite(&raw, &path)?))
        } else if raw.shape != Json::Absent {
            self.warn(
                file,
                format!("object {id}: volumetric lights (shape) are not supported"),
            );
            ObjectKind::Generic(GenericKind::Shape)
        } else if opt_bool(&raw.solid, false).at("solid")? {
            ObjectKind::Generic(GenericKind::Group)
        } else {
            self.warn(
                file,
                format!(
                    "object {id} ({}): unknown object type",
                    self.b.strings.resolve(name)
                ),
            );
            ObjectKind::Generic(GenericKind::Unknown)
        };
        Ok(Object {
            id,
            name,
            dependencies,
            parent,
            origin,
            scale,
            angles,
            visible,
            kind,
        })
    }

    /// 3D model object (spec 09 §1.5). The `.mdl` and its materials are loaded later, by the
    /// renderer side ([`crate::Project::load_model_material`]).
    fn model_object(&mut self, raw: &RawObject<'_>, path: &str) -> Result<ModelObject, Fail> {
        let skin = opt_i32(&raw.skin, 0).at("skin")?;
        let reflected = opt_bool(&raw.reflected, false).at("reflected")?;
        let cast_shadow = opt_bool(&raw.castshadow, true).at("castshadow")?;
        let perspective = opt_bool(&raw.perspective, false).at("perspective")?;
        let b = &mut self.b;
        let parallax_depth = b
            .setting(
                &raw.parallax_depth,
                Some(Value::from_vec2(Vec2::ZERO)),
                false,
            )
            .at("parallaxDepth")?;
        let alpha = b
            .setting(&raw.alpha, Some(Value::from_float(1.0)), false)
            .at("alpha")?;
        let color = b
            .setting(&raw.color, Some(Value::from_color(Vec4::ONE)), true)
            .at("color")?;
        let brightness = b
            .setting(&raw.brightness, Some(Value::from_float(1.0)), false)
            .at("brightness")?;
        Ok(ModelObject {
            file: self.intern(path),
            skin,
            reflected,
            cast_shadow,
            perspective,
            parallax_depth,
            alpha,
            color,
            brightness,
        })
    }

    /// Light object (spec 09 §1.6).
    fn light(&mut self, raw: &RawObject<'_>, kind: LightKind) -> Result<LightObject, Fail> {
        let cast_shadow = opt_bool(&raw.castshadow, false).at("castshadow")?;
        let b = &mut self.b;
        Ok(LightObject {
            kind,
            color: b
                .setting(&raw.color, Some(Value::from_vec3(Vec3::ONE)), true)
                .at("color")?,
            intensity: b
                .setting(&raw.intensity, Some(Value::from_float(1.0)), false)
                .at("intensity")?,
            radius: b
                .setting(&raw.radius, Some(Value::from_float(0.0)), false)
                .at("radius")?,
            exponent: b
                .setting(&raw.exponent, Some(Value::from_float(2.0)), false)
                .at("exponent")?,
            cast_shadow,
        })
    }

    /// Sprite object (spec 09 §1.7): its value is a material path.
    fn sprite(&mut self, raw: &RawObject<'_>, path: &str) -> Result<SpriteObject, Fail> {
        let parallax_depth = self
            .b
            .setting(
                &raw.parallax_depth,
                Some(Value::from_vec2(Vec2::ZERO)),
                false,
            )
            .at("parallaxDepth")?;
        let saved = std::mem::replace(&mut self.model_states, true);
        let material = self.load_material(path);
        self.model_states = saved;
        Ok(SpriteObject {
            file: self.intern(path),
            material: material?,
            parallax_depth,
        })
    }

    fn halign(raw: &RawObject<'_>) -> Result<Alignment, DataErr> {
        let s = match opt_str_opt(&raw.horizontalalign).at("horizontalalign")? {
            Some(s) => s,
            None => opt_str(&raw.alignment, "center").at("alignment")?,
        };
        Ok(Alignment::parse(s))
    }

    fn image(&mut self, file: &str, raw: &RawObject<'_>, path: &str) -> Result<ImageObject, Fail> {
        let b = &mut self.b;
        let scale = b
            .setting(&raw.scale, Some(Value::from_vec3(Vec3::ONE)), false)
            .at("scale")?;
        let angles = b
            .setting(&raw.angles, Some(Value::from_vec3(Vec3::ZERO)), false)
            .at("angles")?;
        let visible = b
            .setting(&raw.visible, Some(Value::from_bool(true)), false)
            .at("visible")?;
        let alpha = b
            .setting(&raw.alpha, Some(Value::from_float(1.0)), false)
            .at("alpha")?;
        let color = b
            .setting(&raw.color, Some(Value::from_color(Vec4::ONE)), true)
            .at("color")?;
        let alignment = Self::halign(raw)?;
        // parity: `size` is a snapshot taken at load (Q18; overrides are already applied).
        let size = b
            .snapshot(&raw.size, Value::from_vec2(Vec2::ZERO), false)
            .at("size")?
            .vec2();
        let parallax_depth = b
            .setting(
                &raw.parallax_depth,
                Some(Value::from_vec2(Vec2::ZERO)),
                false,
            )
            .at("parallaxDepth")?;
        let color_blend_mode = b
            .setting(&raw.color_blend_mode, Some(Value::from_int(0)), false)
            .at("colorBlendMode")?;
        let brightness = b
            .setting(&raw.brightness, Some(Value::from_float(1.0)), false)
            .at("brightness")?;

        let model = self.load_model(path)?;
        let file_sym = self.intern(path);

        let mut effects = Vec::new();
        if raw.effects.kind == JsonKind::Array {
            for (i, e) in raw.effects.items.iter().enumerate() {
                if let Some(eff) = self.image_effect(file, e).at_idx(i).at("effects")? {
                    effects.push(eff);
                }
            }
        }
        let mut animation_layers = Vec::new();
        if raw.animationlayers.kind == JsonKind::Array {
            for (i, l) in raw.animationlayers.items.iter().enumerate() {
                let layer = (|| -> Result<AnimationLayer, DataErr> {
                    require_object(l.kind)?;
                    Ok(AnimationLayer {
                        id: req_i32(&l.id).at("id")?,
                        rate: self
                            .b
                            .setting(&l.rate, Some(Value::from_float(1.0)), false)
                            .at("rate")?,
                        visible: self
                            .b
                            .setting(&l.visible, Some(Value::from_bool(false)), false)
                            .at("visible")?,
                        blend: self
                            .b
                            .setting(&l.blend, Some(Value::from_float(1.0)), false)
                            .at("blend")?,
                        animation: self
                            .b
                            .setting(&l.animation, Some(Value::from_int(0)), false)
                            .at("animation")?,
                    })
                })()
                .at_idx(i)
                .at("animationlayers")?;
                animation_layers.push(layer);
            }
        }
        if raw.instance.kind == JsonKind::Object {
            let textures = self.texture_map(&raw.instance.textures);
            let user_textures = self.texture_map(&raw.instance.usertextures);
            if let Some(m) = model {
                let mat = self.arenas.models[m.index()].material;
                if let Some(first) = self.arenas.materials[mat.index()].passes.first_mut() {
                    // parity: instance textures only fill slots the material leaves empty (Q14).
                    first.textures.fill_missing(&textures);
                    first.user_textures.fill_missing(&user_textures);
                }
            }
        }
        Ok(ImageObject {
            file: file_sym,
            model,
            scale,
            angles,
            visible,
            alpha,
            color,
            alignment,
            size,
            parallax_depth,
            color_blend_mode,
            brightness,
            effects,
            animation_layers,
        })
    }

    fn image_effect(
        &mut self,
        file: &str,
        e: &RawImageEffect<'_>,
    ) -> Result<Option<ImageEffect>, Fail> {
        require_object(e.kind)?;
        let path = req_str(&e.file).at("file")?.to_owned();
        let id = opt_i32(&e.id, -1).at("id")?;
        let name = opt_str_opt(&e.name).at("name")?.map(Box::from);
        let visible = self
            .b
            .setting(&e.visible, Some(Value::from_bool(true)), false)
            .at("visible")?;
        let mut overrides = Vec::new();
        if e.passes.kind == JsonKind::Array {
            for (i, p) in e.passes.items.iter().enumerate() {
                overrides.push(self.pass_override(p).at_idx(i).at("passes")?);
            }
        }
        let Some(effect) = self.load_effect(&path)? else {
            self.warn(file, format!("effect `{path}` skipped"));
            return Ok(None);
        };
        let file_sym = self.intern(&path);
        Ok(Some(ImageEffect {
            file: file_sym,
            effect,
            id,
            name,
            visible,
            overrides,
        }))
    }

    fn pass_override(&mut self, p: &RawPassOverride<'_>) -> Result<PassOverride, DataErr> {
        require_object(p.kind)?;
        Ok(PassOverride {
            id: opt_i32(&p.id, -1).at("id")?,
            combos: self.combos(&p.combos).at("combos")?,
            constants: self
                .constants(&p.constantshadervalues)
                .at("constantshadervalues")?,
            textures: self.texture_map(&p.textures),
            user_textures: self.texture_map(&p.usertextures),
        })
    }

    fn text(&mut self, file: &str, raw: &RawObject<'_>) -> Result<TextObject, Fail> {
        let b = &mut self.b;
        let text = b.setting(&raw.text, None, false).at("text")?;
        // Lumenwall (parity rule 5): the content string stays text. §5.2 would read "26 NOV 2024" as
        // a vec3 and "2024" as a float, whose text view is empty: WE displays them.
        let content = match &raw.text {
            RawSetting::Scalar(Json::Str(s)) => Some(s),
            RawSetting::Object(o) => match &o.value {
                Json::Str(s) => Some(s),
                _ => None,
            },
            _ => None,
        };
        if let Some(s) = content
            && b.settings.meta(text).property.is_none()
            && b.settings.value(text).tag() != crate::value::ValueTag::Text
        {
            b.settings.set(text, Value::from_text(s));
        }
        let font = opt_str(&raw.font, "").at("font")?.to_owned();
        let point_size = b
            .setting(&raw.pointsize, Some(Value::from_float(32.0)), false)
            .at("pointsize")?;
        let size = strict_vec2_setting(&raw.size).at("size")?;
        let scale = b
            .setting(&raw.scale, Some(Value::from_vec3(Vec3::ONE)), false)
            .at("scale")?;
        let color = b
            .setting(&raw.color, Some(Value::from_color(Vec4::ONE)), true)
            .at("color")?;
        let alpha = b
            .setting(&raw.alpha, Some(Value::from_float(1.0)), false)
            .at("alpha")?;
        let visible = b
            .setting(&raw.visible, Some(Value::from_bool(true)), false)
            .at("visible")?;
        let horizontal_align = Self::halign(raw)?;
        let vertical_align =
            Alignment::parse(opt_str(&raw.verticalalign, "center").at("verticalalign")?);
        let padding = strict_vec2_setting(&raw.padding).at("padding")?;
        // Lumenwall: image-layer fields of a text (see `TextObject::layer`).
        let brightness = b
            .setting(&raw.brightness, Some(Value::from_float(1.0)), false)
            .at("brightness")?;
        let parallax_depth = b
            .setting(
                &raw.parallax_depth,
                Some(Value::from_vec2(Vec2::ZERO)),
                false,
            )
            .at("parallaxDepth")?;
        let color_blend_mode = b
            .setting(&raw.color_blend_mode, Some(Value::from_int(0)), false)
            .at("colorBlendMode")?;
        let font = self.intern(&font);
        let mut effects = Vec::new();
        if raw.effects.kind == JsonKind::Array {
            for (i, e) in raw.effects.items.iter().enumerate() {
                if let Some(eff) = self.image_effect(file, e).at_idx(i).at("effects")? {
                    effects.push(eff);
                }
            }
        }
        let model = self.text_model();
        let layer = ImageObject {
            file: self.intern(TEXT_MODEL),
            model: Some(model),
            scale,
            angles: self.constant(Value::from_vec3(Vec3::ZERO)),
            visible,
            alpha: self.constant(Value::from_float(1.0)),
            color: self.constant(Value::from_color(Vec4::ONE)),
            alignment: Alignment {
                top: vertical_align.top,
                bottom: vertical_align.bottom,
                left: horizontal_align.left,
                right: horizontal_align.right,
            },
            size: Vec2::ZERO,
            parallax_depth,
            color_blend_mode,
            brightness: self.constant(Value::from_float(1.0)),
            effects,
            animation_layers: Vec::new(),
        };
        Ok(TextObject {
            text,
            font,
            point_size,
            size,
            scale,
            color,
            alpha,
            visible,
            horizontal_align,
            vertical_align,
            padding,
            brightness,
            layer,
        })
    }

    /// Synthetic model and material of a text layer: Wallpaper Engine's `font` shader (as in
    /// its `materials/fonts/basefont.json`), translucent, no culling, the glyph texture
    /// `TEXT_TEXTURE_PREFIX<material index>` in slot 0. Built in code, like the bloom
    /// material, so a text never depends on an asset file of its own.
    fn text_model(&mut self) -> ModelId {
        let n = self.arenas.materials.len();
        let texture = self.intern(&format!("{TEXT_TEXTURE_PREFIX}{n}"));
        let shader = self.intern("font");
        let file = self.intern(TEXT_MATERIAL);
        self.arenas.materials.push(Material {
            file,
            passes: vec![MaterialPass {
                shader,
                blending: Blending::Translucent,
                cull_mode: CullMode::NoCull,
                depth_test: DepthMode::Disabled,
                depth_write: DepthMode::Disabled,
                textures: TextureMap(vec![(0, texture)]),
                user_textures: TextureMap::default(),
                combos: Vec::new(),
                constants: Vec::new(),
                user_constants: Vec::new(),
            }],
        });
        let material = MaterialId(n as u32);
        let file = self.intern(TEXT_MODEL);
        self.arenas.models.push(Model {
            file,
            material,
            solid_layer: false,
            fullscreen: false,
            passthrough: false,
            autosize: false,
            nopadding: false,
            width: None,
            height: None,
            puppet: None,
        });
        ModelId(self.arenas.models.len() as u32 - 1)
    }

    fn particle(
        &mut self,
        source: ParticleSource,
        raw: &mut RawObject<'_>,
    ) -> Result<ParticleObject, Fail> {
        let b = &mut self.b;
        Ok(ParticleObject {
            source,
            scale: b
                .setting(&raw.scale, Some(Value::from_vec3(Vec3::ONE)), false)
                .at("scale")?,
            angles: b
                .setting(&raw.angles, Some(Value::from_vec3(Vec3::ZERO)), false)
                .at("angles")?,
            visible: b
                .setting(&raw.visible, Some(Value::from_bool(true)), false)
                .at("visible")?,
            parallax_depth: b
                .setting(
                    &raw.parallax_depth,
                    Some(Value::from_vec2(Vec2::ZERO)),
                    false,
                )
                .at("parallaxDepth")?,
            instance_override: raw.instanceoverride.take(),
        })
    }

    // -----------------------------------------------------------------------------------------
    // shared pieces (§11)

    fn texture_map(&mut self, raw: &RawSeq<RawTexEntry<'_>>) -> TextureMap {
        if raw.kind != JsonKind::Array {
            return TextureMap::default();
        }
        let mut out = Vec::new();
        for (i, e) in raw.items.iter().enumerate() {
            if let RawTexEntry::Name(n) = e
                && !n.is_empty()
            {
                out.push((i as u32, self.intern(n)));
            }
        }
        TextureMap(out)
    }

    fn combos(&mut self, raw: &RawMap<'_, Json<'_>>) -> Result<Vec<Combo>, DataErr> {
        if raw.kind != JsonKind::Object {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(raw.entries.len());
        for (name, j) in raw.sorted() {
            let value = match j {
                // Lumenwall accepts integer strings instead of failing the wallpaper (Q29).
                Json::Str(s) => s.trim().parse::<i32>().ok(),
                other => json_i32(other),
            };
            let value = value
                .ok_or_else(|| type_error(Expected::Integer, j))
                .at_key(name)?;
            let name_sym = self.intern(name);
            let key = self.intern(&name.to_ascii_uppercase());
            out.push(Combo {
                name: name_sym,
                key,
                value,
            });
        }
        Ok(out)
    }

    fn constants(&mut self, raw: &RawMap<'_, RawSetting<'_>>) -> Result<Vec<Constant>, DataErr> {
        if raw.kind != JsonKind::Object {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(raw.entries.len());
        for (name, node) in raw.sorted() {
            let setting = self.b.setting(node, None, false).at_key(name)?;
            let name = self.intern(name);
            out.push(Constant { name, setting });
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------------------------
    // models, materials, effects (§8–§10). Each reference loads a fresh copy.

    /// `Ok(None)` when the model or its material file is missing.
    pub fn load_model(&mut self, path: &str) -> Result<Option<ModelId>, LoadError> {
        let Some(bytes) = self.read_optional(path, path) else {
            return Ok(None);
        };
        let mut scratch = None;
        let raw: RawModel<'_> = parse_doc(path, &bytes, &mut scratch)?;
        let r = (|| -> Result<Option<Model>, Fail> {
            require_object(raw.kind)?;
            let material_path = req_str(&raw.material).at("material")?;
            let Some(material) = self.load_material(material_path)? else {
                return Ok(None);
            };
            let puppet = opt_str_opt(&raw.puppet)
                .at("puppet")?
                .map(|p| self.intern(p));
            Ok(Some(Model {
                file: self.intern(path),
                material,
                solid_layer: opt_bool(&raw.solidlayer, false).at("solidlayer")?,
                fullscreen: opt_bool(&raw.fullscreen, false).at("fullscreen")?,
                passthrough: opt_bool(&raw.passthrough, false).at("passthrough")?,
                autosize: opt_bool(&raw.autosize, false).at("autosize")?,
                nopadding: opt_bool(&raw.nopadding, false).at("nopadding")?,
                width: opt_i32_opt(&raw.width).at("width")?,
                height: opt_i32_opt(&raw.height).at("height")?,
                puppet,
            }))
        })();
        let model = r.map_err(|e| e.into_load(path))?;
        Ok(model.map(|m| {
            self.arenas.models.push(m);
            ModelId(self.arenas.models.len() as u32 - 1)
        }))
    }

    /// `Ok(None)` when the file is missing.
    pub fn load_material(&mut self, path: &str) -> Result<Option<MaterialId>, LoadError> {
        let Some(bytes) = self.read_optional(path, path) else {
            return Ok(None);
        };
        let mut scratch = None;
        let raw: RawMaterial<'_> = parse_doc(path, &bytes, &mut scratch)?;
        let material = self
            .material_doc(path, &raw)
            .map_err(|e| e.into_load(path))?;
        self.arenas.materials.push(material);
        Ok(Some(MaterialId(self.arenas.materials.len() as u32 - 1)))
    }

    fn material_doc(&mut self, path: &str, raw: &RawMaterial<'_>) -> Result<Material, DataErr> {
        require_object(raw.kind)?;
        let mut passes = Vec::new();
        match raw.passes.kind {
            JsonKind::Absent => return Err(DataErr::new(DataError::Missing)).at("passes"),
            JsonKind::Array => {
                for (i, p) in raw.passes.items.iter().enumerate() {
                    passes.push(self.material_pass(path, p).at_idx(i).at("passes")?);
                }
            }
            // Not an array: zero passes (§9).
            _ => {}
        }
        Ok(Material {
            file: self.intern(path),
            passes,
        })
    }

    fn material_pass(
        &mut self,
        path: &str,
        p: &RawMaterialPass<'_>,
    ) -> Result<MaterialPass, DataErr> {
        require_object(p.kind)?;
        let shader = req_str(&p.shader).at("shader")?;
        let shader = self.intern(shader);
        let blending = match opt_str(&p.blending, "normal").at("blending")? {
            "normal" => Blending::Normal,
            "translucent" => Blending::Translucent,
            "additive" => Blending::Additive,
            other => {
                self.warn(path, format!("unknown blending {other:?}, using normal"));
                Blending::Normal
            }
        };
        // Spec 09 §1.11.2: passes of 3D models and sprites default to back-face culling and
        // depth test/write; image passes keep the spec 02 defaults.
        let model = self.model_states;
        let (cull_default, depth_default) = if model {
            ("normal", "enabled")
        } else {
            ("nocull", "disabled")
        };
        let cull_mode = match opt_str(&p.cullmode, cull_default).at("cullmode")? {
            "nocull" => CullMode::NoCull,
            "normal" => CullMode::Normal,
            other => {
                self.warn(
                    path,
                    format!("unknown cullmode {other:?}, using {cull_default}"),
                );
                if model {
                    CullMode::Normal
                } else {
                    CullMode::NoCull
                }
            }
        };
        // `depthtesting` / `depthwriting` (misspelt keys of `ricepod`) read as the real keys
        // when those are absent (QO-3D-13).
        let (test_key, test_name) =
            if p.depthtest.kind().is_none() && !p.depthtesting.kind().is_none() {
                (&p.depthtesting, "depthtesting")
            } else {
                (&p.depthtest, "depthtest")
            };
        let (write_key, write_name) =
            if p.depthwrite.kind().is_none() && !p.depthwriting.kind().is_none() {
                (&p.depthwriting, "depthwriting")
            } else {
                (&p.depthwrite, "depthwrite")
            };
        let depth_test = self.depth_mode(path, test_key, test_name, depth_default)?;
        let depth_write = self.depth_mode(path, write_key, write_name, depth_default)?;
        let mut constants = self
            .constants(&p.constantshadervalues)
            .at("constantshadervalues")?;
        self.user_shader_values(path, &p.usershadervalues, &mut constants);
        let user_constants = self.user_constants(path, &p.usershadervalues)?;
        Ok(MaterialPass {
            shader,
            blending,
            cull_mode,
            depth_test,
            depth_write,
            textures: self.texture_map(&p.textures),
            user_textures: self.texture_map(&p.usertextures),
            combos: self.combos(&p.combos).at("combos")?,
            constants,
            user_constants,
        })
    }

    /// `usershadervalues` (`{"<user property>": "<material constant>"}`, absent from spec 02):
    /// the constant follows the property, like a `constantshadervalues` entry written
    /// `{"user": "<property>", "value": …}`, and replaces such an entry. Unknown properties
    /// leave the constant as it is (warning).
    fn user_shader_values(
        &mut self,
        path: &str,
        raw: &RawMap<'_, Json<'_>>,
        constants: &mut Vec<Constant>,
    ) {
        if raw.kind != JsonKind::Object {
            return;
        }
        for (property, key) in raw.sorted() {
            let Json::Str(key) = key else {
                self.warn(path, format!("usershadervalues.{property}: not a string"));
                continue;
            };
            let Some(setting) = self.b.property_setting(property) else {
                self.warn(
                    path,
                    format!("usershadervalues: unknown user property `{property}`"),
                );
                continue;
            };
            let name = self.intern(key);
            match constants.iter_mut().find(|c| c.name == name) {
                Some(c) => c.setting = setting,
                None => constants.push(Constant { name, setting }),
            }
        }
    }

    fn depth_mode(
        &mut self,
        path: &str,
        j: &Json<'_>,
        key: &'static str,
        default: &'static str,
    ) -> Result<DepthMode, DataErr> {
        Ok(match opt_str(j, default).at(key)? {
            "disabled" => DepthMode::Disabled,
            "enabled" => DepthMode::Enabled,
            other => {
                self.warn(path, format!("unknown {key} {other:?}, using {default}"));
                if default == "enabled" {
                    DepthMode::Enabled
                } else {
                    DepthMode::Disabled
                }
            }
        })
    }

    /// `usershadervalues` (spec 09 §1.11.3): `{"<user property>": "<material alias>"}`, each
    /// alias bound live to its property. A missing property is skipped with a warning.
    fn user_constants(
        &mut self,
        path: &str,
        raw: &RawMap<'_, Json<'_>>,
    ) -> Result<Vec<Constant>, DataErr> {
        let mut out = Vec::new();
        if raw.kind != JsonKind::Object {
            return Ok(out);
        }
        for (property, alias) in raw.sorted() {
            // Same tolerance as `user_shader_values`: a non-string alias is skipped, not fatal.
            let Ok(alias) = req_str(alias) else {
                self.warn(
                    path,
                    format!("usershadervalues: {property:?} is not a constant name, ignored"),
                );
                continue;
            };
            if self.b.props.find(property).is_none() {
                self.warn(
                    path,
                    format!("usershadervalues: user property {property:?} not found, ignored"),
                );
                continue;
            }
            let json = serde_json::json!({ "user": property, "value": 0 });
            let setting = match RawSetting::deserialize(&json) {
                Ok(raw) => self
                    .b
                    .setting(&raw, None, false)
                    .at_key(property)
                    .at("usershadervalues")?,
                Err(_) => continue,
            };
            out.push(Constant {
                name: self.intern(alias),
                setting,
            });
        }
        Ok(out)
    }

    /// `Ok(None)` when the effect file or one of its materials is missing.
    pub fn load_effect(&mut self, path: &str) -> Result<Option<EffectId>, LoadError> {
        let Some(bytes) = self.read_optional(path, path) else {
            return Ok(None);
        };
        let mut scratch = None;
        let raw: RawEffect<'_> = parse_doc(path, &bytes, &mut scratch)?;
        let effect = self.effect_doc(path, &raw).map_err(|e| e.into_load(path))?;
        Ok(effect.map(|e| {
            self.arenas.effects.push(e);
            EffectId(self.arenas.effects.len() as u32 - 1)
        }))
    }

    fn effect_doc(&mut self, path: &str, raw: &RawEffect<'_>) -> Result<Option<Effect>, Fail> {
        require_object(raw.kind)?;
        let name = opt_str(&raw.name, "").at("name")?.into();
        let description = opt_str(&raw.description, "").at("description")?.into();
        let group = opt_str(&raw.group, "").at("group")?.into();
        let preview = opt_str(&raw.preview, "").at("preview")?.into();
        let mut dependencies = Vec::new();
        if raw.dependencies.kind == JsonKind::Array {
            for (i, d) in raw.dependencies.items.iter().enumerate() {
                let d = req_str(d).at_idx(i).at("dependencies")?;
                dependencies.push(self.intern(d));
            }
        }
        let mut passes = Vec::new();
        match raw.passes.kind {
            JsonKind::Absent => return Err(Fail::from(DataError::Missing)).at("passes"),
            JsonKind::Array => {
                for (i, p) in raw.passes.items.iter().enumerate() {
                    match self.effect_pass(p).at_idx(i).at("passes")? {
                        Some(pass) => passes.push(pass),
                        None => return Ok(None),
                    }
                }
            }
            _ => {}
        }
        let mut fbos = Vec::new();
        if raw.fbos.kind == JsonKind::Array {
            for (i, f) in raw.fbos.items.iter().enumerate() {
                let fbo = (|| -> Result<Fbo, DataErr> {
                    require_object(f.kind)?;
                    let name = req_str(&f.name).at("name")?;
                    let format = opt_str(&f.format, "rgba8888").at("format")?;
                    Ok(Fbo {
                        name: self.b.strings.intern(name),
                        format: self.b.strings.intern(format),
                        scale: opt_f32(&f.scale, 1.0).at("scale")?,
                        unique: opt_bool(&f.unique, false).at("unique")?,
                    })
                })()
                .at_idx(i)
                .at("fbos")?;
                fbos.push(fbo);
            }
        }
        Ok(Some(Effect {
            file: self.intern(path),
            name,
            description,
            group,
            preview,
            dependencies,
            passes,
            fbos,
        }))
    }

    /// `Ok(None)` when the pass material file is missing.
    fn effect_pass(&mut self, p: &RawEffectPass<'_>) -> Result<Option<EffectPass>, Fail> {
        require_object(p.kind)?;
        let material = match opt_str_opt(&p.material).at("material")? {
            Some(m) => match self.load_material(m)? {
                Some(id) => Some(id),
                None => return Ok(None),
            },
            None => None,
        };
        let mut binds: Vec<Bind> = Vec::new();
        if p.bind.kind == JsonKind::Array {
            for (i, b) in p.bind.items.iter().enumerate() {
                let bind = (|| -> Result<Bind, DataErr> {
                    require_object(b.kind)?;
                    let index = req_i32(&b.index).at("index")?;
                    let name = req_str(&b.name).at("name")?;
                    Ok(Bind {
                        index,
                        source: self.bind_source(name),
                    })
                })()
                .at_idx(i)
                .at("bind")?;
                // First bind of a duplicated index wins.
                if let Err(pos) = binds.binary_search_by_key(&bind.index, |b| b.index) {
                    binds.insert(pos, bind);
                }
            }
        }
        let command = match &p.command {
            Json::Absent | Json::Null => None,
            Json::Str(s) if s == "copy" => Some(Command::Copy),
            _ => Some(Command::Swap),
        };
        let (source, target) = if command.is_some() {
            (
                Some(req_str(&p.source).at("source")?),
                Some(req_str(&p.target).at("target")?),
            )
        } else {
            (
                opt_str_opt(&p.source).at("source")?,
                opt_str_opt(&p.target).at("target")?,
            )
        };
        let source = source.map(|s| self.bind_source(s));
        let target = target.map(|t| self.intern(t));
        Ok(Some(EffectPass {
            material,
            binds,
            command,
            source,
            target,
        }))
    }

    fn bind_source(&mut self, name: &str) -> BindSource {
        if name == "previous" {
            BindSource::Previous
        } else {
            BindSource::Named(self.intern(name))
        }
    }

    /// Settings used for synthetic data (§14): an unbound constant.
    pub fn constant(&mut self, v: Value) -> SettingId {
        self.b.settings.push_constant(v)
    }
}

/// Strict optional vec2 of text objects. Lumenwall also accepts the object form `{"value": …}`
/// (its `value` is read strictly) instead of failing (Q19).
fn strict_vec2_setting(raw: &RawSetting<'_>) -> Result<Vec2, DataErr> {
    match raw {
        RawSetting::Absent => Ok(Vec2::ZERO),
        RawSetting::Scalar(j) => opt_vec2(j, Vec2::ZERO),
        RawSetting::Object(o) => opt_vec2(&o.value, Vec2::ZERO).at("value"),
    }
}

/// A string value that is neither empty nor `null` (spec 09 §1.4: `null` and `""` count as
/// absent when classifying objects).
fn non_empty<'j>(j: &'j Json<'_>) -> Option<&'j str> {
    match j {
        Json::Str(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

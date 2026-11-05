//! SceneScript integration (spec 06): the scene's [`SceneHost`] for `lumenwall-script`.
//!
//! Scripts read and write the `lumenwall-scene` settings that the renderer already reads every
//! frame (visibility, parallax depth) or that [`crate::SceneRuntime::update`] refreshes when a
//! script changed them (transforms, `g_Alpha`/`g_Color`/`g_Brightness`, material constants).
//! Nothing here exists for a scene without scripts.
//!
//! Members are resolved by name on every access, without allocation for numeric values. A write
//! that leaves a setting unchanged reports no change, so a script returning the same value every
//! frame causes no redraw.

use std::cell::Ref;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use glam::{Vec2, Vec3};
use lumenwall_scene::{
    GenericKind, ObjectIndex, ObjectKind, Project, PropertyKind, SceneBuild, SettingId,
    Value as SValue, ValueTag,
};
use lumenwall_script::{
    Clock, Cursor, CursorEvent, CursorEventKind, Display, LayerId, MediaSnapshot, ObjectRef,
    PropertyScript, SceneHost, ScriptConfig, ScriptEngine, ScriptId, ScriptStats, TextScript,
    Value,
};

use crate::scene::{FrameState, chain_visible, visibility_chain, world_transform};

mod animation;
mod dynamic;
pub(crate) mod media;
pub(crate) use dynamic::{Dynamic, Structure, workshop_ids};

/// Receives the text that scripts compute for text layers: the junction between scripting and
/// the text renderer, which registers itself with [`crate::SceneRuntime::set_text_sink`].
///
/// Called after the frame's script work, before drawing, for:
/// - dynamic text layers (spec 06 §9): the `text` of a text object carries a script; the new
///   text is never empty (an empty result becomes `" "`, §9.6), and the `text` setting itself is
///   not modified (§9.6);
/// - writes of `thisLayer.text` / `layer.text` by property scripts on text layers without a text
///   script: the `text` setting holds the same value.
///
/// Every other text property (`pointsize`, `color`, `alpha`, `visible`, `origin`, `scale`) is
/// written by scripts into the object's `lumenwall-scene` settings, which the renderer reads.
pub trait TextSink {
    /// `object` is the scene object id (`TextObject` owner).
    fn on_text_changed(&mut self, object: i32, text: &str);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayerKind {
    Image,
    Text,
    Particle,
    Group,
    /// Listed and found (`getLayer('jump').play()`), never scripted (spec 06 §1.2).
    Sound,
}

/// A scriptable object (spec 06 §1.2) and the settings behind its members.
#[derive(Debug)]
struct Layer {
    id: i32,
    object: ObjectIndex,
    kind: LayerKind,
    /// Created ancestor that is also a layer.
    parent: Option<usize>,
    origin: SettingId,
    /// Setting read by the renderer first, then the group copy parsed from the same key
    /// (they are equal for groups). Writes go to both.
    scale: [SettingId; 2],
    angles: [SettingId; 2],
    visible: [SettingId; 2],
    alpha: Option<SettingId>,
    color: Option<SettingId>,
    brightness: Option<SettingId>,
    parallax_depth: Option<SettingId>,
    point_size: Option<SettingId>,
    text: Option<SettingId>,
    /// Drawn size in pixels (images: as set up by the renderer; texts: the text box).
    size: Vec2,
    /// A property script runs on this layer (cursor events are hit-tested against it).
    scripted: bool,
    /// Its `text` is driven by a text script.
    text_script: bool,
}

/// What scripts changed during a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    /// An `origin`, `scale` or `angles` setting.
    pub transforms: bool,
    /// A setting feeding a uniform (`alpha`, `color`, `brightness`, a material constant).
    pub uniforms: bool,
}

/// Per-frame inputs copied from [`FrameState`].
#[derive(Clone, Copy, Debug, Default)]
struct FrameData {
    clock: Clock,
    cursor: Cursor,
    display: Display,
    /// `FrameState::time` (seconds), used as the monotonic timer clock.
    time: f64,
}

/// Everything the host needs besides the project.
struct HostData {
    /// Every layer ever listed; destroyed ones stay (their index may be reused by a recycled
    /// layer) but leave `by_id` and `order`.
    layers: Vec<Layer>,
    by_id: HashMap<i32, usize>,
    /// Live layers in render order (indices into `layers`): `getLayerIndex`, `sortLayer`,
    /// `enumerateLayers`, and the draw order of the scene when it changes.
    order: Vec<usize>,
    /// Layers created and destroyed by scripts; `None` when no script uses them.
    dynamic: Option<Box<Dynamic>>,
    /// Image objects whose `alignment` a script changed (applied by the scene update).
    aligned: Vec<ObjectIndex>,
    frame: FrameData,
    /// Origin of [`SceneHost::now`]: timers follow scene time.
    epoch: Instant,
    changes: Changes,
    /// Text layers whose `text` setting a property script wrote (layer indices).
    text_written: Vec<usize>,
    /// Timelines played by scripts (`getAnimation()`).
    anims: animation::Animations,
}

impl HostData {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.by_id.get(&id).map(|&i| &self.layers[i])
    }
}

/// The [`SceneHost`] lent to the script engine for one call.
struct HostView<'a> {
    data: &'a mut HostData,
    project: &'a mut Project,
    /// Spectrum of this frame (left = right), `None` outside frames.
    audio: Option<(&'a [f32; 16], &'a [f32; 32], &'a [f32; 64])>,
}

/// `lumenwall-scene` value → script value (spec 06 §4.1).
fn to_script(v: &SValue) -> Value {
    match v.tag() {
        ValueTag::Null => Value::Null,
        ValueTag::Float => Value::Float(v.float()),
        ValueTag::Int => Value::Int(v.int()),
        ValueTag::Bool => Value::Bool(v.boolean()),
        ValueTag::Vec2 => Value::Vec2(v.vec2().to_array()),
        ValueTag::Vec3 => Value::Vec3(v.vec3().to_array()),
        ValueTag::Vec4 => Value::Vec4(v.vec4().to_array()),
        ValueTag::Text => Value::String(v.text().to_owned()),
    }
}

/// Writes a script value into a setting (spec 06 §2.2, §2.3). Returns whether the setting
/// changed; values that are not dynamic values (objects, arrays…) are refused.
fn assign(project: &mut Project, id: SettingId, v: &Value) -> Option<bool> {
    let cur = project.settings.get(id)?;
    let condition = project.settings.meta(id).condition;
    let next = match v {
        Value::Null => {
            let mut n = cur.clone();
            n.set_null();
            n
        }
        Value::Bool(b) => SValue::from_bool(*b),
        Value::Int(i) => SValue::from_int(*i),
        Value::Float(f) => SValue::from_float(*f),
        Value::Vec2(c) => SValue::from_vec2(Vec2::from_array(*c)),
        Value::Vec3(c) => SValue::from_vec3(Vec3::from_array(*c)),
        Value::Vec4(c) => SValue::from_vec4((*c).into()),
        Value::String(s) => match condition {
            // Spec 06 §2.2: a string assigned to a conditional value compares with it.
            Some(c) => {
                let mut n = SValue::null();
                n.set_text_condition(Some(s.as_str().into()), project.strings.resolve(c));
                n
            }
            None if cur.tag() == ValueTag::Text && cur.text() == s => return Some(false),
            None => SValue::from_text(s),
        },
        _ => return None,
    };
    if next == *cur {
        return Some(false);
    }
    *project.settings.value_mut(id) = next;
    Some(true)
}

/// `ILayer.color` as scripts see it: WE documents a `Vec3`, alpha being its own member.
fn color_to_script(v: &SValue) -> Value {
    match v.tag() {
        ValueTag::Vec4 => Value::Vec3(v.vec3().to_array()),
        _ => to_script(v),
    }
}

/// Writes `ILayer.color`. we-doc: a `Vec3` keeps the stored alpha of a 4-component colour
/// (spec 06 §2.2 would reset it to 0, hiding layers whose shader reads `g_Color4`).
fn assign_color(project: &mut Project, id: SettingId, v: &Value) -> Option<bool> {
    let cur = project.settings.get(id)?;
    match v {
        Value::Vec3([r, g, b]) if cur.tag() == ValueTag::Vec4 => {
            let w = cur.vec4().w;
            assign(project, id, &Value::Vec4([*r, *g, *b, w]))
        }
        _ => assign(project, id, v),
    }
}

/// Member name without the `@…` location suffix of effect and material scripts.
fn member(name: &str) -> &str {
    name.split_once('@').map_or(name, |(m, _)| m)
}

impl HostView<'_> {
    fn scene_setting(&self, name: &str) -> Option<SettingId> {
        let scene = self.project.scene.as_ref()?;
        let (g, c) = (&scene.general, &scene.camera);
        Some(match name {
            "bloom" => g.bloom,
            "bloomstrength" => g.bloom_strength,
            "bloomthreshold" => g.bloom_threshold,
            "clearcolor" => g.clear_color,
            "ambientcolor" => g.ambient_color,
            "skylightcolor" => g.skylight_color,
            "fov" => c.fov,
            "nearz" => c.nearz,
            "farz" => c.farz,
            "camerafade" => g.camera_fade,
            "camerashake" => g.camera_shake,
            "camerashakespeed" => g.camera_shake_speed,
            "camerashakeamplitude" => g.camera_shake_amplitude,
            "camerashakeroughness" => g.camera_shake_roughness,
            "cameraparallax" => g.camera_parallax,
            "cameraparallaxamount" => g.camera_parallax_amount,
            "cameraparallaxdelay" => g.camera_parallax_delay,
            "cameraparallaxmouseinfluence" => g.camera_parallax_mouse_influence,
            _ => return None,
        })
    }

    fn layer_name(&self, l: &Layer) -> &str {
        self.project.scene.as_ref().map_or("", |s| {
            self.project.strings.resolve(s.object(l.object).name)
        })
    }

    fn image_effects(&self, layer: LayerId) -> &[lumenwall_scene::ImageEffect] {
        let Some(l) = self.data.layer(layer) else {
            return &[];
        };
        self.project
            .scene
            .as_ref()
            .and_then(|s| s.object(l.object).as_image())
            .map_or(&[], |i| &i.effects)
    }

    fn effect_name(&self, e: &lumenwall_scene::ImageEffect) -> String {
        match &e.name {
            Some(n) => n.to_string(),
            None => self
                .project
                .scene
                .as_ref()
                .map_or_else(String::new, |s| s.effect(e.effect).name.to_string()),
        }
    }

    /// Setting of a material constant (`IMaterial` = an effect pass override).
    fn constant(&self, layer: LayerId, effect: u32, pass: u32, name: &str) -> Option<SettingId> {
        let e = self.image_effects(layer).get(effect as usize)?;
        let o = e.overrides.get(pass as usize)?;
        o.constants
            .iter()
            .find(|c| self.project.strings.resolve(c.name) == name)
            .map(|c| c.setting)
    }

    fn effect_index(&self, layer: LayerId, arg: &Value) -> Option<usize> {
        let effects = self.image_effects(layer);
        match arg {
            Value::Int(i) => usize::try_from(*i).ok().filter(|&i| i < effects.len()),
            Value::Float(f) if *f >= 0.0 => Some(*f as usize).filter(|&i| i < effects.len()),
            Value::String(name) => effects.iter().position(|e| {
                e.name.as_deref() == Some(name.as_str())
                    || self
                        .project
                        .scene
                        .as_ref()
                        .is_some_and(|s| *s.effect(e.effect).name == **name)
            }),
            _ => None,
        }
    }

    fn write(&mut self, id: SettingId, v: &Value) -> bool {
        assign(self.project, id, v).is_some()
    }
}

/// The script layer of a created object: its kind and the settings behind its members.
/// `None` for objects scripts do not see (lights, shapes, unknown objects).
fn new_layer(obj: &lumenwall_scene::Object, idx: ObjectIndex, size: Option<Vec2>) -> Option<Layer> {
    // Group copies; the type-specific setting read by the renderer goes first below.
    let (mut scale, mut angles, mut visible) = ([obj.scale; 2], [obj.angles; 2], [obj.visible; 2]);
    let mut l = Layer {
        id: obj.id,
        object: idx,
        kind: LayerKind::Group,
        parent: None,
        origin: obj.origin,
        scale,
        angles,
        visible,
        alpha: None,
        color: None,
        brightness: None,
        parallax_depth: None,
        point_size: None,
        text: None,
        size: Vec2::ZERO,
        scripted: false,
        text_script: false,
    };
    match &obj.kind {
        ObjectKind::Image(i) => {
            l.kind = LayerKind::Image;
            (scale[0], angles[0], visible[0]) = (i.scale, i.angles, i.visible);
            l.alpha = Some(i.alpha);
            l.color = Some(i.color);
            l.brightness = Some(i.brightness);
            l.parallax_depth = Some(i.parallax_depth);
            l.size = size.unwrap_or(i.size);
        }
        ObjectKind::Text(t) => {
            l.kind = LayerKind::Text;
            (scale[0], visible[0]) = (t.scale, t.visible);
            l.alpha = Some(t.alpha);
            l.color = Some(t.color);
            l.point_size = Some(t.point_size);
            l.text = Some(t.text);
            l.size = t.size;
        }
        ObjectKind::Particle(p) => {
            l.kind = LayerKind::Particle;
            (scale[0], angles[0], visible[0]) = (p.scale, p.angles, p.visible);
            l.parallax_depth = Some(p.parallax_depth);
        }
        ObjectKind::Generic(GenericKind::Group) => {}
        // we-doc: sound layers are layers (`thisScene.getLayer('jump').play()` in Wallpaper
        // Engine's Dino Run); their members are never scripted (spec 06 §1.2).
        ObjectKind::Sound(_) => l.kind = LayerKind::Sound,
        // Lights, shapes and unknown objects are not scriptable (spec 06 §1.2).
        _ => return None,
    }
    (l.scale, l.angles, l.visible) = (scale, angles, visible);
    Some(l)
}

/// `ILayer.alignment` text of an anchor (Wallpaper Engine values: `center`, `top`,
/// `bottomleft`…).
fn alignment_name(a: lumenwall_scene::Alignment) -> &'static str {
    match (a.top, a.bottom, a.left, a.right) {
        (true, _, true, _) => "topleft",
        (true, _, _, true) => "topright",
        (true, ..) => "top",
        (_, true, true, _) => "bottomleft",
        (_, true, _, true) => "bottomright",
        (_, true, ..) => "bottom",
        (.., true, _) => "left",
        (.., true) => "right",
        _ => "center",
    }
}

impl HostView<'_> {
    fn image_alignment(&self, idx: ObjectIndex) -> Option<lumenwall_scene::Alignment> {
        match &self.project.scene.as_ref()?.objects.get(idx.index())?.kind {
            ObjectKind::Image(i) => Some(i.alignment),
            _ => None,
        }
    }

    /// Setting a `getAnimation(name)` call on `obj` designates: a layer member, the `visible` or
    /// a constant of an effect, a constant of one of its materials.
    fn animated_setting(&self, obj: ObjectRef, name: &str) -> Option<SettingId> {
        match obj {
            ObjectRef::Layer(id) => {
                let l = self.data.layer(id)?;
                Some(match name {
                    "origin" => l.origin,
                    "scale" => l.scale[0],
                    "angles" => l.angles[0],
                    "visible" => l.visible[0],
                    "alpha" => l.alpha?,
                    "color" => l.color?,
                    "brightness" => l.brightness?,
                    "parallaxDepth" => l.parallax_depth?,
                    "pointsize" => l.point_size?,
                    _ => return None,
                })
            }
            ObjectRef::Effect { layer, effect } => {
                let e = self.image_effects(layer).get(effect as usize)?;
                if name == "visible" {
                    return Some(e.visible);
                }
                (0..e.overrides.len() as u32).find_map(|p| self.constant(layer, effect, p, name))
            }
            ObjectRef::Material {
                layer,
                effect,
                material,
            } => self.constant(layer, effect, material, name),
            _ => None,
        }
    }

    /// Writes `ILayer.alignment` of an image: the anchor of its rectangle.
    fn set_alignment(&mut self, idx: ObjectIndex, value: &Value) -> bool {
        let Value::String(s) = value else {
            return false;
        };
        let a = lumenwall_scene::Alignment::parse(s);
        let Some(ObjectKind::Image(i)) = self
            .project
            .scene
            .as_mut()
            .and_then(|sc| sc.objects.get_mut(idx.index()))
            .map(|o| &mut o.kind)
        else {
            return false;
        };
        if i.alignment != a {
            i.alignment = a;
            if !self.data.aligned.contains(&idx) {
                self.data.aligned.push(idx);
            }
        }
        true
    }
}

impl SceneHost for HostView<'_> {
    fn clock(&self) -> Clock {
        self.data.frame.clock
    }

    fn now(&self) -> Instant {
        self.data.epoch + Duration::from_secs_f64(self.data.frame.time.max(0.0))
    }

    fn cursor(&self) -> Cursor {
        self.data.frame.cursor
    }

    fn display(&self) -> Display {
        self.data.frame.display
    }

    fn audio_spectrum(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let Some((a16, a32, a64)) = self.audio else {
            return false;
        };
        let src: &[f32] = match left.len() {
            16 => a16,
            32 => a32,
            _ => a64,
        };
        let n = src.len().min(left.len());
        // Spectrum arrays are the same for both channels (spec 07 §13.6).
        left[..n].copy_from_slice(&src[..n]);
        let n = src.len().min(right.len());
        right[..n].copy_from_slice(&src[..n]);
        true
    }

    fn get(&self, obj: ObjectRef, name: &str) -> Option<Value> {
        let settings = &self.project.settings;
        match obj {
            ObjectRef::Scene => self
                .scene_setting(name)
                .map(|id| to_script(settings.value(id))),
            ObjectRef::Layer(id) => {
                let l = self.data.layer(id)?;
                let setting = match name {
                    "name" => return Some(Value::String(self.layer_name(l).to_owned())),
                    "size" => {
                        return (l.size != Vec2::ZERO).then(|| Value::Vec2(l.size.to_array()));
                    }
                    "origin" => l.origin,
                    "scale" => l.scale[0],
                    "angles" => l.angles[0],
                    "visible" => l.visible[0],
                    "alpha" => l.alpha?,
                    "color" => return Some(color_to_script(settings.value(l.color?))),
                    "brightness" => l.brightness?,
                    "parallaxDepth" => l.parallax_depth?,
                    "pointsize" => l.point_size?,
                    "text" => l.text?,
                    "alignment" => {
                        let a = self.image_alignment(l.object)?;
                        return Some(Value::String(alignment_name(a).to_owned()));
                    }
                    _ => return None,
                };
                Some(to_script(settings.value(setting)))
            }
            ObjectRef::Effect { layer, effect } => {
                let e = self.image_effects(layer).get(effect as usize)?;
                match member(name) {
                    "visible" => Some(to_script(settings.value(e.visible))),
                    "name" => Some(Value::String(self.effect_name(e))),
                    _ => None,
                }
            }
            ObjectRef::Material {
                layer,
                effect,
                material,
            } => self
                .constant(layer, effect, material, member(name))
                .map(|id| to_script(settings.value(id))),
            ObjectRef::Animation(id) => self.data.anims.get(self.project, id, name),
            _ => None,
        }
    }

    fn set(&mut self, obj: ObjectRef, name: &str, value: &Value) -> bool {
        match obj {
            ObjectRef::Scene => match self.scene_setting(name) {
                Some(id) => self.write(id, value),
                None => false,
            },
            ObjectRef::Layer(id) => {
                let Some(&li) = self.data.by_id.get(&id) else {
                    return false;
                };
                let l = &self.data.layers[li];
                if name == "alignment" {
                    return self.set_alignment(l.object, value);
                }
                let (targets, transform, uniform): ([Option<SettingId>; 2], bool, bool) = match name
                {
                    "origin" => ([Some(l.origin), None], true, false),
                    "scale" => ([Some(l.scale[0]), Some(l.scale[1])], true, false),
                    "angles" => ([Some(l.angles[0]), Some(l.angles[1])], true, false),
                    "visible" => ([Some(l.visible[0]), Some(l.visible[1])], false, false),
                    "alpha" => ([l.alpha, None], false, true),
                    "color" => ([l.color, None], false, true),
                    "brightness" => ([l.brightness, None], false, true),
                    "parallaxDepth" => ([l.parallax_depth, None], false, false),
                    "pointsize" => ([l.point_size, None], false, false),
                    "text" => ([l.text, None], false, false),
                    _ => return false,
                };
                let Some(first) = targets[0] else {
                    return false;
                };
                let written = if name == "color" {
                    assign_color(self.project, first, value)
                } else {
                    assign(self.project, first, value)
                };
                let Some(changed) = written else {
                    return false;
                };
                if let Some(second) = targets[1].filter(|&s| s != first) {
                    assign(self.project, second, value);
                }
                if changed {
                    let c = &mut self.data.changes;
                    c.transforms |= transform;
                    c.uniforms |= uniform;
                    let l = &self.data.layers[li];
                    if name == "text" && !l.text_script && !self.data.text_written.contains(&li) {
                        self.data.text_written.push(li);
                    }
                }
                true
            }
            ObjectRef::Effect { layer, effect } => {
                if member(name) != "visible" {
                    return false;
                }
                let Some(id) = self
                    .image_effects(layer)
                    .get(effect as usize)
                    .map(|e| e.visible)
                else {
                    return false;
                };
                // The renderer reads the setting every frame and switches the chain of the
                // image (`lumenwall-scene` `image_passes_with`); nothing else to flag.
                assign(self.project, id, value).is_some()
            }
            ObjectRef::Material {
                layer,
                effect,
                material,
            } => {
                let Some(id) = self.constant(layer, effect, material, member(name)) else {
                    return false;
                };
                match assign(self.project, id, value) {
                    Some(changed) => {
                        self.data.changes.uniforms |= changed;
                        true
                    }
                    None => false,
                }
            }
            ObjectRef::Animation(id) => self.data.anims.set(id, name, value),
            _ => false,
        }
    }

    fn call(&mut self, obj: ObjectRef, method: &str, args: &[Value]) -> Option<Value> {
        match (obj, method) {
            (ObjectRef::Animation(id), m) => {
                let (v, changed) = self.data.anims.call(self.project, id, m, args);
                if changed {
                    self.data.changes.uniforms = true;
                    self.data.changes.transforms = true;
                }
                v
            }
            (
                obj @ (ObjectRef::Layer(_) | ObjectRef::Effect { .. } | ObjectRef::Material { .. }),
                "getAnimation",
            ) => {
                let Some(Value::String(name)) = args.first() else {
                    return None;
                };
                let setting = self.animated_setting(obj, member(name))?;
                let id = self.data.anims.acquire(self.project, setting)?;
                Some(Value::Object(ObjectRef::Animation(id)))
            }
            (ObjectRef::Scene, "createLayer") => self.create_layer(args.first()?),
            (ObjectRef::Scene, "destroyLayer") => {
                Some(Value::Bool(self.destroy_layer(args.first()?)))
            }
            (ObjectRef::Scene, "sortLayer") => {
                let index = args.get(1).map_or(0, Value::int);
                Some(Value::Bool(self.sort_layer(args.first()?, index)))
            }
            (ObjectRef::Scene, "getInitialLayerConfig") => self.initial_layer_config(args.first()?),
            (ObjectRef::Layer(id), "getEffectCount") => {
                Some(Value::Int(self.image_effects(id).len() as i32))
            }
            (ObjectRef::Layer(id), "getEffect") => {
                let e = self.effect_index(id, args.first()?)?;
                Some(Value::Object(ObjectRef::Effect {
                    layer: id,
                    effect: e as u32,
                }))
            }
            (ObjectRef::Layer(id), "getParent") => {
                let l = self.data.layer(id)?;
                let p = l.parent?;
                Some(Value::Object(ObjectRef::Layer(self.data.layers[p].id)))
            }
            (ObjectRef::Layer(id), "getChildren") => {
                let li = *self.data.by_id.get(&id)?;
                let by_id = &self.data.by_id;
                Some(Value::Array(
                    self.data
                        .order
                        .iter()
                        .map(|&i| &self.data.layers[i])
                        .filter(|l| l.parent == Some(li) && by_id.contains_key(&l.id))
                        .map(|l| Value::Object(ObjectRef::Layer(l.id)))
                        .collect(),
                ))
            }
            (ObjectRef::Effect { layer, effect }, "getMaterialCount") => {
                let e = self.image_effects(layer).get(effect as usize)?;
                Some(Value::Int(e.overrides.len() as i32))
            }
            (ObjectRef::Effect { layer, effect }, "getMaterial") => {
                let i = args.first().map_or(0, Value::int);
                let n = self
                    .image_effects(layer)
                    .get(effect as usize)?
                    .overrides
                    .len();
                let i = u32::try_from(i).ok().filter(|&i| (i as usize) < n)?;
                Some(Value::Object(ObjectRef::Material {
                    layer,
                    effect,
                    material: i,
                }))
            }
            (ObjectRef::Effect { layer, effect }, "setMaterialProperty") => {
                let Value::String(name) = args.first()? else {
                    return Some(Value::Undefined);
                };
                let value = args.get(1)?;
                let passes = self
                    .image_effects(layer)
                    .get(effect as usize)?
                    .overrides
                    .len();
                for pass in 0..passes as u32 {
                    if let Some(id) = self.constant(layer, effect, pass, name)
                        && assign(self.project, id, value) == Some(true)
                    {
                        self.data.changes.uniforms = true;
                    }
                }
                Some(Value::Undefined)
            }
            _ => None,
        }
    }

    fn layer_exists(&self, id: LayerId) -> bool {
        self.data.by_id.contains_key(&id)
    }

    fn find_layer(&self, name: &str) -> Option<LayerId> {
        self.data
            .order
            .iter()
            .map(|&i| &self.data.layers[i])
            .find(|l| self.layer_name(l) == name)
            .map(|l| l.id)
    }

    fn layer_count(&self) -> usize {
        self.data.order.len()
    }

    fn layer_at(&self, index: usize) -> Option<LayerId> {
        self.data.order.get(index).map(|&i| self.data.layers[i].id)
    }

    fn register_asset(&mut self, path: &str, _precache: bool) -> Option<u32> {
        // Registered assets are prepared at once (the first `createLayer` reuses it).
        self.register(path)
    }

    fn script_property(&self, binding: u64, name: &str) -> Option<Value> {
        let script = self.project.settings.scripts().get(binding as usize)?;
        let strings = &self.project.strings;
        script
            .properties
            .iter()
            .find(|(n, _)| strings.resolve(*n) == name)
            .map(|(_, id)| to_script(self.project.settings.value(*id)))
    }

    fn set_script_property(&mut self, binding: u64, name: &str, value: &Value) -> bool {
        let Some(script) = self.project.settings.scripts().get(binding as usize) else {
            return false;
        };
        let strings = &self.project.strings;
        let Some(id) = script
            .properties
            .iter()
            .find(|(n, _)| strings.resolve(*n) == name)
            .map(|(_, id)| *id)
        else {
            return false;
        };
        self.write(id, value)
    }
}

/// A text object whose `text` carries a script (spec 06 §9).
struct TextLayer {
    id: i32,
    script: ScriptId,
    visible: Box<[SettingId]>,
}

/// Queued events, dispatched at the start of the next frame (spec 06 §12 step 2).
#[derive(Default)]
struct PendingEvents {
    properties: Vec<(String, Value)>,
    media: Option<MediaSnapshot>,
    media_enabled: Option<bool>,
}

/// Cursor state used to derive `cursor*` events.
#[derive(Default)]
struct CursorTrack {
    world: Option<Vec2>,
    left: bool,
    hovered: Option<usize>,
    pressed_on: Option<usize>,
}

/// The scripts of one scene.
pub(crate) struct SceneScripts {
    engine: ScriptEngine,
    data: HostData,
    texts: Vec<TextLayer>,
    sink: Option<Box<dyn TextSink>>,
    pending: PendingEvents,
    cursor: CursorTrack,
    /// `engine.screenResolution` of the previous frame (`resizeScreen`).
    screen: (u32, u32),
    /// The scene draws something that changes over time without scripts.
    pub base_animated: bool,
    ticks: u64,
    tick_time: Duration,
}

/// A script found on a scriptable member, owned so that the project can be lent to the host.
struct Found {
    layer: LayerId,
    object: Option<ObjectRef>,
    property: String,
    source: String,
    binding: u64,
    names: Vec<String>,
}

fn user_value(p: &lumenwall_scene::Property) -> Value {
    match p.kind() {
        // lumenwall-script: colors are handed to scripts as Vec3.
        PropertyKind::Color => Value::Vec3(p.value().vec3().to_array()),
        _ => to_script(p.value()),
    }
}

impl SceneScripts {
    /// Loads every script of the created scriptable objects (spec 06 §1.2, §8.1, §9.1), in
    /// creation order. `None` when the scene has none: no runtime is created.
    pub(crate) fn load(
        project: &mut Project,
        build: &SceneBuild,
        image_sizes: &[(ObjectIndex, Vec2)],
        scene_size: Vec2,
        mut dynamic: Option<Box<Dynamic>>,
    ) -> Option<Box<Self>> {
        if project.settings.scripted().is_empty() {
            return None;
        }
        let scene = project.scene.as_ref()?;
        let settings = &project.settings;
        let script_of = |id: SettingId| settings.meta(id).script;

        // Layers in render order.
        let mut layers = Vec::new();
        let mut by_id = HashMap::new();
        for &idx in &build.render_order {
            if !build.created.get(idx.index()).copied().unwrap_or(false)
                || build.bloom.is_some_and(|b| b.object == idx)
            {
                continue;
            }
            let size = image_sizes.iter().find(|(o, _)| *o == idx).map(|(_, s)| *s);
            let Some(l) = new_layer(scene.object(idx), idx, size) else {
                continue;
            };
            by_id.entry(l.id).or_insert(layers.len());
            layers.push(l);
        }
        if let Some(d) = &mut dynamic {
            d.first_id(scene.objects.iter().map(|o| o.id).max().unwrap_or(0));
        }
        for l in &mut layers {
            let parent = scene.object(l.object).parent;
            l.parent = parent.and_then(|p| by_id.get(&p).copied());
        }

        // Scripts on registered members, in creation order (spec 06 §1.2, §8.1).
        let source = |sid: lumenwall_scene::ScriptId| -> (String, Vec<String>, u64) {
            let s = settings.script(sid);
            let names = s
                .properties
                .iter()
                .map(|(n, _)| project.strings.resolve(*n).to_owned())
                .collect();
            (s.source.to_string(), names, sid.index() as u64)
        };
        let mut found = Vec::new();
        let mut texts_found = Vec::new();
        for &idx in &build.creation_order {
            let Some(li) = layers.iter().position(|l| l.object == idx) else {
                continue;
            };
            let l = &layers[li];
            if l.kind == LayerKind::Sound {
                continue;
            }
            let mut add = |member: &str, ids: &[SettingId], object: Option<ObjectRef>| {
                if let Some(sid) = ids.iter().find_map(|&id| script_of(id)) {
                    let (src, names, binding) = source(sid);
                    found.push((
                        li,
                        Found {
                            layer: l.id,
                            object,
                            property: member.to_owned(),
                            source: src,
                            binding,
                            names,
                        },
                    ));
                }
            };
            add("origin", &[l.origin], None);
            add("scale", &l.scale, None);
            add("angles", &l.angles, None);
            add("visible", &l.visible, None);
            match &scene.object(idx).kind {
                ObjectKind::Image(i) => {
                    add("alpha", &[i.alpha], None);
                    add("color", &[i.color], None);
                    add("parallaxDepth", &[i.parallax_depth], None);
                    add("brightness", &[i.brightness], None);
                    for (e, fx) in i.effects.iter().enumerate() {
                        let e = e as u32;
                        let effect = ObjectRef::Effect {
                            layer: l.id,
                            effect: e,
                        };
                        add(&format!("visible@effect{e}"), &[fx.visible], Some(effect));
                        for (p, o) in fx.overrides.iter().enumerate() {
                            let material = ObjectRef::Material {
                                layer: l.id,
                                effect: e,
                                material: p as u32,
                            };
                            for c in &o.constants {
                                let name = project.strings.resolve(c.name);
                                add(
                                    &format!("{name}@effect{e}.pass{p}"),
                                    &[c.setting],
                                    Some(material),
                                );
                            }
                        }
                    }
                }
                ObjectKind::Text(t) => {
                    add("color", &[t.color], None);
                    add("alpha", &[t.alpha], None);
                    add("pointsize", &[t.point_size], None);
                    // Spec 06 F2: the text itself is a dynamic text layer (M2), not M1.
                    if let Some(sid) = script_of(t.text) {
                        let (src, names, binding) = source(sid);
                        let initial = settings.value(t.text).text().to_owned();
                        texts_found.push((li, src, names, binding, initial));
                    }
                }
                ObjectKind::Particle(p) => add("parallaxDepth", &[p.parallax_depth], None),
                _ => {}
            }
        }
        if found.is_empty() && texts_found.is_empty() {
            tracing::debug!("scene scripts are on members that never run (spec 06 §1.3)");
            return None;
        }
        for (li, _) in &found {
            layers[*li].scripted = true;
        }
        for (li, ..) in &texts_found {
            layers[*li].text_script = true;
        }

        let user_properties = project
            .properties
            .iter()
            .map(|(_, p)| (p.name().to_owned(), user_value(p)))
            .collect();
        let engine = match ScriptEngine::new(ScriptConfig {
            user_properties,
            ..ScriptConfig::default()
        }) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("scripts disabled: {e}");
                return None;
            }
        };
        let order = (0..layers.len()).collect();
        let mut s = Box::new(SceneScripts {
            engine,
            data: HostData {
                layers,
                by_id,
                order,
                dynamic,
                aligned: Vec::new(),
                frame: FrameData {
                    display: Display {
                        screen_resolution: scene_size.to_array(),
                        canvas_size: scene_size.to_array(),
                        ..Display::default()
                    },
                    ..FrameData::default()
                },
                epoch: Instant::now(),
                // Values written at load reach the renderer on the first frame.
                changes: Changes {
                    transforms: true,
                    uniforms: true,
                },
                text_written: Vec::new(),
                anims: animation::Animations::default(),
            },
            texts: Vec::new(),
            sink: None,
            pending: PendingEvents::default(),
            cursor: CursorTrack::default(),
            screen: (0, 0),
            base_animated: false,
            ticks: 0,
            tick_time: Duration::ZERO,
        });
        let (mut loaded, mut failed) = (0usize, 0usize);
        for (_, f) in &found {
            let names: Vec<&str> = f.names.iter().map(String::as_str).collect();
            let mut host = HostView {
                data: &mut s.data,
                project,
                audio: None,
            };
            let r = s.engine.load_property_script(
                &mut host,
                &PropertyScript {
                    layer: f.layer,
                    property: &f.property,
                    object: f.object,
                    source: &f.source,
                    binding: f.binding,
                    script_properties: &names,
                },
            );
            match r {
                Ok(_) => loaded += 1,
                // The failure itself is logged by lumenwall-script.
                Err(lumenwall_script::ScriptError::Failed { .. }) => failed += 1,
                Err(e) => tracing::debug!("{e}"),
            }
        }
        for (li, src, names, binding, initial) in &texts_found {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let id = s.data.layers[*li].id;
            let mut host = HostView {
                data: &mut s.data,
                project,
                audio: None,
            };
            let r = s.engine.create_text_script(
                &mut host,
                &TextScript {
                    layer: id,
                    source: src,
                    initial_text: initial,
                    binding: *binding,
                    script_properties: &names,
                },
            );
            match r {
                Ok(script) => {
                    loaded += 1;
                    let object = s.data.layers[*li].object;
                    s.texts.push(TextLayer {
                        id,
                        script,
                        visible: visibility_chain(project, build, object),
                    });
                }
                Err(e) => {
                    // Spec 06 §9.4: the text stays at its initial value.
                    failed += 1;
                    let object = s.data.layers[*li].object;
                    let name = project
                        .scene
                        .as_ref()
                        .map_or("", |sc| project.strings.resolve(sc.object(object).name));
                    tracing::error!("text object {id} '{name}': dynamic text not created: {e}");
                }
            }
        }
        tracing::info!(
            "scripts: {loaded} loaded, {failed} failed, {} per-frame update(s), {} text layer(s)",
            s.engine.stats().per_frame,
            s.texts.len()
        );
        Some(s)
    }

    /// Whether frames must keep coming for scripts (updates, timers, audio buffers, text
    /// layers, queued events).
    pub(crate) fn needs_frames(&self) -> bool {
        self.engine.has_work()
            || self.data.anims.playing()
            || !self.texts.is_empty()
            || !self.pending.properties.is_empty()
            || self.pending.media.is_some()
            || self.pending.media_enabled.is_some()
    }

    pub(crate) fn set_text_sink(&mut self, sink: Box<dyn TextSink>) {
        self.sink = Some(sink);
    }

    /// Current text of a dynamic text layer.
    pub(crate) fn text(&self, object: i32) -> Option<Ref<'_, str>> {
        let t = self.texts.iter().find(|t| t.id == object)?;
        self.engine.text(t.script)
    }

    pub(crate) fn stats(&self) -> ScriptStats {
        self.engine.stats()
    }

    /// Mean wall time of [`SceneScripts::tick`].
    pub(crate) fn mean_tick(&self) -> Duration {
        self.tick_time / u32::try_from(self.ticks.max(1)).unwrap_or(u32::MAX)
    }

    /// A user property changed: `applyUserProperties` at the next frame.
    pub(crate) fn property_changed(&mut self, project: &Project, name: &str) {
        let Some(id) = project.property(name) else {
            return;
        };
        let v = user_value(project.properties.get(id));
        match self.pending.properties.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = v,
            None => self.pending.properties.push((name.to_owned(), v)),
        }
    }

    /// Media metadata or artwork changed: the four media events at the next frame.
    pub(crate) fn media_changed(&mut self, media: MediaSnapshot) {
        self.pending.media = Some(media);
    }

    /// A media source appeared or vanished: `mediaStatusChanged` at the next frame.
    pub(crate) fn media_status(&mut self, enabled: bool) {
        self.pending.media_enabled = Some(enabled);
    }

    /// One frame of script work (spec 06 §12): queued events, cursor events, timers and
    /// `update`s, then the visible text layers. Returns what scripts changed.
    pub(crate) fn tick(
        &mut self,
        project: &mut Project,
        build: &SceneBuild,
        f: &FrameState<'_>,
        scene_size: Vec2,
    ) -> Changes {
        let started = Instant::now();
        let world = f.pointer.normalized * scene_size;
        let screen = f.screen.size;
        self.data.frame = FrameData {
            clock: Clock {
                runtime: f.time as f32,
                frametime: f.dt,
                time_of_day: f.daytime,
            },
            cursor: Cursor {
                screen_position: f.screen.cursor.to_array(),
                world_position: [world.x, world.y, 0.0],
                left_down: f.screen.left_down,
            },
            display: Display {
                screen_resolution: if screen.0 > 0 && screen.1 > 0 {
                    [screen.0 as f32, screen.1 as f32]
                } else {
                    scene_size.to_array()
                },
                canvas_size: scene_size.to_array(),
                ..Display::default()
            },
            time: f.time,
        };
        let Self {
            engine,
            data,
            texts,
            sink,
            pending,
            cursor,
            screen: last_screen,
            ..
        } = self;
        let mut host = HostView {
            data,
            project,
            audio: Some((f.audio16, f.audio32, f.audio64)),
        };

        if screen != *last_screen && screen.0 > 0 && screen.1 > 0 {
            if *last_screen != (0, 0) {
                engine.resize_screen(&mut host, [screen.0 as f32, screen.1 as f32]);
            }
            *last_screen = screen;
        }
        if !pending.properties.is_empty() {
            engine.apply_user_properties(&mut host, &pending.properties);
            pending.properties.clear();
        }
        if let Some(enabled) = pending.media_enabled.take() {
            engine.media_status_changed(&mut host, enabled);
        }
        if let Some(media) = pending.media.take() {
            engine.media_changed(&mut host, &media);
        }
        cursor_events(engine, &mut host, cursor, build, world, f.screen.left_down);
        engine.tick(&mut host);
        // Timelines scripts play: values of this frame (a `play()` above starts at 0).
        if host.data.anims.advance(host.project, f.dt.max(0.0)) {
            host.data.changes.uniforms = true;
            host.data.changes.transforms = true;
        }

        // Text layers tick only while visible (spec 06 §9.5).
        for t in texts.iter() {
            if !chain_visible(&host.project.settings, &t.visible) {
                continue;
            }
            if engine.tick_text(&mut host, t.script)
                && let Some(text) = engine.text(t.script)
            {
                tracing::debug!("text object {}: {:?}", t.id, &*text);
                if let Some(sink) = sink.as_mut() {
                    sink.on_text_changed(t.id, &text);
                }
            }
        }
        // Destroyed layers (spec: deferred removal): `destroy()` of their scripts, their JS
        // objects dropped. A `destroy()` may destroy more.
        for _ in 0..8 {
            let ids = match host.data.dynamic.as_mut() {
                Some(d) => d.take_removed_ids(),
                None => break,
            };
            if ids.is_empty() {
                break;
            }
            for id in ids {
                engine.remove_layer(&mut host, id);
            }
        }
        if !host.data.text_written.is_empty() {
            for li in host.data.text_written.drain(..) {
                let l = &host.data.layers[li];
                if let (Some(sink), Some(id)) = (sink.as_mut(), l.text) {
                    sink.on_text_changed(l.id, host.project.settings.value(id).text());
                }
            }
        }
        let changes = std::mem::take(&mut host.data.changes);
        self.ticks += 1;
        self.tick_time += started.elapsed();
        changes
    }

    /// Layers created, destroyed or reordered by scripts since the last call.
    pub(crate) fn take_structure(&mut self) -> Option<Structure> {
        self.data.dynamic.as_mut()?.take_structure()
    }

    /// Image objects whose anchor scripts changed since the last call.
    pub(crate) fn take_aligned(&mut self) -> Vec<ObjectIndex> {
        std::mem::take(&mut self.data.aligned)
    }

    /// Objects of the live layers, in draw order.
    pub(crate) fn render_objects(&self) -> impl Iterator<Item = ObjectIndex> + '_ {
        self.data.order.iter().map(|&i| self.data.layers[i].object)
    }

    /// Whether `idx` is the object of a live layer.
    pub(crate) fn is_live(&self, idx: ObjectIndex) -> bool {
        let slot = self
            .data
            .dynamic
            .as_ref()
            .and_then(|d| d.slot_of(idx))
            .or_else(|| self.data.layers.iter().position(|l| l.object == idx));
        slot.is_some_and(|s| self.data.by_id.get(&self.data.layers[s].id) == Some(&s))
    }

    /// Whether `idx` is listed as a layer (live or destroyed): scripts decide its place.
    pub(crate) fn lists(&self, idx: ObjectIndex) -> bool {
        self.data.layers.iter().any(|l| l.object == idx)
    }

    /// Drawn size of a created image layer, once the renderer set it up (`ILayer.size`, hit
    /// tests).
    pub(crate) fn set_layer_size(&mut self, idx: ObjectIndex, size: Vec2) {
        let slot = self.data.dynamic.as_ref().and_then(|d| d.slot_of(idx));
        if let Some(l) = slot.and_then(|s| self.data.layers.get_mut(s)) {
            l.size = size;
        }
    }

    /// The created-layer side of the host, when the scene's scripts use it.
    pub(crate) fn dynamic(&self) -> Option<&Dynamic> {
        self.data.dynamic.as_deref()
    }

    /// Calls `destroy()` of every script and flushes `localStorage`.
    pub(crate) fn shutdown(&mut self, project: &mut Project) {
        let mut host = HostView {
            data: &mut self.data,
            project,
            audio: None,
        };
        self.engine.shutdown(&mut host);
    }
}

/// Topmost visible image or text layer under the cursor (last drawn first) whose scripts
/// handle cursor events, with the cursor in its local, unscaled pixels from its centre. The
/// alignment is not applied. Layers without cursor handlers do not catch the cursor: Wallpaper
/// Engine's Dino Run draws a full-screen post-process layer (scripted through its effect)
/// over the dinosaur that receives the clicks.
fn hit_test(
    engine: &ScriptEngine,
    host: &HostView<'_>,
    build: &SceneBuild,
    world: Vec2,
) -> Option<(usize, Vec2)> {
    let settings = &host.project.settings;
    for &i in host.data.order.iter().rev() {
        let l = &host.data.layers[i];
        if !l.scripted
            || !matches!(l.kind, LayerKind::Image | LayerKind::Text)
            || l.size == Vec2::ZERO
            || !settings.value(l.visible[0]).boolean()
        {
            continue;
        }
        let t = world_transform(host.project, build, l.object);
        let d = world - t.origin.truncate();
        let (s, c) = (-t.angle).sin_cos();
        let local = Vec2::new(d.x * c - d.y * s, d.x * s + d.y * c);
        let scale = t.scale.truncate();
        if scale.x == 0.0 || scale.y == 0.0 {
            continue;
        }
        let local = local / scale;
        let half = l.size * 0.5;
        if local.x.abs() <= half.x && local.y.abs() <= half.y && engine.handles_cursor(l.id) {
            return Some((i, local));
        }
    }
    None
}

/// Derives `cursorEnter/Leave/Move/Down/Up/Click` from the cursor of this frame. The layer under
/// the cursor receives them; nothing runs while the cursor and the button are still.
fn cursor_events(
    engine: &mut ScriptEngine,
    host: &mut HostView<'_>,
    track: &mut CursorTrack,
    build: &SceneBuild,
    world: Vec2,
    left: bool,
) {
    let moved = track.world != Some(world);
    if !moved && left == track.left {
        return;
    }
    let hit = hit_test(engine, host, build, world);
    let hovered = hit.map(|(i, _)| i);
    let event = CursorEvent {
        world_position: [world.x, world.y, 0.0],
        local_position: hit.map_or([0.0; 3], |(_, p)| [p.x, p.y, 0.0]),
        hit_box: None,
    };
    let send = |engine: &mut ScriptEngine, host: &mut HostView<'_>, kind, i: usize| {
        let layer = host.data.layers[i].id;
        engine.cursor_event(host, kind, Some(layer), &event);
    };
    if hovered != track.hovered {
        if let Some(old) = track.hovered {
            send(engine, host, CursorEventKind::Leave, old);
        }
        if let Some(new) = hovered {
            send(engine, host, CursorEventKind::Enter, new);
        }
    }
    if let Some(h) = hovered {
        if moved && track.world.is_some() {
            send(engine, host, CursorEventKind::Move, h);
        }
        if left && !track.left {
            send(engine, host, CursorEventKind::Down, h);
        }
        if !left && track.left {
            send(engine, host, CursorEventKind::Up, h);
            if track.pressed_on == Some(h) {
                send(engine, host, CursorEventKind::Click, h);
            }
        }
    }
    if left && !track.left {
        track.pressed_on = hovered;
    } else if !left {
        track.pressed_on = None;
    }
    track.world = Some(world);
    track.left = left;
    track.hovered = hovered;
}

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use lumenwall_render::math::Pointer;
    use lumenwall_scene::{BuildOptions, PropertyOverrides, load_project};

    use super::*;
    use crate::scene::ScreenState;

    struct Counting;

    thread_local! {
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
    }

    // SAFETY: forwards to the system allocator; only adds a const-initialised thread-local
    // counter (reading it never allocates).
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCS.with(|c| c.set(c.get() + 1));
            // SAFETY: same contract as the caller's.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: same contract as the caller's.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            ALLOCS.with(|c| c.set(c.get() + 1));
            // SAFETY: same contract as the caller's.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

    const PROJECT: &str = r#"{ "type": "scene", "title": "t", "file": "scene.json",
        "general": { "properties": { "flip": { "type": "bool", "text": "Flip", "value": false } } } }"#;
    const CAMERA: &str = r#""camera": { "center": "0 0 -1", "eye": "0 0 0", "up": "0 1 0" }"#;

    fn json_str(s: &str) -> String {
        serde_json::to_string(s).unwrap()
    }

    /// An image object with `extra` JSON members.
    pub(super) fn image(id: i32, name: &str, extra: &str) -> String {
        format!(
            r#"{{ "id": {id}, "name": "{name}", "image": "models/m.json", "origin": "100 100 0", "size": "200 100"{extra} }}"#
        )
    }

    pub(super) fn scripted(value: &str, source: &str) -> String {
        format!(r#"{{ "value": {value}, "script": {} }}"#, json_str(source))
    }

    pub(super) fn load(objects: &[String]) -> (Project, SceneBuild) {
        let scene = format!(
            r#"{{ {CAMERA}, "general": {{ "orthogonalprojection": {{ "width": 1920, "height": 1080 }} }}, "objects": [ {} ] }}"#,
            objects.join(", ")
        );
        let files: HashMap<String, Vec<u8>> = [
            ("project.json", PROJECT),
            ("scene.json", scene.as_str()),
            ("models/m.json", r#"{ "material": "materials/m.json" }"#),
            (
                "materials/m.json",
                r#"{ "passes": [ { "shader": "genericimage2" } ] }"#,
            ),
            (
                "effects/e/effect.json",
                r#"{ "name": "fx", "passes": [ { "material": "materials/fx.json" } ] }"#,
            ),
            (
                "materials/fx.json",
                r#"{ "passes": [ { "shader": "fx" } ] }"#,
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.as_bytes().to_vec()))
        .collect();
        let mut p = load_project(&files, PropertyOverrides::new()).expect("project");
        let b = p
            .build(&files, &BuildOptions::new(1920, 1080))
            .expect("build");
        (p, b)
    }

    pub(super) fn scripts(p: &mut Project, b: &SceneBuild) -> Box<SceneScripts> {
        SceneScripts::load(p, b, &[], Vec2::new(1920.0, 1080.0), None).expect("scripts")
    }

    /// Scripts with created layers enabled, on an in-memory VFS holding the test model.
    fn dynamic_scripts(p: &mut Project, b: &SceneBuild) -> Box<SceneScripts> {
        let mut files = lumenwall_assets::MemoryFiles::default();
        files.insert_static("models/m.json", br#"{ "material": "materials/m.json" }"#);
        files.insert_static(
            "materials/m.json",
            br#"{ "passes": [ { "shader": "genericimage2" } ] }"#,
        );
        files.insert_static(
            "models/workshop/77/bar.json",
            br#"{ "material": "materials/m.json" }"#,
        );
        let mut vfs = lumenwall_assets::Vfs::new();
        vfs.mount_adapter("/", lumenwall_assets::Adapter::Memory(files));
        let ids = workshop_ids(p.settings.scripts().iter().map(|s| &*s.source));
        let d = Dynamic::new(
            std::sync::Arc::new(vfs),
            ids,
            HashMap::new(),
            BuildOptions::new(1920, 1080),
        );
        SceneScripts::load(p, b, &[], Vec2::new(1920.0, 1080.0), Some(Box::new(d)))
            .expect("scripts")
    }

    fn eval(s: &mut SceneScripts, p: &mut Project, src: &str) -> Value {
        let mut host = HostView {
            data: &mut s.data,
            project: p,
            audio: None,
        };
        s.engine.eval(&mut host, src).expect("eval")
    }

    struct Audio([f32; 16], [f32; 32], [f32; 64]);

    fn frame(t: f64, a: &Audio) -> FrameState<'_> {
        FrameState {
            time: t,
            dt: 1.0 / 30.0,
            daytime: 0.5,
            pointer: Pointer {
                position: Vec2::splat(0.5),
                normalized: Vec2::splat(0.5),
            },
            audio16: &a.0,
            audio32: &a.1,
            audio64: &a.2,
            screen: ScreenState {
                size: (1920, 1080),
                cursor: Vec2::ZERO,
                left_down: false,
            },
        }
    }

    pub(super) fn tick(s: &mut SceneScripts, p: &mut Project, b: &SceneBuild, t: f64) -> Changes {
        let a = Audio([0.0; 16], [0.0; 32], [0.0; 64]);
        s.tick(p, b, &frame(t, &a), Vec2::new(1920.0, 1080.0))
    }

    fn object(p: &Project, i: usize) -> &lumenwall_scene::Object {
        &p.scene.as_ref().unwrap().objects[i]
    }

    pub(super) fn img(p: &Project, i: usize) -> &lumenwall_scene::ImageObject {
        object(p, i).as_image().unwrap()
    }

    #[test]
    fn a_scene_without_scripts_creates_no_runtime() {
        let (mut p, b) = load(&[image(1, "a", "")]);
        assert!(SceneScripts::load(&mut p, &b, &[], Vec2::ONE, None).is_none());
    }

    #[test]
    fn updates_write_settings_and_report_what_changed() {
        let (mut p, b) = load(&[image(
            1,
            "a",
            &format!(
                r#", "origin": {}, "alpha": {}"#,
                scripted(
                    r#""100 100 0""#,
                    "export function update(v) { v.x += 10; return v; }"
                ),
                scripted("1", "export function update() { return 0.5; }"),
            ),
        )]);
        let mut s = scripts(&mut p, &b);
        assert!(s.needs_frames());
        let c = tick(&mut s, &mut p, &b, 0.0);
        assert!(c.transforms && c.uniforms);
        let (origin, alpha) = (object(&p, 0).origin, img(&p, 0).alpha);
        assert_eq!(
            p.settings.value(origin).vec3(),
            Vec3::new(110.0, 100.0, 0.0)
        );
        assert_eq!(p.settings.value(alpha).float(), 0.5);
        // The alpha script keeps returning 0.5: only the transform changes.
        let c = tick(&mut s, &mut p, &b, 0.1);
        assert_eq!(
            c,
            Changes {
                transforms: true,
                uniforms: false
            }
        );
        assert_eq!(p.settings.value(origin).vec3().x, 120.0);
    }

    #[test]
    fn a_script_returning_the_same_value_changes_nothing() {
        let (mut p, b) = load(&[image(
            1,
            "a",
            &format!(
                r#", "scale": {}"#,
                scripted(
                    r#""2 2 1""#,
                    "let d; export function init(v) { d = v; } export function update() { return d; }"
                )
            ),
        )]);
        let mut s = scripts(&mut p, &b);
        tick(&mut s, &mut p, &b, 0.0);
        for i in 1..5 {
            assert_eq!(tick(&mut s, &mut p, &b, f64::from(i)), Changes::default());
        }
        // Both copies of `scale` (image and group) carry the value.
        assert_eq!(
            p.settings.value(img(&p, 0).scale).vec3(),
            Vec3::new(2.0, 2.0, 1.0)
        );
    }

    #[test]
    fn user_property_changes_reach_apply_user_properties() {
        let src = "let flip = false;
            export function applyUserProperties(c) { if (c.hasOwnProperty('flip')) flip = c.flip; }
            export function update(v) { return new Vec3(flip ? -1 : 1, 1, 1); }";
        let (mut p, b) = load(&[image(
            1,
            "a",
            &format!(r#", "scale": {}"#, scripted(r#""1 1 1""#, src)),
        )]);
        let mut s = scripts(&mut p, &b);
        tick(&mut s, &mut p, &b, 0.0);
        let scale = img(&p, 0).scale;
        assert_eq!(p.settings.value(scale).vec3().x, 1.0);
        p.set_property_text("flip", "1").unwrap();
        s.property_changed(&p, "flip");
        assert!(s.needs_frames());
        let c = tick(&mut s, &mut p, &b, 0.1);
        assert!(c.transforms);
        assert_eq!(p.settings.value(scale).vec3().x, -1.0);
    }

    #[test]
    fn layers_are_found_by_name_and_written_through_the_host() {
        let driver = "export function update() {
                const t = thisScene.getLayer('target');
                t.alpha = 0.25;
                t.color = new Vec3(1, 0, 0);
                t.visible = thisScene.getLayer('missing') === null;
                return true;
            }";
        let (mut p, b) = load(&[
            image(1, "target", ""),
            image(
                2,
                "driver",
                &format!(r#", "visible": {}"#, scripted("true", driver)),
            ),
        ]);
        let mut s = scripts(&mut p, &b);
        let c = tick(&mut s, &mut p, &b, 0.0);
        assert!(c.uniforms);
        let t = img(&p, 0);
        assert_eq!(p.settings.value(t.alpha).float(), 0.25);
        // we-doc: a Vec3 colour keeps the stored alpha.
        assert_eq!(
            p.settings.value(t.color).vec4(),
            glam::Vec4::new(1.0, 0.0, 0.0, 1.0)
        );
        assert!(p.settings.value(t.visible).boolean());
    }

    #[test]
    fn effect_visibility_and_material_constants_are_scriptable() {
        let effect = format!(
            r#", "effects": [ {{ "file": "effects/e/effect.json", "visible": {}, "passes": [ {{ "constantshadervalues": {{ "multiply": {} }} }} ] }} ]"#,
            scripted("true", "export function update() { return false; }"),
            scripted(
                "1",
                "export function update(v) { return thisObject.getAnimation() === undefined ? v + 1 : 0; }"
            ),
        );
        let (mut p, b) = load(&[image(1, "a", &effect)]);
        let mut s = scripts(&mut p, &b);
        assert_eq!(s.stats().per_frame, 2);
        let c = tick(&mut s, &mut p, &b, 0.0);
        assert!(c.uniforms);
        let e = &img(&p, 0).effects[0];
        assert!(!p.settings.value(e.visible).boolean());
        assert_eq!(
            p.settings
                .value(e.overrides[0].constants[0].setting)
                .float(),
            2.0
        );
    }

    struct Sink(Rc<RefCell<Vec<(i32, String)>>>);

    impl TextSink for Sink {
        fn on_text_changed(&mut self, object: i32, text: &str) {
            self.0.borrow_mut().push((object, text.to_owned()));
        }
    }

    #[test]
    fn text_layers_feed_the_text_sink_while_visible() {
        let text = |id: i32, visible: bool| {
            format!(
                r#"{{ "id": {id}, "name": "t{id}", "font": "", "visible": {visible}, "text": {} }}"#,
                scripted(
                    r#""start""#,
                    "'use strict';\nlet n = 0;\nexport function update(value) { return 'n' + (++n); }"
                )
            )
        };
        let (mut p, b) = load(&[text(5, true), text(6, false)]);
        let mut s = scripts(&mut p, &b);
        let got = Rc::new(RefCell::new(Vec::new()));
        s.set_text_sink(Box::new(Sink(got.clone())));
        assert_eq!(s.text(5).as_deref(), Some("start"));
        tick(&mut s, &mut p, &b, 0.0);
        tick(&mut s, &mut p, &b, 0.1);
        assert_eq!(
            *got.borrow(),
            [(5, "n1".to_owned()), (5, "n2".to_owned())],
            "the hidden text layer does not run"
        );
        // Spec 06 §9.6: the `text` setting is not modified.
        let ObjectKind::Text(t) = &object(&p, 0).kind else {
            panic!("text object");
        };
        assert_eq!(p.settings.value(t.text).text(), "start");
    }

    #[test]
    fn world_transforms_match_the_scene_model() {
        let (p, b) = load(&[
            r#"{ "id": 1, "name": "g", "origin": "10 20 0", "scale": "2 2 1", "angles": "0 0 0.5" }"#
                .to_owned(),
            image(2, "child", r#", "parent": 1, "angles": "0 0 0.25""#),
        ]);
        let idx = b.created_object(2).unwrap();
        let ours = world_transform(&p, &b, idx);
        let theirs = p.world_transform(idx, &b);
        assert_eq!(ours, theirs);
        assert_ne!(ours.origin, Vec3::new(100.0, 100.0, 0.0));
    }

    #[test]
    fn created_layers_are_listed_sorted_destroyed_and_reused() {
        let (mut p, b) = load(&[
            image(
                1,
                "a",
                &format!(
                    r#", "visible": {}"#,
                    scripted(
                        "true",
                        "export let __workshopId = '77';
                         let asset = engine.registerAsset('models/m.json');
                         export function init() {
                             shared.n0 = thisScene.getLayerCount();
                             let l = thisScene.createLayer(asset);
                             shared.n1 = thisScene.getLayerCount();
                             shared.last = thisScene.getLayerIndex(l);
                             thisScene.sortLayer(l, 0);
                             shared.first = thisScene.getLayerIndex(l);
                             l.alignment = 'bottomleft';
                             shared.alignment = l.alignment;
                             l.origin = new Vec3(5, 6, 0);
                             shared.layer = l;
                             shared.bar = thisScene.createLayer('models/bar.json');
                             shared.config = thisScene.getInitialLayerConfig(l).image;
                             shared.sound = thisScene.getLayer('snd') !== null;
                         }"
                    )
                ),
            ),
            image(2, "b", ""),
            r#"{ "id": 3, "name": "snd", "sound": ["sounds/x.mp3"] }"#.to_owned(),
        ]);
        let mut s = dynamic_scripts(&mut p, &b);
        // Sounds are layers (Dino Run's `getLayer('jump').play()`).
        assert_eq!(eval(&mut s, &mut p, "shared.n0"), Value::Int(3));
        assert_eq!(eval(&mut s, &mut p, "shared.n1"), Value::Int(4));
        assert_eq!(eval(&mut s, &mut p, "shared.last"), Value::Int(3));
        assert_eq!(eval(&mut s, &mut p, "shared.first"), Value::Int(0));
        assert_eq!(
            eval(&mut s, &mut p, "shared.alignment"),
            Value::String("bottomleft".into())
        );
        assert_eq!(eval(&mut s, &mut p, "shared.sound"), Value::Bool(true));
        assert_eq!(
            eval(&mut s, &mut p, "shared.config"),
            Value::String("models/m.json".into())
        );
        // A Workshop script's relative path resolves under its own folder.
        assert_ne!(eval(&mut s, &mut p, "shared.bar"), Value::Undefined);
        assert_eq!(
            eval(
                &mut s,
                &mut p,
                "thisScene.getInitialLayerConfig(shared.bar).image"
            ),
            Value::String("models/workshop/77/bar.json".into())
        );
        // The precached object, the bar's object; the first is reused by `createLayer`.
        let st = s.take_structure().expect("structure");
        assert_eq!(st.created.len(), 2);
        assert_eq!(st.reused.len(), 1);
        assert!(st.reordered);
        let created = st.reused[0];
        assert!(s.is_live(created));
        assert_eq!(s.take_aligned(), vec![created]);
        let order: Vec<_> = s.render_objects().collect();
        assert_eq!(order[0], created);
        assert_eq!(order.len(), 5);

        // Destroyed: unlisted, its object parked; the next layer of the asset reuses it with
        // a new id and its initial values.
        assert_eq!(eval(&mut s, &mut p, "shared.layer.origin.x"), Value::Int(5));
        assert_eq!(
            eval(&mut s, &mut p, "thisScene.destroyLayer(shared.layer)"),
            Value::Bool(true)
        );
        assert_eq!(
            eval(&mut s, &mut p, "thisScene.getLayerCount()"),
            Value::Int(4)
        );
        assert_eq!(
            eval(&mut s, &mut p, "shared.layer.origin"),
            Value::Undefined
        );
        assert!(!s.is_live(created));
        tick(&mut s, &mut p, &b, 0.1);
        let st = s.take_structure().expect("structure");
        assert_eq!(st.removed, vec![created]);
        assert_eq!(
            eval(
                &mut s,
                &mut p,
                "let again = thisScene.createLayer('models/m.json');
                 shared.again = again;
                 [again.origin.x, thisScene.getLayerIndex(again), again.alignment]"
            ),
            Value::Array(vec![
                Value::Int(0),
                Value::Int(4),
                Value::String("center".into())
            ])
        );
        let st = s.take_structure().expect("structure");
        assert_eq!(st.reused, vec![created]);
        assert!(st.created.is_empty());
        assert_eq!(
            eval(&mut s, &mut p, "shared.again === shared.layer"),
            Value::Bool(false)
        );
        // Bad arguments are neutral.
        assert_eq!(
            eval(&mut s, &mut p, "thisScene.destroyLayer('nothing')"),
            Value::Bool(false)
        );
        assert_eq!(
            eval(&mut s, &mut p, "thisScene.createLayer('fonts/x.ttf')"),
            Value::Undefined
        );
    }

    #[test]
    fn steady_state_with_created_layers_does_not_allocate() {
        let (mut p, b) = load(&[image(
            1,
            "a",
            &format!(
                r#", "visible": {}"#,
                scripted(
                    "true",
                    "let bars = [];
                     let o = new Vec3(0, 0, 0);
                     let sc = new Vec3(1, 1, 1);
                     export function init() {
                         for (let i = 0; i < 16; ++i) {
                             let l = thisScene.createLayer('models/m.json');
                             thisScene.sortLayer(l, 0);
                             bars.push(l);
                         }
                     }
                     export function update() {
                         for (let i = 0; i < bars.length; ++i) {
                             o.x = 10 * i;
                             o.y = 100 + Math.sin(engine.runtime + i) * 50;
                             bars[i].origin = o;
                             sc.y = 1 + i * Math.cos(engine.runtime);
                             bars[i].scale = sc;
                             bars[i].alpha = 0.5 + 0.5 * Math.sin(engine.runtime);
                             bars[i].visible = bars[i].alpha > 0.1;
                         }
                     }"
                ),
            ),
        )]);
        let mut s = dynamic_scripts(&mut p, &b);
        let st = s.take_structure().expect("structure");
        assert_eq!(st.created.len(), 16);
        assert_eq!(s.render_objects().count(), 17);
        let a = Audio([0.0; 16], [0.0; 32], [0.0; 64]);
        let size = Vec2::new(1920.0, 1080.0);
        for i in 0..10 {
            s.tick(&mut p, &b, &frame(f64::from(i) * 0.1, &a), size);
        }
        let before = ALLOCS.with(Cell::get);
        for i in 10..110 {
            let c = s.tick(&mut p, &b, &frame(f64::from(i) * 0.1, &a), size);
            assert!(c.transforms);
        }
        let n = ALLOCS.with(Cell::get) - before;
        assert_eq!(n, 0, "{n} heap allocations in 100 frames");
        assert!(s.take_structure().is_none());
    }

    #[test]
    fn steady_state_frames_do_not_allocate() {
        let (mut p, b) = load(&[image(
            1,
            "a",
            &format!(
                r#", "origin": {}, "alpha": {}, "color": {}"#,
                scripted(
                    r#""100 100 0""#,
                    "export function update(v) { v.x = 100 + Math.sin(engine.runtime) * 50; return v; }"
                ),
                scripted(
                    "1",
                    "export function update() { return 0.5 + 0.5 * Math.cos(engine.runtime); }"
                ),
                scripted(
                    r#""1 1 1""#,
                    "export function update(v) { v.y = engine.frametime; return v; }"
                ),
            ),
        )]);
        let mut s = scripts(&mut p, &b);
        let a = Audio([0.0; 16], [0.0; 32], [0.0; 64]);
        let size = Vec2::new(1920.0, 1080.0);
        for i in 0..10 {
            s.tick(&mut p, &b, &frame(f64::from(i) * 0.1, &a), size);
        }
        let before = ALLOCS.with(Cell::get);
        for i in 10..110 {
            s.tick(&mut p, &b, &frame(f64::from(i) * 0.1, &a), size);
        }
        let n = ALLOCS.with(Cell::get) - before;
        assert_eq!(n, 0, "{n} heap allocations in 100 frames");
    }

    /// The cover effects of "Soren <3": the effect is hidden at load, its `visible` script turns
    /// it on when a cover arrives and off again a second later; the fade of `multiply` is a
    /// timeline its script restarts with `thisObject.getAnimation()`.
    #[test]
    fn media_thumbnail_scripts_toggle_an_effect_and_play_its_fade() {
        let fade = r#"{ "value": 0, "animation": { "c0": [
                { "frame": 0, "value": 1 }, { "frame": 15, "value": 0 } ],
                "options": { "fps": 15, "length": 15, "mode": "single", "startpaused": true } },
            "script": "export function mediaThumbnailChanged(event) { if (event.hasThumbnail) { var anim = thisObject.getAnimation(); anim.stop(); anim.play(); } }" }"#;
        let visible = scripted(
            "false",
            "var last; export function mediaThumbnailChanged(event) {
                if (last) { last(); last = undefined; }
                thisObject.visible = event.hasThumbnail;
                if (event.hasThumbnail) last = engine.setTimeout(() => { thisObject.visible = false; }, 1000);
            }",
        );
        let effect = format!(
            r#", "effects": [ {{ "file": "effects/e/effect.json", "visible": {visible},
                "passes": [ {{ "constantshadervalues": {{ "multiply": {fade} }} }} ] }} ]"#
        );
        let (mut p, b) = load(&[image(1, "cover", &effect)]);
        let mut s = scripts(&mut p, &b);
        let fx = img(&p, 0).effects[0].visible;
        let multiply = img(&p, 0).effects[0].overrides[0].constants[0].setting;
        assert!(!p.settings.value(fx).boolean(), "hidden at load");
        tick(&mut s, &mut p, &b, 0.0);
        assert!(!p.settings.value(fx).boolean());
        assert_eq!(
            p.settings.value(multiply).float(),
            0.0,
            "not animated before a cover"
        );
        let mut media = lumenwall_script::MediaSnapshot::default();
        media.thumbnail.has_thumbnail = true;
        s.media_changed(media.clone());
        let c = tick(&mut s, &mut p, &b, 1.0);
        assert!(c.uniforms, "the fade started");
        assert!(p.settings.value(fx).boolean(), "shown by the script");
        assert!(s.needs_frames());
        eprintln!("DBG {:?} anim={:?}", eval(&mut s, &mut p, "const a = thisScene.getLayer('cover').getEffect(0).getMaterial(0).getAnimation('multiply'); [a.fps, a.isPlaying(), a.getFrame()]"), p.settings.animation(multiply).is_some());
        let first = p.settings.value(multiply).float();
        assert!(
            first > 0.9,
            "previous cover fully there at the start: {first}"
        );
        // Half a second later (15 frames/s at 30 fps ticks): half way.
        let mut last = first;
        for i in 1..=15 {
            tick(&mut s, &mut p, &b, 1.0 + f64::from(i) / 30.0);
            let v = p.settings.value(multiply).float();
            assert!(v <= last + 1e-5, "fading");
            last = v;
        }
        assert!((0.3..0.7).contains(&last), "{last}");
        for i in 16..=40 {
            tick(&mut s, &mut p, &b, 1.0 + f64::from(i) / 30.0);
        }
        assert_eq!(p.settings.value(multiply).float(), 0.0, "fade over");
        assert!(
            !p.settings.value(fx).boolean(),
            "hidden again after a second"
        );
        // A new cover before the timeout cancels it (the script's `lastHideEvent()`).
        s.media_changed(media.clone());
        tick(&mut s, &mut p, &b, 3.0);
        assert!(p.settings.value(fx).boolean());
        assert!(p.settings.value(multiply).float() > 0.9, "restarted");
        s.media_changed(media);
        tick(&mut s, &mut p, &b, 3.5);
        tick(&mut s, &mut p, &b, 4.2);
        assert!(
            p.settings.value(fx).boolean(),
            "the first timeout was cancelled"
        );
    }
}

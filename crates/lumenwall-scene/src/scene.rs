//! Scene model (spec 02 §6–§11). Models, materials and effects live in arenas owned by the
//! [`Scene`] and are referenced by index; every reference from the JSON gets its own copy (§1.1-4).

use std::collections::HashMap;

use glam::{Vec2, Vec3};
use serde_json::value::RawValue;

use crate::intern::Interner;
pub use crate::intern::Sym;
pub(crate) use crate::load::Arenas;
use crate::setting::SettingId;

/// Interned names the builder compares against.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Names {
    pub blendmode: Sym,
    pub composite: Sym,
    pub compositecolor: Sym,
    pub color: Sym,
    pub alpha: Sym,
    pub tex0format: Sym,
}

impl Names {
    pub fn new(strings: &mut Interner) -> Self {
        Self {
            blendmode: strings.intern("BLENDMODE"),
            composite: strings.intern("COMPOSITE"),
            compositecolor: strings.intern("compositecolor"),
            color: strings.intern("color"),
            alpha: strings.intern("alpha"),
            tex0format: strings.intern("TEX0FORMAT"),
        }
    }
}

macro_rules! index_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub(crate) u32);

        impl $name {
            /// Handle for a position in its arena.
            pub const fn new(index: u32) -> Self {
                Self(index)
            }

            /// Position in its arena.
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

index_type!(
    /// Index in [`Scene::objects`].
    ObjectIndex
);
index_type!(
    /// Index of a [`Model`] in its scene.
    ModelId
);
index_type!(
    /// Index of a [`Material`] in its scene.
    MaterialId
);
index_type!(
    /// Index of an [`Effect`] in its scene.
    EffectId
);

/// A loaded `scene.json`.
#[derive(Debug)]
pub struct Scene {
    /// `camera`.
    pub camera: Camera,
    /// `camera.paths` (spec 09 §1.2): camera trajectory files, in order.
    pub camera_paths: Vec<Sym>,
    /// `general`.
    pub general: SceneGeneral,
    /// Objects in file order, followed by the synthetic bloom object once built (§14).
    pub objects: Vec<Object>,
    pub(crate) arenas: Arenas,
    pub(crate) names: Names,
    /// First object of each id (objects from the file only).
    pub(crate) by_id: HashMap<i32, ObjectIndex>,
    /// Number of objects read from the file.
    pub(crate) file_objects: usize,
    pub(crate) bloom: Option<ObjectIndex>,
    pub(crate) util_tint: Option<Option<MaterialId>>,
    pub(crate) util_passthrough: Option<Option<MaterialId>>,
}

impl Scene {
    /// Model by id.
    pub fn model(&self, id: ModelId) -> &Model {
        &self.arenas.models[id.index()]
    }

    /// Material by id.
    pub fn material(&self, id: MaterialId) -> &Material {
        &self.arenas.materials[id.index()]
    }

    /// Effect by id.
    pub fn effect(&self, id: EffectId) -> &Effect {
        &self.arenas.effects[id.index()]
    }

    /// All models.
    pub fn models(&self) -> &[Model] {
        &self.arenas.models
    }

    /// All materials.
    pub fn materials(&self) -> &[Material] {
        &self.arenas.materials
    }

    /// All effects.
    pub fn effects(&self) -> &[Effect] {
        &self.arenas.effects
    }

    /// Object by index.
    pub fn object(&self, i: ObjectIndex) -> &Object {
        &self.objects[i.index()]
    }

    /// First object of the file with this `id`.
    pub fn find_object(&self, id: i32) -> Option<ObjectIndex> {
        self.by_id.get(&id).copied()
    }

    /// Objects read from the file (excludes the synthetic bloom object).
    pub fn file_objects(&self) -> &[Object] {
        &self.objects[..self.file_objects]
    }

    /// The synthetic bloom object, once [`crate::Project::build`] created it.
    pub fn bloom_object(&self) -> Option<ObjectIndex> {
        self.bloom
    }
}

/// `camera` (§6.2).
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// Point looked at.
    pub center: Vec3,
    /// Position.
    pub eye: Vec3,
    /// Up vector.
    pub up: Vec3,
    /// Near plane (`camera.nearz`, else `general.nearz`, else 0).
    pub nearz: SettingId,
    /// Far plane (`camera.farz`, else `general.farz`, else 1000).
    pub farz: SettingId,
    /// Field of view (`camera.fov`, else `general.fov`, else 50).
    pub fov: SettingId,
}

/// `general` of a scene (§6.3).
#[derive(Clone, Copy, Debug)]
#[allow(missing_docs)]
pub struct SceneGeneral {
    pub ambient_color: SettingId,
    pub skylight_color: SettingId,
    /// Vec3 view read once at build, alpha forced to 1.
    pub clear_color: SettingId,
    pub camera_fade: SettingId,
    pub camera_preview: bool,
    /// Boolean view read once at build: creates the bloom object (§14).
    pub bloom: SettingId,
    pub bloom_strength: SettingId,
    pub bloom_threshold: SettingId,
    pub camera_parallax: SettingId,
    pub camera_parallax_amount: SettingId,
    pub camera_parallax_delay: SettingId,
    pub camera_parallax_mouse_influence: SettingId,
    pub camera_shake: SettingId,
    pub camera_shake_amplitude: SettingId,
    pub camera_shake_roughness: SettingId,
    pub camera_shake_speed: SettingId,
    /// `None` for a perspective scene (no `orthogonalprojection`).
    pub orthogonal_projection: Option<OrthogonalProjection>,
    /// `clearenabled` (spec 09 §1.2): clear the scene target each frame; `null` = true.
    pub clear_enabled: SettingId,
    /// `transparentsorting` (spec 09 §3.6), read once.
    pub transparent_sorting: bool,
    /// `hdr` (spec 09 §2.8), read once.
    pub hdr: bool,
    /// `perspectiveoverridefov` (2D scenes, spec 09 §3.12).
    pub perspective_override_fov: Option<f32>,
}

/// `general.orthogonalprojection`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrthogonalProjection {
    /// Size computed from the images at build.
    pub auto: bool,
    /// Width (0 when `auto`).
    pub width: i32,
    /// Height (0 when `auto`).
    pub height: i32,
}

/// A scene object (§7).
#[derive(Debug)]
pub struct Object {
    /// `id` (−1 when unreadable, §7.2 degraded mode).
    pub id: i32,
    /// `name`.
    pub name: Sym,
    /// `dependencies`.
    pub dependencies: Box<[i32]>,
    /// `parent` id.
    pub parent: Option<i32>,
    /// `origin`, the only origin for every type.
    pub origin: SettingId,
    /// Group `scale` (used by the renderer for generic objects only).
    pub scale: SettingId,
    /// Group `angles`.
    pub angles: SettingId,
    /// Group `visible`.
    pub visible: SettingId,
    /// Type-specific data.
    pub kind: ObjectKind,
}

impl Object {
    /// Visibility setting used for rendering (§13.4): the type's own for images, texts and
    /// particles, the group one otherwise.
    pub fn visible_setting(&self) -> SettingId {
        match &self.kind {
            ObjectKind::Image(i) => i.visible,
            ObjectKind::Text(t) => t.visible,
            ObjectKind::Particle(p) => p.visible,
            _ => self.visible,
        }
    }

    /// Scale and angles settings used by the transformation (§13.3); `None` angles means 0.
    pub fn transform_settings(&self) -> (SettingId, Option<SettingId>) {
        match &self.kind {
            ObjectKind::Image(i) => (i.scale, Some(i.angles)),
            ObjectKind::Text(t) => (t.scale, None),
            ObjectKind::Particle(p) => (p.scale, Some(p.angles)),
            _ => (self.scale, Some(self.angles)),
        }
    }

    /// Image data, if this is an image.
    pub fn as_image(&self) -> Option<&ImageObject> {
        match &self.kind {
            ObjectKind::Image(i) => Some(i),
            _ => None,
        }
    }

    /// Image layer drawn for this object: the image itself, or the layer a text object is
    /// rendered as ([`TextObject::layer`]). Render passes (§12) are built from it.
    pub fn as_layer(&self) -> Option<&ImageObject> {
        match &self.kind {
            ObjectKind::Image(i) => Some(i),
            ObjectKind::Text(t) => Some(&t.layer),
            _ => None,
        }
    }
}

/// Type of an object (§7.1).
#[derive(Debug)]
pub enum ObjectKind {
    /// `image`.
    Image(Box<ImageObject>),
    /// `sound`.
    Sound(SoundObject),
    /// `particle`.
    Particle(Box<ParticleObject>),
    /// `text`.
    Text(Box<TextObject>),
    /// `model`: a 3D model object (spec 09 §1.5).
    Model3d(Box<ModelObject>),
    /// `light` of a known type (spec 09 §1.6).
    Light(Box<LightObject>),
    /// `sprite`: camera-facing billboard drawn by a material (spec 09 §1.7).
    Sprite(Box<SpriteObject>),
    /// Group / transform node, or an unsupported type.
    Generic(GenericKind),
}

/// 3D model object (spec 09 §1.5). The `.mdl` is read by the renderer side; its materials are
/// loaded with [`crate::Project::load_model_material`] (model state defaults, §1.11.2).
#[derive(Debug)]
pub struct ModelObject {
    /// Path of the `.mdl`.
    pub file: Sym,
    /// `skin` (index into the material list of each mesh).
    pub skin: i32,
    /// `reflected`: also drawn into `_rt_Reflection` (§3.7).
    pub reflected: bool,
    /// `castshadow` (default true, §2.5).
    pub cast_shadow: bool,
    /// `perspective` (2D scenes only, §3.12).
    pub perspective: bool,
    /// `parallaxDepth`.
    pub parallax_depth: SettingId,
    /// `alpha` → `g_Alpha` (default 1).
    pub alpha: SettingId,
    /// `color` → `g_Color` / `g_Color4` (default white).
    pub color: SettingId,
    /// `brightness` → `g_Brightness` (default 1).
    pub brightness: SettingId,
}

/// Type of a light (spec 09 §1.6, QO-3D-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LightKind {
    /// `point`, `lpoint`.
    Point,
    /// `spot`, `lspot`.
    Spot,
    /// `tube`, `ltube`.
    Tube,
    /// `directional`, `ldirectional`.
    Directional,
}

impl LightKind {
    /// Parses the `light` key; `None` for an unknown type.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "point" | "lpoint" => Self::Point,
            "spot" | "lspot" => Self::Spot,
            "tube" | "ltube" => Self::Tube,
            "directional" | "ldirectional" => Self::Directional,
            _ => return None,
        })
    }
}

/// Light object (spec 09 §1.6). Position and orientation are the object's `origin`/`angles`.
#[derive(Debug)]
pub struct LightObject {
    /// Type.
    pub kind: LightKind,
    /// `color` (floats).
    pub color: SettingId,
    /// `intensity` (default 1).
    pub intensity: SettingId,
    /// `radius` (default 0: no reach).
    pub radius: SettingId,
    /// `exponent` (default 2, QO-3D-8).
    pub exponent: SettingId,
    /// `castshadow` (default false).
    pub cast_shadow: bool,
}

/// Sprite object (spec 09 §1.7): a material drawn on a camera-facing quad.
#[derive(Debug)]
pub struct SpriteObject {
    /// Material path (`sprite`).
    pub file: Sym,
    /// Material, loaded with the model state defaults; `None` when missing.
    pub material: Option<MaterialId>,
    /// `parallaxDepth`.
    pub parallax_depth: SettingId,
}

/// Why an object is generic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenericKind {
    /// `solid: true` group.
    Group,
    /// `light` (not rendered).
    Light,
    /// `shape` (volumetric light, not rendered).
    Shape,
    /// Unknown type (3D models fall here).
    Unknown,
}

/// Anchor flags found by substring search of `top`, `bottom`, `left`, `right` (§7.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Alignment {
    /// Contains `top`.
    pub top: bool,
    /// Contains `bottom`.
    pub bottom: bool,
    /// Contains `left`.
    pub left: bool,
    /// Contains `right`.
    pub right: bool,
}

impl Alignment {
    /// Parses an alignment string.
    pub fn parse(s: &str) -> Self {
        Self {
            top: s.contains("top"),
            bottom: s.contains("bottom"),
            left: s.contains("left"),
            right: s.contains("right"),
        }
    }

    /// Centred on both axes.
    pub fn is_center(self) -> bool {
        self == Self::default()
    }
}

/// Image object (§7.3).
#[derive(Debug)]
pub struct ImageObject {
    /// Model path (`image`).
    pub file: Sym,
    /// Loaded model; `None` when the model or its material file is missing (the image is then
    /// not set up).
    pub model: Option<ModelId>,
    #[allow(missing_docs)]
    pub scale: SettingId,
    /// Only `z` is used.
    pub angles: SettingId,
    #[allow(missing_docs)]
    pub visible: SettingId,
    #[allow(missing_docs)]
    pub alpha: SettingId,
    /// Tint (colour setting).
    pub color: SettingId,
    /// `horizontalalign`, else `alignment`, else centre.
    pub alignment: Alignment,
    /// `size` vec2 view at load; zero means "from the texture".
    pub size: Vec2,
    /// `parallaxDepth`.
    pub parallax_depth: SettingId,
    /// `colorBlendMode` (integer view read at setup, §12.6).
    pub color_blend_mode: SettingId,
    #[allow(missing_docs)]
    pub brightness: SettingId,
    /// `effects` whose file loaded.
    pub effects: Vec<ImageEffect>,
    /// `animationlayers`.
    pub animation_layers: Vec<AnimationLayer>,
}

/// Effect applied to an image (§7.4).
#[derive(Debug)]
pub struct ImageEffect {
    /// Effect path.
    pub file: Sym,
    /// Loaded effect.
    pub effect: EffectId,
    /// `id` (−1 by default).
    pub id: i32,
    /// `name`.
    pub name: Option<Box<str>>,
    /// `visible` (live setting; see [`crate::ImagePasses`]).
    pub visible: SettingId,
    /// `passes`: overrides matched in order to the effect passes that have a material (§12.2).
    pub overrides: Vec<PassOverride>,
}

/// Pass override of an image effect (§7.4).
#[derive(Debug, Default)]
pub struct PassOverride {
    /// `id` (not used for matching).
    pub id: i32,
    /// `combos`.
    pub combos: Vec<Combo>,
    /// `constantshadervalues`.
    pub constants: Vec<Constant>,
    /// `textures`.
    pub textures: TextureMap,
    /// `usertextures`.
    pub user_textures: TextureMap,
}

/// Puppet animation layer (§7.5).
#[derive(Debug)]
#[allow(missing_docs)]
pub struct AnimationLayer {
    pub id: i32,
    pub rate: SettingId,
    pub visible: SettingId,
    pub blend: SettingId,
    pub animation: SettingId,
}

/// Sound object (§7.6).
#[derive(Debug)]
pub struct SoundObject {
    /// Audio asset paths.
    pub files: Vec<Sym>,
    /// `playbackmode` is `"loop"`.
    pub looping: bool,
}

/// Text object (§7.7).
#[derive(Debug)]
pub struct TextObject {
    /// Content (text view); may carry a script.
    pub text: SettingId,
    /// Font asset path; empty or `systemfont_*` means the fallback font.
    pub font: Sym,
    /// `pointsize`.
    pub point_size: SettingId,
    /// Bounding box, frozen at load.
    pub size: Vec2,
    #[allow(missing_docs)]
    pub scale: SettingId,
    #[allow(missing_docs)]
    pub color: SettingId,
    #[allow(missing_docs)]
    pub alpha: SettingId,
    #[allow(missing_docs)]
    pub visible: SettingId,
    /// `left` / `right` flags (centre when neither).
    pub horizontal_align: Alignment,
    /// `top` / `bottom` flags (centre when neither).
    pub vertical_align: Alignment,
    /// Padding, frozen at load.
    pub padding: Vec2,
    /// `brightness`: multiplies the glyph colour.
    pub brightness: SettingId,
    /// Lumenwall (Wallpaper Engine behaviour, parity rule 5; the reference draws texts outside the
    /// pass chain, spec 04 §18): the image layer the text is drawn as. Synthetic model and
    /// material (WE `font` shader, translucent, glyph texture [`TEXT_TEXTURE_PREFIX`]`<n>` in
    /// slot 0), the object's `effects`, `colorBlendMode`, `parallaxDepth` and visibility,
    /// alignment from `horizontalalign` + `verticalalign`, size from the texture. Its colour,
    /// alpha and brightness are neutral constants: the glyph pass applies the text's own
    /// ([`Self::color`], [`Self::alpha`], [`Self::brightness`]).
    pub layer: ImageObject,
}

/// Name prefix of the glyph texture of a text layer (slot 0 of its synthetic material). The
/// renderer registers the rasterised text under the name it finds there.
pub const TEXT_TEXTURE_PREFIX: &str = "<lumenwall>/text/";
/// Label of the synthetic material of a text layer.
pub(crate) const TEXT_MATERIAL: &str = "<lumenwall>/text/material.json";
/// Label of the synthetic model of a text layer.
pub(crate) const TEXT_MODEL: &str = "<lumenwall>/text/model.json";

/// Where a particle system definition comes from. The content is parsed by `lumenwall-particles`.
#[derive(Debug)]
pub enum ParticleSource {
    /// Path of a particle file.
    File(Sym),
    /// Inline definition, verbatim JSON text.
    Inline(Box<RawValue>),
}

/// Particle object (§7.8). Only the scene-side fields are modelled here.
#[derive(Debug)]
pub struct ParticleObject {
    /// Definition source.
    pub source: ParticleSource,
    #[allow(missing_docs)]
    pub scale: SettingId,
    #[allow(missing_docs)]
    pub angles: SettingId,
    #[allow(missing_docs)]
    pub visible: SettingId,
    /// `parallaxDepth`.
    pub parallax_depth: SettingId,
    /// `instanceoverride` as written; its keys are settings that `lumenwall-particles` registers
    /// with [`crate::Project::add_setting_json`].
    pub instance_override: Option<serde_json::Value>,
}

/// `models/*.json` (§8).
#[derive(Debug)]
#[allow(missing_docs)]
pub struct Model {
    /// Path of this model.
    pub file: Sym,
    pub material: MaterialId,
    pub solid_layer: bool,
    pub fullscreen: bool,
    pub passthrough: bool,
    pub autosize: bool,
    pub nopadding: bool,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub puppet: Option<Sym>,
}

/// `materials/*.json` (§9).
#[derive(Debug)]
pub struct Material {
    /// Path of this material.
    pub file: Sym,
    /// Passes.
    pub passes: Vec<MaterialPass>,
}

/// Blend mode of a pass. Discriminants are the values printed by the structure dump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum Blending {
    Normal = 1,
    Translucent = 2,
    Additive = 3,
}

/// Face culling. Discriminants are the values printed by the structure dump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum CullMode {
    Normal = 1,
    NoCull = 2,
}

/// Depth test / write. Discriminants are the values printed by the structure dump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum DepthMode {
    Disabled = 1,
    Enabled = 2,
}

/// Material pass (§9).
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct MaterialPass {
    /// Shader name without extension.
    pub shader: Sym,
    pub blending: Blending,
    pub cull_mode: CullMode,
    pub depth_test: DepthMode,
    pub depth_write: DepthMode,
    pub textures: TextureMap,
    pub user_textures: TextureMap,
    pub combos: Vec<Combo>,
    pub constants: Vec<Constant>,
    /// `usershadervalues` (spec 09 §1.11.3): material aliases bound live to user
    /// properties. They win over a [`Self::constants`] entry of the same alias.
    pub user_constants: Vec<Constant>,
}

/// Slot → texture name, sorted by slot (§11.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextureMap(pub(crate) Vec<(u32, Sym)>);

impl TextureMap {
    /// Entries sorted by slot.
    pub fn entries(&self) -> &[(u32, Sym)] {
        &self.0
    }

    /// Texture in a slot.
    pub fn get(&self, slot: u32) -> Option<Sym> {
        self.0
            .binary_search_by_key(&slot, |e| e.0)
            .ok()
            .map(|i| self.0[i].1)
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Adds the slots of `other` that are missing here (§12.1).
    pub(crate) fn fill_missing(&mut self, other: &TextureMap) {
        for &(slot, name) in &other.0 {
            if let Err(pos) = self.0.binary_search_by_key(&slot, |e| e.0) {
                self.0.insert(pos, (slot, name));
            }
        }
    }

    /// Writes every slot of `other` over this map.
    pub(crate) fn overlay(&mut self, other: &TextureMap) {
        for &(slot, name) in &other.0 {
            match self.0.binary_search_by_key(&slot, |e| e.0) {
                Ok(pos) => self.0[pos].1 = name,
                Err(pos) => self.0.insert(pos, (slot, name)),
            }
        }
    }
}

/// Shader combo (§11.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combo {
    /// Name as written.
    pub name: Sym,
    /// ASCII upper-case name used for matching (§12.4).
    pub key: Sym,
    /// Value.
    pub value: i32,
}

/// Shader constant (§11.3): parameter alias → setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Constant {
    /// Parameter alias from the shader annotations.
    pub name: Sym,
    /// Live value.
    pub setting: SettingId,
}

/// `effects/*.json` (§10).
#[derive(Debug)]
#[allow(missing_docs)]
pub struct Effect {
    /// Path of this effect.
    pub file: Sym,
    pub name: Box<str>,
    pub description: Box<str>,
    pub group: Box<str>,
    pub preview: Box<str>,
    pub dependencies: Vec<Sym>,
    pub passes: Vec<EffectPass>,
    pub fbos: Vec<Fbo>,
}

/// Effect pass (§10).
#[derive(Debug)]
pub struct EffectPass {
    /// Material; `None` for a command pass.
    pub material: Option<MaterialId>,
    /// `bind`, sorted by index, first duplicate kept.
    pub binds: Vec<Bind>,
    /// `command`.
    pub command: Option<Command>,
    /// `source`.
    pub source: Option<BindSource>,
    /// `target` render target.
    pub target: Option<Sym>,
}

/// Effect command (§10): `"copy"`, anything else is a swap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Copy `source` to `target`.
    Copy,
    /// Any other command (not supported by the renderer, Q15).
    Swap,
}

/// Texture bound to a slot by an effect pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bind {
    /// Slot.
    pub index: i32,
    /// What is bound.
    pub source: BindSource,
}

/// Source of a bind or of a copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindSource {
    /// Render target name.
    Named(Sym),
    /// `"previous"`: output of the previous pass of the chain.
    Previous,
    /// The object's own composite buffer "A" (synthetic bloom pass 4, §14).
    OwnComposite,
}

/// `fbos` entry (§10).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fbo {
    /// Render target name.
    pub name: Sym,
    /// Format name (`"rgba8888"` by default).
    pub format: Sym,
    /// Size divisor.
    pub scale: f32,
    /// `unique`.
    pub unique: bool,
}

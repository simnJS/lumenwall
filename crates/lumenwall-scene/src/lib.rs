//! Data model for `project.json`, scenes, materials, effects and models (spec 02).
//!
//! Loading is a single typed serde pass per file into short-lived lenient structures that borrow
//! from the input, followed by a conversion that reports wrong types with their JSON path. The
//! resulting model is index-based: settings live contiguously in [`Settings`], strings are
//! interned ([`Sym`]), models/materials/effects live in arenas of the [`Scene`]. Bindings to user
//! properties are resolved once at load; a property update touches only its own subscribers.
//!
//! ```
//! use std::collections::HashMap;
//! use lumenwall_scene::{load_project, PropertyOverrides};
//!
//! let mut files: HashMap<String, Vec<u8>> = HashMap::new();
//! files.insert("project.json".into(), br#"{"type": "video", "title": "t", "file": "a.mp4"}"#.to_vec());
//! let project = load_project(&files, PropertyOverrides::new()).unwrap();
//! assert_eq!(project.file, "a.mp4");
//! ```

mod animation;
mod asset;
mod build;
mod convert;
mod dump;
mod error;
mod intern;
mod load;
mod passes;
mod project;
mod property;
pub mod raw;
mod scene;
mod setting;
mod text;
mod value;

pub use animation::{AnimationId, AnimationMode, Keyframe, PropertyAnimation};
pub use asset::{AssetError, AssetSource};
pub use build::{
    AddedObject, BLOOM_MATERIALS, BloomInfo, BuildOptions, MAX_PARENT_DEPTH, PASSTHROUGH_MATERIAL,
    RenderTargetDesc, SceneBuild, TINT_MATERIAL, TargetFormat, Transform,
};
pub use error::{DataError, Expected, JsonPath, LoadError, NotAWallpaper, Segment, Warning};
pub use intern::{Interner, Sym};
pub use passes::{
    ConstantSource, ImagePasses, MainTextureFormat, OverrideRef, PassDesc, PassKind,
    SyntheticOverride, TextureRef,
};
pub use project::{LoadContext, PROJECT_FILE, Project, ProjectKind, load_project};
pub use property::{
    ComboOption, PropId, Properties, Property, PropertyKind, PropertyOverrides,
    PropertyUpdateError, Subscriber,
};
pub use scene::{
    Alignment, AnimationLayer, Bind, BindSource, Blending, Camera, Combo, Command, Constant,
    CullMode, DepthMode, Effect, EffectId, EffectPass, Fbo, GenericKind, ImageEffect, ImageObject,
    LightKind, LightObject, Material, MaterialId, MaterialPass, Model, ModelId, ModelObject,
    Object, ObjectIndex, ObjectKind, OrthogonalProjection, ParticleObject, ParticleSource,
    PassOverride, Scene, SceneGeneral, SoundObject, SpriteObject, TEXT_TEXTURE_PREFIX, TextObject,
    TextureMap,
};
pub use setting::{Script, ScriptId, SettingId, SettingMeta, Settings};
pub use text::ColorError;
pub use value::{Value, ValueTag};

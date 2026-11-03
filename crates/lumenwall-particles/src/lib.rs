//! CPU particle simulation and vertex generation (spec `specs/05-particules.md`).
//!
//! No GPU code: the crate simulates particle systems on the CPU and writes vertex data ready to
//! upload, plus the render state ([`RenderInfo`]) and uniform values ([`ParticleUniforms`]) the
//! renderer needs.
//!
//! ```no_run
//! use lumenwall_particles::*;
//! # let (def_json, object_json) = (serde_json::Value::Null, serde_json::Value::Null);
//! let def = ParticleDefinition::from_json(&def_json);
//! let object = ParticleObject::from_json(&object_json);
//! let opts = SystemOptions { width: 1920.0, height: 1080.0, ..SystemOptions::default() };
//! let mut system = ParticleSystem::new(&def, &object, &opts);
//! let mut vertices = Vec::new();
//! let input = FrameInput { time: 1.0, width: 1920.0, height: 1080.0, mouse: glam::Vec2::ZERO };
//! if system.update(&input) {
//!     let quads = system.write_vertices_vec(&mut vertices);
//!     // upload `vertices`, draw `quads * 6` indices
//! }
//! ```
//!
//! Performance: structure-of-arrays pool allocated once at `N_pool`, one monomorphic loop per
//! operator, stable compaction by `memmove` runs, no allocation in the steady-state frame loop.

mod boids;
mod children;
pub mod defaults;
mod emitter;
mod event;
mod initializer;
mod model;
mod noise;
mod operator;
mod pool;
mod remap;
mod render;
mod rng;
mod sheet;
mod system;
mod value;

pub use children::ChildAssets;
pub use emitter::EmitterImage;
pub use event::ParentParticle;
pub use model::{
    AnimationMode, BoidsDef, ChildDef, ChildKind, CollisionBehavior, CollisionDef, CollisionKind,
    ControlPointDef, EmitterDef, EmitterKind, HsvColorDef, InheritMode, InitializerDef,
    InstanceOverride, LimitBehavior, OperatorBlend, OperatorDef, OscillateDef, ParticleDefinition,
    ParticleObject, ParticleSource, PositionOffsetDef, RampDef, RemapDef, RemapInput,
    RemapOperation, RemapOutput, RemapTransform, RendererDef, RendererKind, SequenceBetweenDef,
    TurbulenceDef, TurbulentVelocityDef, VortexDef,
};
pub use noise::{curl, fbm3, noise, noise3};
pub use render::{
    BlendMode, DepthRange, EYE, GeometryKind, ParallaxInput, ParticleUniforms, QUAD_INDICES,
    REFRACT_AMOUNT, ROPE_ATTRIBUTES, ROPE_VERTEX_FLOATS, RenderInfo, SPRITE_ATTRIBUTES,
    SPRITE_VERTEX_FLOATS, UniformInput, VertexAttribute, write_quad_indices,
};
pub use rng::Rng;
pub use sheet::{AnimatedTexture, SpriteSheet, TextureInfo};
pub use system::{
    CONTROL_POINTS, ControlPoint, DEFAULT_POOL, FrameInput, MAX_DT, MAX_POOL, ParticleSystem,
    ParticlesView, SystemOptions,
};
pub use value::{Binding, UserValue, Value};

//! JSON data model of particle systems (spec 05 §1, §4.2, §4.4, §7, §8, §14).
//!
//! Parsing is lenient like the reference: a wrong type falls back to the documented default (or to
//! zero for [U] arrays), unknown initializer/operator names are dropped, unknown emitters are dropped
//! with a log line. The types implement `serde::Deserialize` (through `serde_json::Value`, because
//! values are polymorphic and type errors must not fail the whole file).

use crate::value::{
    Obj, UserValue, Value, color, raw_bool, raw_f32, raw_flags, raw_i32, raw_i64, raw_offset,
    raw_sign, raw_str, raw_vec3, user, user_f, user_v3,
};
use glam::{IVec3, Vec2, Vec3};
use serde_json::Value as Json;
use std::f32::consts::TAU;

macro_rules! deserialize_via_json {
    ($t:ty) => {
        impl<'de> serde::Deserialize<'de> for $t {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let j = Json::deserialize(d)?;
                Ok(<$t>::from_json(&j))
            }
        }
    };
}

fn as_obj(j: &Json) -> Option<&Obj> {
    j.as_object()
}

fn objects<'a>(o: &'a Obj, key: &str) -> impl Iterator<Item = &'a Obj> {
    o.get(key)
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(as_obj)
}

// ---------------------------------------------------------------------------------------------
// Scene object (§1.2, §1.5)

/// Where the particle definition comes from (`particle` key of a scene object, §0.1).
#[derive(Clone, Debug, PartialEq)]
pub enum ParticleSource {
    /// Path of a definition file in the assets.
    Path(String),
    /// Inline definition.
    Inline(Box<ParticleDefinition>),
}

impl ParticleSource {
    /// Detects a particle system: an object, or a non-empty string other than `"None"`.
    pub fn from_json(j: &Json) -> Option<Self> {
        match j {
            Json::Object(_) => Some(Self::Inline(Box::new(ParticleDefinition::from_json(j)))),
            Json::String(s) if !s.is_empty() && s != "None" => Some(Self::Path(s.clone())),
            _ => None,
        }
    }
}

/// `instanceoverride` multipliers (§1.5).
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceOverride {
    /// True when the `instanceoverride` key exists.
    pub present: bool,
    /// Ignored by the simulation.
    pub enabled: UserValue,
    pub alpha: UserValue,
    pub size: UserValue,
    pub lifetime: UserValue,
    /// Read once at system creation.
    pub rate: UserValue,
    pub speed: UserValue,
    /// Read once at system creation.
    pub count: UserValue,
    /// Only for the `g_Color` / `g_Color4` uniforms.
    pub color: UserValue,
    pub colorn: UserValue,
}

impl Default for InstanceOverride {
    /// The block is absent: every field is the scalar 1, `enabled` is false.
    fn default() -> Self {
        let one = || UserValue::number(1.0);
        Self {
            present: false,
            enabled: UserValue::new(Value::Bool(false)),
            alpha: one(),
            size: one(),
            lifetime: one(),
            rate: one(),
            speed: one(),
            count: one(),
            color: one(),
            colorn: one(),
        }
    }
}

impl InstanceOverride {
    /// Parses an `instanceoverride` object (present block: missing fields take the table defaults).
    pub fn from_json(j: &Json) -> Self {
        let empty = Obj::new();
        let o = as_obj(j).unwrap_or(&empty);
        Self {
            present: true,
            enabled: user(o, "enabled", Value::Bool(true)),
            alpha: user_f(o, "alpha", 1.0),
            size: user_f(o, "size", 1.0),
            lifetime: user_f(o, "lifetime", 1.0),
            rate: user_f(o, "rate", 1.0),
            speed: user_f(o, "speed", 1.0),
            count: user_f(o, "count", 1.0),
            color: user_v3(o, "color", Vec3::ONE),
            colorn: user_v3(o, "colorn", Vec3::ONE),
        }
    }

    pub(crate) fn visit(&mut self, f: &mut dyn FnMut(&mut UserValue)) {
        for v in [
            &mut self.enabled,
            &mut self.alpha,
            &mut self.size,
            &mut self.lifetime,
            &mut self.rate,
            &mut self.speed,
            &mut self.count,
            &mut self.color,
            &mut self.colorn,
        ] {
            f(v);
        }
    }
}

/// Scene-object keys used by a particle system (§1.2).
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleObject {
    /// `None` when the object is not a particle system.
    pub particle: Option<ParticleSource>,
    /// Read at creation and on resolution change only.
    pub origin: UserValue,
    pub scale: UserValue,
    /// Radians.
    pub angles: UserValue,
    pub visible: UserValue,
    pub parallax_depth: UserValue,
    pub instance_override: InstanceOverride,
}

impl Default for ParticleObject {
    fn default() -> Self {
        Self::from_json(&Json::Null)
    }
}

impl ParticleObject {
    pub fn from_json(j: &Json) -> Self {
        let empty = Obj::new();
        let o = as_obj(j).unwrap_or(&empty);
        Self {
            particle: o.get("particle").and_then(ParticleSource::from_json),
            origin: user_v3(o, "origin", Vec3::ZERO),
            scale: user_v3(o, "scale", Vec3::ONE),
            angles: user_v3(o, "angles", Vec3::ZERO),
            visible: user(o, "visible", Value::Bool(true)),
            parallax_depth: user(o, "parallaxDepth", Value::Vec2(Vec2::ZERO)),
            instance_override: o
                .get("instanceoverride")
                .map_or_else(InstanceOverride::default, InstanceOverride::from_json),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Definition file (§1.3)

/// `animationmode` (§10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationMode {
    /// `"sequence"` and any unknown value.
    #[default]
    Sequence,
    Once,
    RandomFrame,
}

/// Root of a particle definition file.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleDefinition {
    pub material: Option<String>,
    pub emitters: Vec<EmitterDef>,
    pub initializers: Vec<InitializerDef>,
    pub operators: Vec<OperatorDef>,
    /// First entry of `renderer` (default `sprite`).
    pub renderer: RendererDef,
    /// All entries in file order, including invalid ids.
    pub control_points: Vec<ControlPointDef>,
    /// Parsed for compatibility, never instantiated (§14).
    pub children: Vec<ChildDef>,
    pub animation_mode: AnimationMode,
    pub sequence_multiplier: f32,
    pub max_count: u32,
    /// Ignored.
    pub start_time: i64,
    /// Bit 4: perspective particles.
    pub flags: u32,
}

impl Default for ParticleDefinition {
    fn default() -> Self {
        Self::from_json(&Json::Null)
    }
}

impl ParticleDefinition {
    /// Flag bit of the system: perspective particles.
    pub const FLAG_PERSPECTIVE: u32 = 4;

    pub fn from_json(j: &Json) -> Self {
        let empty = Obj::new();
        let o = as_obj(j).unwrap_or(&empty);
        let emitters = objects(o, "emitter")
            .filter_map(|e| {
                let def = EmitterDef::from_obj(e);
                if def.is_none() {
                    tracing::warn!(
                        name = raw_str(e, "name").unwrap_or(""),
                        "unknown particle emitter ignored"
                    );
                }
                def
            })
            .collect();
        let renderer = o
            .get("renderer")
            .and_then(Json::as_array)
            .and_then(|a| a.first())
            .and_then(as_obj)
            .map_or_else(|| RendererDef::from_obj(&Obj::new()), RendererDef::from_obj);
        Self {
            material: raw_str(o, "material").map(String::from),
            emitters,
            initializers: objects(o, "initializer")
                .filter_map(InitializerDef::from_obj)
                .collect(),
            operators: objects(o, "operator")
                .filter_map(OperatorDef::from_obj)
                .collect(),
            renderer,
            control_points: objects(o, "controlpoint")
                .map(ControlPointDef::from_obj)
                .collect(),
            children: objects(o, "children").map(ChildDef::from_obj).collect(),
            animation_mode: match raw_str(o, "animationmode") {
                Some("once") => AnimationMode::Once,
                Some("randomframe") => AnimationMode::RandomFrame,
                _ => AnimationMode::Sequence,
            },
            sequence_multiplier: raw_f32(o, "sequencemultiplier", 1.0),
            max_count: raw_i64(o, "maxcount", 100).clamp(0, i64::from(u32::MAX)) as u32,
            start_time: raw_i64(o, "starttime", 0),
            flags: raw_flags(o, "flags"),
        }
    }

    /// True when the system renders in perspective (`flags & 4`).
    pub fn perspective(&self) -> bool {
        self.flags & Self::FLAG_PERSPECTIVE != 0
    }
}

deserialize_via_json!(ParticleDefinition);
deserialize_via_json!(ParticleObject);
deserialize_via_json!(InstanceOverride);

// ---------------------------------------------------------------------------------------------
// Emitters (§1.4)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmitterKind {
    BoxRandom,
    SphereRandom,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmitterDef {
    pub kind: EmitterKind,
    /// Ignored.
    pub id: i64,
    pub directions: Vec3,
    pub distance_min: Vec3,
    pub distance_max: Vec3,
    pub origin: Vec3,
    pub sign: IVec3,
    pub instantaneous: u32,
    pub speed_min: f32,
    pub speed_max: f32,
    pub rate: f32,
    pub control_point: i32,
    /// Bit 2: at most one particle per frame; bit 4: periodic emission (box only).
    pub flags: u32,
    /// Ignored.
    pub cone: f32,
    pub delay: f32,
    pub duration: f32,
    pub min_periodic_delay: f32,
    pub max_periodic_delay: f32,
    pub min_periodic_duration: f32,
    pub max_periodic_duration: f32,
}

impl EmitterDef {
    pub const FLAG_ONE_PER_FRAME: u32 = 2;
    pub const FLAG_PERIODIC: u32 = 4;

    /// `None` for an unknown `name`.
    pub fn from_obj(o: &Obj) -> Option<Self> {
        let kind = match raw_str(o, "name").unwrap_or("") {
            "boxrandom" => EmitterKind::BoxRandom,
            "sphererandom" => EmitterKind::SphereRandom,
            _ => return None,
        };
        Some(Self {
            kind,
            id: raw_i64(o, "id", -1),
            directions: raw_vec3(o, "directions", Vec3::new(1.0, 1.0, 0.0)),
            distance_min: raw_vec3(o, "distancemin", Vec3::ZERO),
            distance_max: raw_vec3(o, "distancemax", Vec3::new(256.0, 256.0, 0.0)),
            origin: raw_vec3(o, "origin", Vec3::ZERO),
            sign: raw_sign(o, "sign"),
            instantaneous: raw_i64(o, "instantaneous", 0).clamp(0, i64::from(u32::MAX)) as u32,
            speed_min: raw_f32(o, "speedmin", 0.0),
            speed_max: raw_f32(o, "speedmax", 0.0),
            rate: raw_f32(o, "rate", 10.0),
            control_point: raw_i32(o, "controlpoint", 0),
            flags: raw_flags(o, "flags"),
            cone: raw_f32(o, "cone", 0.0),
            delay: raw_f32(o, "delay", 0.0),
            duration: raw_f32(o, "duration", 0.0),
            min_periodic_delay: raw_f32(o, "minperiodicdelay", 1.0),
            max_periodic_delay: raw_f32(o, "maxperiodicdelay", 2.0),
            min_periodic_duration: raw_f32(o, "minperiodicduration", 2.0),
            max_periodic_duration: raw_f32(o, "maxperiodicduration", 3.0),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Initializers (§7)

/// `turbulentvelocityrandom` parameters (§7.8).
#[derive(Clone, Debug, PartialEq)]
pub struct TurbulentVelocityDef {
    pub speed_min: UserValue,
    pub speed_max: UserValue,
    pub scale: UserValue,
    pub offset: UserValue,
    pub forward: UserValue,
    pub right: UserValue,
    pub time_scale: UserValue,
    pub phase_min: UserValue,
    pub phase_max: UserValue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InitializerDef {
    ColorRandom {
        min: UserValue,
        max: UserValue,
    },
    SizeRandom {
        min: UserValue,
        max: UserValue,
        exponent: UserValue,
    },
    AlphaRandom {
        min: UserValue,
        max: UserValue,
    },
    LifetimeRandom {
        min: UserValue,
        max: UserValue,
    },
    VelocityRandom {
        min: UserValue,
        max: UserValue,
    },
    RotationRandom {
        min: UserValue,
        max: UserValue,
    },
    AngularVelocityRandom {
        min: UserValue,
        max: UserValue,
        exponent: UserValue,
    },
    TurbulentVelocityRandom(Box<TurbulentVelocityDef>),
    MapSequenceAroundControlPoint {
        control_point: UserValue,
        count: UserValue,
        speed_min: UserValue,
        speed_max: UserValue,
    },
}

impl InitializerDef {
    /// `None` for an unknown `name` (silently ignored, §1.3).
    pub fn from_obj(o: &Obj) -> Option<Self> {
        Some(match raw_str(o, "name")? {
            "colorrandom" => Self::ColorRandom {
                min: color(o, "min", Vec3::ZERO),
                max: color(o, "max", Vec3::ONE),
            },
            "sizerandom" => Self::SizeRandom {
                min: user_f(o, "min", 0.0),
                max: user_f(o, "max", 20.0),
                exponent: user_f(o, "exponent", 1.0),
            },
            "alpharandom" => Self::AlphaRandom {
                min: user_f(o, "min", 0.05),
                max: user_f(o, "max", 1.0),
            },
            "lifetimerandom" => Self::LifetimeRandom {
                min: user_f(o, "min", 0.0),
                max: user_f(o, "max", 1.0),
            },
            "velocityrandom" => Self::VelocityRandom {
                min: user_v3(o, "min", Vec3::splat(-32.0)),
                max: user_v3(o, "max", Vec3::splat(32.0)),
            },
            "rotationrandom" => Self::RotationRandom {
                min: user_v3(o, "min", Vec3::ZERO),
                max: user_v3(o, "max", Vec3::new(0.0, 0.0, TAU)),
            },
            "angularvelocityrandom" => Self::AngularVelocityRandom {
                min: user_v3(o, "min", Vec3::new(0.0, 0.0, -5.0)),
                max: user_v3(o, "max", Vec3::new(0.0, 0.0, 5.0)),
                exponent: user_f(o, "exponent", 1.0),
            },
            "turbulentvelocityrandom" => {
                Self::TurbulentVelocityRandom(Box::new(TurbulentVelocityDef {
                    speed_min: user_f(o, "speedmin", 100.0),
                    speed_max: user_f(o, "speedmax", 250.0),
                    scale: user_f(o, "scale", 1.0),
                    offset: user_f(o, "offset", 0.0),
                    forward: user_v3(o, "forward", Vec3::Y),
                    right: user_v3(o, "right", Vec3::Z),
                    time_scale: user_f(o, "timescale", 1.0),
                    phase_min: user_f(o, "phasemin", 0.0),
                    phase_max: user_f(o, "phasemax", 0.1),
                }))
            }
            "mapsequencearoundcontrolpoint" => Self::MapSequenceAroundControlPoint {
                control_point: user_f(o, "controlpoint", 0.0),
                count: user_f(o, "count", 1.0),
                speed_min: user_v3(o, "speedmin", Vec3::ZERO),
                speed_max: user_v3(o, "speedmax", Vec3::splat(100.0)),
            },
            _ => return None,
        })
    }

    pub(crate) fn visit(&mut self, f: &mut dyn FnMut(&mut UserValue)) {
        let list: &mut [&mut UserValue] = match self {
            Self::ColorRandom { min, max }
            | Self::AlphaRandom { min, max }
            | Self::LifetimeRandom { min, max }
            | Self::VelocityRandom { min, max }
            | Self::RotationRandom { min, max } => &mut [min, max],
            Self::SizeRandom { min, max, exponent }
            | Self::AngularVelocityRandom { min, max, exponent } => &mut [min, max, exponent],
            Self::TurbulentVelocityRandom(t) => &mut [
                &mut t.speed_min,
                &mut t.speed_max,
                &mut t.scale,
                &mut t.offset,
                &mut t.forward,
                &mut t.right,
                &mut t.time_scale,
                &mut t.phase_min,
                &mut t.phase_max,
            ],
            Self::MapSequenceAroundControlPoint {
                control_point,
                count,
                speed_min,
                speed_max,
            } => &mut [control_point, count, speed_min, speed_max],
        };
        for v in list {
            f(v);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Operators (§8)

/// `sizechange` / `alphachange` / `colorchange` ramp parameters (§8.5, §8.6).
#[derive(Clone, Debug, PartialEq)]
pub struct RampDef {
    pub start_time: UserValue,
    pub end_time: UserValue,
    pub start_value: UserValue,
    pub end_value: UserValue,
}

/// `turbulence` parameters (§8.7).
#[derive(Clone, Debug, PartialEq)]
pub struct TurbulenceDef {
    pub scale: UserValue,
    pub speed_min: UserValue,
    pub speed_max: UserValue,
    pub time_scale: UserValue,
    pub mask: UserValue,
    pub phase_min: UserValue,
    pub phase_max: UserValue,
}

/// `vortex` / `vortex_v2` parameters (§8.8).
#[derive(Clone, Debug, PartialEq)]
pub struct VortexDef {
    pub control_point: i32,
    /// Bit 1: infinite axis; bit 2: centripetal force; bit 4: ring mode.
    pub flags: u32,
    pub axis: UserValue,
    pub offset: UserValue,
    pub distance_inner: UserValue,
    pub distance_outer: UserValue,
    pub speed_inner: UserValue,
    pub speed_outer: UserValue,
    pub center_force: UserValue,
    pub ring_radius: UserValue,
    pub ring_width: UserValue,
    pub ring_pull_distance: UserValue,
    pub ring_pull_force: UserValue,
    /// Read once: > 0 disables the operator.
    pub audio_processing_mode: UserValue,
}

impl VortexDef {
    pub const FLAG_INFINITE_AXIS: u32 = 1;
    pub const FLAG_CENTER_FORCE: u32 = 2;
    pub const FLAG_RING: u32 = 4;
}

/// `oscillatealpha` / `oscillatesize` / `oscillateposition` parameters (§8.10, §8.11).
#[derive(Clone, Debug, PartialEq)]
pub struct OscillateDef {
    pub frequency_min: UserValue,
    pub frequency_max: UserValue,
    pub scale_min: UserValue,
    pub scale_max: UserValue,
    pub phase_min: UserValue,
    pub phase_max: UserValue,
    /// `oscillateposition` only.
    pub mask: UserValue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OperatorDef {
    Movement {
        drag: UserValue,
        gravity: UserValue,
    },
    AngularMovement {
        drag: UserValue,
        force: UserValue,
    },
    AlphaFade {
        fade_in_time: UserValue,
        fade_out_time: UserValue,
    },
    SizeChange(RampDef),
    AlphaChange(RampDef),
    ColorChange(RampDef),
    Turbulence(Box<TurbulenceDef>),
    Vortex(Box<VortexDef>),
    ControlPointAttract {
        control_point: i32,
        origin: UserValue,
        scale: UserValue,
        threshold: UserValue,
    },
    OscillateAlpha(Box<OscillateDef>),
    OscillateSize(Box<OscillateDef>),
    OscillatePosition(Box<OscillateDef>),
}

fn ramp_def(o: &Obj, start: Value, end: Value) -> RampDef {
    RampDef {
        start_time: user_f(o, "starttime", 0.0),
        end_time: user_f(o, "endtime", 1.0),
        start_value: user(o, "startvalue", start),
        end_value: user(o, "endvalue", end),
    }
}

fn oscillate_def(o: &Obj, freq_max: f32, scale_min: f32, scale_max: f32) -> Box<OscillateDef> {
    Box::new(OscillateDef {
        frequency_min: user_f(o, "frequencymin", 0.0),
        frequency_max: user_f(o, "frequencymax", freq_max),
        scale_min: user_f(o, "scalemin", scale_min),
        scale_max: user_f(o, "scalemax", scale_max),
        phase_min: user_f(o, "phasemin", 0.0),
        phase_max: user_f(o, "phasemax", TAU),
        mask: user_v3(o, "mask", Vec3::new(1.0, 1.0, 0.0)),
    })
}

impl OperatorDef {
    /// `None` for an unknown `name` (silently ignored, §1.3).
    pub fn from_obj(o: &Obj) -> Option<Self> {
        Some(match raw_str(o, "name")? {
            "movement" => Self::Movement {
                drag: user_f(o, "drag", 0.0),
                gravity: user_v3(o, "gravity", Vec3::ZERO),
            },
            "angularmovement" => Self::AngularMovement {
                drag: user_f(o, "drag", 0.0),
                force: user_v3(o, "force", Vec3::ZERO),
            },
            "alphafade" => Self::AlphaFade {
                fade_in_time: user_f(o, "fadeintime", 0.5),
                fade_out_time: user_f(o, "fadeouttime", 0.5),
            },
            "sizechange" => Self::SizeChange(ramp_def(o, Value::Number(1.0), Value::Number(0.0))),
            "alphachange" => Self::AlphaChange(ramp_def(o, Value::Number(1.0), Value::Number(0.0))),
            "colorchange" => {
                Self::ColorChange(ramp_def(o, Value::Vec3(Vec3::ONE), Value::Vec3(Vec3::ONE)))
            }
            "turbulence" => Self::Turbulence(Box::new(TurbulenceDef {
                scale: user_f(o, "scale", 0.005),
                speed_min: user_f(o, "speedmin", 500.0),
                speed_max: user_f(o, "speedmax", 1000.0),
                time_scale: user_f(o, "timescale", 0.01),
                mask: user_v3(o, "mask", Vec3::new(1.0, 1.0, 0.0)),
                phase_min: user_f(o, "phasemin", 0.0),
                phase_max: user_f(o, "phasemax", 0.0),
            })),
            "vortex" | "vortex_v2" => Self::Vortex(Box::new(VortexDef {
                control_point: raw_i32(o, "controlpoint", 0),
                flags: raw_flags(o, "flags"),
                axis: user_v3(o, "axis", Vec3::Z),
                offset: user_v3(o, "offset", Vec3::ZERO),
                distance_inner: user_f(o, "distanceinner", 500.0),
                distance_outer: user_f(o, "distanceouter", 650.0),
                speed_inner: user_f(o, "speedinner", 2500.0),
                speed_outer: user_f(o, "speedouter", 0.0),
                center_force: user_f(o, "centerforce", 1.0),
                ring_radius: user_f(o, "ringradius", 300.0),
                ring_width: user_f(o, "ringwidth", 50.0),
                ring_pull_distance: user_f(o, "ringpulldistance", 50.0),
                ring_pull_force: user_f(o, "ringpullforce", 10.0),
                audio_processing_mode: user_f(o, "audioprocessingmode", 0.0),
            })),
            "controlpointattract" => Self::ControlPointAttract {
                control_point: raw_i32(o, "controlpoint", 0),
                origin: user_v3(o, "origin", Vec3::ZERO),
                scale: user_f(o, "scale", 100.0),
                threshold: user_f(o, "threshold", 1000.0),
            },
            "oscillatealpha" => Self::OscillateAlpha(oscillate_def(o, 10.0, 0.0, 1.0)),
            "oscillatesize" => Self::OscillateSize(oscillate_def(o, 10.0, 0.8, 1.2)),
            "oscillateposition" => Self::OscillatePosition(oscillate_def(o, 5.0, 0.0, 10.0)),
            _ => return None,
        })
    }

    pub(crate) fn visit(&mut self, f: &mut dyn FnMut(&mut UserValue)) {
        let list: &mut [&mut UserValue] = match self {
            Self::Movement { drag, gravity: v } | Self::AngularMovement { drag, force: v } => {
                &mut [drag, v]
            }
            Self::AlphaFade {
                fade_in_time,
                fade_out_time,
            } => &mut [fade_in_time, fade_out_time],
            Self::SizeChange(r) | Self::AlphaChange(r) | Self::ColorChange(r) => &mut [
                &mut r.start_time,
                &mut r.end_time,
                &mut r.start_value,
                &mut r.end_value,
            ],
            Self::Turbulence(t) => &mut [
                &mut t.scale,
                &mut t.speed_min,
                &mut t.speed_max,
                &mut t.time_scale,
                &mut t.mask,
                &mut t.phase_min,
                &mut t.phase_max,
            ],
            Self::Vortex(v) => &mut [
                &mut v.axis,
                &mut v.offset,
                &mut v.distance_inner,
                &mut v.distance_outer,
                &mut v.speed_inner,
                &mut v.speed_outer,
                &mut v.center_force,
                &mut v.ring_radius,
                &mut v.ring_width,
                &mut v.ring_pull_distance,
                &mut v.ring_pull_force,
                &mut v.audio_processing_mode,
            ],
            Self::ControlPointAttract {
                origin,
                scale,
                threshold,
                ..
            } => &mut [origin, scale, threshold],
            Self::OscillateAlpha(d) | Self::OscillateSize(d) | Self::OscillatePosition(d) => &mut [
                &mut d.frequency_min,
                &mut d.frequency_max,
                &mut d.scale_min,
                &mut d.scale_max,
                &mut d.phase_min,
                &mut d.phase_max,
                &mut d.mask,
            ],
        };
        for v in list {
            f(v);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Renderer (§4.2)

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RendererKind {
    /// `sprite` and any unknown name.
    #[default]
    Sprite,
    SpriteTrail,
    Rope,
    RopeTrail,
}

impl RendererKind {
    pub fn is_rope(self) -> bool {
        matches!(self, Self::Rope | Self::RopeTrail)
    }

    pub fn is_trail(self) -> bool {
        matches!(self, Self::SpriteTrail | Self::RopeTrail)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RendererDef {
    pub kind: RendererKind,
    pub length: f32,
    pub max_length: f32,
    pub min_length: f32,
    pub subdivision: u32,
    /// Read, without effect.
    pub segments: i64,
    pub uv_scale: f32,
    pub uv_scrolling: bool,
    pub uv_smoothing: bool,
    /// Ignored.
    pub fade_alpha: bool,
    /// Ignored.
    pub fade_size: bool,
}

impl RendererDef {
    pub fn from_obj(o: &Obj) -> Self {
        let kind = match raw_str(o, "name").unwrap_or("sprite") {
            "spritetrail" => RendererKind::SpriteTrail,
            "rope" => RendererKind::Rope,
            "ropetrail" => RendererKind::RopeTrail,
            _ => RendererKind::Sprite,
        };
        let length_default = if kind == RendererKind::RopeTrail {
            1.0
        } else {
            0.05
        };
        let subdivision_default = if kind == RendererKind::Rope { 4.0 } else { 1.0 };
        let mut segments = raw_i64(o, "segments", 4);
        if kind == RendererKind::RopeTrail {
            segments = segments.max(2);
        }
        Self {
            kind,
            length: raw_f32(o, "length", length_default),
            max_length: raw_f32(o, "maxlength", 10.0),
            min_length: raw_f32(o, "minlength", 0.0),
            // `as u32` truncates toward zero and saturates negatives (and NaN) to 0.
            subdivision: raw_f32(o, "subdivision", subdivision_default) as u32,
            segments,
            uv_scale: raw_f32(o, "uvscale", 1.0),
            uv_scrolling: raw_bool(o, "uvscrolling", false),
            uv_smoothing: raw_bool(o, "uvsmoothing", true),
            fade_alpha: raw_bool(o, "fadealpha", false),
            fade_size: raw_bool(o, "fadesize", false),
        }
    }

    /// `σ = max(1, subdivision)` (§12.4).
    pub fn sigma(&self) -> u32 {
        self.subdivision.max(1)
    }
}

// ---------------------------------------------------------------------------------------------
// Control points (§4.4) and children (§14)

#[derive(Clone, Debug, PartialEq)]
pub struct ControlPointDef {
    /// Entries outside `[0, 8)` are ignored.
    pub id: i64,
    /// Bit 1: follows the mouse; bit 2: world space.
    pub flags: u32,
    pub offset: Vec3,
    /// Ignored (only bit 1 of `flags` counts).
    pub lock_to_pointer: bool,
}

impl ControlPointDef {
    pub const FLAG_MOUSE: u32 = 1;
    pub const FLAG_WORLD_SPACE: u32 = 2;

    pub fn from_obj(o: &Obj) -> Self {
        Self {
            id: raw_i64(o, "id", -1),
            flags: raw_flags(o, "flags"),
            offset: raw_offset(o, "offset"),
            lock_to_pointer: raw_bool(o, "locktopointer", false),
        }
    }
}

/// Child system entry: parsed, never instantiated (§14).
#[derive(Clone, Debug, PartialEq)]
pub struct ChildDef {
    pub kind: String,
    pub name: String,
    pub particle: String,
    pub max_count: i64,
    pub control_point_start_index: i64,
    pub probability: f32,
    pub angles: Vec3,
    pub origin: Vec3,
    pub scale: Vec3,
}

impl ChildDef {
    pub fn from_obj(o: &Obj) -> Self {
        Self {
            kind: raw_str(o, "type").unwrap_or("static").to_owned(),
            name: raw_str(o, "name").unwrap_or("").to_owned(),
            particle: raw_str(o, "particle").unwrap_or("").to_owned(),
            max_count: raw_i64(o, "maxcount", 20),
            control_point_start_index: raw_i64(o, "controlpointstartindex", 0),
            probability: raw_f32(o, "probability", 1.0),
            angles: raw_vec3(o, "angles", Vec3::ZERO),
            origin: raw_vec3(o, "origin", Vec3::ZERO),
            scale: raw_vec3(o, "scale", Vec3::ONE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_detection() {
        assert!(ParticleSource::from_json(&json!("")).is_none());
        assert!(ParticleSource::from_json(&json!("None")).is_none());
        assert!(ParticleSource::from_json(&json!(3)).is_none());
        assert_eq!(
            ParticleSource::from_json(&json!("particles/x.json")),
            Some(ParticleSource::Path("particles/x.json".into()))
        );
        assert!(matches!(
            ParticleSource::from_json(&json!({})),
            Some(ParticleSource::Inline(_))
        ));
    }

    #[test]
    fn definition_defaults() {
        let d = ParticleDefinition::from_json(&json!({}));
        assert_eq!(d.max_count, 100);
        assert_eq!(d.renderer.kind, RendererKind::Sprite);
        assert_eq!(d.renderer.length, 0.05);
        assert_eq!(d.renderer.subdivision, 1);
        assert_eq!(d.animation_mode, AnimationMode::Sequence);
        assert_eq!(d.sequence_multiplier, 1.0);
        assert!(!d.perspective());
        let r = ParticleDefinition::from_json(
            &json!({"renderer": [{"name": "rope"}, {"name": "sprite"}]}),
        );
        assert_eq!(r.renderer.kind, RendererKind::Rope);
        assert_eq!(r.renderer.subdivision, 4);
        let rt = ParticleDefinition::from_json(
            &json!({"renderer": [{"name": "ropetrail", "segments": 0}]}),
        );
        assert_eq!(rt.renderer.length, 1.0);
        assert_eq!(rt.renderer.subdivision, 1);
        assert_eq!(rt.renderer.segments, 2);
        let neg = ParticleDefinition::from_json(
            &json!({"renderer": [{"name": "rope", "subdivision": -3.5}]}),
        );
        assert_eq!(neg.renderer.subdivision, 0);
        assert_eq!(neg.renderer.sigma(), 1);
    }

    #[test]
    fn unknown_entries_are_dropped() {
        let d = ParticleDefinition::from_json(&json!({
            "emitter": [{"name": "boxrandom"}, {"name": "nope"}, {"name": "sphererandom"}],
            "initializer": [{"name": "colorrandom"}, {"name": "bogus"}, {"noname": 1}],
            "operator": [{"name": "movement"}, {"name": "capvelocity"}, {"name": "vortex_v2"}]
        }));
        assert_eq!(d.emitters.len(), 2);
        assert_eq!(d.initializers.len(), 1);
        assert_eq!(d.operators.len(), 2);
        assert!(matches!(d.operators[1], OperatorDef::Vortex(_)));
    }

    #[test]
    fn emitter_defaults() {
        let d = ParticleDefinition::from_json(&json!({"emitter": [{"name": "boxrandom"}]}));
        let e = &d.emitters[0];
        assert_eq!(e.directions, Vec3::new(1.0, 1.0, 0.0));
        assert_eq!(e.distance_max, Vec3::new(256.0, 256.0, 0.0));
        assert_eq!(e.rate, 10.0);
        assert_eq!(e.control_point, 0);
        assert_eq!((e.min_periodic_delay, e.max_periodic_delay), (1.0, 2.0));
        assert_eq!(
            (e.min_periodic_duration, e.max_periodic_duration),
            (2.0, 3.0)
        );
    }

    #[test]
    fn instance_override_defaults() {
        let absent = ParticleObject::from_json(&json!({"particle": "p.json"}));
        assert!(!absent.instance_override.present);
        assert_eq!(absent.instance_override.color.value, Value::Number(1.0));
        assert!(!absent.instance_override.enabled.bool());
        let present = ParticleObject::from_json(&json!({"instanceoverride": {"alpha": 0.5}}));
        let io = &present.instance_override;
        assert!(io.present && io.enabled.bool());
        assert_eq!(io.alpha.f32(), 0.5);
        assert_eq!(io.color.value, Value::Vec3(Vec3::ONE));
        assert_eq!(io.color.vec4(), glam::Vec4::new(1.0, 1.0, 1.0, 0.0));
        assert_eq!(absent.scale.vec3(), Vec3::ONE);
        assert!(absent.visible.bool());
    }

    #[test]
    fn serde_entry_point() {
        let d: ParticleDefinition =
            serde_json::from_str(r#"{"maxcount": 12.7, "flags": 4, "material": "m.json"}"#)
                .unwrap();
        assert_eq!(d.max_count, 12);
        assert!(d.perspective());
        assert_eq!(d.material.as_deref(), Some("m.json"));
    }

    #[test]
    fn control_points_and_children() {
        let d = ParticleDefinition::from_json(&json!({
            "controlpoint": [{"id": 1, "flags": 3, "offset": "10 20"}, {"offset": [1, 2, 3]}],
            "children": [{"name": "c", "maxcount": 5}]
        }));
        assert_eq!(d.control_points[0].id, 1);
        assert_eq!(d.control_points[0].flags, 3);
        assert_eq!(d.control_points[0].offset, Vec3::new(10.0, 20.0, 0.0));
        assert_eq!(d.control_points[1].id, -1);
        assert_eq!(d.control_points[1].offset, Vec3::ZERO);
        assert_eq!(d.children[0].max_count, 5);
        assert_eq!(d.children[0].kind, "static");
        assert_eq!(d.children[0].scale, Vec3::ONE);
    }
}

//! Child particle systems (spec 05 A6.5): static children and children created on particle
//! events (`eventspawn`, `eventdeath`, `eventfollow`).
//!
//! Every instance an entry can ever need (`maxcount`, or 1 for `static`) is built when the parent
//! is created; events recycle them, so the frame loop does not allocate.

use crate::defaults as d;
use crate::model::{ChildDef, ChildKind, ParticleDefinition};
use crate::sheet::{SpriteSheet, TextureInfo};
use crate::system::ParticleSystem;
use glam::{Mat4, Vec3};

/// What a child definition loader returns for a child path (`ChildDef::name`).
#[derive(Clone, Debug, Default)]
pub struct ChildAssets {
    pub definition: ParticleDefinition,
    /// Sprite sheet of the child's main texture.
    pub sprite_sheet: SpriteSheet,
    /// Main texture dimensions of the child's material.
    pub texture: Option<TextureInfo>,
}

/// Upper bound of instances per event entry (memory guard).
pub(crate) const MAX_INSTANCES: i64 = 256;

#[derive(Debug)]
pub(crate) struct ChildInstance {
    pub sys: Box<ParticleSystem>,
    pub active: bool,
    /// Attachment point in the parent's local space.
    pub anchor: Vec3,
    /// Parent particle tracked by id (`eventspawn` / `eventfollow`).
    pub follow: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ChildEntry {
    pub def: ChildDef,
    pub kind: ChildKind,
    /// `T(origin) · Rz(−angles.z) · Ry(angles.y) · Rx(−angles.x) · S(scale)` (§2.3 convention).
    pub attach_base: Mat4,
    pub instances: Vec<ChildInstance>,
}

impl ChildEntry {
    pub fn new(def: ChildDef, instances: Vec<ChildInstance>) -> Self {
        let a = def.angles;
        let attach_base = Mat4::from_translation(def.origin)
            * Mat4::from_rotation_z(-a.z)
            * Mat4::from_rotation_y(a.y)
            * Mat4::from_rotation_x(-a.x)
            * Mat4::from_scale(def.scale);
        Self {
            kind: def.child_kind(),
            def,
            attach_base,
            instances,
        }
    }

    /// Number of instances to build for this entry.
    pub fn instance_count(def: &ChildDef) -> usize {
        match def.child_kind() {
            ChildKind::Static => 1,
            _ => def.max_count.clamp(0, MAX_INSTANCES) as usize, // lumen: memory guard.
        }
    }

    /// `controlpointstartindex` (`null` → 0), clamped to the 8 slots.
    pub fn cp_start(&self) -> usize {
        self.def.control_point_start_index.clamp(0, 7) as usize
    }

    /// True when the child's control points follow the parent's particles.
    pub fn sets_control_points(&self) -> bool {
        self.def.flags & d::CHILD_FLAG_SET_CONTROL_POINTS != 0
    }

    /// Model matrix of an instance relative to the parent's local space.
    pub fn attach(&self, anchor: Vec3) -> Mat4 {
        Mat4::from_translation(anchor) * self.attach_base
    }
}

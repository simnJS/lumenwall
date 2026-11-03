//! Values handed from a parent particle to a child system instance (spec 05 A6.5, item 10).

use crate::model::InheritMode;
use crate::pool::Pool;
use glam::{Vec3, Vec3A};

/// Snapshot of the parent particle associated with a child instance, in the parent's local space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParentParticle {
    pub position: Vec3,
    pub velocity: Vec3,
    pub color: Vec3,
    pub alpha: f32,
    pub size: f32,
    pub rotation: Vec3,
    pub angular_velocity: Vec3,
}

impl ParentParticle {
    pub(crate) fn from_pool(pool: &Pool, i: usize) -> Self {
        Self {
            position: pool.pos[i].into(),
            velocity: pool.vel[i].into(),
            color: pool.color[i].into(),
            alpha: pool.alpha[i],
            size: pool.size[i],
            rotation: pool.rot[i].into(),
            angular_velocity: pool.angvel[i].into(),
        }
    }
}

/// Applies an inheritance option to particle `i`; `initial` also updates the initial values
/// (initializer form).
pub(crate) fn apply_inherit(
    mode: InheritMode,
    parent: &ParentParticle,
    pool: &mut Pool,
    i: usize,
    initial: bool,
) {
    let pc = Vec3A::from(parent.color);
    let set_color = |pool: &mut Pool, c: Vec3A| {
        pool.color[i] = c;
        if initial {
            pool.init_color[i] = c;
        }
    };
    match mode {
        InheritMode::SetColor => set_color(pool, pc),
        InheritMode::MultiplyColor => set_color(pool, pool.color[i] * pc),
        InheritMode::SetOpacity => set_alpha(pool, i, parent.alpha, initial),
        InheritMode::MultiplyOpacity => set_alpha(pool, i, pool.alpha[i] * parent.alpha, initial),
        InheritMode::SetColorOpacity => {
            set_color(pool, pc);
            set_alpha(pool, i, parent.alpha, initial);
        }
        InheritMode::MultiplyColorOpacity => {
            set_color(pool, pool.color[i] * pc);
            set_alpha(pool, i, pool.alpha[i] * parent.alpha, initial);
        }
        InheritMode::SetVelocity => pool.vel[i] = parent.velocity.into(),
        InheritMode::AddVelocity => pool.vel[i] += Vec3A::from(parent.velocity),
        InheritMode::SetSize => set_size(pool, i, parent.size, initial),
        InheritMode::MultiplySize => set_size(pool, i, pool.size[i] * parent.size, initial),
        InheritMode::SetRotation => pool.rot[i] = parent.rotation.into(),
        InheritMode::AddRotation => pool.rot[i] += Vec3A::from(parent.rotation),
        InheritMode::SetAngularVelocity => pool.angvel[i] = parent.angular_velocity.into(),
        InheritMode::AddAngularVelocity => pool.angvel[i] += Vec3A::from(parent.angular_velocity),
    }
}

fn set_alpha(pool: &mut Pool, i: usize, a: f32, initial: bool) {
    pool.alpha[i] = a;
    if initial {
        pool.init_alpha[i] = a;
    }
}

fn set_size(pool: &mut Pool, i: usize, s: f32, initial: bool) {
    pool.size[i] = s;
    if initial {
        pool.init_size[i] = s;
    }
}

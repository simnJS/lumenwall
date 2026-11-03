//! Particle system runtime and per-frame loop (spec 05 §4, §5, §10, §13 and amendments A1, A3,
//! A6.1, A6.5).

use crate::children::{ChildAssets, ChildEntry, ChildInstance};
use crate::defaults as d;
use crate::emitter::{EmitterImage, EmitterRt};
use crate::event::ParentParticle;
use crate::initializer::{InitializerRt, SpawnCtx};
use crate::model::{
    AnimationMode, ChildKind, ControlPointDef, InitializerDef, OperatorDef, ParticleDefinition,
    ParticleObject, RemapTransform, RendererDef,
};
use crate::operator::{BlendScratch, OpCtx, OperatorRt, life};
use crate::pool::Pool;
use crate::render::RopeScratch;
use crate::rng::{Rng, mix64};
use crate::sheet::{SpriteSheet, TextureInfo};
use crate::value::{UserValue, Value};
use glam::{Mat4, Vec2, Vec3, Vec3A};
use serde_json::Value as Json;

/// Simulation step cap, seconds (§5 step 3).
pub const MAX_DT: f32 = 0.1;
/// Number of control point slots (§4.4).
pub const CONTROL_POINTS: usize = 8;
/// Pool size used when `maxcount × override.count` truncates to 0 (§4.1).
pub const DEFAULT_POOL: usize = 1000;
/// Safety cap on the pool size (not in the reference): a hostile `maxcount` must not exhaust memory.
pub const MAX_POOL: usize = 1 << 20;

/// A control point slot (§4.4, §13), in the system's local space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ControlPoint {
    pub position: Vec3,
    pub offset: Vec3,
    pub follows_mouse: bool,
    pub world_space: bool,
}

/// Creation parameters that do not come from the JSON.
#[derive(Clone, Debug, Default)]
pub struct SystemOptions {
    /// Scene orthographic projection size `W × H` at creation.
    pub width: f32,
    pub height: f32,
    /// Sprite sheet of the main texture (`SpriteSheet::default()` when none).
    pub sprite_sheet: SpriteSheet,
    /// Main texture dimensions, for `g_RenderVar1` (`None` without texture).
    pub texture: Option<TextureInfo>,
    /// Fixed RNG seed (tests, replays). `None`: random seed per system, like the reference.
    pub seed: Option<u64>,
}

/// Per-frame inputs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameInput {
    /// `t_g`: global application time, seconds (`g_Time`).
    pub time: f32,
    /// Current scene projection size `W × H`.
    pub width: f32,
    pub height: f32,
    /// Mouse position normalised in scene space, `[0,1]²`, origin top-left, Y down.
    pub mouse: Vec2,
}

/// Read-only view of the live particles, oldest first.
#[derive(Clone, Copy, Debug)]
pub struct ParticlesView<'a> {
    pub positions: &'a [Vec3A],
    pub velocities: &'a [Vec3A],
    pub rotations: &'a [Vec3A],
    pub angular_velocities: &'a [Vec3A],
    pub colors: &'a [Vec3A],
    pub alphas: &'a [f32],
    pub sizes: &'a [f32],
    pub lifetimes: &'a [f32],
    pub ages: &'a [f32],
    /// Sprite sheet frame (`-1` before the first animation step).
    pub frames: &'a [f32],
}

/// A particle system instance: CPU simulation state and everything needed to build its geometry.
#[derive(Debug)]
pub struct ParticleSystem {
    pub(crate) pool: Pool,
    emitters: Vec<EmitterRt>,
    initializers: Vec<InitializerRt>,
    operators: Vec<OperatorRt>,
    pub(crate) renderer: RendererDef,
    pub(crate) animation_mode: AnimationMode,
    anim_speed: f32,
    pub(crate) sheet: SpriteSheet,
    pub(crate) texture: Option<TextureInfo>,
    pub(crate) perspective: bool,
    /// Last `lifetimerandom` has `min == max` at creation (§7.4).
    pub(crate) uniform_lifetimes: bool,
    cps: [ControlPoint; CONTROL_POINTS],
    /// Control points as built at creation (recycled child instances start from them).
    cps_initial: [ControlPoint; CONTROL_POINTS],
    /// A6.1-e: positions at the previous simulated frame and velocities.
    cp_prev: [Vec3; CONTROL_POINTS],
    cp_velocity: [Vec3; CONTROL_POINTS],
    cp_history: bool,
    pub(crate) object: ParticleObject,
    rng: Rng,
    frame_salt: u64,
    t_ref: f32,
    /// `t_g` of the last processed frame.
    pub(crate) time: f32,
    size: Vec2,
    /// `O`: system origin in centred scene space (§2.2).
    pub(crate) origin: Vec3,
    pub(crate) rope: RopeScratch,
    /// A1: pre-simulation duration, step, and whether it ran.
    prewarm: f32,
    prewarm_step: f32,
    prewarmed: bool,
    /// Σ of the simulated `dt` (A6.3.1 `particlesystemtime`).
    system_time: f32,
    time_of_day: f32,
    lifetime_mul: f32,
    blend_scratch: BlendScratch,
    material: Option<String>,
    // Children (A6.5).
    children: Vec<ChildEntry>,
    /// Active instances `(entry, instance)` in creation order.
    child_order: Vec<(u16, u16)>,
    has_spawn_events: bool,
    has_death_events: bool,
    /// For a child instance: its model matrix relative to the parent's local space.
    pub(crate) child_attach: Option<Mat4>,
    /// For a child instance: the associated parent particle and whether it still lives.
    parent_particle: Option<ParentParticle>,
    parent_alive: bool,
    /// For an `eventfollow` instance whose particle died: emitters stop.
    emission_stopped: bool,
}

/// `O` from the object `origin` and the scene size (§2.2).
fn scene_origin(origin: Vec3, size: Vec2) -> Vec3 {
    Vec3::new(origin.x - size.x / 2.0, size.y / 2.0 - origin.y, origin.z)
}

/// A1: pre-simulation step for an application frame cap.
fn prewarm_step(fps: f32) -> f32 {
    let h = 1.0 / fps;
    if h.is_nan() {
        1.0 / d::PREWARM_FPS
    } else {
        h.clamp(d::PREWARM_STEP_MIN, d::PREWARM_STEP_MAX)
    }
}

impl ParticleSystem {
    /// Builds a system from its definition and scene object (§4). Allocates everything up front:
    /// the steady-state frame loop does not allocate. Child systems are not instantiated; use
    /// [`Self::new_with_children`] for them.
    pub fn new(def: &ParticleDefinition, object: &ParticleObject, opts: &SystemOptions) -> Self {
        let object = ParticleObject {
            particle: None,
            origin: object.origin.clone(),
            scale: object.scale.clone(),
            angles: object.angles.clone(),
            visible: object.visible.clone(),
            parallax_depth: object.parallax_depth.clone(),
            instance_override: object.instance_override.clone(),
        };
        Self::build(def, object, opts)
    }

    /// Like [`Self::new`], also instantiating the child systems (A6.5). `loader` returns the
    /// definition (and texture data) of a child path; children it cannot load are skipped.
    pub fn new_with_children(
        def: &ParticleDefinition,
        object: &ParticleObject,
        opts: &SystemOptions,
        loader: &mut dyn FnMut(&str) -> Option<ChildAssets>,
    ) -> Self {
        let mut sys = Self::new(def, object, opts);
        sys.load_children(def, opts, loader, 0);
        sys
    }

    fn build(def: &ParticleDefinition, mut object: ParticleObject, opts: &SystemOptions) -> Self {
        if def.overrides_disabled() {
            // A6.1-f: all five "disable overrides" bits set.
            let o = &mut object.instance_override;
            for v in [
                &mut o.colorn,
                &mut o.count,
                &mut o.lifetime,
                &mut o.size,
                &mut o.speed,
            ] {
                *v = UserValue::number(1.0);
            }
        }
        let over = &object.instance_override;
        let seed = opts.seed.unwrap_or_else(|| Rng::from_entropy().next_u64());
        let mut rng = Rng::seeded(seed);
        let frame_salt = mix64(seed ^ 0x5ca1_ab1e_f00d_d00d);

        // §4.1 / A3: N_pool = trunc(f32(maxcount) × count) with an f32 product.
        // parity: f32 multiplication then truncation (150 × 0.63999999 → 96); 0, negative or NaN → 1000.
        let n = (def.max_count as f32 * over.count.f32()).trunc();
        let cap = if n >= 1.0 {
            (n as usize).min(MAX_POOL) // lumen: memory guard against a hostile maxcount.
        } else {
            DEFAULT_POOL
        };

        let first_cp_mouse = def
            .control_points
            .first()
            .is_some_and(|c| c.flags & ControlPointDef::FLAG_MOUSE != 0);
        let (rate_mul, lifetime_mul) = (over.rate.f32(), over.lifetime.f32());
        let emitters = def
            .emitters
            .iter()
            .map(|e| EmitterRt::new(e.clone(), first_cp_mouse, rate_mul, lifetime_mul))
            .collect();
        let initializers: Vec<InitializerRt> = def
            .initializers
            .iter()
            .cloned()
            .map(InitializerRt::new)
            .collect();
        let uniform_lifetimes = def
            .initializers
            .iter()
            .rev()
            .find_map(|i| match i {
                InitializerDef::LifetimeRandom { min, max, .. } => Some(min.f32() == max.f32()),
                _ => None,
            })
            .unwrap_or(false);
        let has = |f: fn(&OperatorDef) -> bool| def.operators.iter().any(f);
        let noise_remap = |t: RemapTransform| {
            matches!(t, RemapTransform::SimplexNoise | RemapTransform::FbmNoise)
        };
        let needs_phase = def
            .operators
            .iter()
            .any(|o| matches!(o, OperatorDef::RemapValue(r) if noise_remap(r.transform)))
            || def.initializers.iter().any(
                |i| matches!(i, InitializerDef::RemapInitialValue(r) if noise_remap(r.transform)),
            );
        let needs_seq =
            has(|o| matches!(o, OperatorDef::MaintainDistanceBetweenControlPoints { .. }));
        let has_spawn_events = def.children.iter().any(|c| {
            matches!(
                c.child_kind(),
                ChildKind::EventSpawn | ChildKind::EventFollow
            )
        });
        let pool = Pool::new(
            cap,
            has(|o| matches!(o, OperatorDef::OscillateAlpha(_))),
            has(|o| matches!(o, OperatorDef::OscillateSize(_))),
            has(|o| matches!(o, OperatorDef::OscillatePosition(_))),
        )
        .with_extras(has_spawn_events, needs_seq, needs_phase);
        let slots = pool.slots();
        // §4.5: the only draws at creation are the turbulence ones, in operator order.
        let operators: Vec<OperatorRt> = def
            .operators
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let blend = def.operator_blending.get(i).cloned().flatten();
                OperatorRt::with_blend(o.clone(), blend, &mut rng, slots)
            })
            .collect();
        let blend_scratch = if operators.iter().any(|o| o.blend.is_some()) {
            BlendScratch::with_capacity(slots)
        } else {
            BlendScratch::default()
        };

        let size = Vec2::new(opts.width, opts.height);
        let origin = scene_origin(object.origin.vec3(), size);
        let mut cps = [ControlPoint::default(); CONTROL_POINTS];
        for c in &def.control_points {
            let Some(slot) = usize::try_from(c.id).ok().and_then(|i| cps.get_mut(i)) else {
                continue;
            };
            let follows_mouse = c.flags & ControlPointDef::FLAG_MOUSE != 0;
            let world_space = c.flags & ControlPointDef::FLAG_WORLD_SPACE != 0;
            // parity: offsets are not Y-flipped, unlike emitter origins.
            let position = match (follows_mouse, world_space) {
                (true, _) => Vec3::ZERO,
                (false, true) => c.offset - origin,
                (false, false) => c.offset,
            };
            *slot = ControlPoint {
                position,
                offset: c.offset,
                follows_mouse,
                world_space,
            };
        }

        let rope = if def.renderer.kind.is_rope() {
            RopeScratch::with_capacity((cap - 1) * def.renderer.sigma() as usize + 1)
        } else {
            RopeScratch::default()
        };

        let mut sys = Self {
            pool,
            emitters,
            initializers,
            operators,
            renderer: def.renderer.clone(),
            animation_mode: def.animation_mode,
            anim_speed: if def.sequence_multiplier > 0.0 {
                def.sequence_multiplier
            } else {
                1.0
            },
            sheet: opts.sprite_sheet,
            texture: opts.texture,
            perspective: def.perspective(),
            uniform_lifetimes,
            cps,
            cps_initial: cps,
            cp_prev: [Vec3::ZERO; CONTROL_POINTS],
            cp_velocity: [Vec3::ZERO; CONTROL_POINTS],
            cp_history: false,
            object,
            rng,
            frame_salt,
            t_ref: 0.0,
            time: 0.0,
            size,
            origin,
            rope,
            prewarm: def.start_time_secs,
            prewarm_step: prewarm_step(d::PREWARM_FPS),
            prewarmed: false,
            system_time: 0.0,
            time_of_day: 0.0,
            lifetime_mul,
            blend_scratch,
            material: def.material.clone(),
            children: Vec::new(),
            child_order: Vec::new(),
            has_spawn_events: false,
            has_death_events: false,
            child_attach: None,
            parent_particle: None,
            parent_alive: false,
            emission_stopped: false,
        };
        sys.apply_control_point_overrides();
        sys.cps_initial = sys.cps;
        sys
    }

    /// Instantiates the child entries of `def` (A6.5) with every instance they can need.
    fn load_children(
        &mut self,
        def: &ParticleDefinition,
        opts: &SystemOptions,
        loader: &mut dyn FnMut(&str) -> Option<ChildAssets>,
        depth: u32,
    ) {
        if depth >= d::CHILD_MAX_DEPTH {
            // lumen: guard against self-referencing child definitions.
            return;
        }
        for cdef in &def.children {
            let Some(assets) = loader(cdef.path()) else {
                tracing::warn!(
                    path = cdef.path(),
                    "particle child system not found, ignored"
                );
                continue;
            };
            let count = ChildEntry::instance_count(cdef);
            let mut instances = Vec::with_capacity(count);
            for _ in 0..count {
                // The child receives the parent's instance override and visibility (A6.5 item 1).
                let mut over = self.object.instance_override.clone();
                over.control_points = Default::default();
                let object = ParticleObject {
                    particle: None,
                    origin: UserValue::from_vec3(Vec3::ZERO),
                    scale: UserValue::from_vec3(Vec3::ONE),
                    angles: UserValue::from_vec3(Vec3::ZERO),
                    visible: self.object.visible.clone(),
                    parallax_depth: UserValue::new(Value::Vec2(Vec2::ZERO)),
                    instance_override: over,
                };
                let child_opts = SystemOptions {
                    width: opts.width,
                    height: opts.height,
                    sprite_sheet: assets.sprite_sheet,
                    texture: assets.texture,
                    seed: Some(self.rng.next_u64()),
                };
                let mut sys = Self::build(&assets.definition, object, &child_opts);
                sys.load_children(&assets.definition, &child_opts, loader, depth + 1);
                sys.child_attach = Some(Mat4::IDENTITY);
                instances.push(ChildInstance {
                    sys: Box::new(sys),
                    active: false,
                    anchor: Vec3::ZERO,
                    follow: None,
                });
            }
            self.children.push(ChildEntry::new(cdef.clone(), instances));
        }
        let total: usize = self.children.iter().map(|e| e.instances.len()).sum();
        self.child_order = Vec::with_capacity(total);
        self.has_spawn_events = self
            .children
            .iter()
            .any(|e| matches!(e.kind, ChildKind::EventSpawn | ChildKind::EventFollow));
        self.has_death_events = self
            .children
            .iter()
            .any(|e| e.kind == ChildKind::EventDeath);
        if self.has_spawn_events && self.pool.ids.is_empty() {
            let slots = self.pool.slots();
            self.pool.ids = vec![0; slots];
        }
        // Static children exist as long as the parent.
        for (e, entry) in self.children.iter_mut().enumerate() {
            if entry.kind == ChildKind::Static {
                let attach = entry.attach(Vec3::ZERO);
                if let Some(inst) = entry.instances.first_mut() {
                    inst.active = true;
                    inst.sys.child_attach = Some(attach);
                    self.child_order.push((e as u16, 0));
                }
            }
        }
    }

    /// Back to the creation state (recycled child instance). The RNG keeps running.
    fn reset(&mut self) {
        self.pool.clear();
        let lifetime_mul = self.lifetime_mul;
        for e in &mut self.emitters {
            e.reset(lifetime_mul);
        }
        for i in &mut self.initializers {
            i.reset();
        }
        self.cps = self.cps_initial;
        self.cp_history = false;
        self.cp_velocity = [Vec3::ZERO; CONTROL_POINTS];
        self.system_time = 0.0;
        self.parent_particle = None;
        self.parent_alive = false;
        self.emission_stopped = false;
        for entry in &mut self.children {
            for (k, inst) in entry.instances.iter_mut().enumerate() {
                inst.active = entry.kind == ChildKind::Static && k == 0;
                if inst.active {
                    inst.sys.reset();
                }
            }
        }
        self.child_order
            .retain(|&(e, _)| self.children[e as usize].kind == ChildKind::Static);
    }

    /// Runs one frame (§5 steps 1–11). Returns `true` when the system must be drawn this frame
    /// (then call `write_vertices`, which writes nothing when there is nothing to draw).
    pub fn update(&mut self, input: &FrameInput) -> bool {
        // 1. Invisible: no simulation, no draw, the reference time does not advance.
        if !self.object.visible.bool() {
            return false;
        }
        self.time = input.time;
        // 2. First frame.
        // parity: "first frame" means t_ref == 0 exactly, so a frame at t_g = 0 keeps the next one first too.
        if self.t_ref == 0.0 {
            self.t_ref = input.time;
            if !self.prewarmed {
                self.prewarmed = true;
                self.prewarm(input);
            }
            return true;
        }
        // 3. Time step.
        let dt = input.time - self.t_ref;
        self.t_ref = input.time;
        if dt > 0.0 {
            // parity: dt is capped at 0.1 s (slow motion below 10 FPS, 0.1 s after a pause).
            self.simulate(dt.min(MAX_DT), input);
        }
        true
    }

    /// A1: `N` steps of `h` ending at `t_g`, during the first processed frame, then the draw.
    fn prewarm(&mut self, input: &FrameInput) {
        let t0 = self.prewarm;
        if t0 <= 0.0 {
            return;
        }
        let h = self.prewarm_step;
        let n = ((t0 / h).round() as u32).max(1);
        for k in 1..=n {
            let t = input.time - (n - k) as f32 * h;
            self.simulate(h, &FrameInput { time: t, ..*input });
        }
    }

    /// A6.1-d: `instanceoverride.controlpointN` replaces the offset of control point N.
    fn apply_control_point_overrides(&mut self) {
        for (cp, value) in self
            .cps
            .iter_mut()
            .zip(&self.object.instance_override.control_points)
        {
            if let Some(v) = value {
                cp.offset = v.vec3();
                if !cp.follows_mouse {
                    cp.position = if cp.world_space {
                        cp.offset - self.origin
                    } else {
                        cp.offset
                    };
                }
            }
        }
    }

    fn simulate(&mut self, dt: f32, input: &FrameInput) {
        self.system_time += dt;
        // 4. Resolution change.
        let size = Vec2::new(input.width, input.height);
        if size != self.size {
            self.size = size;
            if self.child_attach.is_none() {
                self.origin = scene_origin(self.object.origin.vec3(), size);
            }
            for cp in &mut self.cps {
                if !cp.follows_mouse && cp.world_space {
                    cp.position = cp.offset - self.origin;
                }
            }
            // A6.1-e: no velocity on the frame of a resolution change.
            self.cp_history = false;
        }
        self.apply_control_point_overrides();
        // 5. Mouse control points.
        let (w, h) = (size.x, size.y);
        let mouse = Vec3::new(
            input.mouse.x * w - w / 2.0,
            h / 2.0 - input.mouse.y * h,
            0.0,
        );
        for cp in &mut self.cps {
            if cp.follows_mouse {
                // parity: no compensation of the system's scale, rotation or parallax.
                cp.position = mouse + cp.offset - self.origin;
            }
        }
        // A6.1-e: control point velocities.
        let prev = if self.cp_history {
            self.cp_prev
        } else {
            self.cps.map(|c| c.position)
        };
        for k in 0..CONTROL_POINTS {
            let p = self.cps[k].position;
            self.cp_velocity[k] = if self.cp_history {
                (p - self.cp_prev[k]) / dt
            } else {
                Vec3::ZERO
            };
            self.cp_prev[k] = p;
        }
        self.cp_history = true;
        // 6. Emitters, each applying all initializers to its new particles.
        let len_before = self.pool.len;
        if !self.emission_stopped {
            let ctx = SpawnCtx {
                cps: &self.cps,
                cp_velocity: &self.cp_velocity,
                over: &self.object.instance_override,
                time: input.time,
                system_time: self.system_time,
                time_of_day: self.time_of_day,
                perspective: self.perspective,
                origin: self.origin,
                parent: self.parent_particle.as_ref(),
            };
            for (k, e) in self.emitters.iter_mut().enumerate() {
                e.emit(
                    k as u32,
                    dt,
                    &mut self.pool,
                    &mut self.initializers,
                    &ctx,
                    &mut self.rng,
                );
            }
        }
        if self.has_spawn_events && self.pool.len > len_before {
            let r = self.pool.range();
            for i in r.end - (self.pool.len - len_before)..r.end {
                self.particle_event(i, false);
            }
        }
        // 7. Ageing, newborns included.
        let live = self.pool.range();
        for a in &mut self.pool.age[live] {
            *a += dt;
        }
        // 8. Operators.
        let octx = OpCtx {
            dt,
            time: input.time,
            speed: self.object.instance_override.speed.f32(),
            cps: &self.cps,
            cp_prev: &prev,
            system_time: self.system_time,
            time_of_day: self.time_of_day,
            scene_size: size,
            origin: self.origin,
            perspective: self.perspective,
            parent: if self.parent_alive {
                self.parent_particle.as_ref()
            } else {
                None
            },
        };
        for op in &mut self.operators {
            op.run_blended(
                &mut self.pool,
                &octx,
                &mut self.rng,
                &mut self.blend_scratch,
            );
        }
        // 9. Sprite sheet.
        if self.sheet.frames > 0 {
            self.animate();
        }
        // 10. Death events, then stable compaction.
        if self.has_death_events {
            for i in self.pool.range() {
                if self.pool.age[i] >= self.pool.lifetime[i] {
                    self.particle_event(i, true);
                }
            }
        }
        self.pool.compact();
        // Children, after the parent, in creation order (A6.5 item 9).
        if !self.child_order.is_empty() {
            self.step_children(dt, input);
        }
    }

    /// Spawn (`death == false`) or death event of the particle in slot `i` (A6.5 items 4–6).
    fn particle_event(&mut self, i: usize, death: bool) {
        let parent = ParentParticle::from_pool(&self.pool, i);
        let id = self.pool.ids.get(i).copied();
        for e in 0..self.children.len() {
            let kind = self.children[e].kind;
            let wanted = match kind {
                ChildKind::EventDeath => death,
                ChildKind::EventSpawn | ChildKind::EventFollow => !death,
                ChildKind::Static => false,
            };
            if !wanted {
                continue;
            }
            let probability = self.children[e].def.probability;
            if self.rng.uniform(0.0, 1.0) >= probability {
                continue;
            }
            let entry = &mut self.children[e];
            // Lost when `maxcount` instances are alive.
            let Some(k) = entry.instances.iter().position(|inst| !inst.active) else {
                continue;
            };
            let attach = entry.attach(parent.position);
            let inst = &mut entry.instances[k];
            inst.sys.reset();
            inst.active = true;
            inst.anchor = parent.position;
            inst.follow = if death { None } else { id };
            inst.sys.child_attach = Some(attach);
            inst.sys.parent_particle = Some(parent);
            inst.sys.parent_alive = !death;
            inst.sys.origin = self.origin + attach.transform_point3(Vec3::ZERO);
            if self.child_order.len() < self.child_order.capacity() {
                self.child_order.push((e as u16, k as u16));
            }
        }
    }

    /// Simulates the active child instances (A6.5 items 6–9).
    fn step_children(&mut self, dt: f32, input: &FrameInput) {
        let Self {
            children,
            child_order,
            pool,
            origin,
            ..
        } = self;
        let mut ended = false;
        for &(e, k) in child_order.iter() {
            let entry = &mut children[e as usize];
            let kind = entry.kind;
            let sets_cps = entry.sets_control_points();
            let cp_start = entry.cp_start();
            let attach_base = entry.attach_base;
            let inst = &mut entry.instances[k as usize];
            // Track the parent particle (spawn / follow).
            if let Some(id) = inst.follow {
                match pool.find_id(id) {
                    Some(li) => {
                        let snap = ParentParticle::from_pool(pool, pool.head + li);
                        if kind == ChildKind::EventFollow {
                            inst.anchor = snap.position;
                        }
                        inst.sys.parent_particle = Some(snap);
                    }
                    None => {
                        inst.follow = None;
                        inst.sys.parent_alive = false;
                        if kind == ChildKind::EventFollow {
                            inst.sys.emission_stopped = true;
                        }
                    }
                }
            }
            let attach = Mat4::from_translation(inst.anchor) * attach_base;
            inst.sys.child_attach = Some(attach);
            inst.sys.origin = *origin + attach.transform_point3(Vec3::ZERO);
            if sets_cps {
                let inv = attach.inverse();
                if kind == ChildKind::Static {
                    let src = pool.live_ref().pos;
                    for (slot, p) in inst.sys.cps.iter_mut().skip(cp_start).zip(src) {
                        slot.position = inv.transform_point3(Vec3::from(*p));
                    }
                } else {
                    let p = inst
                        .sys
                        .parent_particle
                        .map_or(inst.anchor, |pp| pp.position);
                    inst.sys.cps[cp_start].position = inv.transform_point3(p);
                }
            }
            inst.sys.time = input.time;
            inst.sys.simulate(dt, input);
            // Instance end: no live particle and no emitter able to emit.
            if kind != ChildKind::Static && inst.sys.is_empty() && !inst.sys.can_emit() {
                inst.active = false;
                ended = true;
            }
        }
        if ended {
            child_order.retain(|&(e, k)| children[e as usize].instances[k as usize].active);
        }
    }

    /// True while at least one emitter can still emit.
    fn can_emit(&self) -> bool {
        !self.emission_stopped && self.emitters.iter().any(EmitterRt::can_emit)
    }

    /// Sprite sheet frame update (§10).
    fn animate(&mut self) {
        let f = self.sheet.frames as f32;
        let speed = self.anim_speed;
        let head = self.pool.head;
        let p = self.pool.live();
        match self.animation_mode {
            AnimationMode::RandomFrame => {
                for (i, frame) in p.frame.iter_mut().enumerate() {
                    if *frame < 0.0 {
                        // parity: the reference seeds a local generator from the particle's slot, so a
                        // given slot always yields the same frame during a run.
                        let h = mix64(self.frame_salt ^ (head + i) as u64);
                        *frame = (((h >> 32) * u64::from(self.sheet.frames)) >> 32) as f32;
                    }
                }
            }
            AnimationMode::Once => {
                for ((frame, &age), &lifetime) in
                    p.frame.iter_mut().zip(p.age.iter()).zip(p.lifetime)
                {
                    *frame = (life(age, lifetime) * f * speed).min(f - 1.0);
                }
            }
            AnimationMode::Sequence => {
                let t = self.sheet.duration;
                if t > 0.0 {
                    for (frame, &age) in p.frame.iter_mut().zip(p.age.iter()) {
                        *frame = ((age * speed) % t / t * f) % f;
                    }
                } else {
                    for ((frame, &age), &lifetime) in
                        p.frame.iter_mut().zip(p.age.iter()).zip(p.lifetime)
                    {
                        *frame = (life(age, lifetime) * f * speed) % f;
                    }
                }
            }
        }
    }

    /// Number of live particles.
    pub fn len(&self) -> usize {
        self.pool.len
    }

    /// True when no particle is alive.
    pub fn is_empty(&self) -> bool {
        self.pool.len == 0
    }

    /// Pool size `N_pool`.
    pub fn capacity(&self) -> usize {
        self.pool.cap
    }

    /// Live particles, oldest first.
    pub fn particles(&self) -> ParticlesView<'_> {
        let p = self.pool.live_ref();
        ParticlesView {
            positions: p.pos,
            velocities: p.vel,
            rotations: p.rot,
            angular_velocities: p.angvel,
            colors: p.color,
            alphas: p.alpha,
            sizes: p.size,
            lifetimes: p.lifetime,
            ages: p.age,
            frames: p.frame,
        }
    }

    /// Control point slots.
    pub fn control_points(&self) -> &[ControlPoint; CONTROL_POINTS] {
        &self.cps
    }

    /// Control point velocities of the last simulated frame (A6.1-e).
    pub fn control_point_velocities(&self) -> &[Vec3; CONTROL_POINTS] {
        &self.cp_velocity
    }

    /// Scene-object values (origin, scale, angles, visible, parallax depth, instance override).
    pub fn object(&self) -> &ParticleObject {
        &self.object
    }

    /// Mutable scene-object values, e.g. for SceneScript writes to `scale`, `angles`, `visible`,
    /// `parallaxDepth`, `instance.controlpointN`. `origin`, `instanceoverride.rate` and `.count`
    /// are only read at creation (and `origin` on resolution change).
    pub fn object_mut(&mut self) -> &mut ParticleObject {
        &mut self.object
    }

    /// Material path of the definition (the renderer loads it; for child systems too).
    pub fn material(&self) -> Option<&str> {
        self.material.as_deref()
    }

    /// A1: sets the pre-simulation step from the application frame cap (default 30 FPS; the step
    /// is clamped to `[1/240, 0.1]` s). Call before the first `update`.
    pub fn set_prewarm_fps(&mut self, fps: f32) {
        self.prewarm_step = prewarm_step(fps);
        for entry in &mut self.children {
            for inst in &mut entry.instances {
                inst.sys.set_prewarm_fps(fps);
            }
        }
    }

    /// Pre-simulation duration (`starttime`, A1), seconds.
    pub fn prewarm_seconds(&self) -> f32 {
        self.prewarm
    }

    /// A6.3.1 `timeofday` input: local time of day as a fraction of 24 h (`[0, 1)`).
    pub fn set_time_of_day(&mut self, fraction: f32) {
        self.time_of_day = fraction;
        for entry in &mut self.children {
            for inst in &mut entry.instances {
                inst.sys.set_time_of_day(fraction);
            }
        }
    }

    /// Σ of the simulated time steps since creation, pre-simulation included.
    pub fn system_time(&self) -> f32 {
        self.system_time
    }

    /// A6.4: emission bitmap of the `layerimage` emitter at `emitter_index` (index in the
    /// definition's `emitter` list after unknown entries were dropped). Returns `false` when that
    /// emitter does not exist or is not a `layerimage`.
    pub fn set_emitter_image(&mut self, emitter_index: usize, image: EmitterImage) -> bool {
        match self.emitters.get_mut(emitter_index) {
            Some(e) if e.def.kind == crate::model::EmitterKind::LayerImage => {
                e.image = Some(Box::new(image));
                true
            }
            _ => false,
        }
    }

    /// Active child instances in creation order (A6.5); draw each right after this system, with
    /// [`Self::child_uniforms`]. Children can have children of their own.
    pub fn children(&self) -> impl Iterator<Item = &ParticleSystem> + '_ {
        self.child_order
            .iter()
            .map(|&(e, k)| &*self.children[e as usize].instances[k as usize].sys)
    }

    /// Calls `f` on every active child instance, in creation order (e.g. to write its vertices).
    pub fn for_each_child_mut(&mut self, f: &mut dyn FnMut(&mut ParticleSystem)) {
        let Self {
            children,
            child_order,
            ..
        } = self;
        for &(e, k) in child_order.iter() {
            f(&mut children[e as usize].instances[k as usize].sys);
        }
    }

    /// Number of active child instances (direct children only).
    pub fn child_count(&self) -> usize {
        self.child_order.len()
    }

    /// True for a child instance (its model matrix is relative to its parent).
    pub fn is_child(&self) -> bool {
        self.child_attach.is_some()
    }

    /// Visits every [U]/[C] value of the system (object, override, initializers, operators).
    pub fn visit_values_mut(&mut self, f: &mut dyn FnMut(&mut UserValue)) {
        let o = &mut self.object;
        for v in [
            &mut o.origin,
            &mut o.scale,
            &mut o.angles,
            &mut o.visible,
            &mut o.parallax_depth,
        ] {
            f(v);
        }
        o.instance_override.visit(f);
        for i in &mut self.initializers {
            i.def.visit(f);
        }
        for op in &mut self.operators {
            op.def.visit(f);
            if let Some(b) = &mut op.blend {
                b.visit(f);
            }
        }
        for entry in &mut self.children {
            for inst in &mut entry.instances {
                inst.sys.visit_values_mut(f);
            }
        }
    }

    /// Sets every value bound to user property `name`. Returns how many values were updated.
    pub fn set_user_property(&mut self, name: &str, value: &Value) -> usize {
        let mut count = 0;
        self.visit_values_mut(&mut |v| {
            if v.property() == Some(name) {
                v.set(value.clone());
                count += 1;
            }
        });
        count
    }

    /// Like [`Self::set_user_property`] from a raw JSON property value, parsed with each bound
    /// value's own rules ([U] or [C]).
    pub fn set_user_property_json(&mut self, name: &str, value: &Json) -> usize {
        let mut count = 0;
        self.visit_values_mut(&mut |v| {
            if v.property() == Some(name) {
                v.set_from_json(value);
                count += 1;
            }
        });
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::children::ChildAssets;
    use crate::emitter::EmitterImage;
    use serde_json::json;

    fn system(
        def: serde_json::Value,
        obj: serde_json::Value,
        sheet: SpriteSheet,
    ) -> ParticleSystem {
        let def = ParticleDefinition::from_json(&def);
        let obj = ParticleObject::from_json(&obj);
        let opts = SystemOptions {
            width: 800.0,
            height: 600.0,
            sprite_sheet: sheet,
            texture: None,
            seed: Some(1),
        };
        ParticleSystem::new(&def, &obj, &opts)
    }

    fn frame(t: f32) -> FrameInput {
        FrameInput {
            time: t,
            width: 800.0,
            height: 600.0,
            mouse: Vec2::ZERO,
        }
    }

    #[test]
    fn pool_size() {
        let s = system(
            json!({"maxcount": 10}),
            json!({"instanceoverride": {"count": 2.55}}),
            SpriteSheet::default(),
        );
        assert_eq!(s.capacity(), 25);
        let s = system(
            json!({"maxcount": 10}),
            json!({"instanceoverride": {"count": 0.05}}),
            SpriteSheet::default(),
        );
        assert_eq!(s.capacity(), DEFAULT_POOL);
        let s = system(json!({"maxcount": 0}), json!({}), SpriteSheet::default());
        assert_eq!(s.capacity(), DEFAULT_POOL);
    }

    #[test]
    fn frame_loop_first_frame_dt_cap_and_ageing() {
        let mut s = system(
            json!({"emitter": [{"name": "boxrandom", "rate": 100, "instantaneous": 1}],
                   "initializer": [{"name": "lifetimerandom", "min": 10, "max": 10}]}),
            json!({}),
            SpriteSheet::default(),
        );
        // t = 0: t_ref stays 0 → next frame is "first" again.
        assert!(s.update(&frame(0.0)));
        assert!(s.is_empty());
        assert!(s.update(&frame(1.0)));
        assert!(s.is_empty(), "first frame: no simulation");
        // dt = 5 s capped at 0.1 → burst 1 + 100 × 0.1 = 10.
        assert!(s.update(&frame(6.0)));
        assert_eq!(s.len(), 11);
        // Newborns are aged in the same frame.
        assert!(s.particles().ages.iter().all(|&a| a == MAX_DT));
        // dt <= 0: no simulation.
        assert!(s.update(&frame(6.0)));
        assert!(s.update(&frame(5.0)));
        assert_eq!(s.len(), 11);
        // Invisible: nothing, time frozen.
        s.object_mut().visible.set(Value::Bool(false));
        assert!(!s.update(&frame(100.0)));
        s.object_mut().visible.set(Value::Bool(true));
        assert!(s.update(&frame(100.05)));
        // t_ref was 5.0 (invisible frames do not advance it) → dt capped to 0.1 → +10 particles.
        assert_eq!(s.len(), 21);
    }

    #[test]
    fn death_is_stable_and_after_operators() {
        let mut s = system(
            json!({"emitter": [{"name": "boxrandom", "rate": 8, "distancemax": "0 0 0"}],
                   "initializer": [{"name": "lifetimerandom", "min": 0.3, "max": 0.3}]}),
            json!({}),
            SpriteSheet::default(),
        );
        s.update(&frame(1.0));
        for k in 1..=10 {
            s.update(&frame(1.0 + k as f32 * 0.125));
        }
        // One particle per frame, lifetime 0.3 → ages 0.125, 0.25 alive; 0.375 dead.
        assert_eq!(s.len(), 2);
        let ages = s.particles().ages;
        assert!(ages[0] > ages[1], "oldest first");
    }

    #[test]
    fn lifetime_zero_never_drawn() {
        let mut s = system(
            json!({"emitter": [{"name": "boxrandom", "rate": 0, "instantaneous": 5}],
                   "initializer": [{"name": "lifetimerandom", "min": 0, "max": 0}]}),
            json!({}),
            SpriteSheet::default(),
        );
        s.update(&frame(1.0));
        s.update(&frame(1.05));
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn control_points_world_space_and_mouse() {
        let mut s = system(
            json!({"controlpoint": [
                {"id": 0, "flags": 1, "offset": "10 0 0"},
                {"id": 1, "flags": 2, "offset": "5 5 0"},
                {"id": 2, "offset": "7 8 9"},
                {"id": 9, "offset": "1 1 1"}
            ]}),
            json!({"origin": "500 200 0"}),
            SpriteSheet::default(),
        );
        // O = (500 - 400, 300 - 200, 0) = (100, 100, 0).
        assert_eq!(s.origin, Vec3::new(100.0, 100.0, 0.0));
        assert_eq!(s.control_points()[1].position, Vec3::new(-95.0, -95.0, 0.0));
        assert_eq!(s.control_points()[2].position, Vec3::new(7.0, 8.0, 9.0));
        s.update(&frame(1.0));
        let mut f = frame(1.05);
        f.mouse = Vec2::new(0.75, 0.25);
        s.update(&f);
        // (0.75·800 - 400, 300 - 0.25·600, 0) + (10,0,0) - (100,100,0) = (110, 50, 0).
        assert_eq!(s.control_points()[0].position, Vec3::new(110.0, 50.0, 0.0));
        // Resolution change recomputes O and world-space points.
        let f = FrameInput {
            time: 1.1,
            width: 1000.0,
            height: 600.0,
            mouse: Vec2::ZERO,
        };
        s.update(&f);
        assert_eq!(s.origin, Vec3::new(0.0, 100.0, 0.0));
        assert_eq!(s.control_points()[1].position, Vec3::new(5.0, -95.0, 0.0));
        assert_eq!(s.control_points()[2].position, Vec3::new(7.0, 8.0, 9.0));
    }

    #[test]
    fn emitter_follows_control_point_zero() {
        let mut s = system(
            json!({"emitter": [{"name": "boxrandom", "rate": 0, "instantaneous": 1, "distancemax": "0 0 0", "origin": "0 30 0"}],
                   "controlpoint": [{"id": 0, "offset": "100 0 0"}],
                   "initializer": [{"name": "lifetimerandom", "min": 5, "max": 5}]}),
            json!({}),
            SpriteSheet::default(),
        );
        s.update(&frame(1.0));
        s.update(&frame(1.05));
        assert_eq!(s.particles().positions[0], Vec3A::new(100.0, -30.0, 0.0));
    }

    #[test]
    fn sprite_sheet_modes() {
        let sheet = SpriteSheet {
            columns: 2,
            rows: 2,
            frames: 4,
            duration: 2.0,
        };
        let base = json!({"emitter": [{"name": "boxrandom", "rate": 0, "instantaneous": 1}],
                          "initializer": [{"name": "lifetimerandom", "min": 4, "max": 4}]});
        // sequence with T_anim = 2: frame = fmod(fmod(age·k, 2) / 2 · 4, 4).
        let mut def = base.clone();
        def["sequencemultiplier"] = json!(2.0);
        let mut s = system(def, json!({}), sheet);
        s.update(&frame(1.0));
        s.update(&frame(1.1)); // age 0.1
        let f = s.particles().frames[0];
        assert!((f - (0.2f32 % 2.0) / 2.0 * 4.0).abs() < 1e-6, "{f}");
        // once: min(life · F · k, F - 1).
        let mut def = base.clone();
        def["animationmode"] = json!("once");
        let mut s = system(def, json!({}), sheet);
        s.update(&frame(1.0));
        s.update(&frame(1.1));
        assert!((s.particles().frames[0] - 0.1).abs() < 1e-6);
        for k in 2..40 {
            s.update(&frame(1.0 + k as f32 * 0.1));
        }
        assert_eq!(s.particles().frames[0], 3.0);
        // sequence without duration: fmod(life · F · k, F).
        let mut s = system(
            base.clone(),
            json!({}),
            SpriteSheet {
                duration: 0.0,
                ..sheet
            },
        );
        s.update(&frame(1.0));
        s.update(&frame(1.1));
        assert!((s.particles().frames[0] - 0.1).abs() < 1e-6);
        // randomframe: chosen once, integer, same slot → same frame.
        let mut def = base;
        def["animationmode"] = json!("randomframe");
        let mut s = system(def, json!({}), sheet);
        s.update(&frame(1.0));
        s.update(&frame(1.1));
        let f0 = s.particles().frames[0];
        assert!((0.0..4.0).contains(&f0) && f0.fract() == 0.0);
        s.update(&frame(1.2));
        assert_eq!(s.particles().frames[0], f0);
    }

    #[test]
    fn uniform_lifetimes_flag_uses_last_entry() {
        let s = system(
            json!({"initializer": [{"name": "lifetimerandom", "min": 1, "max": 2},
                                   {"name": "lifetimerandom", "min": 3, "max": 3}]}),
            json!({}),
            SpriteSheet::default(),
        );
        assert!(s.uniform_lifetimes);
        let s = system(
            json!({"initializer": [{"name": "lifetimerandom", "min": 3, "max": 3},
                                   {"name": "lifetimerandom", "min": 1, "max": 2}]}),
            json!({}),
            SpriteSheet::default(),
        );
        assert!(!s.uniform_lifetimes);
    }

    #[test]
    fn user_properties() {
        let mut s = system(
            json!({"operator": [{"name": "movement", "gravity": {"value": "0 10 0", "user": "grav"}}]}),
            json!({"scale": {"value": 2, "user": "grav"}, "instanceoverride": {"colorn": {"value": "1 1 1", "user": "tint"}}}),
            SpriteSheet::default(),
        );
        assert_eq!(
            s.set_user_property("grav", &Value::Vec3(Vec3::new(0.0, 5.0, 0.0))),
            2
        );
        assert_eq!(s.object().scale.vec3(), Vec3::new(0.0, 5.0, 0.0));
        assert_eq!(s.set_user_property_json("tint", &json!("0.5 0.5 0.5")), 1);
        assert_eq!(s.object().instance_override.colorn.vec3(), Vec3::splat(0.5));
        assert_eq!(s.set_user_property("nope", &Value::Null), 0);
    }

    #[test]
    fn full_pipeline_is_deterministic_with_seed() {
        let def = json!({
            "maxcount": 200,
            "emitter": [{"name": "sphererandom", "rate": 50, "distancemax": 50, "speedmin": 10, "speedmax": 20}],
            "initializer": [{"name": "colorrandom"}, {"name": "sizerandom"}, {"name": "lifetimerandom", "min": 1, "max": 2},
                            {"name": "turbulentvelocityrandom"}],
            "operator": [{"name": "movement", "gravity": "0 -50 0"}, {"name": "turbulence"},
                         {"name": "oscillatesize"}, {"name": "alphafade"}]
        });
        let mut a = system(def.clone(), json!({}), SpriteSheet::default());
        let mut b = system(def, json!({}), SpriteSheet::default());
        for k in 0..100 {
            let f = frame(1.0 + k as f32 / 60.0);
            a.update(&f);
            b.update(&f);
        }
        assert!(a.len() > 50);
        assert_eq!(a.particles().positions, b.particles().positions);
        assert!(a.particles().positions.iter().all(|p| p.is_finite()));
    }

    fn system_with(
        def: serde_json::Value,
        obj: serde_json::Value,
        children: &[(&str, serde_json::Value)],
    ) -> ParticleSystem {
        let def = ParticleDefinition::from_json(&def);
        let obj = ParticleObject::from_json(&obj);
        let opts = SystemOptions {
            width: 800.0,
            height: 600.0,
            seed: Some(5),
            ..SystemOptions::default()
        };
        let mut loader = |path: &str| {
            children
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, j)| ChildAssets {
                    definition: ParticleDefinition::from_json(j),
                    ..ChildAssets::default()
                })
        };
        ParticleSystem::new_with_children(&def, &obj, &opts, &mut loader)
    }

    #[test]
    fn pool_rounding_vectors_a3() {
        for (maxcount, count, expected) in [
            (150, 0.63999999, 96),
            (300, 0.57, 171),
            (200, 0.7, 140),
            (1000, 0.29, 290),
            (100, 0.3, 30),
        ] {
            let s = system(
                json!({"maxcount": maxcount}),
                json!({"instanceoverride": {"count": count}}),
                SpriteSheet::default(),
            );
            assert_eq!(s.capacity(), expected, "{maxcount} × {count}");
        }
    }

    #[test]
    fn prewarm_runs_in_the_first_frame() {
        let def = json!({"starttime": 5, "maxcount": 1000,
            "emitter": [{"name": "boxrandom", "rate": 10, "distancemax": "0 0 0"}],
            "initializer": [{"name": "lifetimerandom", "min": 100, "max": 100}]});
        let mut s = system(def.clone(), json!({}), SpriteSheet::default());
        assert_eq!(s.prewarm_seconds(), 5.0);
        // N = round(5 / (1/30)) = 150 steps of 1/30 s: 50 particles, drawn in that same frame.
        assert!(s.update(&frame(10.0)));
        assert_eq!(s.len(), 50);
        assert!((s.system_time() - 5.0).abs() < 1e-3);
        // The oldest particle was born at step 3 and aged 148 steps; the next frame continues normally.
        assert!((s.particles().ages[0] - 148.0 / 30.0).abs() < 1e-3);
        s.update(&frame(10.1));
        assert_eq!(s.len(), 51);
        // Custom frame cap: 60 FPS → 300 steps.
        let mut s = system(def, json!({}), SpriteSheet::default());
        s.set_prewarm_fps(60.0);
        s.update(&frame(10.0));
        assert_eq!(s.len(), 50);
        // starttime parsing: float kept, negative → 0, capped at 60.
        let d = ParticleDefinition::from_json(&json!({"starttime": 0.80000001}));
        assert_eq!((d.start_time, d.start_time_secs), (0, 0.8));
        assert_eq!(
            ParticleDefinition::from_json(&json!({"starttime": -3})).start_time_secs,
            0.0
        );
        assert_eq!(
            ParticleDefinition::from_json(&json!({"starttime": 900})).start_time_secs,
            60.0
        );
    }

    #[test]
    fn prewarm_turbulence_sees_virtual_time() {
        // Runs without panic and keeps everything finite with time-dependent operators.
        let mut s = system(
            json!({"starttime": 2, "emitter": [{"name": "sphererandom", "rate": 50}],
                   "initializer": [{"name": "turbulentvelocityrandom"}],
                   "operator": [{"name": "movement"}, {"name": "turbulence"}]}),
            json!({}),
            SpriteSheet::default(),
        );
        s.update(&frame(3.0));
        assert!(s.len() > 30);
        assert!(s.particles().positions.iter().all(|p| p.is_finite()));
    }

    #[test]
    fn control_point_override_and_velocity() {
        let mut s = system(
            json!({"controlpoint": [{"id": 1, "flags": 2, "offset": "1 1 0"}],
                   "initializer": [{"name": "lifetimerandom", "min": 5, "max": 5}]}),
            json!({"origin": "500 200 0", "instanceoverride": {"controlpoint1": "10 20 0", "controlpoint3": {"value": "7 8 9"}}}),
            SpriteSheet::default(),
        );
        // World space (bit 2) respected: offset − O, with O = (100, 100, 0).
        assert_eq!(s.control_points()[1].position, Vec3::new(-90.0, -80.0, 0.0));
        // Undeclared CP 3 treated as declared with flags 0.
        assert_eq!(s.control_points()[3].position, Vec3::new(7.0, 8.0, 9.0));
        s.update(&frame(1.0));
        s.update(&frame(1.1));
        assert_eq!(s.control_point_velocities()[3], Vec3::ZERO);
        s.object_mut().instance_override.control_points[3] =
            Some(UserValue::from_vec3(Vec3::new(17.0, 8.0, 9.0)));
        s.update(&frame(1.2));
        let v = s.control_point_velocities()[3];
        assert!((v - Vec3::new(100.0, 0.0, 0.0)).length() < 1e-2, "{v}");
    }

    #[test]
    fn disabled_overrides_flag() {
        let s = system(
            json!({"flags": 248, "maxcount": 10}),
            json!({"instanceoverride": {"count": 3, "speed": 2, "alpha": 0.5}}),
            SpriteSheet::default(),
        );
        assert_eq!(s.capacity(), 10);
        assert_eq!(s.object().instance_override.speed.f32(), 1.0);
        assert_eq!(
            s.object().instance_override.alpha.f32(),
            0.5,
            "alpha is not one of the five"
        );
    }

    #[test]
    fn layer_image_emitter_through_the_system() {
        let mut s = system(
            json!({"emitter": [{"name": "layerimage", "rate": 100}],
                   "initializer": [{"name": "lifetimerandom", "min": 5, "max": 5}]}),
            json!({"origin": "400 300 0"}),
            SpriteSheet::default(),
        );
        let img = EmitterImage::from_alpha(
            1,
            1,
            &[255],
            Mat4::from_translation(Vec3::new(50.0, 60.0, 0.0)),
        );
        assert!(s.set_emitter_image(0, img.clone()));
        assert!(!s.set_emitter_image(1, img));
        s.update(&frame(1.0));
        s.update(&frame(1.1));
        assert_eq!(s.len(), 10);
        // Scene (50..51, 60..61) minus O = (0, 0, 0).
        assert!(
            s.particles()
                .positions
                .iter()
                .all(|p| (50.0..51.0).contains(&p.x) && (60.0..61.0).contains(&p.y))
        );
    }

    #[test]
    fn children_static_spawn_death_follow() {
        let child = json!({"maxcount": 50, "emitter": [{"name": "boxrandom", "rate": 0, "instantaneous": 3, "distancemax": "0 0 0"}],
            "initializer": [{"name": "lifetimerandom", "min": 0.15, "max": 0.15}, {"name": "inheritinitialvaluefromevent"}]});
        let parent = json!({
            "emitter": [{"name": "boxrandom", "rate": 0, "instantaneous": 2, "distancemax": "0 0 0", "origin": "10 0 0"}],
            "initializer": [{"name": "lifetimerandom", "min": 0.125, "max": 0.125}, {"name": "colorrandom", "min": "0.5 0.5 0.5", "max": "0.5 0.5 0.5"}],
            "operator": [{"name": "movement"}],
            "children": [
                {"type": "static", "name": "c.json", "origin": "5 0 0"},
                {"type": "eventspawn", "name": "c.json", "maxcount": 1},
                {"name": "c.json", "type": "eventdeath", "maxcount": 5},
                {"name": "missing.json", "type": "eventfollow"}
            ]
        });
        let mut s = system_with(parent, json!({}), &[("c.json", child)]);
        let step = |k: u32| frame(1.0 + k as f32 * 0.0625);
        // Static instance from the start; the follow entry could not be loaded.
        assert_eq!(s.child_count(), 1);
        s.update(&step(0));
        s.update(&step(1));
        // 2 parent particles born: eventspawn maxcount 1 → one instance (second event lost).
        assert_eq!(s.len(), 2);
        assert_eq!(s.child_count(), 2);
        let spawn_child = s.children().nth(1).unwrap();
        assert_eq!(spawn_child.len(), 3, "burst of the spawn instance");
        // Inherited colour from the parent particle.
        assert!(
            spawn_child
                .particles()
                .colors
                .iter()
                .all(|c| (*c - Vec3A::splat(0.5)).length() < 1e-6)
        );
        assert!(spawn_child.is_child());
        // The parents die at 0.125 s: 2 death instances.
        s.update(&step(2));
        assert_eq!(s.len(), 0);
        assert_eq!(s.child_count(), 4);
        // Children particles die at 0.15 s; event instances then end (burst done, rate 0).
        for k in 3..7 {
            s.update(&step(k));
        }
        assert_eq!(s.child_count(), 1, "only the static instance remains");
        // Child uniforms: parent model × attach (static origin (5, 0, 0)).
        let u_in = crate::render::UniformInput {
            width: 800.0,
            height: 600.0,
            scene_view_projection: Mat4::IDENTITY,
            fov_degrees: 50.0,
            near: 0.1,
            far: 1000.0,
            depth: crate::render::DepthRange::ZeroToOne,
            parallax: None,
            brightness: 1.0,
        };
        let pu = s.uniforms(&u_in);
        let cu = s
            .children()
            .next()
            .unwrap()
            .child_uniforms(&u_in, &pu.model);
        let o = cu.model.transform_point3(Vec3::ZERO);
        assert!((o - (pu.model.transform_point3(Vec3::new(5.0, 0.0, 0.0)))).length() < 1e-4);
    }

    #[test]
    fn children_follow_and_set_control_points() {
        let child = json!({"maxcount": 10, "emitter": [{"name": "boxrandom", "rate": 20, "distancemax": "0 0 0"}],
            "initializer": [{"name": "lifetimerandom", "min": 5, "max": 5}]});
        let parent = json!({
            "emitter": [{"name": "boxrandom", "rate": 0, "instantaneous": 1, "distancemax": "0 0 0"}],
            "initializer": [{"name": "lifetimerandom", "min": 0.25, "max": 0.25}, {"name": "velocityrandom", "min": "100 0 0", "max": "100 0 0"}],
            "operator": [{"name": "movement"}],
            "children": [{"type": "eventfollow", "name": "f.json", "flags": 1, "controlpointstartindex": 2}]
        });
        let mut s = system_with(parent, json!({}), &[("f.json", child)]);
        let step = |k: u32| frame(1.0 + k as f32 * 0.0625);
        s.update(&step(0));
        s.update(&step(1));
        assert_eq!(s.child_count(), 1);
        s.update(&step(2));
        // The parent particle moved by 100 × 0.0625; the child CP 2 is the particle in the child frame
        // (anchor = particle position → CP at the child origin).
        let c = s.children().next().unwrap();
        assert!(c.control_points()[2].position.length() < 1e-3);
        assert!(c.len() >= 2);
        // After the followed particle dies, the instance stops emitting; it ends once empty.
        for k in 3..12 {
            s.update(&step(k));
        }
        assert_eq!(s.len(), 0);
        assert_eq!(
            s.child_count(),
            1,
            "child particles still alive (lifetime 5)"
        );
        let n = s.children().next().unwrap().len();
        s.update(&step(12));
        assert_eq!(s.children().next().unwrap().len(), n, "no more emission");
    }
}

//! Particle system runtime and per-frame loop (spec 05 §4, §5, §10, §13).

use crate::emitter::EmitterRt;
use crate::initializer::{InitializerRt, SpawnCtx};
use crate::model::{
    AnimationMode, ControlPointDef, InitializerDef, OperatorDef, ParticleDefinition,
    ParticleObject, RendererDef,
};
use crate::operator::{OpCtx, OperatorRt, life};
use crate::pool::Pool;
use crate::render::RopeScratch;
use crate::rng::{Rng, mix64};
use crate::sheet::{SpriteSheet, TextureInfo};
use crate::value::{UserValue, Value};
use glam::{Vec2, Vec3, Vec3A};
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
}

/// `O` from the object `origin` and the scene size (§2.2).
fn scene_origin(origin: Vec3, size: Vec2) -> Vec3 {
    Vec3::new(origin.x - size.x / 2.0, size.y / 2.0 - origin.y, origin.z)
}

impl ParticleSystem {
    /// Builds a system from its definition and scene object (§4). Allocates everything up front:
    /// the steady-state frame loop does not allocate.
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
        let over = &object.instance_override;
        let seed = opts.seed.unwrap_or_else(|| Rng::from_entropy().next_u64());
        let mut rng = Rng::seeded(seed);
        let frame_salt = mix64(seed ^ 0x5ca1_ab1e_f00d_d00d);

        // §4.1: N_pool = trunc(maxcount × count) in f32.
        // parity: a pool that truncates to 0 gets 1000 slots instead.
        let n = (def.max_count as f32 * over.count.f32()).trunc();
        let cap = if n >= 1.0 {
            (n as usize).min(MAX_POOL)
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
                InitializerDef::LifetimeRandom { min, max } => Some(min.f32() == max.f32()),
                _ => None,
            })
            .unwrap_or(false);
        // §4.5: the only draws at creation are the turbulence ones, in operator order.
        let operators: Vec<OperatorRt> = def
            .operators
            .iter()
            .map(|o| OperatorRt::new(o.clone(), &mut rng))
            .collect();
        let has = |f: fn(&OperatorDef) -> bool| def.operators.iter().any(f);
        let pool = Pool::new(
            cap,
            has(|o| matches!(o, OperatorDef::OscillateAlpha(_))),
            has(|o| matches!(o, OperatorDef::OscillateSize(_))),
            has(|o| matches!(o, OperatorDef::OscillatePosition(_))),
        );

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

        Self {
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
            object,
            rng,
            frame_salt,
            t_ref: 0.0,
            time: 0.0,
            size,
            origin,
            rope,
        }
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

    fn simulate(&mut self, dt: f32, input: &FrameInput) {
        // 4. Resolution change.
        let size = Vec2::new(input.width, input.height);
        if size != self.size {
            self.size = size;
            self.origin = scene_origin(self.object.origin.vec3(), size);
            for cp in &mut self.cps {
                if !cp.follows_mouse && cp.world_space {
                    cp.position = cp.offset - self.origin;
                }
            }
        }
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
        // 6. Emitters, each applying all initializers to its new particles.
        let ctx = SpawnCtx {
            cps: &self.cps,
            over: &self.object.instance_override,
            time: input.time,
            perspective: self.perspective,
        };
        for e in &mut self.emitters {
            e.emit(
                dt,
                &mut self.pool,
                &mut self.initializers,
                &ctx,
                &mut self.rng,
            );
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
        };
        for op in &mut self.operators {
            op.run(&mut self.pool, &octx, &mut self.rng);
        }
        // 9. Sprite sheet.
        if self.sheet.frames > 0 {
            self.animate();
        }
        // 10. Death and stable compaction.
        self.pool.compact();
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

    /// Scene-object values (origin, scale, angles, visible, parallax depth, instance override).
    pub fn object(&self) -> &ParticleObject {
        &self.object
    }

    /// Mutable scene-object values, e.g. for SceneScript writes to `scale`, `angles`, `visible`,
    /// `parallaxDepth`. `origin`, `instanceoverride.rate` and `.count` are only read at creation
    /// (and `origin` on resolution change).
    pub fn object_mut(&mut self) -> &mut ParticleObject {
        &mut self.object
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
}

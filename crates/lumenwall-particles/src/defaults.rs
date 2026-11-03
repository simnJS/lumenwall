//! Provisional constants of spec 05 amendments (A1, A6, A7.5).
//!
//! Wallpaper Engine only writes values that differ from its defaults, so the defaults of the types
//! the reference ignores are not observable in the data. They are centralised here so they can be
//! recalibrated against real Wallpaper Engine output without touching the simulation code.

/// A1: `starttime` upper bound, seconds (anti-hang guard).
pub const PREWARM_MAX_SECONDS: f32 = 60.0;
/// A1: application frame cap used for the pre-simulation step `h = 1 / fps`.
pub const PREWARM_FPS: f32 = 30.0;
/// A1: bounds of the pre-simulation step, seconds.
pub const PREWARM_STEP_MIN: f32 = 1.0 / 240.0;
pub const PREWARM_STEP_MAX: f32 = 0.1;

/// A6.1-f: system flags that disable the colour/count/lifetime/size/speed overrides together.
pub const DISABLE_OVERRIDES_MASK: u32 = 248;

/// A6.2.1 `mapsequencebetweencontrolpoints`.
pub const SEQ_BETWEEN_COUNT: f32 = 32.0;
pub const SEQ_BETWEEN_ARC_AMOUNT: f32 = 0.5;
pub const SEQ_BETWEEN_CP_START: i32 = 0;
pub const SEQ_BETWEEN_CP_END: i32 = 1;
/// Flag bits (inferred): reduce outer velocities / sizes / positions, arc, periodic restart.
pub const SEQ_FLAG_REDUCE_VELOCITY: u32 = 1;
pub const SEQ_FLAG_REDUCE_SIZE: u32 = 2;
pub const SEQ_FLAG_REDUCE_POSITION: u32 = 4;
pub const SEQ_FLAG_ARC: u32 = 8;
pub const SEQ_FLAG_PERIODIC_RESTART: u32 = 16;

/// A6.2.2 `positionoffsetrandom`.
pub const OFFSET_DISTANCE: f32 = 50.0;
pub const OFFSET_SCALE: f32 = 0.01;
pub const OFFSET_TIME_SCALE: f32 = 1.0;
pub const OFFSET_OCTAVES: f32 = 3.0;

/// A6.2.3 `hsvcolorrandom`.
pub const HSV_HUE_MIN: f32 = 0.0;
pub const HSV_HUE_MAX: f32 = 1.0;
pub const HSV_HUE_STEPS: f32 = 0.0;
pub const HSV_SATURATION_MIN: f32 = 0.0;
pub const HSV_SATURATION_MAX: f32 = 1.0;
pub const HSV_VALUE_MIN: f32 = 0.0;
pub const HSV_VALUE_MAX: f32 = 1.0;

/// A6.2.4 `inheritcontrolpointvelocity`.
pub const INHERIT_CP_VELOCITY_MIN: f32 = 1.0;
pub const INHERIT_CP_VELOCITY_MAX: f32 = 1.0;

/// A6.3.1 `remapvalue` / `remapinitialvalue`.
pub const REMAP_INPUT: &str = "lifetimefraction";
pub const REMAP_OUTPUT: &str = "size";
/// Bit 1: clamp the input; bit 2: clamp the output.
pub const REMAP_FLAGS: u32 = 1;
/// Octaves of the `fbmnoise` transform.
pub const REMAP_FBM_OCTAVES: u32 = 4;
/// Upper bound of the per-particle noise phase `φ_p`.
pub const REMAP_PHASE_RANGE: f32 = 256.0;

/// A6.3.2 `reducemovementnearcontrolpoint`.
pub const REDUCE_DISTANCE_INNER: f32 = 100.0;
pub const REDUCE_DISTANCE_OUTER: f32 = 200.0;
pub const REDUCE_REDUCTION_INNER: f32 = 1.0;
pub const REDUCE_REDUCTION_OUTER: f32 = 0.0;

/// A6.3.4 `capvelocity`.
pub const CAP_MAX_SPEED: f32 = 100.0;

/// A6.3.5 `boids`.
pub const BOIDS_NEIGHBOR_THRESHOLD: f32 = 100.0;
pub const BOIDS_SEPARATION_THRESHOLD: f32 = 25.0;
pub const BOIDS_FACTOR: f32 = 1.0;
pub const BOIDS_MAX_SPEED: f32 = 1000.0;
/// Bit 1: clamp the speed.
pub const BOIDS_FLAG_CLAMP_SPEED: u32 = 1;

/// A6.3.6 `maintaindistancetocontrolpoint`.
pub const MAINTAIN_DISTANCE: f32 = 100.0;
pub const MAINTAIN_STRENGTH: f32 = 1.0;

/// A6.3.7 collisions.
pub const COLLISION_BOUNCE_FACTOR: f32 = 0.5;
pub const COLLISION_SPHERE_RADIUS: f32 = 100.0;
pub const COLLISION_QUAD_SIZE: f32 = 100.0;

/// A6.4 `layerimage`: emission bitmap threshold and maximum resolution.
pub const LAYER_ALPHA_THRESHOLD: u8 = 128;
pub const LAYER_MAX_BITMAP: u32 = 256;

/// A6.5 children: `maxcount`, recursion limit of nested child systems.
pub const CHILD_MAX_COUNT: i64 = 20;
pub const CHILD_MAX_DEPTH: u32 = 4;
/// Child flag bit: set control points from the parent's particles (supposed).
pub const CHILD_FLAG_SET_CONTROL_POINTS: u32 = 1;
/// `inheritvaluefromevent` / `inheritinitialvaluefromevent` default option.
pub const INHERIT_DEFAULT: &str = "setcolor";

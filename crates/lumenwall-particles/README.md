# lumenwall-particles

CPU simulation of Wallpaper Engine particle systems and generation of their vertex data. No GPU
code: the renderer uploads the vertices and uses `RenderInfo` / `ParticleUniforms`.

## Sources

Clean-room implementation built only from:

- `specs/05-particules.md`, body and **amendments of 2026-11-03 (A1–A7)**, the single source of
  behaviour;
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/research/render.md`;
- public Rust documentation (glam, serde_json, criterion).

Real Wallpaper Engine files (`docs/TESTING.md`) are only read at run time by `#[ignore]` tests and
by one bench case; nothing from them is copied into the repository.

## Coverage

| Spec | Status |
|---|---|
| §1 JSON model: [U]/[C]/[B] values, defaults, arrays → 0, `instanceoverride`, unknown names dropped | done (`model.rs`, `value.rs`) |
| §2 frames, Y flips, model/VP matrices, parallax | done (`render.rs` uniforms) |
| §3–§4 particle state, pool size, renderer choice, sprite sheet resolution, control points, RNG | done |
| §5 frame order, first frame, `dt` cap, invisibility | done (`system.rs`) |
| §6 `boxrandom`, `sphererandom` | done |
| §7 the 9 initializers, §8 the 12 operators (+ `vortex_v2`) | done |
| §9 improved noise (f64), curl, Catmull-Rom; §10 sprite sheets; §12 vertices, combos, uniforms; §13 mouse | done |
| A1 `starttime` pre-simulation (first processed frame, `N = round(T0 / h)` steps, virtual time) | done; `set_prewarm_fps` (default 30) |
| A2 `instanceoverride.brightness` → `g_Brightness` / `g_Overbright` (RGB only) | done (`ParticleUniforms::brightness`, `overbright`) |
| A3 pool size `trunc(f32 × f32)` with the 5 test vectors | done |
| A4 oscillators applied in the birth frame, one state per type and particle | done |
| A5 `mapsequencearoundcontrolpoint`: f32 θ, `bounds`, `limitbehavior` repeat/mirror, `axis` | done |
| A6.1 operator blending, `exponent`, sphere delay/duration/periodic, `maxtoemitperperiod`, `instanceoverride.controlpointN`, control point velocities, system flags 248 | done |
| A6.2 `mapsequencebetweencontrolpoints`, `positionoffsetrandom`, `hsvcolorrandom`, `inheritcontrolpointvelocity`, `colorlist` | done |
| A6.3 `remapvalue` / `remapinitialvalue`, `reducemovementnearcontrolpoint`, `maintaindistancebetweencontrolpoints`, `capvelocity`, `boids` (hashed grid), `maintaindistancetocontrolpoint`, collisions plane/sphere/bounds/quad | done; `collisionmodel` parsed and ignored (logged), as the spec says |
| A6.4 `layerimage` emitter | done; the bitmap comes from the caller (`set_emitter_image`) |
| A6.5 children `static` / `eventspawn` / `eventdeath` / `eventfollow`, set control points, `inherit*valuefromevent` | done (`new_with_children`) |
| A6.6 keys read without effect (`orientation`, `movement.flags`, …) | unchanged |

Every reproduced "surprising" behaviour carries a one-line `// parity:` comment; the places that
deliberately differ carry `// lumenwall:`. The provisional defaults of A7.5 are all in
`src/defaults.rs` (`lumenwall_particles::defaults`) so they can be recalibrated.

### API

- `ParticleDefinition::from_json` / `ParticleObject::from_json` (both also `serde::Deserialize`).
- `ParticleSystem::new` (no children) or `ParticleSystem::new_with_children(def, object, opts,
  loader)`: `loader(path) -> Option<ChildAssets>` returns a child's definition and texture data;
  every instance a child entry can need is built there.
- Per frame: `update(&FrameInput) -> bool`, then `write_vertices(&mut [f32])` (e.g. straight into a
  mapped staging buffer) or `write_vertices_vec(&mut Vec<f32>)`. The index pattern is constant:
  `write_quad_indices(max_quads)` once.
- Children: `children()` / `for_each_child_mut()` give the active instances in creation order (draw
  each right after its parent, recursively); `child_uniforms(input, &parent.model)`; `material()`.
- Other inputs: `set_prewarm_fps`, `set_time_of_day` (remap `timeofday`), `set_emitter_image`
  (`EmitterImage::from_alpha`), `object_mut()` for script writes (including
  `instance_override.control_points[N]`), `set_user_property[_json]` / `visit_values_mut`.

Additive changes since the first version: new enum variants and new struct fields
(`InitializerDef` variants gained `exponent` and `MapSequenceAroundControlPoint` gained `bounds`,
`limit_behavior`, `axis`: exhaustive patterns on them need `..`). No existing function signature
changed.

## Performance

- Structure of arrays, `Vec3A` for vector attributes, everything allocated once at creation
  (optional per-particle data — oscillators, ids, sequence positions, noise phases — only when a
  type needs it); child instances are pre-built and recycled.
- One monomorphic loop per operator (enum + match outside the loop), no dyn, no map.
- Stable compaction packs survivors toward the end of a sliding window: a dead prefix is free.
- Curl noise evaluates the 12 samples that matter with shared lattice hashing, bit-identical to the
  18-sample formula (tested).
- Blending snapshots only the fields its operator writes, and only for operators with blending keys.
- Boids use a hashed uniform grid (counting sort, buckets visited once, branch-free accumulation),
  checked against a brute-force reference.
- `tests/no_alloc.rs` (counting global allocator): zero heap allocation in steady-state frames for
  sprites, ropes, and a system using pre-simulation, blending, sequences, remaps, boids, collisions
  and event children created and recycled every frame.

Bench (`cargo bench -p lumenwall-particles`, Ryzen 9 7950X3D shared with other build jobs (load ≈ 9),
10 000 live particles unless stated, pool full with continuous respawn):

| Case | Time per frame | Per particle |
|---|---|---|
| typical (boxrandom + 7 initializers + 5 operators), update + vertices | 168–183 µs | 17–18 ns |
| update only / vertices only | 78 µs / 92 µs | 7.8 ns / 9.2 ns |
| + `turbulence` | 2.24–2.37 ms | ≈ 230 ns (curl noise) |
| + 3 collision operators (plane, sphere, bounds) | 225 µs | +1.9 ns per collision operator |
| + `capvelocity` with blending | 219 µs | +5 ns |
| + `boids` (threshold 50, clustered flock) | 8.3 ms | 830 ns |
| `boids` at preset scale (250 particles, threshold 150) | 124 µs | 500 ns |
| real `rainperspective.json` of wallpaper 3018340632 (~90 particles) | 1.06 µs | 12 ns |

Boids are quadratic inside a cluster by nature; WE presets use them on 16–250 particles.

## Tests

- `cargo test -p lumenwall-particles`: 82 unit tests (every initializer, operator, emitter mode,
  collision, blending, remap, sequence, child event kind, pre-simulation, pool rounding vectors,
  control point overrides/velocities, value forms, noise hand values), plus the allocation test.
- `cargo test -p lumenwall-particles -- --ignored` (local content of `docs/TESTING.md`):
  - the 4 particle systems of wallpaper 3018340632 with their scene objects: rain 90 particles
    (≈ 93 expected), ash with `visible` forced on 55/59 (≈ 54/58 expected with its 5 s
    pre-simulation), and ≈ 23 ash particles already on screen at the first drawn frame;
  - all **289** particle definitions of `$LUMENWALL_WE_ASSETS` (**209 unique**, 232 under `presets/`),
    children loaded from their preset folder: 10 s at 30 fps each, with and without an instance
    override, no panic, finite positions, plausible counts; 71 run live children, 97 are
    pre-simulated; 0 failures (debug and release).
- `cargo check -p lumenwall-particles --target x86_64-pc-windows-gnu` passes (library; criterion's
  dev-dependency needs a MinGW C compiler for `--all-targets`).

## Deviations (`// lumenwall:`)

- Pool size capped at `MAX_POOL` (2^20) so a hostile `maxcount` cannot exhaust memory; child
  instances per entry capped at 256; child nesting capped at 4 levels.
- Angle wrapping falls back to a modulo for |angle| > 10^4 and leaves non-finite angles alone.
- `oscillateposition` skips axes whose mask is 0 (identical for finite amplitudes/frequencies).
- Perspective projection depth range is a parameter, `[0, 1]` (wgpu) by default; the reference
  uses OpenGL `[-1, 1]`.

## Choices where the amendments leave room

1. `remapvalue` with a vector output and `multiply`: the Y flip of `velocity` / `position` is not
   applied (a multiplier is a scale factor); it is applied for `remap`, `add`, `subtract`.
2. `maintaindistancebetweencontrolpoints`: the first application only records `u_p` (a particle
   born this frame was placed relative to the current control points); the delta applies from the
   next frame.
3. Collisions: `stop` / `delete` act, like `bounce` / `slide`, only when the particle moves into the
   surface (`vn < 0`), as the response list reads.
4. Children: the child receives the parent's instance override without its `controlpointN`
   overrides; its own `starttime` is not pre-simulated (it is created on events); its system origin
   for world-space/mouse control points is the parent's plus the attachment translation (rotation
   and scale ignored, as for the parent's own control points).
5. `layerimage`: the caller provides the source layer's alpha bitmap and its `(u, v) → scene`
   transform; the emitter emits nothing without it.

## Open questions (not settled)

1. A7.1 `starttime` of hidden systems (ash is hidden by default): currently pre-simulated at the
   first *visible* frame.
2. A7.2 `instance.count` / `rate` semantics, A7.3 brightness placement, A7.4 sequence flag bits,
   A7.5 every provisional default (`defaults.rs`), A7.6 system flags bits 1/2, A7.7 control point
   bits 4/16 and `parentcontrolpoint`, A7.9 oscillator frequency unit — all implemented as the
   amendments state, pending calibration on real Wallpaper Engine.
3. `t_g` is taken as `f32` (`g_Time`): `dt` loses precision after long uptimes (≈ 8 ms resolution
   after a day). Depends on the clock spec.
4. Quad/ribbon expansion is done by the WE shaders `genericparticle` / `genericropeparticle`
   (§16.1 item 18); the renderer must translate them, or a spec of the expansion is needed.

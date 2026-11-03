# lumen-particles

CPU simulation of Wallpaper Engine particle systems and generation of their vertex data. No GPU
code: the renderer uploads the vertices and uses `RenderInfo` / `ParticleUniforms`.

## Sources

Clean-room implementation built only from:

- `specs/05-particules.md` (revision of 2026-11-03), the single source of behaviour;
- `docs/PERFORMANCE.md`, `docs/research/render.md`;
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
| §6 `boxrandom` (delay, duration, periodic, burst), `sphererandom` (2D / perspective, sign, speed) | done |
| §7 all 9 initializers | done |
| §8 all 12 operators (+ `vortex_v2`) | done |
| §9 improved noise (f64), curl, Catmull-Rom | done |
| §10 sprite sheet animation (`sequence`, `once`, `randomframe`) | done |
| §12 sprite (17 floats) and rope (26 floats) vertices, combos, blend modes, uniforms, capacities | done |
| §13 mouse control points | done |
| §14 `children` | parsed, ignored (as the reference) |

Every reproduced "surprising" behaviour carries a one-line `// parity:` comment; the few places that
deliberately differ carry `// lumen:`.

API: `ParticleDefinition::from_json` / `ParticleObject::from_json` (both also `serde::Deserialize`),
`ParticleSystem::new`, then per frame `update(&FrameInput) -> bool` and
`write_vertices(&mut [f32])` (e.g. straight into a mapped staging buffer) or
`write_vertices_vec(&mut Vec<f32>)`. The index pattern is constant: build it once with
`write_quad_indices(max_quads)`. User properties: `set_user_property[_json]` or
`visit_values_mut`; script writes go through `object_mut()`.

## Performance

- Structure of arrays, `Vec3A` for vector attributes, everything allocated once at creation.
- One monomorphic loop per operator (enum + match outside the loop), no dyn, no map.
- Stable compaction packs survivors toward the end of a sliding window: a dead prefix is free and
  only survivors older than the last death move (one `memmove` per attribute and run).
- Curl noise evaluates the 12 samples that matter (not 18) with shared lattice hashing; the result
  is bit-identical to the spec's 18-sample formula (tested).
- `tests/no_alloc.rs` checks with a counting global allocator that steady-state frames (update,
  vertices, uniforms; sprites and ropes) perform zero heap allocations.

Bench (`cargo bench -p lumen-particles`, Ryzen 9 7950X3D, 10 000 live particles, boxrandom + 7
initializers + 5 operators + sprite, pool full with continuous respawn):

| Case | Time per frame | Per particle |
|---|---|---|
| update + vertices | 169 µs | 16.9 ns |
| update only | 82 µs | 8.2 ns |
| vertices only (2.7 MB written) | 95 µs | 9.5 ns |
| + `turbulence` operator | 2.17 ms | 217 ns (curl noise ≈ 150–200 ns) |
| real `rainperspective.json` of wallpaper 3018340632 (~90 particles) | 1.1 µs | 12 ns |

Vertex output is bandwidth-bound (4 identical vertices per particle, imposed by the WE vertex
format). If Lumen ever ships its own particle shaders, an instanced layout (one 17-float record per
particle) would cut it by 4.

## Tests

- `cargo test -p lumen-particles`: unit tests for every initializer, operator, emitter mode, value
  form, sprite sheet mode, the frame loop, control points, noise (hand-computed values), compaction.
- `cargo test -p lumen-particles -- --ignored` (needs the local content of `docs/TESTING.md`):
  the 4 particle systems of wallpaper 3018340632 with their scene objects (particle counts checked
  against `rate × lifetime`), and all 289 particle definitions found under `$LUMEN_WE_ASSETS`,
  simulated 10 s at 30 fps without panic, with finite positions and plausible counts.
- `cargo check -p lumen-particles --target x86_64-pc-windows-gnu` passes (library; criterion's
  dev-dependency needs a MinGW C compiler for `--all-targets`).

## Deviations (`// lumen:`)

- Pool size capped at `MAX_POOL` (2^20) so a hostile `maxcount` cannot exhaust memory.
- Angle wrapping falls back to a modulo for |angle| > 10^4 and leaves non-finite angles alone
  (repeated ±2π would never terminate there).
- `oscillateposition` skips axes whose mask is 0 (identical for finite amplitudes/frequencies).
- Perspective projection depth range is a parameter, `[0, 1]` (wgpu) by default; the reference
  uses OpenGL `[-1, 1]` (only matters with depth test, and depth clamp is on).

## Open questions (not settled by the spec)

1. `N_pool = trunc(maxcount × count)`: precision of the product. In f32 (implemented) the reference
   ash system gives `150 × 0.63999999 = 96`; in f64 it would be 95.
2. Oscillators (§8.10/8.11): implemented as "initialise on first application, then apply in the
   same frame". The spec's "Ensuite" could also mean "from the next frame on".
3. `mapsequencearoundcontrolpoint`: `θ = (k / count) × 2π` implemented with a float division
   (an integer division would always give 0).
4. Value-form edge cases assumed: `{"user": …}` without `value` → 0; [B] vec3 strings with fewer
   than 3 components → missing components 0 (as for control point offsets); [U] strings with more
   than 4 components → first 4; [C] strings with 1 component → grey scalar, 2 → `(r, g, 0)`.
5. `randomframe` (§16.1 item 8): implemented as a hash of the pool slot salted per system (same
   slot → same frame during a run, different across runs), as recommended.
6. `t_g` is taken as `f32` (`g_Time`): `dt` loses precision after long uptimes (≈ 8 ms resolution
   after a day). Depends on the clock spec.
7. Reference wallpaper data the spec does not cover: `instanceoverride.brightness` (1.64 on the ash
   objects; ignored, should it scale `g_Brightness`?), `starttime: 5` on `ash.json` (ignored per
   spec, so ash starts empty instead of pre-simulated), system `flags: 6` (bit 2 ignored).
8. Types present in the stock assets and ignored per spec (§16.3): emitter `layerimage`;
   initializers `hsvcolorrandom`, `colorlist`, `positionoffsetrandom`,
   `mapsequencebetweencontrolpoints`, `inheritcontrolpointvelocity`, `remapinitialvalue`,
   `inheritinitialvaluefromevent`; operators `boids`, `capvelocity`, `collision*`,
   `maintaindistance*`, `reducemovementnearcontrolpoint`, `remapvalue`, `inheritvaluefromevent`.
9. Quad/ribbon expansion is done by the WE shaders `genericparticle` / `genericropeparticle`
   (§16.1 item 18); the renderer must translate them, or a spec of the expansion is needed.

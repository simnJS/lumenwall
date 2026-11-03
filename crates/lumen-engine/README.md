# lumen-engine

Integration layer of Lumen: wires the asset VFS, the scene model, the shader translator, the
renderer, the particle simulation, audio and the platform surfaces into one wallpaper engine.
The `lumen` binary drives it; `examples/render_png.rs` renders wallpapers off screen.

## Sources (clean room)

- `specs/04-rendu-scene-effets.md` (frame loop, creation and render order, chains, uniforms,
  bloom, composition), `specs/02-modele-scene.md` (§12–§14 through `lumen-scene`),
  `specs/05-particules.md` §12 (vertex formats, uniforms), `specs/07-plateforme-audio-video-web.md`
  §1, §4, §9, §11–§13, §18, `specs/08-puppet-squelette.md` §9–§11 (puppet animation, skinning,
  attachments).
- The README contracts of the other crates (`lumen-render` "Integration contract" first).
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/TESTING.md`.

No upstream code, fork or third-party reimplementation was read.

## Architecture

```
lumen (CLI, config, plan) ──EnginePlan──▶ Engine::load ── Wallpaper::load (VFS + project.json + scene)
                                          Engine::start ── platform surfaces ─▶ wgpu device (first surface)
                                                        ── Audio (sounds, spectrum capture, automute)
                                                        ── build_scene per target (first output)
                                          Engine::run ─── blocking platform wait ─▶ Frame(output)
                                                           └ SceneRuntime::update ─▶ encode ─▶ compose ─▶ present
```

| Module | Content |
|---|---|
| `plan` | `EnginePlan` (what to run), `EngineControl`, `Stopped`. Independent of the `lumen` crate, which depends on this one and converts its `ExecutionPlan` (in `crates/lumen/src/main.rs`). |
| `assets` | `WallpaperResolver` (Workshop id or path through `lumen-assets` Steam discovery, Snap install included; installation and `assets` folders), `SceneSource` (VFS → `lumen-scene::AssetSource`, borrowed `.pkg` entries), `VfsBytes` (zero-copy sound data from the mapped `.pkg`), `ShaderSource` (VFS → `lumen-shader::ShaderFs`). |
| `wallpaper` | `Wallpaper`: VFS + `Project` with `--set-property` overrides applied before the scene is read. |
| `texture` | `.tex` → `TextureUpload`: BC1/2/3 uploaded as is when `supports_bc()`, decoded otherwise; RG8/R8/RGBA8; every image of an animated texture; `TEX0FORMAT` forcing; per-scene cache by name. CPU copies are dropped right after the upload. |
| `shaders` | `ShaderProvider` trait. `WeShaders` (feature `we-shaders`, default): `lumen-shader` → one naga module per pass, deduplicated by `PassShader::key`, on-disk cache in `~/.cache/lumen/shaders`, material constants and sampler defaults from the annotations. `FallbackShaders`: WGSL stand-ins following the binding contract, used per shader when a translation fails (logged once per shader) and for `commands/copy`. |
| `scene` | `build_scene`: `Project::build` → render graph in two phases (creation order: textures, composites, effect `fbos` scopes, slot candidates, targets, shaders, constants; render order: `add_image`, particle passes, bloom). `SceneRuntime::update`: frame globals, parallax, image geometry, visibility, animated textures, particles. |
| `particles` | Definition + material + texture (sprite sheet from the `.tex`, `TEX0FORMAT`) → `ParticleSystem` and a vertex stream of the graph; per frame `update` + `write_vertices` into one reused buffer + uniforms of spec 05 §12.2. |
| `audio` | Scene sounds (`lumen-audio` player, `random`/`loop` modes, zero-copy data), spectrum capture (Pulse/PipeWire-pulse on Linux, WASAPI on Windows) feeding `g_AudioSpectrum*` through the frame block, automute. |
| `engine` | `Engine`: platform backend selection (desktop, X11 fallback, window), GPU context, outputs, event loop, pause, pointer, screenshots, statistics. |
| `headless` | `render_headless`: one wallpaper, N frames at a fixed step, PNG files. |
| `puppet` | Spec 08 §9 as pure CPU math (public, unit tested with the §10 vectors): `Rig` (bones, inverse bind matrices, rest poses, attachments, animations), `Layer` (clock, loop/mirror/single, ramps), `Puppet::advance` / `evaluate` (sampling, additive/replace blending, `W`, `K`), `write_palette` (`g_Bones`), `skin_vertices` (CPU path), `decompose_attachment`. A module rather than a crate: its state is per-scene runtime state advanced by this crate's frame loop, which will also host the SceneScript bone API; it depends only on `lumen-formats` data and `glam`. |

### Frame loop

- The loop blocks in `WallpaperSurface::wait(Wait::Block)`; controls (stop, fps, properties)
  arrive through the waker. One wake-up per rendered frame (measured: 150 per 5 s at 30 fps).
- A frame is requested only while the scene changes over time: a pass reads `g_Time` (or the
  frame block), particles, animated textures, spectrum, accumulation settling. A static scene
  renders once and then sleeps; pointer events wake it only if it reads the pointer.
- Paused outputs (fullscreen, covered, locked, display off) get no frame; when every output is
  paused the spectrum capture stops.
- One scene per target, updated and encoded once per loop iteration even when several outputs
  show it; each output then composes it with its own scaling and clamp.

### lumen-render extensions

- Vertex streams for particles (17/26-float layouts): `SceneBuilder::add_stream(layout,
  max_vertices, indices)`, `Geometry::Stream`, `RenderScene::write_stream` (one `write_buffer`,
  draw count set per frame), vertex inputs matched by name
  (`ShaderInterface::vertex_input_names`), 32-bit indices, stream passes never bundled.
- `add_image_with_puppet`: indexed mesh for the first pass (`M_copie`), destination cleared
  first, single-pass chains mapped to the image rectangle (spec 04 Q22); with bone weights it
  becomes `Geometry::Skinned` (static skin buffer feeding `a_BlendIndices`, `a_BlendWeights`,
  `a_Normal`, `a_Tangent4`).
- Integration note: `ImagePass::blend` must be the material's **own** blending; the chain planner
  applies the §9.8 transfer, which `lumen_scene::ImagePasses` has already applied to its
  `blending` field (feeding that one makes every multi-pass image `normal`, i.e. opaque).

## What is wired

- Scene wallpapers: images with every effect chain, effect `fbos`, `copy` commands, compatibility
  tint/colour-blend passes, composites, render-target name scopes, bloom, camera, parallax,
  hierarchy transforms, animated textures, live visibility, user properties (`--set-property`,
  configuration reload of properties and fps), material constants bound to properties.
- Real Wallpaper Engine shaders for images, effects and particles (0 stand-in on the 7 installed
  scene wallpapers).
- Puppet warps (spec 08): `.mdl` read by `lumen_formats::mdl::parse_model` (warnings logged with
  the path); every image whose puppet has a skeleton gets a `PuppetRuntime`, drawn or not
  (control puppets with `alpha` 0 carry the children). Animation layers come from
  `lumen-scene` (`rate`, `blend`, `visible` as live settings, `animation` resolved once) plus
  the keys it does not model, read from the raw object (`additive`, `blendin`, `blendout`,
  `blendtime`). Per frame (§9.11): clocks of visible puppets, poses, then their meshes, then the
  transforms. GPU skinning: pass 0 is compiled with the engine combos `SKINNING` = 1 and
  `BONECOUNT` = max(1, n) (n ≤ 128) and its `g_Bones` receives `K' = C·K·C⁻¹` (local pixels)
  only when a matrix changed; when the shader has no skinning path (stand-in, custom shader,
  more than 128 bones) the mesh vertices are skinned on the CPU into a reused buffer instead
  (identical output, measured ≤ 1 LSB on 24 pixels of 2 M). Children with `attachment` get an
  attachment node between them and their parent, recomposed every frame (§9.10); chains go
  through several puppets (control puppet → body bottom → body → hair).
- Particles (sprites, trails, ropes), sounds, audio spectrum, automute.
- Window (`-w XxYxWxH`, or `LUMEN_WINDOWED=1` for the 640×480 window), desktop on every output
  (positional wallpaper only) or on `-r` outputs: Wayland layer-shell, and X11 desktop windows,
  also used automatically through XWayland when the compositor has no layer-shell (GNOME).
- `--list-properties`, `--dump-structure`, `--screenshot` (PNG data), `--render-debug`
  (`base-only`, `no-solid-final`, `pass-log`, `object=`, `skip-object=`, `skip-effect=`),
  `--disable-particles/-mouse/-parallax`, `--fps`, `--volume`, `--silent`, `--noautomute`,
  `--no-audio-processing`, `--assets-dir`, fullscreen pause options.

## Not done yet

| Feature | Current behaviour |
|---|---|
| Text objects | Skipped with a warning (glyph rasteriser not integrated; `lumen-render` has `text_matrix`). |
| Puppet extras (spec 08 §11) | Bone physics, inverse kinematics, blend rules, blend shapes (`MORPHING`) and `SKINNING_ALPHA` have no known format: not implemented. Particles attached to bones are not wired (images only). The SceneScript bone and layer API waits for `lumen-script` integration. |
| Video wallpapers and video textures | Load error / texture skipped (`lumen-video` not integrated). |
| Web wallpapers | Load error (`lumen-web` not integrated). |
| SceneScript | Scripts are ignored; values keep their initial setting (`lumen-script` not integrated). |
| Bloom shaders | The five bloom passes use `lumen-render`'s WGSL stand-ins, not the translated WE materials. |
| Span groups | Each member output shows the whole wallpaper (no per-output slice yet). |
| Playlists | Only the first item is shown (no `PlaylistRunner` timer). |
| Configuration reload | `fps` and properties apply live; other changes are logged for the next start. |
| Volume textures (LUT) | Not uploaded (renderer has 2D textures only). |
| Screenshot formats | PNG data whatever the extension. |

## Deviations

- Visibility is inherited from created ancestors (effective = own and every parent's). The
  reference ignores the parent (spec 02 Q16); Wallpaper Engine hides the children of a hidden
  layer, and wallpapers rely on it to switch layout variants by property (rule 5). To confirm
  against Wallpaper Engine.
- `TEX0FORMAT` is forced for particle passes from the particle texture, like for an image's main
  texture (spec 04 §13.2); without it R8 particle textures draw as opaque squares.
- The particle sprite sheet comes from the `.tex` (TEXS or `.tex-json`, parsed by `lumen-formats`)
  before `lumen-particles`' own inference.
- A positional wallpaper without `-w`/`-r` runs on every output as a desktop wallpaper (decided
  for the product; the reference opens a 640×480 window). Lives in the `lumen` glue.
- Screenshots read `_rt_FullFrameBuffer` and always write PNG.
- Puppet layer clocks advance by at most 0.25 s per frame: the engine clock keeps running while
  an output is paused, and spec 08 §9.3 excludes the pause (QO-P18). Animations only slow down
  below 4 fps.
- A vertex whose four weights are all zero collapses onto local pixel (0, 0) (the image's
  top-left corner), not onto the mesh-space origin, because positions are uploaded in local
  pixels with the conjugated palette (spec 08 §9.8). Absent from the corpus (QO-P17).

## Decisions on the open questions of spec 08

| QO | Decision | Rule |
|---|---|---|
| P6 | Linear interpolation between the precomputed frames, angles not wrapped. | 3 (inferred) |
| P7 | Mode strings `loop`, `mirror`, `single`; any other string plays as `loop` with a warning. | 2 |
| P8 | Additive layers add `w · (sample − rest)` to position, angles and scale. | 3 (inferred) |
| P9 | Linear ramps: `blendin` from the layer's creation (no script `play()` yet), `blendout` only in `single` mode; `blendtime` ≤ 0 means no ramp. | 3 (inferred) |
| P10 | `Rz(a)` counter-clockwise in mesh space (y up), like `angles.z`; the spec 08 §10 vectors pass. | 3 |
| P11 | `MDAT` transforms are relative to their bone; the parent's alignment is ignored; a shear in `A(t)` is dropped by the decomposition (σ.y negative when mirrored). | 3 |
| P12 | Parent visibility propagates to children (already done for every object, see above). | 5 |
| P13 | A mesh pass without a skinning path is skinned on the CPU (also when the `SKINNING` variant of a shader fails to translate). | 4 |
| P14 | `BONECOUNT` = max(1, n); GPU skinning up to 128 bones (8 KiB of palette per pass block), CPU above. | 1 |
| P17 | Zero-weight vertices follow the shader formula (see Deviations); weights on a missing bone reject the skeleton (`lumen-formats`). | 2 |
| P18 | Layer clocks advance even when the layer is hidden, never while its image is hidden; pauses are capped out (see Deviations); a missing `visible` stays false (spec 02). | 3/6 |
| P19, P5 | `SKINNING_ALPHA` and `MORPHING` combos are never enabled. | 4 (unknown data) |

## Running

```bash
cargo run --release -p lumen -- 3018340632                 # desktop wallpaper on every output
cargo run --release -p lumen -- -w 0x0x1720x720 3018340632 # borderless window
LUMEN_WINDOWED=1 cargo run --release -p lumen -- 3018340632 # 640x480 window
LUMEN_STATS=1 ...                                          # fps, CPU/GPU ms per frame, memory every 5 s
cargo run -p lumen-engine --example render_png -- --size 1720x720 --frames 60 --save 0,30,59 3018340632
```

PNG files go to `target/agent-integration/shots/` by default (never into the repository: the
content is copyrighted, `docs/TESTING.md`).

### Start with the session (Linux)

Install the binary in the `PATH` (for example `~/.local/bin/lumen`), choose the wallpaper in
`~/.config/lumen/config.toml`, then copy the autostart entry:

```bash
mkdir -p ~/.config/lumen ~/.config/autostart
printf 'wallpaper = "3018340632"\n' > ~/.config/lumen/config.toml
cp packaging/linux/lumen.desktop ~/.config/autostart/
```

The desktop environment starts `lumen` at login (GNOME, KDE, XFCE… honour
`~/.config/autostart`). On GNOME Wayland Lumen uses X11 desktop windows through XWayland;
on wlroots compositors and KDE it uses layer-shell. Editing `config.toml` while Lumen runs
applies the fps and property changes live. Remove the file from `~/.config/autostart` to stop.

## Measurements (RTX 4080 SUPER, Vulkan, release, reference wallpaper 3440×1440 scene)

| Situation | Result |
|---|---|
| Load (scene, 13 textures, 23 WE passes from the shader cache, 4 particle systems) | 80–230 ms |
| Window 1720×720, 30 fps cap | 30.0 fps, CPU 0.4–0.6 ms/frame, 1.8 % of one core, GPU 0.8–4.7 ms/frame (clock-dependent: the GPU stays at low clocks under this load; 0.5 ms headless at full clocks) |
| Desktop on DP-4 + DP-5 (2560×1440 each, X11 via XWayland) | 30.0 fps, 1.6 % of one core, RSS 240 MiB (anonymous 49 MiB, the rest is file-backed driver libraries and the mapped `.pkg`), VRAM 268 MiB (nvidia-smi), 127 MiB tracked by lumen-render |
| Fullscreen window on top | paused, 0.2 % of one core (sound thread) |
| Covered output | paused (`OCCLUDED`), no frame |
| Puppets, 3463520581 at 1920×1080 (16 skeletons, 78 bones, 58 attached objects), 600 frames, particles off | puppet work (clocks, blending, matrices, palettes, attachment chains) **8.5–9 µs CPU per frame**; whole frame CPU 0.37 vs 0.34–0.41 ms and GPU 0.77 vs 0.77–0.79 ms before/after (noise); no allocation per frame (`tests/puppet_alloc.rs`); 5 `BONECOUNT` variants of `genericimage4` compiled once (shader cache) |

## Tests

- `cargo test -p lumen-engine`: stand-in shaders against the binding contract (every variant
  parses, validates and reflects), particle material parsing, property bindings, uniform views.
- `cargo test -p lumen-engine -- --ignored` (GPU + content of `docs/TESTING.md`): the reference
  wallpaper renders with real shaders (0 stand-in), 1 image, 4 particle systems, live rain;
  3463520581 has 16 puppets, all skinned on the GPU, and 58 attached objects.
- Spec 08 §10 vectors (V1–V10, GPU palette, two-bone mix, attached child) in `puppet::tests`;
  `tests/puppet_alloc.rs`: the per-frame puppet path allocates nothing.
- `cargo run -p lumen-engine --example render_png -- <ids>`: visual checks.

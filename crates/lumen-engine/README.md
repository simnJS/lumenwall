# lumen-engine

Integration layer of Lumen: wires the asset VFS, the scene model, the shader translator, the
renderer, the particle simulation, audio and the platform surfaces into one wallpaper engine.
The `lumen` binary drives it; `examples/render_png.rs` renders wallpapers off screen.

## Sources (clean room)

- `specs/04-rendu-scene-effets.md` (frame loop, creation and render order, chains, uniforms,
  bloom, composition), `specs/02-modele-scene.md` (§12–§14 through `lumen-scene`),
  `specs/05-particules.md` §12 (vertex formats, uniforms), `specs/06-scripting.md` (§1.2, §8, §9,
  §12, §14 through `lumen-script`), `specs/07-plateforme-audio-video-web.md` §1, §4, §9, §11–§13, §18.
- The README contracts of the other crates (`lumen-render` "Integration contract" first,
  `lumen-script` "Use" and its `SceneHost` trait).
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
| `script` | SceneScript (`lumen-script`): the scene's `SceneHost` (layers, effects, materials, scene members, clock, cursor, display, spectrum, script properties), script loading at the end of `build_scene`, per-frame tick inside `SceneRuntime::update`, events, cursor hit tests, the `TextSink` junction. Absent (no runtime, no cost) when no created object carries a script. |
| `audio` | Scene sounds (`lumen-audio` player, `random`/`loop` modes, zero-copy data), spectrum capture (Pulse/PipeWire-pulse on Linux, WASAPI on Windows) feeding `g_AudioSpectrum*` through the frame block, automute. |
| `engine` | `Engine`: platform backend selection (desktop, X11 fallback, window), GPU context, outputs, event loop, pause, pointer, screenshots, statistics. |
| `headless` | `render_headless`: one wallpaper, N frames at a fixed step, PNG files. |

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
- Scripts with per-frame work (`update`, timers, audio buffers, text layers, queued events) keep
  frames coming, but a frame is only redrawn when a script changed something: unchanged writes
  report no change, and the frame globals (`g_Time`, pointer, spectrum) are not rewritten when no
  pass reads them.

### lumen-render extensions

- Vertex streams for particles (17/26-float layouts): `SceneBuilder::add_stream(layout,
  max_vertices, indices)`, `Geometry::Stream`, `RenderScene::write_stream` (one `write_buffer`,
  draw count set per frame), vertex inputs matched by name
  (`ShaderInterface::vertex_input_names`), 32-bit indices, stream passes never bundled.
- `add_image_with_puppet`: indexed rest-pose mesh for the first pass (`M_copie`), destination
  cleared first, single-pass chains mapped to the image rectangle (spec 04 Q22).
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
- Puppet meshes in their rest pose (`.mdl` through `lumen-formats::mdl`, `add_image_with_puppet`).
- Particles (sprites, trails, ropes), sounds, audio spectrum, automute.
- SceneScript (see below): property scripts, dynamic text layers, timers, events.
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
| Puppet skeletons | Puppet meshes are drawn in their rest pose (spec 04 §20, like the reference); bones, animation layers and layers attached to bones are not applied, so a child layer that follows a bone in Wallpaper Engine stays at its rest position (e.g. part of the hair of 3463520581). |
| Video wallpapers and video textures | Load error / texture skipped (`lumen-video` not integrated). |
| Web wallpapers | Load error (`lumen-web` not integrated). |
| `thisScene.createLayer` / `destroyLayer` / `sortLayer` | Neutral results (`undefined` / `false`): layers cannot be added to the render graph after load. Scripts that build layers in `init` (audio visualizers) fail there. |
| Media events | No media source (MPRIS / Windows media session) yet: `SceneRuntime::media_changed` / `media_status` are the entry points. |
| Script effects on fixed state | Effect visibility, particle `origin`/`scale`/`angles` (read by `lumen-particles` at creation), camera/bloom/clear-colour members: scripts can read and write them, the image is unchanged. |
| `localStorage` | In memory (not persisted between runs). |
| Bloom shaders | The five bloom passes use `lumen-render`'s WGSL stand-ins, not the translated WE materials. |
| Span groups | Each member output shows the whole wallpaper (no per-output slice yet). |
| Playlists | Only the first item is shown (no `PlaylistRunner` timer). |
| Configuration reload | `fps` and properties apply live; other changes are logged for the next start. |
| Volume textures (LUT) | Not uploaded (renderer has 2D textures only). |
| Screenshot formats | PNG data whatever the extension. |

## SceneScript

`SceneScripts::load` runs at the end of `build_scene` when `Settings::scripted()` is not empty:
it lists the created scriptable objects (images, texts, particles, groups) in render order, then
loads, in creation order, every script attached to a member of spec 06 §1.2 — `origin`, `scale`,
`angles`, `visible`, image `alpha`/`color`/`parallaxDepth`/`brightness`, text
`color`/`alpha`/`pointsize`, particle `parallaxDepth` — plus image effect `visible` and effect pass
`constantshadervalues`. The `text` of a text object is a dynamic text layer (spec 06 F2).

Scripts read and write the `lumen-scene` settings. `SceneRuntime::update` ticks the scripts first;
when they changed a transform, the world transforms of the images are recomputed (allocation-free
parent walk); when they changed `alpha`, `color`, `brightness` or a material constant, the uniforms
fed by settings are rewritten (`g_Alpha`, `g_UserAlpha`, `g_Color`, `g_Color4`, `g_Brightness`,
`g_CompositeColor`, constants; compare-on-write). Visibility and `parallaxDepth` are read every
frame. Changed user properties are queued by `property_changed` and reach `applyUserProperties`
at the next update; `resizeScreen` fires when the output size of `FrameState::screen` changes;
cursor events go to the topmost visible scripted image or text layer under the cursor.
`Engine::run` calls `destroy()` and flushes `localStorage` at the end (`shutdown_scripts`).

| Host member | Source |
|---|---|
| `engine.runtime` / `frametime` / `timeOfDay` | `FrameState::time`, `dt`, `daytime` |
| timers (`setTimeout`, `setInterval`) | scene time (`FrameState::time`), so headless renders are deterministic |
| `engine.screenResolution` / `canvasSize` | `FrameState::screen.size` (scene size when unknown) / scene size |
| `input.cursorScreenPosition` | output pixels, origin bottom-left |
| `input.cursorWorldPosition` | scene coordinates (`Pointer::normalized` × scene size) |
| `input.cursorLeftDown` | left button of the output's pointer |
| `engine.registerAudioBuffers` | the frame spectrum (left = right, spec 07 §13.6) |
| `ILayer.getEffect(name or index)`, `getEffectCount`, `getParent`, `getChildren`; `IEffect.getMaterial`, `getMaterialCount`, `setMaterialProperty` | scene model |
| `scriptProperties` | the settings of the value's `scriptproperties` (live user-property bindings) |

Effect and constant scripts are bound to `ObjectRef::Effect` / `ObjectRef::Material` (`thisObject`
is the `IEffect` / `IMaterial`, the material index being the effect pass override); their bound
member carries a location suffix (`visible@effect0`, `multiply@effect1.pass0`) so that the
`<member>_<layer>` keys of `lumen-script` stay unique.

### Text junction (`TextSink`)

The text renderer does not depend on this module and vice versa: it implements

```rust
pub trait TextSink {
    fn on_text_changed(&mut self, object: i32, text: &str);
}
```

and registers it with `SceneRuntime::set_text_sink(Box::new(…))`. `object` is the scene object id
of the text object. The sink is called during `SceneRuntime::update`, before drawing:

- for a text object whose `text` carries a script (spec 06 §9), each time the visible layer's
  script produces a new text (never empty: `" "`; the `text` setting itself is not modified,
  §9.6). The initial text is `SceneRuntime::script_text(object)` (the setting's text until the
  first change). Hidden text layers do not run (§9.5, visibility chain included);
- when a property script writes `layer.text` on a text object without text script (the `text`
  setting holds the same value).

Every other text property scripts can change (`pointsize`, `color`, `alpha`, `visible`, `origin`,
`scale`) is written into the object's `lumen-scene` settings, which the text renderer reads.

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
- SceneScript, following Wallpaper Engine where the reference's data model contradicts its
  documented API (`docs/PARITY-POLICY.md` rules 4–5, `// we-doc:`):
  - `scale`, `angles` and `visible` scripts of images, texts and particles drive the copy the
    renderer reads (spec 06 §1.2 / Q-M1-4: the reference writes an unused copy, no visible effect);
  - `ILayer.color` is a `Vec3` for scripts and a `Vec3` write keeps the stored alpha (spec 06
    §2.2 would reset `w` to 0 and hide layers whose shader reads `g_Color4`);
  - scripts on effect visibility and effect pass constants run (spec 06 §1.3: never executed);
  - groups are scriptable layers (spec 06 §1.2: not scriptable), so their scripts move or hide
    their children.

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

SceneScript (headless 1280×720, release, `render_png`; script time = `SceneScripts::tick`):

| Wallpaper | Scripts | Script time per frame | Frames redrawn |
|---|---|---|---|
| 3018340632 (no script) | none: no runtime | 0 | unchanged (particles) |
| 3089056751 (scale + clock text) | 2, 1 `update` | 3–9 µs | 1 of 91 |
| 3044659344 (2 scale + clock text) | 3, 2 `update` | 5–8 µs | 1 of 91 |
| 3372348904 (20 scripts, 7 text layers) | 19 active | 10–18 µs | particles |
| Local test variant of 3089056751 (8 animated scripts) | 7 `update`, 1 timer | 11–18 µs | every frame |

JS heap 190–245 KiB per wallpaper; no heap allocation on the Rust side in a steady-state frame of
numeric scripts (unit test `steady_state_frames_do_not_allocate`).

## Tests

- `cargo test -p lumen-engine`: stand-in shaders against the binding contract (every variant
  parses, validates and reflects), particle material parsing, property bindings, uniform views,
  SceneScript on in-memory scenes (`script::tests`: setting writes and change reports, no change
  for constant scripts, `applyUserProperties`, layer lookup, effect and constant scripts, text
  sink, transforms, zero allocation per frame).
- `cargo test -p lumen-engine -- --ignored` (GPU + content of `docs/TESTING.md`): the reference
  wallpaper renders with real shaders (0 stand-in), 1 image, 4 particle systems, live rain.
- `cargo run -p lumen-engine --example render_png -- <ids>`: visual checks.

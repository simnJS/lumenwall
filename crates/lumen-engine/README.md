# lumen-engine

Integration layer of Lumen: wires the asset VFS, the scene model, the shader translator, the
renderer, the particle simulation, audio and the platform surfaces into one wallpaper engine.
The `lumen` binary drives it; `examples/render_png.rs` renders wallpapers off screen.

## Sources (clean room)

- `specs/04-rendu-scene-effets.md` (frame loop, creation and render order, chains, uniforms,
  bloom, composition, §18 text objects), `specs/02-modele-scene.md` (§7.7 text objects, §12–§14
  through `lumen-scene`),
  `specs/05-particules.md` §12 (vertex formats, uniforms), `specs/07-plateforme-audio-video-web.md`
  §1, §4, §9, §11–§13, §18.
- The README contracts of the other crates (`lumen-render` "Integration contract" first).
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/TESTING.md`.
- Text: Wallpaper Engine's own font material (`assets/materials/fonts/basefont.json`, shader
  `font`), the `size`/`padding` values the WE editor writes for text objects and the preview
  images of the installed wallpapers (layout and placement, see Deviations); public `cosmic-text`
  0.15 documentation.

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
| `text` | Text objects as image layers: `Fonts` (empty `cosmic-text` database, the wallpaper's font file through the VFS, fallback fonts loaded only on need), layout (300 dpi, line breaks, `limitwidth`/`maxwidth` wrapping, `limitrows`/`maxrows`, ellipsis), R8 glyph texture registered in the texture cache under the layer's slot-0 name, anchor offset, `TextRuntime` (per-frame `g_Color4` of the glyph pass, regeneration only when the string or point size changes). |
| `particles` | Definition + material + texture (sprite sheet from the `.tex`, `TEX0FORMAT`) → `ParticleSystem` and a vertex stream of the graph; per frame `update` + `write_vertices` into one reused buffer + uniforms of spec 05 §12.2. |
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
- Text objects: the wallpaper's `.ttf`/`.otf` (`.pkg`, folder or WE `assets/fonts`), shaped and
  rasterised once into an R8 texture, drawn by the WE `font` shader as the first pass of an
  image layer, then the object's effects and `colorBlendMode` pass like an image; colour, alpha,
  brightness, visibility, parent, parallax depth. `SceneRuntime::set_text(object_id, &str)`
  changes the content (for SceneScript); the texture is regenerated only when the string or the
  point size changes (`text_regenerations()` counts it).
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
| Text scripts | The initial `value` is shown until something calls `SceneRuntime::set_text` (SceneScript is not integrated here). |
| Text layer growth | The glyph texture keeps the size reserved at load (text, editor `size`, 1.5× + 1 em for scripted or bound texts, `maxwidth`); a longer string is drawn smaller to fit instead of growing the layer. |
| Integer colour strings | `lumen-scene` reads `"1 1 1"` as 0–255 (spec 02 Q1): the default white clock tint of 3089056751 draws black, while its WE preview shows white (with `--set "clocktint=1.0 1.0 1.0"` Lumen matches the preview). To settle in spec 02 Q1. |
| Text styling | `opaquebackground`/`backgroundcolor`, outline, drop shadow, MSDF, colour (emoji) fonts and `blockalign` are not drawn; the object's own `angles` are not applied (the parents' are). |
| Puppet skeletons | Puppet meshes are drawn in their rest pose (spec 04 §20, like the reference); bones, animation layers and layers attached to bones are not applied, so a child layer that follows a bone in Wallpaper Engine stays at its rest position (e.g. part of the hair of 3463520581). |
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
- Text objects follow Wallpaper Engine rather than the reference renderer of spec 04 §18 (rule 5;
  evidence: the `size` the WE editor stores for every text of 3044659344, 3089056751 and
  3372348904, and their WE preview images registered against Lumen renders of the same scene):
  - drawn as an image layer (glyph pass, then the object's effects and `colorBlendMode`), not
    outside the pass chain; parents, parallax depth and brightness apply;
  - 1 pt = 300/72 layer pixels (the reference: 1 pt = 1 pixel compensated by the scale, which
    draws these texts about 4× too small); line height = hhea ascender − descender;
  - UTF-8 text shaped with kerning and ligatures, line breaks honoured (the reference reads
    bytes as Latin-1 on one line); `limitwidth`/`maxwidth`, `limitrows`/`maxrows` and
    `limituseellipsis` are applied (public WE text layer options);
  - placement: the text block, not its padded quad, is anchored on the origin (a left-aligned
    text starts exactly at `origin.x`; 3089056751 matches its preview within 2 px at 4K), and
    vertical centring uses the block from the first ascender to the last baseline (a single
    centred line has its baseline ascender/2 below the origin; verified on 3089056751 and
    3372348904). The reference ignores alignment and padding;
  - fallback fonts: DejaVu Sans then Liberation Sans (Segoe UI then Arial on Windows, and
    `systemfont_<name>` as `<name>.ttf` there), then Wallpaper Engine's `fonts/NotoSans-Regular.ttf`;
    they are also used for glyphs the wallpaper's font lacks.

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

Text (release, same machine, `cargo test -p lumen-engine --release --test text_content -- --ignored --nocapture`):

| Situation | Result |
|---|---|
| Steady state, 7 texts (3372348904) / 1 text (3044659344) | 0 glyph texture regenerated over 120 frames; whole `SceneRuntime::update` 1.0 µs / 0.3 µs per frame; a static scene with text stops redrawing |
| One regeneration (`set_text`): layout + rasterisation + upload | 0.2 ms (32 pt, 3372348904), 1.0 ms (96 pt = 400 px per em, 1954×640 texture, 3044659344) |
| Layout + rasterisation alone, 32 pt clock string (unit test) | 0.4 ms |

## Tests

- `cargo test -p lumen-engine`: stand-in shaders against the binding contract (every variant
  parses, validates and reflects), particle material parsing, property bindings, uniform views.
- `cargo test -p lumen-engine -- --ignored` (GPU + content of `docs/TESTING.md`): the reference
  wallpaper renders with real shaders (0 stand-in), 1 image, 4 particle systems, live rain.
- `cargo test -p lumen-engine -- --ignored` also runs `tests/text_content.rs` (3044659344,
  3372348904): every text drawn with the WE font shader, no regeneration in the steady state,
  one regeneration per `set_text` change, none for the same string.
- `cargo run -p lumen-engine --example render_png -- <ids>`: visual checks.

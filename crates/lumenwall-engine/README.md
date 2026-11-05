# lumenwall-engine

Integration layer of Lumenwall: wires the asset VFS, the scene model, the shader translator, the
renderer, the particle simulation, audio and the platform surfaces into one wallpaper engine.
The `lumenwall` binary drives it; `examples/render_png.rs` renders wallpapers off screen.

## Sources (clean room)

- `specs/04-rendu-scene-effets.md` (frame loop, creation and render order, chains, uniforms,
  bloom, composition, §18 text objects), `specs/02-modele-scene.md` (§7.7 text objects, §12–§14
  through `lumenwall-scene`), `specs/05-particules.md` §12 (vertex formats, uniforms),
  `specs/06-scripting.md` (§1.2, §8, §9, §12, §14 through `lumenwall-script`),
  `specs/07-plateforme-audio-video-web.md` §1, §4, §9, §10.4, §11–§13, §15, §18,
  `specs/08-puppet-squelette.md` §9–§11 (puppet animation, skinning, attachments).
  `specs/07-plateforme-audio-video-web.md` §1, §4, §9, §11–§13, §18,
  `specs/08-puppet-squelette.md` §9–§11 (puppet animation, skinning, attachments),
  `specs/09-scenes-3d.md` (3D models, sprites, perspective camera and paths, lights, depth,
  planar reflection, particles and images of a perspective scene).
- The README contracts of the other crates (`lumenwall-render` "Integration contract" first,
  `lumenwall-script` "Use" and its `SceneHost` trait, `lumenwall-web` "Main process integration").
- Web frames: public documentation of wgpu-hal 30 (`vulkan::Device::texture_from_dmabuf_fd`,
  `wgpu::Device::create_texture_from_hal`).
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/TESTING.md`.
- Text: Wallpaper Engine's own font material (`assets/materials/fonts/basefont.json`, shader
  `font`), the `size`/`padding` values the WE editor writes for text objects and the preview
  images of the installed wallpapers (layout and placement, see Deviations); public `cosmic-text`
  0.15 documentation.

No upstream code, fork or third-party reimplementation was read.

## Architecture

```
lumenwall (CLI, config, plan) ──EnginePlan──▶ Engine::load ── Wallpaper::load (VFS + project.json + scene)
                                          Engine::start ── platform surfaces ─▶ wgpu device (first surface)
                                                        ── Audio (sounds, spectrum capture, automute)
                                                        ── build_scene per target (first output)
                                          Engine::run ─── blocking platform wait ─▶ Frame(output)
                                                           └ SceneRuntime::update ─▶ encode ─▶ compose ─▶ present
```

| Module | Content |
|---|---|
| `plan` | `EnginePlan` (what to run), `EngineControl`, `Stopped`. Independent of the `lumenwall` crate, which depends on this one and converts its `ExecutionPlan` (in `crates/lumenwall/src/main.rs`). |
| `assets` | `WallpaperResolver` (Workshop id or path through `lumenwall-assets` Steam discovery, Snap install included; installation and `assets` folders), `SceneSource` (VFS → `lumenwall-scene::AssetSource`, borrowed `.pkg` entries), `VfsBytes` (zero-copy sound data from the mapped `.pkg`), `ShaderSource` (VFS → `lumenwall-shader::ShaderFs`). |
| `wallpaper` | `Wallpaper`: VFS + `Project` with `--set-property` overrides applied before the scene is read. |
| `texture` | `.tex` → `TextureUpload`: BC1/2/3 uploaded as is when `supports_bc()`, decoded otherwise; RG8/R8/RGBA8; every image of an animated texture; volume textures (flag 0x40, colour LUTs) → `VolumeUpload`; `TEX0FORMAT` forcing; per-scene cache by name. CPU copies are dropped right after the upload. |
| `shaders` | `ShaderProvider` trait. `WeShaders` (feature `we-shaders`, default): `lumenwall-shader` → one naga module per pass, deduplicated by `PassShader::key`, on-disk cache in `~/.cache/lumenwall/shaders`, material constants and sampler defaults from the annotations. `FallbackShaders`: WGSL stand-ins following the binding contract, used per shader when a translation fails (logged once per shader) and for `commands/copy`. |
| `bloom` | Spec 04 §17 with the WE materials: the five shaders of `lumenwall-scene`'s synthetic bloom image (`genericimage2`, `downsample_quarter_bloom`, `downsample_eighth_blur_v`, `blur_h_bloom`, `combine`) translated through the `ShaderProvider`, every material constant written once (`bloomstrength` → `g_BloomStrength`, `bloomthreshold` → `g_BloomThreshold`, `g_BloomTint` annotation default), drawn by `lumenwall_render::bloom::add_bloom`. If one shader is a stand-in, the whole chain uses lumenwall-render's WGSL approximation. |
| `scene` | `build_scene`: `Project::build` → render graph in two phases (creation order: textures, composites, effect `fbos` scopes, slot candidates, targets, shaders, constants; render order: `add_image`, particle passes, bloom). `SceneRuntime::update`: frame globals, parallax, image geometry, visibility, animated textures, particles. |
| `text` | Text objects as image layers: `Fonts` (empty `cosmic-text` database, the wallpaper's font file through the VFS, fallback fonts loaded only on need), layout (300 dpi, line breaks, `limitwidth`/`maxwidth` wrapping, `limitrows`/`maxrows`, ellipsis), R8 glyph texture registered in the texture cache under the layer's slot-0 name, anchor offset, `TextRuntime` (per-frame `g_Color4` of the glyph pass, regeneration only when the string or point size changes). |
| `particles` | Definition + material + texture (sprite sheet from the `.tex`, `TEX0FORMAT`) → `ParticleSystem` and a vertex stream of the graph; per frame `update` + `write_vertices` into one reused buffer + uniforms of spec 05 §12.2. |
| `script` | SceneScript (`lumenwall-script`): the scene's `SceneHost` (layers, effects, materials, scene members, clock, cursor, display, spectrum, script properties), script loading at the end of `build_scene`, per-frame tick inside `SceneRuntime::update`, events, cursor hit tests, the `TextSink` junction. Absent (no runtime, no cost) when no created object carries a script. `script/dynamic.rs`: layers scripts create, destroy and sort (scene-model side, pools). |
| `scene/layers.rs` | GPU side of the layers scripts create after load: `RenderScene::extend`, parking and reuse, draw order (see "Layers created by scripts"). |
| `scene/media.rs`, `script/media.rs` | Media integration: the now-playing state of the system's media session reaching scenes (SceneScript media events, `$mediaThumbnail` / `$mediaPreviousThumbnail` render targets) and web pages (`wallpaperRegisterMedia*Listener`); see "Media integration". |
| `vulkan_driver` | `restrict_vulkan_loader` (first call of `main`, Linux and Windows): `VK_LOADER_DRIVERS_SELECT` set to the manifest glob of the driver used last time (`<cache>/vulkan-driver`: glob, PCI vendor and device), so the loader does not open the other drivers (Linux: Mesa's stay mapped, ≈ 50 MiB RSS, 25 ms per instance here). Windows: the glob is the adapter's manifest name read from its driver's registry key (`*nv-vk64.json`, `lumenwall_platform::vulkan_registry`), and the implicit layers the other adapters' drivers register are turned off with `VK_LOADER_LAYERS_DISABLE` (AMD's `VK_LAYER_AMD_switchable_graphics` lives in `amdvlk64.dll`, which the loader mapped into every Vulkan process even with only the NVIDIA driver selected). `open_gpu` lifts it and opens again with every driver when the restricted loader finds no adapter or not the remembered one, then remembers the new choice. Untouched when the user sets a loader driver variable. |
| `audio` | Scene sounds (`lumenwall-audio` player, `random`/`loop` modes, zero-copy data), spectrum capture (Pulse/PipeWire-pulse on Linux, WASAPI on Windows) feeding `g_AudioSpectrum*` through the frame block, automute. |
| `engine` | `Engine`: in this process (`Local`) or, on Windows, in an engine process of its own (`engine/remote.rs`, `engine/host.rs`, below). `Local`: platform backend selection (desktop, X11 fallback, window), GPU context (one backend per instance, `gpu_backends`: Vulkan, then DX12 when Vulkan has no adapter; `LUMENWALL_GPU_BACKEND=vulkan\|dx12`), outputs, event loop, pause (platform and user), pointer, screenshots, statistics, scene media events; live plan changes (`EngineControl::Reload`: wallpapers reused or loaded before anything is released, outputs rebound, surfaces reopened only for layout/layer/pause-policy changes, idle without wallpaper: no surface, no GPU device); `engine/release.rs`: GPU released after a long pause and restored on resume (Frame loop below). |
| `monitor` | `EngineMonitor`: outputs, wallpapers, states, property values (rewritten on changes only) and frame counters (recorded only while sampling) for the IPC daemon. |
| `headless` | `render_headless`: one wallpaper, N frames at a fixed step, PNG files. |
| `model3d` | Spec 09: `Builder3d` (before the creation loop: `.mdl` → static meshes, model materials through `Project::load_model_material`, 3D shaders, textures, `_rt_Reflection`, the scene depth buffer; draws in render order, reflection pass first) and `Scene3d` (per frame: camera pose from the paths, `V`, `P`, lights, visibility and the uniforms of every 3D pass, compare-on-write). |
| `puppet` | Spec 08 §9 as pure CPU math (public, unit tested with the §10 vectors): `Rig` (bones, inverse bind matrices, rest poses, attachments, animations), `Layer` (clock, loop/mirror/single, ramps), `Puppet::advance` / `evaluate` (sampling, additive/replace blending, `W`, `K`), `write_palette` (`g_Bones`), `skin_vertices` (CPU path), `decompose_attachment`. A module rather than a crate: its state is per-scene runtime state advanced by this crate's frame loop, which will also host the SceneScript bone API; it depends only on `lumenwall-formats` data and `glam`. |
| `web` | Web wallpapers: `WebSlot` per target (one `lumenwall-web` host process, page at the size of the first output showing it), frames copied into an RGBA8 render target (DMA-BUF imported once per buffer of the browser's pool, one draw that crops to the visible rectangle, wait for that submission, release; shared-memory upload as fallback, restarting the host), composition like a scene, pause/fps/volume/properties/resize/mouse forwarded, spectrum to `wallpaperRegisterAudioListener`, host restarts, `render_headless_web`. |
| `video` | Video wallpapers: one `lumenwall-video` player per target (zero-copy Vulkan Video on the render device when possible), its sound track through the `lumenwall-audio` player (mapped file, loop, cubic volume, automute), pause with its outputs, frame deadlines for the loop (`Wait::Until`, decoder wake-up through the engine waker). |

### Frame loop

- The loop blocks in `WallpaperSurface::wait(Wait::Block)`; controls (stop, fps, properties)
  arrive through the waker. One wake-up per rendered frame (measured: 150 per 5 s at 30 fps).
- A frame is requested only while the scene changes over time: a pass reads `g_Time` (or the
  frame block), particles, animated textures, spectrum, accumulation settling. A static scene
  renders once and then sleeps; pointer events wake it only if it reads the pointer.
- Paused outputs (fullscreen, covered, locked, display off) get no frame; when every output is
  paused the spectrum capture stops and the scene sounds are suspended (`Player::set_hidden`).
- GPU release (`engine/release.rs`, `EnginePlan::gpu_release`, default 5 min, `None` = never):
  once every output has been paused (or held by a user pause with nothing left to show) for
  the delay, the renderers, swap chains, device and instance are dropped; the platform
  surfaces, wallpapers (mapped `.pkg`, scene model, property values), audio player and clock
  stay. The windows keep their last image. When an output resumes, the user unpauses, a
  property preview needs a frame or a new plan arrives, the device is recreated and every
  target rebuilt as at start-up (shader cache + on-disk pipeline cache). A rebuild restarts
  particles (with their `starttime` warm-up), scripts (`init` again, in-memory `localStorage`
  lost), videos (from the start, with a fresh sound track) and web pages (reloaded; the output
  keeps its image until the page's first frame).
- Freed heap pages go back to the system (`malloc_trim(0)` with glibc; on Windows
  `HeapOptimizeResources` and `HeapCompact` on every heap, `lumenwall_platform::process`) after
  a wallpaper is unloaded by a new plan and after the GPU release: glibc keeps them in its
  arenas otherwise (scene → video → scene: 249 → 220 MiB RSS; released GPU: 117 → 101 MiB),
  the Windows heaps keep them committed. 1–3 ms (glibc), 0.1–0.4 ms (Windows), on those events
  only.
- Windows: the working set is emptied (`K32EmptyWorkingSet`) 1 s after the first frame of a
  load (start, new wallpaper, GPU restore), when every output pauses and after the GPU release:
  what loading touched (driver and shader-compiler pages, decoded files, the mapped `.pkg`)
  goes back to the system and the frames fault back only what they use. 3–14 ms; private bytes
  unchanged.
- Windows: the instance and the device are created on a thread while the platform opens
  (the adapter is checked against the first output's surface afterwards), and each output's
  swap chain is created on a thread while its renderer is built.
- One scene per target, updated and encoded once per loop iteration even when several outputs
  show it; each output then composes it with its own scaling and clamp.
- Scene time (`clock.rs`, `SceneClock`, one per scene, read as `FrameState::time`/`dt`): the
  running time of the wallpaper, started at one frame (`1/fps`) on its first frame, advanced by
  the real time between frames capped at max(0.25 s, 2/fps), kept in f64 until upload. It feeds
  `g_Time`, `engine.runtime`/`frametime`, timers, particles, puppets, animated textures and camera
  paths. A wallpaper applied to a daemon that has been running for hours therefore starts at 0
  with a one-frame first step (the engine-wide clock gave it the daemon's age as time and as first
  `frametime`), pauses are not counted, and the same seconds give the same state at any frame
  rate. A rebuild (GPU restore) starts a new clock, like everything else it restarts.
- Scripts with per-frame work (`update`, timers, audio buffers, text layers, queued events) keep
  frames coming, but a frame is only redrawn when a script changed something: unchanged writes
  report no change, and the frame globals (`g_Time`, pointer, spectrum) are not rewritten when no
  pass reads them.

### Engine process (Windows)

A process that has opened a Vulkan device keeps the GPU driver loaded for good: measured with a
bare wgpu program on an RTX 4080 SUPER, 1.2 MiB private at start, ≥ 30 MiB private, 45 MiB
working set and 6 driver threads after its device and instance are destroyed (`nvoglv64`,
`nvgpucomp64`, `nvspcap64`, `nvrtum64` stay mapped). So on Windows the daemon never opens one:

- `main` calls `run_engine_host` right after `restrict_vulkan_loader`. With the hidden first
  argument `--lumenwall-engine-host` the process is an engine process: it runs `Local` on its
  main thread and nothing else (no command line, configuration or IPC server). Otherwise it is
  a daemon, whose `Engine` is `engine/remote.rs`.
- The engine process starts when a plan brings a wallpaper while none runs, with that plan; a
  new plan is applied in place by the same process (its GPU device included); it ends when the
  last wallpaper goes (`clear`), and the daemon then trims its heaps and working set.
- It is tied to the daemon by a job object (`KILL_ON_JOB_CLOSE`, `lumenwall_platform::process`):
  it dies with the daemon however the daemon ends; the end of its stdin also stops it.
- It is restarted once with the current plan when it dies unexpectedly; a second death within
  30 s leaves the targets in error (`the engine process stopped unexpectedly (…); apply a
  wallpaper to try again`) until the next plan.
- One JSON object per line on its stdin and stdout (`engine/wire.rs`): plans and controls one
  way; answers, the monitor's report on change, frame counters on request (`get_stats`) and log
  records the other. Records are re-emitted by the daemon through callsites of their own target
  and level (`engine/logfwd.rs`), so the daemon's filter and log file treat them like its own;
  `log` records (wgpu, naga) included, through tracing-log.
- The process restricts the Vulkan loader itself (the daemon's variables are removed from its
  environment) and follows the system's media sessions only while one of its scenes handles
  media events or samples a cover (see "Media integration"); the daemon follows none.
- `LUMENWALL_ENGINE_PROCESS=0` keeps the engine in the daemon; `--list-properties` and
  `--dump-structure` always do. Linux: the engine stays in the daemon (the driver cost there is
  covered by the GPU release and `malloc_trim`; the split is not measured on Linux yet).

### lumenwall-render extensions

- Vertex streams for particles (17/26-float layouts): `SceneBuilder::add_stream(layout,
  max_vertices, indices)`, `Geometry::Stream`, `RenderScene::write_stream` (one `write_buffer`,
  draw count set per frame), vertex inputs matched by name
  (`ShaderInterface::vertex_input_names`), 32-bit indices, stream passes never bundled.
- `add_image_with_puppet`: indexed mesh for the first pass (`M_copie`), destination cleared
  first, single-pass chains mapped to the image rectangle (spec 04 Q22); with bone weights it
  becomes `Geometry::Skinned` (static skin buffer feeding `a_BlendIndices`, `a_BlendWeights`,
  `a_Normal`, `a_Tangent4`).
- Blending contract (spec 04 §9.8): `lumenwall_scene::PassDesc::blending` is the material pass's own
  mode and goes unchanged into `ImagePass::blend`; only `lumenwall-render`'s chain planner applies
  the first/last transfer (both crate READMEs, "Blending contract"). `tests/blend_contract.rs`
  fails if it is applied zero or two times.
- Volume textures: `SceneBuilder::add_volume_texture`; a `sampler3D` slot (`texture_3d`) binds
  them like any `TextureId`.

## What is wired

- Scene wallpapers: images with every effect chain, effect `fbos`, `copy` commands, compatibility
  tint/colour-blend passes, composites, render-target name scopes, bloom with the WE materials,
  colour LUTs (volume textures in `sampler3D` slots, e.g. `ccsimple` with `LUT`), camera, parallax,
  hierarchy transforms, animated textures (several images, or one sprite-sheet image whose frames
  are selected through `g_Texture0Translation`/`g_Texture0Rotation`, spec 04 §16.3), live
  visibility, user properties (`--set-property`,
  configuration reload of properties and fps), material constants bound to properties.
- Real Wallpaper Engine shaders for images, effects and particles (0 stand-in on the 7 installed
  scene wallpapers).
- Text objects: the wallpaper's `.ttf`/`.otf` (`.pkg`, folder or WE `assets/fonts`), shaped and
  rasterised once into an R8 texture, drawn by the WE `font` shader as the first pass of an
  image layer, then the object's effects and `colorBlendMode` pass like an image; colour, alpha,
  brightness, visibility, parent, parallax depth. `SceneRuntime::set_text(object_id, &str)`
  changes the content (for SceneScript); the texture is regenerated only when the string or the
  point size changes (`text_regenerations()` counts it).
- Puppet warps (spec 08): `.mdl` read by `lumenwall_formats::mdl::parse_model` (warnings logged with
  the path); every image whose puppet has a skeleton gets a `PuppetRuntime`, drawn or not
  (control puppets with `alpha` 0 carry the children). Animation layers come from
  `lumenwall-scene` (`rate`, `blend`, `visible` as live settings, `animation` resolved once) plus
  the keys it does not model, read from the raw object (`additive`, `blendin`, `blendout`,
  `blendtime`). Per frame (§9.11): clocks of visible puppets, poses, then their meshes, then the
  transforms. GPU skinning: pass 0 is compiled with the engine combos `SKINNING` = 1 and
  `BONECOUNT` = max(1, n) (n ≤ 128) and its `g_Bones` receives `K' = C·K·C⁻¹` (local pixels)
  only when a matrix changed; when the shader has no skinning path (stand-in, custom shader,
  more than 128 bones) the mesh vertices are skinned on the CPU into a reused buffer instead
  (identical output, measured ≤ 1 LSB on 24 pixels of 2 M). Children with `attachment` get an
  attachment node between them and their parent, recomposed every frame (§9.10); chains go
  through several puppets (control puppet → body bottom → body → hair).
- Video wallpapers (`type: video`, `--hwdec`): decoding, cadence, loop, pause, sound and
  measurements in `crates/lumenwall-video/README.md`. Engine side: the device gets video decode
  queues when a video is loaded (`ContextOptions::video_decode`), a video target is rendered
  by `render_video` (convert on a new frame, compose per output like a scene). A video applied
  over IPC to a device created for scenes only (`--hwdec auto`, Vulkan) recreates the device
  with the queues through the GPU release mechanism (`engine/release.rs`, Vulkan instance
  kept): 0.19 s for the switch, then zero copy; the device keeps its queues when a scene comes
  back (+4 MiB VRAM).
- Web wallpapers (`type: web`, needs `lumenwall-web-host` built with the `cef` feature next to
  `lumenwall`, see `crates/lumenwall-web/README.md`): offscreen Chromium in its own process,
  frames shared on the GPU and composed on every output with the target's scaling, the Wallpaper
  Engine web API (`applyUserProperties` with `--set-property` and live changes, `setPaused`,
  `applyGeneralProperties`, audio listener fed with the spectrum), pointer and buttons, page
  sound under `--volume`/`--silent`, pause freezing the page, hot switching scene ↔ web through
  `EngineControl::Reload` (IPC `apply`). Frames and wake-ups only when the page paints.
- Playlists, battery pause and playback state are the daemon's (`crates/lumenwall`, README "Playlists"): the engine sees them as plan reloads of one target (`TargetContent`) and as pause/resume, never a playlist object. The engine process on Windows (above) is started and ended by the daemon the same way.
- Media integration (now playing and its cover) for scenes and web pages: see "Media integration".
- Particles (sprites, trails, ropes), sounds, audio spectrum, automute.
- SceneScript (see below): property scripts, dynamic text layers, timers, events.
- Window (`-w XxYxWxH`, or `LUMENWALL_WINDOWED=1` for the 640×480 window), desktop on every output
  (positional wallpaper only) or on `-r` outputs: Wayland layer-shell, and X11 desktop windows,
  also used automatically through XWayland when the compositor has no layer-shell (GNOME).
- `--list-properties`, `--dump-structure`, `--screenshot` (PNG data), `--render-debug`
  (`base-only`, `no-solid-final`, `pass-log`, `object=`, `skip-object=`, `skip-effect=`),
  `--disable-particles/-mouse/-parallax`, `--fps`, `--volume`, `--silent`, `--noautomute`,
  `--no-audio-processing`, `--assets-dir`, fullscreen pause options.

## 3D scenes (spec 09)

A scene is 3D when it is perspective (`orthogonalprojection` absent or `null`) or has model or
sprite objects; otherwise `Builder3d::prepare` returns `None` and nothing below exists (no depth
buffer, no extra pass, no per-frame work).

- **Models** (`model` objects): `lumenwall_formats::mdl::parse_static_model`, one static mesh per
  sub-mesh (vertices uploaded as stored, inputs fed by name: `a_Position`, `a_Normal`,
  `a_Tangent4`, `a_TexCoord`, `a_TexCoordVec4` with both UV sets or `(u, v, 0, 1)`), material of
  the object's `skin` (skin 0 with a warning when missing), every pass compiled as
  `PassKind::Model`: WE clip convention without the clip fix, `HLSL` = 1, never a stand-in (a mesh
  whose shader fails is not drawn, the others are). States: material values with the model
  defaults (depth test/write, back faces culled, `FrontFace::Ccw`). Textures: the material's, then
  the shader's sampler defaults; `_rt_*` names resolve in the scene scope (`_rt_Reflection` is
  created on first use). Constants: `usershadervalues` (live) win over `constantshadervalues`.
  A failed `.mdl` or material is a warning, never a scene failure.
- **Transforms**: `T(origin)·Rz(a.z)·Ry(a.y)·Rx(a.x)·S(scale)` composed through created parents,
  recomputed every frame (no allocation). Uniforms of spec 09 §2.9: model, inverse, normal
  matrix (only when declared with the engine's type), `P·V`, MVP and inverses, eye, view axes,
  `g_Screen`, lights, ambient and skylight (live), object alpha/colour/brightness (live).
- **Camera**: `general.fov`/`nearz`/`farz` (defaults 50 / 0.01 / 10000, invalid planes fall back
  with a warning), `math::perspective_we`. Paths (`camera.paths`): played in order and looped,
  linear interpolation, instants stretched to the duration when the last one differs (`demon_core`).
  `LUMENWALL_STATIC_CAMERA=1` keeps the `scene.json` pose (debugging aid).
  `camerashake` (QO-3D-25, provisional, `// parity:`): the same offset added to the eye and the
  target, Perlin slices (`lumenwall_particles::noise`) of distinct seeds per axis at
  `t·speed`, the vertical one with a double-frequency octave × `roughness`, × `amplitude`.
  `camerafade` with two paths or more (QO-3D-24, provisional): a pass after the bloom darkens
  the target (black, alpha `1 − φ`), `φ` going 1 → 0 over the last 0.5 s of a path and 0 → 1
  over the first 0.5 s of the next (not at the very start); the pass is disabled while `φ` = 1.
- **Lights**: the first four visible `point`/`lpoint` lights: `g_LightsPosition[4]`,
  `g_LightsColorRadius[4]` = (colour × intensity, radius), unused slots zero, and the same lights
  for the `LightingV1` module of lumenwall-shader (`g_LumenLightPosition[4]` with the exponent,
  `g_LumenLightColor[4]`): `generic4` and its family get PBR point lighting (checked on WE's
  `collisionmodel` preview scene). Other types are warned about and skipped.
- **Models of a 2D scene** get `SCENE_ORTHO` = 1 (spec 09 §2.4, §3.12).
- **Depth**: one `Depth32Float` buffer on `_rt_FullFrameBuffer` (and on `_rt_Reflection`), cleared
  once per frame. Every pass drawn into the scene target gets a depth state (images, particles,
  bloom: "always", no write).
- **Reflection** (`_rt_Reflection`, `reflected` objects): cleared to the clear colour, then the
  visible reflected objects with `V·Sc(1, −1, 1)` and the mirrored eye, before the main pass.
- **Sprites**: four vertices at the origin with corner UVs, drawn by the material's shader with
  the model defaults.
- **Images of a perspective scene**: laid out with the canonical 2D camera of the target size,
  clip-space geometry (`ImageOptions::clip_space`), so `cloudsbg.vert` covers the target.
- **Particles of a perspective scene**: world space, the scene camera (`perspective_upstream`, the
  shaders keep the clip fix), eye and view axes of the camera, sprite orientation from the camera
  axes in the system's local space, depth test/write from the particle material.
- **Models of a 2D scene** (`retro`): orthographic WE projection in pixels, depth spread over
  ±`farz` (spec 09 §3.12).
- `clearenabled` is honoured in perspective scenes (2D scenes always clear, as before).

Decisions on the open questions (rules of `docs/PARITY-POLICY.md`, evidence from the installed
previews where there is some):

| QO | Decision | Rule / evidence |
|---|---|---|
| 3D-2 | `fov` vertical. The square previews do not separate the two; `neon_sunset`'s sun measures 0.448 of the width with fov 50 (spec value), the preview's camera differs (sun lower and bigger). | 5 (spec) |
| 3D-5 | Key instants stretched to the duration. | spec |
| 3D-6 | Paths in order, looped, linear. | spec |
| 3D-13 | `depthtesting`/`depthwriting` read as the real keys. | spec |
| 3D-16 | Colour × intensity. `arsenal` renders darker than its preview (no specular sheen on the floor); intensity² would brighten it: to calibrate with a capture. | spec |
| 3D-27 | `Rz·Ry·Rx` (M-1, M-2 pass). Not separable on the corpus (`neongrid`, `dna_fragment` curves). | spec |
| 3D-30 | Settled by `neon_sunset`: without sorting, the sun quad (first in the file, depth write, transparent outside the disc) cuts a hole in the terrain drawn after it; the preview shows the terrain. Blended models are sorted back to front when `transparentsorting` is absent or true (WE's 3D templates set it to true), from the rank of the first of them, once at load with the initial camera. | 5 |
| 3D-31 | The reflection is stored upside down (clip y flipped as well, so the winding is unchanged): the shaders read `v = y_ndc·0.5 + 0.5`, and `fantasticcar` then shows the car mirrored under itself as on its preview. Plane `y = 0`, clear colour, scene size. | 5 |
| 3D-33 | World-space particles: see Deviations. | — |

Within a model, meshes whose passes all blend are drawn after its opaque meshes (`fantasticcar`:
the windshield comes before the interior in `body.mdl`; the preview shows the interior through it).

## Media integration

The system's now-playing session (Windows: Global System Media Transport Controls; Linux: MPRIS on the session bus) comes from `lumenwall-audio`'s media hub, which runs the OS source only while something subscribes (`aea5fe0`). The engine follows it for exactly these cases, so a wallpaper that does not use it costs nothing and wakes nothing:

| Consumer | Followed when | What it gets |
|---|---|---|
| Scene scripts | a script names a media handler (`wants_media`: `mediaStatusChanged`, `mediaPropertiesChanged`, `mediaThumbnailChanged`, `mediaPlaybackChanged`, `mediaTimelineChanged`) | the SceneScript events of spec 06 §8.4: title, artist, album, playback state, timeline, thumbnail state and its colours; `MediaFeed` (`engine.rs`, `pump_media`) hands the state to the scene and redraws the outputs showing it |
| Scene textures | a slot or a main texture names `$mediaThumbnail` or `$mediaPreviousThumbnail` (`wants_media_artwork`; commit `d7c8758`) | two render targets of the scene's own pool, 1×1 transparent until a cover arrives. On a new cover (deduplicated by its key): the shown cover is copied into `$mediaPreviousThumbnail` on the GPU, the new one is written into `$mediaThumbnail`; bind groups are rebuilt only when a size changes and the `g_TextureNResolution` of the sampling slots follow. Layers created by scripts are covered. The pixels stay on the CPU only for the upload |
| Web pages | the page registered a `wallpaperRegisterMedia*Listener` (`web.rs`: `set_media_listeners`, `forward_media`) | status, properties, thumbnail (PNG data URL and colours), playback and timeline listeners, only what changed; a listener registered late receives the latest event of its kind |

- Artwork is decoded off the render thread, kept within 512 px and re-encoded as PNG; `http(s)` artwork is never downloaded (nothing leaves the machine). The colours of scripts and pages come from the same function (`lumenwall_web::media::MediaEvent::thumbnail_from_png`), once per artwork.
- Tests: the cover's lifecycle on the GPU (creation on first use, change of cover, same cover, no art, memory given back), no cost for scenes without thumbnails, and an ignored headless render of 3372348904 with the media tests' fake source (no real player).
- Not done: playback control (play/pause/next from a wallpaper). Not tested: a playing session's timeline events on Windows, session changes between players, every MPRIS player; the MPRIS bus glue was only type-checked (`lumenwall-audio` README, "Not tested").

## Quality settings

`EngineSettings { quality: QualitySettings }` (`build_scene_with`; `build_scene` reads the developer variables below). `Default` costs exactly what the engine cost before the settings existed. Wallpaper Engine's own preset names are not verified against a public list; the person's WE runs `msaa x2`, `resolution full`, `postprocessing ultra`, `reflection true` (its `config.json`), and Lumenwall's defaults stay cheaper than that.

| Setting (`QualitySettings`) | Values (default first) | Effect | Developer variable |
|---|---|---|---|
| `msaa` | `off`, `x2`, `x4`, `x8` | Multisampled scene target (`_rt_FullFrameBuffer`): passes draw into a companion texture resolved after every render pass, so copies and samplers see the resolved image. Clamped to what the adapter supports for the target format. A scene with multisampling records through wgpu (the direct Vulkan path has no multisampled attachments: `WgpuReason::Quality`). Image chains stay single-sampled (axis-aligned layers gain nothing: 3018340632 changes by 0.001/255 on average). | `LUMENWALL_MSAA` |
| `texture_resolution` | `full`, `half`, `quarter` | The top 1 or 2 mip levels of every `.tex` are not uploaded (textures without mips stay full). The declared size does not change: texture coordinates and `g_Texture0Resolution` stay. | `LUMENWALL_TEXTURE_RESOLUTION` |
| `hdr` | `off`, `scene` | `scene`: scenes with `general.hdr` render into 16-bit float targets (bloom and effects keep values above 1; 3018340632 differs visibly from the 8-bit render). Direct Vulkan path off for those scenes. | `LUMENWALL_HDR` |
| `native_resolution` | `false`, `true` | Image composites and effect targets at the scene's own size instead of the displayed size (Perf A level 2; the scene target still follows the output). | `LUMENWALL_FULL_SIZE_CHAINS=1` |

Reflections: `_rt_Reflection` is always on for 3D scenes (no knob yet).

## Not done yet

| Feature | Current behaviour |
|---|---|
| 3D: integer colour properties | `"1 0 0"` is read as 0–255 (spec 02 Q1): `fantasticcar`'s body, `demon_core`'s core and `retro`'s stripes (through `usershadervalues`) come out black while their previews show red. Renders with `--set "carbodycolor=1.0 0.0 0.0"` match. To settle in spec 02 Q1 (the 3D previews are evidence for floats). `usershadervalues` are applied to 3D passes only for now (images would turn `retro` black). |
| 3D: P2/P3 of spec 09 | No fog (keys unknown), no `_rt_MipMappedFrameBuffer` mips (no installed scene reads it), no shadows, volumetrics or HDR, spot/tube/directional lights skipped, no parallax in perspective (`parallaxDepth` of models ignored), models without skeleton animation, no `TEX<n>FORMAT` from `formatcombo` (materials that need it set it, e.g. `fantasticcar`). |
| Text layer growth | The glyph texture keeps the size reserved at load (text, editor `size`, 1.5× + 1 em for scripted or bound texts, `maxwidth`); a longer string is drawn smaller to fit instead of growing the layer. |
| Text styling | Not done (no spec, unused by the person's wallpapers): `opaquebackground`/`backgroundcolor`/`backgroundbrightness`, outline, drop shadow, MSDF, colour (emoji) fonts and `blockalign`. None of the installed wallpapers' text objects has an outline or shadow key or turns the background on (their title "shadows" are separate text layers with `blurprecise`). The object's own `angles` are not applied (the parents' are). |
| Puppet extras (spec 08 §11) | Bone physics, inverse kinematics, blend rules, blend shapes (`MORPHING`) and `SKINNING_ALPHA` have no known format: not implemented. Particles attached to bones are not wired (images only). The SceneScript bone and layer API waits for `lumenwall-script` integration. |
| Video textures in scenes (spec 07 §14.2) | Texture skipped. `lumenwall-video` plays them from memory (`VideoInput::Memory`); binding its output to the scene texture slot is not wired. |
| Web wallpapers on Windows | Frames of a Vulkan device come from the web host's D3D11 shared textures (zero copy into the render target); a DX12 device has no import and uses shared-memory frames. On a machine with several GPUs the host is not yet told the render adapter's LUID. Mouse input and a desktop run (only headless rendering and the host alone were run) are not tested (`lumenwall-web` README). |
| Web page audio and automute | `--volume`, `--silent` and pause apply; automute does not (the page's stream comes from the host process, which the detector would count as another application and mute the page in a loop), and that stream can automute the scene sounds of other outputs (spec 07 §15.6). |
| Created layers | Puppet meshes are not created after load (the image gets the plain quad); scripts carried by a creation configuration do not run; a created particle system starts at the layer's origin but does not follow later `origin` writes (like the scene's own systems); 3D models (`.mdl`) cannot be created. |
| Sound layers | Listed and found (`getLayer('jump')`), but `play()`/`stop()`/`isPlaying` are neutral (`lumenwall-script` `hostobj.rs`): the scene does not drive the audio player yet. |
| Script effects on fixed state | Effect visibility, particle `origin`/`scale`/`angles` (read by `lumenwall-particles` at creation), camera/bloom/clear-colour members: scripts can read and write them, the image is unchanged. |
| `localStorage` | In memory (not persisted between runs). |
| Span groups | Each member output shows the whole wallpaper (no per-output slice yet). |
| Playlist transitions | Playlists run in the daemon (`lumenwall` README, "Playlists"; the app lists, starts and steps them): the engine only receives a plan reload of the target, a cut. Wallpaper Engine's `transition` (blend into the next item) needs the old and new renderers alive at once and a blend pass: not done. |
| Plan reload | Wallpapers, layout, fps, audio, pause policy, GPU release delay and properties apply live (`EngineControl::Reload`); `hwdec`, input, particles, debug switches apply to wallpapers loaded afterwards. A plan received while the GPU is released restores it first (≈ 0.2 s), even when nothing needs drawing. A video applied to a device created without decode queues recreates the device (below): the other targets are rebuilt too (scripts restart, web pages reload), and a video that fails to open then leaves its target in error instead of keeping the previous wallpaper. |
| Screenshot formats | PNG data whatever the extension. |

## SceneScript

`SceneScripts::load` runs at the end of `build_scene` when `Settings::scripted()` is not empty:
it lists the created scriptable objects (images, texts, particles, groups) in render order, then
loads, in creation order, every script attached to a member of spec 06 §1.2 — `origin`, `scale`,
`angles`, `visible`, image `alpha`/`color`/`parallaxDepth`/`brightness`, text
`color`/`alpha`/`pointsize`, particle `parallaxDepth` — plus image effect `visible` and effect pass
`constantshadervalues`. The `text` of a text object is a dynamic text layer (spec 06 F2).

Scripts read and write the `lumenwall-scene` settings. `SceneRuntime::update` ticks the scripts first;
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
`<member>_<layer>` keys of `lumenwall-script` stay unique.

### Layers created by scripts

`thisScene.createLayer(asset | path | configuration)`, `destroyLayer`, `sortLayer`,
`getInitialLayerConfig` and `engine.registerAsset` (Wallpaper Engine's public `IScene` /
`IEngine` reference) work when a script of the scene names one of them (`build_scene` checks the
sources; otherwise nothing below exists).

- **Scene model.** A created layer is a `lumenwall-scene` object added after build
  (`Project::add_object`, the same conversion as `scene.json` objects): settings, model, material,
  effects, constants and user-property bindings like a file layer, so every script member works.
  A path names an image model (`models/*.json`), a particle system (`particles/*.json`) or a
  sound; a configuration object is a `scene.json` object (a `text` member makes a text layer).
  Workshop scripts name assets relative to their folder: `models/bar.json` resolves to
  `models/workshop/<__workshopId>/bar.json` when the exact path does not exist.
- **Pools.** `destroyLayer` parks the object and its runtime state (image, animated passes,
  particle system, text); the next layer of the same configuration takes it back with its
  initial setting values and a new id (stale script references stay dead). The scene model and
  the GPU resources therefore grow with the peak number of live layers, not with the number of
  `createLayer` calls. `registerAsset` prepares one object at once, set up with the scene, so the
  first `createLayer` in the frame loop creates nothing.
- **GPU.** New objects get their passes through `RenderScene::extend` with the scene's shader
  provider (requests memoised), texture cache, compiled shaders, pipelines and bind group
  layouts: only uniform blocks, quad vertices, composites, bind groups (and a particle stream)
  are created. Uniform blocks and vertices go to the scene's dynamic buffer, which grows with
  headroom.
- **Order.** The frame operations of every drawn object are kept from load; when scripts create,
  destroy or sort layers, the operations are rebuilt in the scripts' layer order
  (`RenderScene::set_ops`) once per frame, and only the render bundles whose runs of passes
  changed are re-recorded. `getLayerIndex` / `sortLayer` indices count every layer, sounds
  included; a created layer goes on top.
- **Timing.** Structural changes made during a frame's scripts (or at load, in `init`) are applied
  after them, before drawing: a layer created and positioned in the same `update` appears at its
  position, and a particle system created there starts at the origin the script gave it.
- `ILayer.alignment` (read/write) moves the anchor of an image's rectangle.
- `render_png` options for these checks: `--pointer X,Y`, `--click frames`, `--audio synthetic`
  (a moving spectrum, nothing is played).

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
`scale`) is written into the object's `lumenwall-scene` settings, which the text renderer reads.

## Deviations

- Visibility is inherited from created ancestors (effective = own and every parent's). The
  reference ignores the parent (spec 02 Q16); Wallpaper Engine hides the children of a hidden
  layer, and wallpapers rely on it to switch layout variants by property (rule 5). To confirm
  against Wallpaper Engine.
- `TEX0FORMAT` is forced for particle passes from the particle texture, like for an image's main
  texture (spec 04 §13.2); without it R8 particle textures draw as opaque squares.
- The particle sprite sheet comes from the `.tex` (TEXS or `.tex-json`, parsed by `lumenwall-formats`)
  before `lumenwall-particles`' own inference.
- A positional wallpaper without `-w`/`-r` runs on every output as a desktop wallpaper (decided
  for the product; the reference opens a 640×480 window). Lives in the `lumenwall` glue.
- Screenshots read `_rt_FullFrameBuffer` and always write PNG.
- An image's composites A/B (spec 04 §8.9) get the image size only when its chain writes them
  (chain planned in creation order from the declared targets, checked again on the prepared
  passes) or when a material, effect or pass target of the wallpaper names them
  (`_rt_imageLayerComposite_<id>_a/_b` among the project's strings); the others are 1×1 and
  would only hold zeros. Identical pixels on every installed scene; render targets of
  3463520581 515 → 384 MiB (headless, textures included), 1960765686 380 → 190 MiB. A layer
  that a script creates later and that names a composite of a load-time image left 1×1 would
  read zeros at another `g_TextureNResolution` (no installed wallpaper does), as would an effect
  made visible later if effect visibility becomes live.
- SceneScript, following Wallpaper Engine where the reference's data model contradicts its
  documented API (`docs/PARITY-POLICY.md` rules 4–5, `// we-doc:`):
  - `scale`, `angles` and `visible` scripts of images, texts and particles drive the copy the
    renderer reads (spec 06 §1.2 / Q-M1-4: the reference writes an unused copy, no visible effect);
  - `ILayer.color` is a `Vec3` for scripts and a `Vec3` write keeps the stored alpha (spec 06
    §2.2 would reset `w` to 0 and hide layers whose shader reads `g_Color4`);
  - scripts on effect visibility and effect pass constants run (spec 06 §1.3: never executed);
  - groups are scriptable layers (spec 06 §1.2: not scriptable), so their scripts move or hide
    their children;
  - layer members read by scripts are copies (`lumenwall-script`), and sound objects are layers;
  - cursor events go to the topmost layer under the cursor whose scripts handle cursor events
    (Wallpaper Engine's Dino Run draws a full-screen post-process layer, scripted through its
    effect, over the dinosaur that receives the clicks).
- Text objects follow Wallpaper Engine rather than the reference renderer of spec 04 §18 (rule 5;
  evidence: the `size` the WE editor stores for every text of 3044659344, 3089056751 and
  3372348904, and their WE preview images registered against Lumenwall renders of the same scene):
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
- An image without `size` takes the `width`/`height` of its model before the real size of its
  main texture (spec 02 Q26: unused by the reference). Evidence: deep_space's `background` model
  (1920×1080 over a 480×270 flow mask) covers the screen in its WE preview; the reference draws it
  as a 480×270 inset (rule 5).
- Particle materials: `usershadervalues` (`{"<user property>": "<material constant>"}`) set the
  constant from the property at load and on every property change (shimmering_particles tints
  its motes with `schemecolor`/`color2`). Image materials get it from `lumenwall-scene`.
- Puppet layer clocks advance by at most 0.25 s per frame: the engine clock keeps running while
  an output is paused, and spec 08 §9.3 excludes the pause (QO-P18). Animations only slow down
  below 4 fps.
- Scene time per wallpaper, pauses excluded (Frame loop, `clock.rs`; rule 5): spec 04 §2.1 and
  spec 03 §13.1 describe the reference's process-wide clock (origin at video start, never reset on
  a wallpaper change, `dt = g_Time` on the first frame, the pause included in the next `dt`).
  Wallpaper Engine documents `engine.runtime` as the time the wallpaper has been running and
  `frametime` as the duration of the last frame, and spec 06 §6.1 stops `t_g` during a pause. In
  the reference one process runs one wallpaper, so its first `dt` is the load time; in a daemon it
  was the uptime, and Dino Run's dinosaur (moved by `speed × frametime`) left the screen. No
  rollover (spec 04 §2.1); the `f32` of `g_Time` only grows with the time the wallpaper is on
  screen since its last build. Each step is capped at max(0.25 s, 2/fps): a scene that sleeps
  between pointer events advances by at most that much per event.
- Particles of a perspective scene (spec 09 §3.9): `lumenwall-particles` is unchanged and keeps
  the Y inversions of spec 05 §2.2 (QO-3D-33). The engine gives the system a zero-sized scene
  (no recentring of its origin) and a world matrix `T(origin)·Rz·Ry·Rx·S(scale)·Sc(1, −1, 1)`: the
  mirror cancels the inversions of every Y-inverted field (emitter origins, box directions,
  velocities, gravity, turbulence frames); fields spec 05 does not invert (sphere directions,
  control point offsets, vortex axes) come out mirrored. Parents of particle systems are ignored.
- 3D transparent sorting is done once at load (spec 09 §3.6 implies per frame): reordering draws
  would rebuild the render bundles; with a moving camera the order of the first frame is kept.
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
| P17 | Zero-weight vertices follow the shader formula (see Deviations); weights on a missing bone reject the skeleton (`lumenwall-formats`). | 2 |
| P18 | Layer clocks advance even when the layer is hidden, never while its image is hidden; pauses are capped out (see Deviations); a missing `visible` stays false (spec 02). | 3/6 |
| P19, P5 | `SKINNING_ALPHA` and `MORPHING` combos are never enabled. | 4 (unknown data) |

## Running

```bash
cargo run --release -p lumenwall -- 3018340632                 # desktop wallpaper on every output
cargo run --release -p lumenwall -- -w 0x0x1720x720 3018340632 # borderless window
LUMENWALL_WINDOWED=1 cargo run --release -p lumenwall -- 3018340632 # 640x480 window
LUMENWALL_STATS=1 ...                                          # fps, CPU/GPU ms per frame, memory every 5 s
cargo run -p lumenwall-engine --example render_png -- --size 1720x720 --frames 60 --save 0,30,59 3018340632
cargo run -p lumenwall-engine --example render_png -- --no-particles --debug object=28 --debug pass-log <path>
cargo run --release -p lumenwall-engine --example render_png -- --frames 90 --save 0,45,89 --pointer 1400,300 <web wallpaper>
cargo run -p lumenwall-engine --example render_png -- --audio synthetic --frames 60 3372348904 # visualizer
cargo run -p lumenwall-engine --example render_png -- --size 686x386 --frames 225 --pointer 214,340 --click 200 <dino_run> # jump into a coin
```

Web wallpapers run in real time off screen (frame `i` at `1 + i/fps` s after the first painted
frame); `--pointer` sends a mouse move, `LUMENWALL_WEB_TEST_AUDIO=1` feeds a synthetic beat to
pages that listen to audio, `LUMENWALL_WEB_FRAMES=memory` forces the shared-memory frames,
`LUMENWALL_WEB_CONSOLE=1` prints the page console.

PNG files go to `target/agent-integration/shots/` by default (never into the repository: the
content is copyrighted, `docs/TESTING.md`).

### Start with the session (Linux)

Install the binary in the `PATH` (for example `~/.local/bin/lumenwall`), choose the wallpaper in
`~/.config/lumenwall/config.toml`, then copy the autostart entry:

```bash
mkdir -p ~/.config/lumenwall ~/.config/autostart
printf 'wallpaper = "3018340632"\n' > ~/.config/lumenwall/config.toml
cp packaging/linux/lumenwall.desktop ~/.config/autostart/
```

The desktop environment starts `lumenwall` at login (GNOME, KDE, XFCE… honour
`~/.config/autostart`). On GNOME Wayland Lumenwall uses X11 desktop windows through XWayland;
on wlroots compositors and KDE it uses layer-shell. Editing `config.toml` while Lumenwall runs
applies the fps and property changes live. Remove the file from `~/.config/autostart` to stop.

## Measurements (RTX 4080 SUPER, Vulkan, release, reference wallpaper 3440×1440 scene)

| Situation | Result |
|---|---|
| Load (scene, 13 textures, 23 WE passes from the shader cache, 4 particle systems) | 80–230 ms |
| Window 1720×720, 30 fps cap | 30.0 fps, CPU 0.4–0.6 ms/frame, 1.8 % of one core, GPU 0.8–4.7 ms/frame (clock-dependent: the GPU stays at low clocks under this load; 0.5 ms headless at full clocks) |
| Desktop on DP-4 + DP-5 (2560×1440 each, X11 via XWayland) | 30.0 fps, 1.6 % of one core, RSS 240 MiB (anonymous 49 MiB, the rest is file-backed driver libraries and the mapped `.pkg`), VRAM 268 MiB (nvidia-smi), 127 MiB tracked by lumenwall-render |
| Fullscreen window on top / user pause | paused; scene sounds suspended, capture closed: 0 wake-ups/s on Lumenwall's threads, 0.00 % CPU (`--silent`, 3018340632 and 3372348904). Left: NVIDIA driver threads created by `vkCreateDevice`, ~115/s (an unnamed one at ~100/s, `[vkrt]` ~10/s, `[vkps]` ~4/s), alive as long as the wgpu device; gone after the GPU release below |
| GPU release after the pause delay (window 480×270, user pause, `gpu-release-delay = 15`; before → after) | 3018340632: 115 → **0 wake-ups/s**, 13 → 6 threads, RSS 193 → 116 MiB (PSS 89 → 62), VRAM 170 → 0 MiB (nvidia-smi). 3463520581: RSS 209 → 131 MiB, VRAM 642 → 0 MiB. Video 3655057993: RSS 250 → 148 MiB, VRAM 222 → 0 MiB. Web 3044696375 (8 processes with the CEF host): 474 → 0 wake-ups/s, 0.5 → 0 % CPU, PSS 446 → 47 MiB. Release itself: 30–75 ms |
| FFmpeg loaded at the first video (lumenwall-video `ffmpeg::sys`), scene 3018340632 | `--version` 18–20 → 2.2–2.5 ms, RSS 145 → 120 MiB, 167 → 49 shared objects; `check_supported` loads it for a video wallpaper, so a missing FFmpeg fails a plan change before anything is released |
| Vulkan loader restricted to the remembered driver (`vulkan_driver`), launch of 3018340632 | instance 71–75 → 47–49 ms, adapter + device 146–183 → 102–123 ms, RSS 194 → 145 MiB, 176 → 167 shared objects; GPU released: RSS 101 → 55 MiB |
| GPU restore, resume → first frame presented | 3018340632: 210–230 ms (instance 54–58 ms, adapter + device 84–93 ms, scene 64–74 ms); 3463520581 (67 images, 156 passes): 256–268 ms; video: 227–243 ms (zero copy kept); web: 367–372 ms (CEF start, the last image stays meanwhile). First restore of a process: up to 330 ms (instance 85 ms) |
| Video wallpaper 3655057993 (4K60 H.264, 30 fps cap), zero copy | 30.0 fps, 2.3–2.6 % of one core, RSS 325 MiB, VRAM 318 MiB; paused: 0.00 % |
| Web wallpapers in a visible 1920×1080 window (all processes of the instance, details in `crates/lumenwall-web/README.md`) | clock 3044696375: 11.7 % of one core (lumenwall 1.7 %), PSS 493 MiB, VRAM 145 MiB; CORSAIR collection 8.1 %; Corsair-O-Tron 15.9 %; paused 0.5 %. Frame copy 0.33 ms (DMA-BUF), 3.9 ms (shared-memory fallback, ≈ 2× the total CPU). Scene 3018340632 in the same window: 2.2 %, PSS 113 MiB, VRAM 302 MiB |
| Hot switch over IPC (`apply`) | scene → web 2–3 ms (host started, page painted ≈ 0.4–1 s later), web → scene 96–122 ms (scene load); scene → video 188–191 ms (device recreated with decode queues, then zero copy: 1.7–2.0 % CPU, 520 wake-ups/s, RSS 261 MiB, VRAM 222 MiB, against NVDEC + copy before: 9.7 %, 1280–1870/s, 576 MiB, 679 MiB) |
| Covered output | paused (`OCCLUDED`), no frame |
| Puppets, 3463520581 at 1920×1080 (16 skeletons, 78 bones, 58 attached objects), 600 frames, particles off | puppet work (clocks, blending, matrices, palettes, attachment chains) **8.5–9 µs CPU per frame**; whole frame CPU 0.37 vs 0.34–0.41 ms and GPU 0.77 vs 0.77–0.79 ms before/after (noise); no allocation per frame (`tests/puppet_alloc.rs`); 5 `BONECOUNT` variants of `genericimage4` compiled once (shader cache) |

Windows 11, RTX 4080 SUPER + AMD iGPU without monitor, release builds, off the desktop: the
daemon with `--no-config --silent --fps 30 -w 0x1600x480x270` (a window below the monitors),
a private IPC endpoint and `LOCALAPPDATA`, at below-normal priority, while the PC was in use
(a game running: absolute times are pessimistic). Counters: `NtQuerySystemInformation`
(private bytes, working set, per-thread context switches over 10 s), `VirtualQueryEx`, the
engine's own log lines. Base: main b4d1db6 (engine in the daemon). Wallpaper Engine, from the
desktop bench (`docs/benchmarks/lumenwall-vs-wallpaper-engine.md`): empty 3.1 MiB private,
16.7 MiB working set; command → first GPU activity 462 ms (3463520581), 331 ms (3018340632).

| Situation | Base | Now |
|---|---|---|
| Empty daemon after a scene and `clear` | 62–81 MiB private, 84–120 MiB working set, 10–14 threads, 0.0–0.1 wake-ups/s; `nvoglv64`, `nvgpucomp64`, `nvspcap64`, `nvrtum64`, `nvapi64` still mapped | **2.23 MiB private, 1.97 MiB working set**, 8 threads, 0.0 wake-ups/s, no GPU module (`dxgi.dll` only, imported by wgpu); after 7 switches from empty and 6 between two wallpapers: 2.35 MiB, 2.08 MiB, 5 threads (`main`, `tracing-appender`, `lumenwall-playlists`, `lumenwall-ipc-accept`, a system pool thread), 0.0 wake-ups/s |
| What made those MiB, one change at a time | — | engine process (the driver never loads in the daemon): 62–81 → 6.3–7.0 MiB; log queue 128 000 → 1024 lines: → 2.28 MiB; Windows heaps trimmed when the engine process ends: 3.14 → 2.40 MiB after the mixed switches above |
| Vulkan modules of the process that renders | NVIDIA and AMD (`amdvlk64`, `AMD\VkCache` created), D3D12 drivers of both vendors (DX12 enumeration) | NVIDIA only (`nvoglv64` and the D3D12 parts NVIDIA's Vulkan presentation uses itself); no `amdvlk64` |
| GPU opening | instance (Vulkan + DX12) 38–126 ms, adapter + device 294–605 ms | instance + device 148–177 ms (cold driver, new process), on a thread while the platform opens |
| First start, process start → renderer ready (3018340632) | 702–772 ms | 446–507 ms |
| Command → first GPU activity, from empty | 808 ms (3463520581), 719 ms (3018340632), desktop bench of 376a97d (off the desktop every switch after `clear` failed: winit) | 279–344 ms (3463520581, one run at 570), 287–377 ms (3018340632) |
| Command → first frame, from empty | — | 511–645 ms (3463520581, one run at 1058), 504–548 ms (3018340632) with parts A and B merged (625–753 and 558–987 ms before them): engine process running 43–95 ms after the command, platform 44–77 ms alongside the GPU 148–177 ms, then the swap chain 197–254 ms alongside the scene build (169–412 ms here) |
| Command → first frame, between two wallpapers (same engine process) | — | **202–241 ms** (3463520581), **231–269 ms** (3018340632) with parts A and B merged (scene builds 2–3× faster; 504–572 and 237–493 ms before them); first GPU activity 45–120 ms |

From empty, the first frame waits for a new process, a cold Vulkan driver and NVIDIA's first
swap chain of a device (≈ 0.2 s): about 0.45 s before any scene work, more than Wallpaper
Engine's first GPU activity. Only a device kept open would remove it, and a device keeps the
driver in memory (≥ 30 MiB private): the two goals are traded against each other.

SceneScript (headless 1280×720, release, `render_png`; script time = `SceneScripts::tick`):

| Wallpaper | Scripts | Script time per frame | Frames redrawn |
|---|---|---|---|
| 3018340632 (no script) | none: no runtime | 0 | unchanged (particles) |
| 3089056751 (scale + clock text) | 2, 1 `update` | 3–9 µs | 1 of 91 |
| 3044659344 (2 scale + clock text) | 3, 2 `update` | 5–8 µs | 1 of 91 |
| 3372348904 (20 scripts, 7 text layers) | 19 active | 10–18 µs | particles |
| Local test variant of 3089056751 (8 animated scripts) | 7 `update`, 1 timer | 11–18 µs | every frame |

JS heap 190–245 KiB per wallpaper; no heap allocation on the Rust side in a steady-state frame of
numeric scripts (unit test `steady_state_frames_do_not_allocate`), also with 16 created layers
moved every frame (`steady_state_with_created_layers_does_not_allocate`; vectors made with
`new Vec3` or read from a layer each frame allocate in `lumenwall-script`).

Created layers (same machine and settings, `render_png`; `created layers:` log line):

| Wallpaper | Result |
|---|---|
| Dino Run, 60 s at 30 fps, 3 jumps | 2 objects prepared at load (coin, pickup particles: 1 ms each, first shader translations); 1 more coin object at run time (scene model 120 µs from the loose folder, GPU 49 µs); 12 `createLayer` served by the pools; 24 order rebuilds, 17 µs each (operations, segments, changed bundles); CPU 0.12 ms/frame (base revision 0.09 ms with its script disabled by the missing API), GPU 0.07 ms |
| 3372348904, visualizer | 63 bars in `init`: 27 µs each in the scene model, 12 µs each on the GPU (5 µs once the shader request is memoised), one order rebuild (80 µs); +0.1 MiB GPU memory; script 106 µs/frame for 64 bars (JS), CPU 0.27 ms/frame |
| Every other installed scene | pixel-identical to the base revision (particles off, frames 0 and 44) |

Text (release, same machine, `cargo test -p lumenwall-engine --release --test text_content -- --ignored --nocapture`):

| Situation | Result |
|---|---|
| Steady state, 7 texts (3372348904) / 1 text (3044659344) | 0 glyph texture regenerated over 120 frames; whole `SceneRuntime::update` 1.0 µs / 0.3 µs per frame; a static scene with text stops redrawing |
| One regeneration (`set_text`): layout + rasterisation + upload | 0.2 ms (32 pt, 3372348904), 1.0 ms (96 pt = 400 px per em, 1954×640 texture, 3044659344) |
| Layout + rasterisation alone, 32 pt clock string (unit test) | 0.4 ms |

3D scenes (headless 1920×1080, 240 frames, release, `render_png`; CPU = update + encode + submit):

| Scene | 3D objects (meshes) | CPU / frame | GPU / frame | Load |
|---|---|---|---|---|
| arsenal | 1 (6) | 0.05 ms | 0.13 ms | 188 ms |
| fantasticcar (reflection) | 4 (9) | 0.05 ms | 0.07 ms | 178 ms |
| demon_core | 2 (2) + particles | 0.05 ms | 0.07 ms | 188 ms |
| dna_fragment | 5 (5) + particles | 0.05 ms | 0.07 ms | 164 ms |
| ricepod (sprite) | 7 (10) + particles | 0.06 ms | 0.06 ms | 189 ms |
| neon_sunset | 2 (2) + 2 images + particles | 0.09 ms | 0.11 ms | 181 ms |
| audiophile (reflection) | 4 (4) | 0.03 ms | 0.04 ms | 164 ms |
| techno | 4 (7) | 0.03 ms | 0.05 ms | 171 ms |
| retro (2D, 2520×1080) | 1 (1) + 2 images | 0.02 ms | 0.04 ms | 165 ms |

2D scenes are unchanged: 3018340632, 3463520581 and 3044659344 render bit-identical to the
previous build without particles (the clock text of 3044659344 aside).

## Tests

- `cargo test -p lumenwall-engine`: stand-in shaders against the binding contract (every variant
  parses, validates and reflects), particle material parsing, property bindings, uniform views,
  the blending contract between `lumenwall-scene` and `lumenwall-render` (`tests/blend_contract.rs`),
  SceneScript on in-memory scenes (`script::tests`: setting writes and change reports, no change
  for constant scripts, `applyUserProperties`, layer lookup, effect and constant scripts, text
  sink, transforms, zero allocation per frame), the scene clock (`clock::tests`: one-frame first
  step whatever the uptime, pauses capped, same running time at 30 and 144 fps).
- `cargo test -p lumenwall-engine -- --ignored` also runs `tests/scene_clock.rs`: Dino Run's
  dinosaur, drawn alone, stands at the same place after one simulated second at 30 fps from
  t = 1 and at 144 or 30 fps from t = 5000 and 100 000 (it was off screen from t = 30 on).
- `cargo test -p lumenwall-engine -- --ignored` (GPU + content of `docs/TESTING.md`): the reference
  wallpaper renders with real shaders (0 stand-in, bloom included), 1 image, 4 particle systems,
  live rain; the colour LUTs of `materials/lut` load as 32³ volume textures (`tests/volume_lut.rs`);
  3463520581 has 16 puppets, all skinned on the GPU, and 58 attached objects.
- `cargo test -p lumenwall-engine -- --ignored` also runs `tests/runtime_layers.rs`: Dino Run
  (coins from a registered model, pickup particles after a click, pools) and 3372348904 (63 bars,
  one order rebuild, bars standing on their baseline only with a spectrum).
- `cargo test -p lumenwall-engine -- --ignored` also runs `tests/text_content.rs` (3044659344,
  3372348904): every text drawn with the WE font shader, no regeneration in the steady state,
  one regeneration per `set_text` change, none for the same string.
- Spec 08 §10 vectors (V1–V10, GPU palette, two-bone mix, attached child) in `puppet::tests`;
  `tests/puppet_alloc.rs`: the per-frame puppet path allocates nothing.
- `tests/web_content.rs` (`--ignored`, needs `lumenwall-web-host`, set `LUMENWALL_WEB_HOST`
  when it is not next to the test binary): the clock 3044696375 renders through the CEF host
  at the output size; corsair_o_tron loads with its quoted property values.
- `cargo run -p lumenwall-engine --example render_png -- <ids>`: visual checks.
- `cargo run -p lumenwall-engine --example render_png -- <ids>`: visual checks (`--start T` sets
  the scene time of the first frame, as if the scene had been running that long; every step
  lasts `1/fps`, the first one too. `--audio L` feeds a constant spectrum to audio-reactive
  scenes).
- `model3d::tests`: vectors M-1, M-2, N-1, C-1 to C-3 and R-1 of spec 09 §4.5, camera fade
  ramps and shake determinism.
- `tests/scene3d_content.rs` (`--ignored`, GPU + `LUMENWALL_WE_ASSETS`): the nine installed 3D
  projects render with their expected 3D objects and meshes, no stand-in shader, no skipped
  object; `arsenal`'s per-frame update (camera path, lights, uniforms) performs zero heap
  allocations over 120 frames and dirties every frame.

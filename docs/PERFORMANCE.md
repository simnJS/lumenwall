# Performance principles

Performance is Lumenwall's first priority. Every design and dependency choice is judged in this order:

1. CPU, GPU, RAM and power cost, idle and under load
2. Visual and behavioural parity
3. Implementation convenience

A change that makes a wallpaper look identical but costs more is a regression.

## Budgets (targets, measured on the reference scenes)

| Situation | Target |
|---|---|
| Desktop fully covered or screen locked | 0 frames rendered, ~0% CPU, ~0% GPU, no timer wake-ups |
| Paused longer than `gpu-release-delay` (default 5 min) | No GPU device: 0 driver threads, 0 VRAM; resume to first frame < 300 ms for a cached scene |
| Static scene (no animation, no effects over time) | Render once, then only on input or property change |
| Animated scene at the target fps | Frame time well below the vsync interval; no CPU frame spikes |
| Steady-state frame | Zero heap allocations on the render thread |
| Memory | Assets resident once; no CPU copy kept after GPU upload unless needed |

## Rules

**Do no work that can be skipped.**
- Stop the render loop when the output is hidden, covered by a fullscreen window, or the session is locked. Do not poll; use OS notifications where they exist.
- After a long pause, release the GPU device itself: a driver keeps threads and memory for every device, rendering or not (NVIDIA: ≈ 115 wake-ups/s). Keep what makes the rebuild fast (parsed wallpapers, mapped archives, shader and pipeline caches), drop the rest.
- Detect static content and stop redrawing.
- Cap the frame rate to the configured target, aligned to the display's refresh rate.

**Keep the GPU fed, the CPU idle.**
- Upload BC1/BC2/BC3 textures directly when the GPU supports them; decode on the CPU only as a fallback.
- Build pipelines and shader modules once; cache translated shaders and pipelines on disk between runs.
- Batch uploads; reuse buffers and bind groups; no per-frame resource creation.
- Run particle simulation on the GPU when the particle system allows it.

**Zero-copy media.**
- Decode video with the hardware decoder and hand the frame to the GPU without a CPU round-trip (D3D11 on Windows, DMA-BUF on Linux).
- Read `.pkg` archives through memory mapping; borrow from the mapping instead of copying.

**Load lazily, free early.**
- Parse and upload only what the scene references.
- Drop CPU-side pixel buffers after upload.

**Measure.**
- Every subsystem exposes frame-time and memory counters under a debug flag.
- Performance regressions are tested with benchmarks (`criterion`) on parsing and with frame-time captures on reference scenes.
- Release builds use fat LTO, one codegen unit and `panic = "abort"`.

## Measurements

Reproducible with the release binary, `--silent`, a 480×270 window (`-w 0x0x480x270`), a private
IPC socket (`LUMENWALL_IPC_ENDPOINT`), the user pause (`lumenctl pause`) and per-thread
`voluntary_ctxt_switches + nonvoluntary_ctxt_switches` deltas from `/proc/<pid>/task/*/status`
over 5–10 s; VRAM from `nvidia-smi`. RTX 4080 SUPER, driver 595.99, GNOME 50 (Wayland).

### GPU release after a long pause

| Wallpaper | Paused, GPU kept | Paused, GPU released | Resume → first frame |
|---|---|---|---|
| 3018340632 (scene, 27 passes) | 115 wake-ups/s, 13 threads, RSS 193 MiB, VRAM 170 MiB | 0 wake-ups/s, 6 threads, RSS 116 MiB, VRAM 0 | 210–230 ms |
| 3463520581 (scene, 67 images, 156 passes) | 115 wake-ups/s, RSS 209 MiB, VRAM 642 MiB | 0 wake-ups/s, RSS 131 MiB, VRAM 0 | 256–268 ms |
| 3655057993 (4K60 video, zero copy) | 115 wake-ups/s, RSS 250 MiB, VRAM 222 MiB | 0 wake-ups/s, RSS 148 MiB, VRAM 0 | 227–243 ms |
| 3044696375 (web, lumenwall + 7 CEF processes) | 474 wake-ups/s, 0.5 % CPU, PSS 446 MiB | 0 wake-ups/s, 0 % CPU, PSS 47 MiB (1 process) | 367–372 ms (page reload; the last image stays) |

Restore breakdown (3018340632): Vulkan instance 54–58 ms (85 ms the first time in a process),
adapter + device 84–93 ms, scene rebuild 64–74 ms from the shader and pipeline caches.
Keeping the Vulkan instance alive across the release was measured and rejected: −55 ms on the
restore, but +100 MiB RSS (driver libraries stay mapped) and 0.5 wake-ups/s while released.
Restricting the Vulkan loader to the NVIDIA driver (`VK_LOADER_DRIVERS_SELECT=*nvidia*`, user
environment) saves another 25–40 ms on instance creation on this machine (9 ICDs installed);
Lumenwall now does it by itself (below).

### Baseline (same machine and method, November 2026)

Facts measured while the GPU release was written; they size the next optimisations.

| Subject | Measurement |
|---|---|
| Steady state, 22 installed scenes, 30 fps cap | every animated scene at 30.0 fps, never above; static scenes (3044659344, 3089056751 with clock texts and scripts, arsenal, audiophile, fantasticcar, techno): 0 frame in 5 s after the first |
| Launch → first frame presented (cached shaders and pipelines) | 3018340632: 300–337 ms (exec + config 19 ms, platform 8–15 ms, Vulkan instance + device ≈ 170 ms, scene 100–110 ms); 3463520581: 345–404 ms; video: 311–337 ms |
| Executable | 19.3 MB stripped, 148 shared libraries at start (135 of them pulled by FFmpeg), `--version` 16 ms (`/bin/true`: 0.3 ms). In a scene process the FFmpeg-only libraries are 24.9 MiB RSS (8.4 MiB PSS) |
| Resident memory of a scene (3018340632) | RSS 192 MiB: heap 19 MiB, executable 12 MiB, NVIDIA libraries and mappings 60 MiB, Mesa ICDs loaded by the Vulkan loader for enumeration (libLLVM 43 MiB, RADV 3.5 MiB, lavapipe 2.5 MiB), mapped `.pkg` 6 MiB. With `VK_LOADER_DRIVERS_SELECT=*nvidia*`: 142 MiB |
| Scene → hot-loaded video → scene (`apply` over IPC) | RSS 194 → 569 → 305 MiB (heap 102 MiB kept); `malloc_trim(0)`: 227 MiB in 5 ms. Zero-copy video → scene: 253 → 218 MiB after trim. `glibc.malloc.arena_max=2`: no change; jemalloc (`LD_PRELOAD`): no reduction |
| Video applied to a scene device (no decode queues) | NVDEC + copy: 8.3 % CPU, 1480 wake-ups/s, RSS 600 MiB, VRAM 683 MiB; the same video started alone (zero copy): 2.5 %, 783 wake-ups/s, 274 MiB, 222 MiB. A scene device created with decode queues: +4 MiB VRAM, +7–17 MiB RSS, +35–55 ms device creation, same wake-ups |
| Render targets (headless) | 3463520581: 452 MiB of targets, 388 MiB of image composites, of which 121 MiB (B) and 10 MiB (A) are never written; 1960765686: 253 MiB of targets, 190 MiB of composites never written |
| GPU time | Headless at full clocks: 0.35–0.80 ms/frame for the heaviest scenes (3840×2160). In a 480×270 window the scene still renders at its own size: 5.8–7.3 ms/frame at the low clocks the driver keeps (since part A below, scenes render at the size they are displayed at) |
| Web 3044696375, Chromium switches (`--renderer-process-limit=1`, `--disable-background-networking`, `--num-raster-threads=1`, `--js-flags=--lite-mode`, `--enable-low-end-device-mode`) | 6.1–7.6 % CPU and 445–463 MiB PSS against 6.6 % and 459 MiB without: no gain beyond noise |

### Optimisations from the baseline (same machine and method)

Each line was measured before and after its own change, the others already in place for the
later lines. Headless renders of every installed scene (Workshop and `defaultprojects`, 1920×1080,
particles off, frames 0 and 44) stay identical to the pixel after each change.

| Change | Before | After |
|---|---|---|
| Video applied to a scene device: device recreated with the decode queues (GPU release mechanism, Vulkan instance kept). Scene 3018340632 → video 3655057993 → scene over IPC | switch 187–257 ms; video by NVDEC + copy: 9.7 % CPU, 1280–1870 wake-ups/s, RSS 570–582 MiB, VRAM 675–683 MiB; back to the scene: RSS 305 MiB, VRAM 258 MiB | switch 188–191 ms; zero copy: 1.7–2.0 % CPU, 520 wake-ups/s, RSS 261 MiB, VRAM 222 MiB (= the video started alone); back to the scene: RSS 251 MiB, VRAM 174 MiB |
| `malloc_trim(0)` (glibc) after a wallpaper is unloaded and after the GPU release, 0.8–2.6 ms each | back to the scene after the video: RSS 249 MiB; GPU released: 3018340632 117 MiB, 3463520581 131 MiB | 219–220 MiB; 101 MiB and 110 MiB |
| Image composites A/B at the image size only when the chain writes them or a material names them, 1×1 otherwise (pixels identical on the 24 installed scenes) | tracked GPU memory, headless 1920×1080: 3463520581 515 MiB, 1960765686 380 MiB, 3372348904 316 MiB, 3089056751 253 MiB, razer_bedroom 206 MiB; VRAM in the window (nvidia-smi): 3463520581 642 MiB, 1960765686 438 MiB, 3372348904 415 MiB, 3018340632 170 MiB | 384, 190, 194, 141, 141 MiB; 514, 247, 257, 170 MiB |
| Vulkan loader restricted to the driver of the GPU used last time (`VK_LOADER_DRIVERS_SELECT` set by `main` from `~/.cache/lumenwall/vulkan-driver`; every driver again when it finds no adapter or another one). Launch of 3018340632 | instance 71–75 ms, adapter + device 146–183 ms, exec → scene loaded 347–401 ms, RSS 194 MiB, 176 shared objects; GPU released: RSS 101 MiB | instance 47–49 ms, adapter + device 102–123 ms, exec → scene loaded 308–374 ms, RSS 144–145 MiB, 167 shared objects; GPU released: RSS 54–55 MiB |
| FFmpeg opened with `dlopen` at the first video (Linux; no `DT_NEEDED` left, 40 functions through a table), 13–15 ms then | `ldd`: 148 libraries; `--version` 17.8–20.3 ms (median); scene 3018340632: RSS 144–145 MiB, 167 shared objects mapped | `ldd`: 26 libraries; `--version` 2.2–2.5 ms; RSS 120 MiB, 49 shared objects; video hot switch and video start unchanged (zero copy) |

The five together, baseline build against the last one, runs interleaved:

| Situation | Baseline | Now |
|---|---|---|
| Launch of 3018340632 | instance 71–78 ms, adapter + device 124–199 ms, exec → scene loaded 333–424 ms, RSS 192–193 MiB, 176 shared objects, `--version` 17.7 ms | 36–39 ms, 109–142 ms, 282–370 ms, RSS 119–120 MiB, 49 shared objects, 2.2 ms |
| Scene → video → scene over IPC | video: 9.7–10.3 % CPU, 1780–1960 wake-ups/s, RSS 568 MiB, VRAM 675 MiB; scene again: RSS 304 MiB, VRAM 258 MiB; switch 185–208 ms | 2.2–2.3 %, 610–830 wake-ups/s, RSS 203 MiB, VRAM 222 MiB; RSS 165–172 MiB, VRAM 174 MiB; 231 ms (the first video of a process loads FFmpeg, 13–15 ms) |
| GPU released after a pause | 3018340632 RSS 117 MiB (163 shared objects), 3463520581 130 MiB | 30 MiB (35), 38 MiB |
| VRAM in the window | 3463520581 642 MiB, 1960765686 436 MiB | 514 MiB, 245 MiB |

## Windows, part A: GPU, VRAM and render thread against Wallpaper Engine

### Starting point (desktop)

Release build of main 376a97d against Wallpaper Engine 2.8.42 (ultra preset), `tools/bench-vs-we/`, monitor 1
(2560×1440), 30 fps, 3 repetitions of 30 s, 4 November 2026, system CPU 8–10 % (details and raw data:
[`benchmarks/lumenwall-vs-wallpaper-engine.md`](benchmarks/lumenwall-vs-wallpaper-engine.md#starting-point-release-build-of-main)).
Main measured the same as the installed 0.1.0.

| Wallpaper | Engine | CPU (% of a core) | Private | GPU 3D | Dedicated VRAM | Shared GPU memory | Command → first GPU |
|---|---|---|---|---|---|---|---|
| 3463520581 (3840×2160) | Wallpaper Engine | 2.71 | 517 MiB | 3.23 % | 323 MiB | 3 MiB | 462 ms |
| 3463520581 | Lumenwall main | 5.50 | 703 MiB | 6.63 % | 608 MiB | 43 MiB | 808 ms |
| 3018340632 (3440×1440) | Wallpaper Engine | 1.51 | 284 MiB | 2.89 % | 275 MiB | 3 MiB | 331 ms |
| 3018340632 | Lumenwall main | 2.94 | 356 MiB | 5.21 % | 264 MiB | 43 MiB | 719 ms |

Black-box hint about the render size: Wallpaper Engine's GPU time over Lumenwall's is 0.555 on 3018340632
(shown at 2560×1072: 0.553 of the scene's pixels) and 0.487 on 3463520581 (0.444): Wallpaper Engine renders the
whole scene, image chains included, at the size it is displayed at. On NVIDIA/Windows every Vulkan memory block
is also committed private memory, so the private bytes follow the GPU memory below.

### Changes

Measured headless (`render_png` example, release, RTX 4080 SUPER, driver 621.01, Vulkan, output 2560×1440,
BelowNormal priority while the PC was in use). Each line against the line before.

| Change | Effect |
|---|---|
| Render-graph aliasing: a scene builder allocates its render targets when the scene is built; transient targets (composites, effect `fbos`, bloom) whose frame lifetimes (first to last access) do not overlap share one texture. A target read before it is written in a frame (feedback, accumulation) keeps its own; the first draw into a shared texture clears it | Pixel-identical on the 8 scenes. Tracked GPU memory of 3463520581 386 → 268 MiB |
| `_rt_FullFrameBuffer` (and bloom, `_rt_Reflection`) at the displayed size: `render_scale` = displayed pixels per scene pixel on the largest output showing the scene, at most 1. The camera keeps the scene's space | 3463520581: 268 → 233 MiB, GPU 0.83 → 0.63 ms/frame (full clocks, frame only) |
| Image composites and effect targets at the displayed size too, reporting their nominal size to the shaders (`RenderTarget::resolution`): texel offsets keep their length in texture coordinates (blur radii, aberration offsets) and only the sampling density follows the output. `LUMENWALL_FULL_SIZE_CHAINS=1` keeps them at the scene's size (read once in `build_scene`, where an engine setting will plug in) | 3018340632: 0.47 → 0.32 ms/frame, 98 → 76 MiB; 3463520581: 233 → 213 MiB |
| Flexible aliasing: a target rendered below its nominal size may be rendered up to 1.5× larger to share a texture with a target of a close size | 3463520581: render targets 148 → 80 MiB, GPU time unchanged (0.514 / 0.529 ms) |
| Allocator blocks 4–8 MiB (`MemoryHints::Manual`) instead of 8–64 MiB: targets and textures above 8 MiB get memory of their exact size | Reserved − allocated: 3463520581 88 → 27 MiB, 3018340632 68 → 13 MiB; steady dedicated VRAM 355 → 295 and 174 → 119 MiB |
| Two swap-chain images on Windows (frame latency 1): at 30 fps the presented image is released long before the next frame | One output-sized image less: 14 MiB per 2560×1440 output (by construction, not measured headless) |
| Per-frame uploads through a `wgpu::util::StagingBelt` (dirty uniform range and particle streams copied in the frame's encoder) | No staging buffer created per call any more (`Queue::write_buffer` made one per particle stream write) |
| Dependencies and hazard copies from the slots a shader samples only: `genericimage4` names `_rt_FullFrameBuffer` in slots its compiled combination never samples | The 4 full-scene hazard copies of 3463520581 and their 14 MiB scratch target are gone; pixel-identical |
| Frame reordering (`schedule.rs`): image chains first, scene draws together, every conflicting pair of operations kept in order; budget of 8 MiB of transient targets kept alive by deferred draws | 3463520581: render passes 89 → 65, GPU 0.470 → 0.414 ms, `finish` 0.39 → 0.33 ms, reserved memory unchanged (191.9 MiB); pixel-identical with the aliasing slack at 1 |

### Before and after (headless, same session, 3 runs each)

| | 3463520581 main | 3463520581 now | 3018340632 main | 3018340632 now |
|---|---|---|---|---|
| GPU per frame (composition included, clocks of the moment) | 1.14–1.16 ms | 0.65–0.69 ms | 0.70–0.74 ms | 0.35–0.36 ms |
| Render-thread CPU per frame (update, encode, `finish`, submit, composition) | 0.87–0.89 ms | 0.41–0.52 ms | 0.22–0.23 ms | 0.19–0.22 ms |
| of which wgpu's `CommandEncoder::finish` | – | 0.30–0.40 ms | – | 0.13–0.15 ms |
| Render passes and copies per frame | 96 | 65 | 24 | 23 |
| GPU memory tracked by Lumenwall | 386 MiB | 136 MiB | 127 MiB | 76 MiB |
| Allocator reserved / allocated | – | 192 / 175 MiB | – | 112 / 99 MiB |
| Dedicated VRAM of the process (steady) | 515 MiB | 199 MiB | 235 MiB | 119 MiB |
| Shared GPU memory | 28 MiB | 22 MiB | 18 MiB | 18 MiB |
| Private bytes | 603–638 MiB | 296–344 MiB | 335–368 MiB | 206–255 MiB |

Headless runs have no swap chain but a 2560×1440 output texture and its PNG read-back buffer (14 MiB each).

### Projection to the desktop bench against Wallpaper Engine

Desktop = main's desktop value scaled by the headless ratio (GPU time) or shifted by the headless difference
(memory, minus one swap-chain image). To be confirmed by the final bench.

| Metric | 3463520581 main → projected (WE) | 3018340632 main → projected (WE) |
|---|---|---|
| Dedicated VRAM | 608 → ≈ 278 MiB (323) | 264 → ≈ 134 MiB (275) |
| Private bytes (follow VRAM) | 703 → ≈ 390 MiB (517) | 356 → ≈ 240 MiB (284) |
| GPU 3D | 6.63 → ≈ 3.3–3.8 % (3.23) | 5.21 → ≈ 2.5 % (2.89) |
| Shared GPU memory | 43 → ≈ 37–43 MiB (3) | 43 → ≈ 43 MiB (3) |

Still above Wallpaper Engine: GPU time on 3463520581 (borderline: 55 composite render passes of tiny draws,
each with its barrier) and shared GPU memory. The shared memory is a baseline of the Vulkan device here: 18–20
MiB headless on every scene measured (2 to 156 passes), plus about 20 MiB on the desktop (the swap chain and its
presentation); Wallpaper Engine (Direct3D 11) shows 3 MiB. Direct3D 12 through wgpu is worse (below).

### Vulkan or Direct3D 12 (headless, same build, `LUMENWALL_GPU_BACKEND`)

| | 3463520581 Vulkan | 3463520581 DX12 | 3018340632 Vulkan | 3018340632 DX12 |
|---|---|---|---|---|
| GPU per frame | 0.68 ms | 0.91 ms | 0.35 ms | 0.38 ms |
| `finish` + submit | 0.84 ms | 0.78 ms | 0.20 ms | 0.34 ms |
| Scene load | 543 ms | 1178 ms | 548 ms | 684 ms |
| Dedicated / shared GPU memory | 232 / 23 MiB | 265 / 82 MiB | 119 / 18 MiB | 152 / 52 MiB |
| Private bytes | 367 MiB | 485 MiB | 230 MiB | 299 MiB |

Vulkan stays first on Windows, Direct3D 12 as the fallback (part C's switch in lumenwall-engine).

### Render thread: where the CPU goes

Lumenwall's own work per frame is small (scene update 0.01–0.06 ms, graph encode 0.01–0.06 ms). wgpu 30 defers
validation, state tracking, barriers and Vulkan encoding to `CommandEncoder::finish`, which costs ≈ 8.5 µs per
render pass whatever the number of draws (8 scenes: 3 passes 0.045 ms … 24 passes 0.32 ms … 96 passes 0.84 ms);
render bundles change nothing. On Vulkan the scene graph is now recorded directly through wgpu-hal (part D
below).

### Visual comparison (headless, 2560×1440, particles off, frame 44, against main)

| Scene (shown at) | Default (all targets at the displayed size) | `LUMENWALL_FULL_SIZE_CHAINS=1` (scene target only) |
|---|---|---|
| 1712475860 (343×193, 1×) | identical | identical |
| 1960765686 (0.67×) | 44.2 dB | 44.2 dB |
| 2234978675 (0.60×) | 40.1 dB | 45.9 dB |
| 3018340632 (0.74×) | 45.7 dB | 52.7 dB |
| 3044659344 (0.67×, clock) | 52.5 dB | 52.5 dB |
| 3089056751 (0.67×, clock) | 44.0 dB | 44.0 dB |
| 3372348904 (portrait, 0.36×) | 36.5 dB | 36.5 dB |
| 3463520581 (0.67×) | 40.5 dB | 48.4 dB |

PSNR of the frame against main's (99 dB = identical). The differences are resampling: edges of fine detail
(main renders at the scene's size and the composition reduces it) and, for 3372348904, the clamped side bands of
a portrait scene, whose edge column is resampled. With every target at the displayed size, each effect that
displaces texture coordinates (shake, sway, water waves) resamples at the display's resolution, so thin details
are a little softer (darker thin hair strands on 3463520581), as on Wallpaper Engine if it renders at the
displayed size as its GPU time suggests.

### Tools

- `LUMENWALL_PROFILE_PASSES=1` (headless): GPU time of every render pass and copy of a frame (timestamp
  queries), grouped by target kind, the 30 most expensive first.
- Headless debug logs (`RUST_LOG=lumenwall_engine::headless=debug,lumenwall_engine::scene=debug`): CPU split of a
  frame (update, encode, `finish`, submit), the device allocator's blocks, GPU memory of the targets by kind.
- Switches for measurements: `LUMENWALL_SCENE_SIZE_TARGET=1`, `LUMENWALL_FULL_SIZE_CHAINS=1`,
  `LUMENWALL_NO_TARGET_ALIASING=1`, `LUMENWALL_ALIAS_SLACK`, `LUMENWALL_REORDER_BUDGET_MIB` (0 = spec order),
  `LUMENWALL_GPU_BLOCKS=<min>-<max>`, `LUMENWALL_FRAME_LATENCY`, `LUMENWALL_GPU_BACKEND=vulkan|dx12`.
- Part D: `LUMENWALL_DIRECT_VULKAN=0` (every scene through wgpu), `render_png --pace <fps>` (a run held to real
  time), headless debug log `lumenwall_render::direct=debug` (the path each scene took, its render passes, draws,
  pipelines and barriers; the recording time per frame).

## Windows, part B: CPU and RAM of scene content (measured by part B)

Headless harness that builds the real scenes through `build_scene`, Windows 11, Ryzen 9 7950X3D, RTX 4080 SUPER,
release, warm shader and pipeline caches, Vulkan only, medians of 5 interleaved runs against main b4d1db6 (a game
was running on the machine). Headless renders of the 8 installed scenes, both builds back to back: 16/16
byte-identical.

| Change | Before | After |
|---|---|---|
| Scene load: shader units, includes and passes read and assembled once, scene files read once, one canonicalize per folder read | scene build 3463520581 244 ms (237–304), 3018340632 106 ms (104–117); `prepare` run for each of the 156 / 27 passes; parse 7 / 2 ms | 98 ms (94–116), 96 ms (95–105); `prepare` 19 / 22 runs; parse 4 / 1 ms |

| Fact | Measurement |
|---|---|
| Per-frame CPU of scene content (`SceneRuntime::update`), 30 fps | 3018340632 72–81 µs (particle simulation 17 µs, of which `turbulence` on the 2 ash systems most; `write_stream` 55 µs), 3463520581 96–136 µs (16 puppets 20 µs, palettes 1 µs, particles 19 µs, `write_stream` 56 µs, the image loop the rest). The heap allocations per frame (10 / 19) all came from `write_stream`'s `Queue::write_buffer`, removed by part A (staging belt) |
| Process memory, Vulkan on NVIDIA/Windows | every `VkDeviceMemory` is committed private memory (protect 0x404, never resident): +64.1 MiB private for a 64 MiB texture, 545 of 715 MiB private on 3463520581, 200 of 356 on 3018340632 (main). Rust heap after load 3.6 / 2.0 MiB live (peak 12 / 33 MiB while loading, returned); process heap 20 / 16 MiB committed with 5 / 2.6 MiB free, `HeapCompact` recovers nothing |
| Working set | made of load-time pages: `K32EmptyWorkingSet` after 40 frames (8–11 ms, first frame after +2–5 ms) → 19 MiB (3018340632) and 49 MiB (3463520581) 330 frames later, headless (part C) |
| Threads at 30 fps (main, `GetThreadTimes`, game running) | 3018340632: render thread 2.65 % of a core (update 72 µs + encode/submit 644 µs per frame), one NVIDIA driver thread 0.70 %; 3463520581: 6.85 % (update 119 µs + encode/submit 1983 µs), driver thread 2.34 %, 12 other threads ≈ 0. With the content update at 20–60 µs, the process CPU is the render thread's encode and submit plus that driver thread, which follows what is submitted |

## Windows, part C: daemon at rest and wallpaper switch (measured by part C)

Release builds, off the desktop (the daemon with `--no-config --silent --fps 30` in a 480×270 window
below the monitors, private IPC endpoint and `LOCALAPPDATA`, below-normal priority, a game running:
absolute times are pessimistic), Windows 11, RTX 4080 SUPER + AMD iGPU without monitor. Base: main
b4d1db6 (engine in the daemon). Details in `crates/lumenwall-engine/README.md`, "Measurements".
Wallpaper Engine (desktop bench): empty 3.1 MiB private, 16.7 MiB working set; command → first GPU
activity 462 ms (3463520581), 331 ms (3018340632).

| Change | Before | After |
|---|---|---|
| Engine in a process of its own on Windows (the daemon never loads the GPU driver; the process ends on `clear`, is reused between two wallpapers, tied to the daemon by a job object) | empty daemon after a scene and `clear`: 62–81 MiB private, 84–120 MiB working set, 10–14 threads, `nvoglv64` and 4 other NVIDIA modules mapped | 6.3–7.0 MiB private, working set 2.3 MiB, no GPU module |
| Log file queue of 1024 lines instead of tracing-appender's 128 000 (preallocated) | 6.29 MiB private | 2.28 MiB |
| Windows heaps trimmed (`HeapOptimizeResources` + `HeapCompact`) after an unload and when the engine process ends | 3.14 MiB after 7 switches from empty and 6 between wallpapers | 2.40 MiB |
| One GPU backend per instance (Vulkan, Direct3D 12 only as a fallback; `LUMENWALL_GPU_BACKEND`) | adapter + device 294–605 ms (DX12 enumeration) | instance + device 148–177 ms (cold driver, new process) |
| Vulkan loader restricted to the adapter's driver, AMD's implicit layer off | `amdvlk64` mapped, `AMD\VkCache` created | NVIDIA modules only |
| GPU opened alongside the platform, swap chain alongside the scene build | first start, process → renderer ready (3018340632) 702–772 ms | 446–507 ms |

| Situation (with parts A and B merged) | Result |
|---|---|
| Empty daemon after a scene and `clear` | **2.23 MiB private, 1.97 MiB working set**, 0.0 wake-ups/s (2.35 / 2.08 MiB after 13 mixed switches) |
| Command → first frame between two wallpapers (same engine process) | **202–241 ms** (3463520581), **231–269 ms** (3018340632); first GPU activity 45–120 ms |
| Command → first frame from empty | 511–645 ms (3463520581), 504–548 ms (3018340632); first GPU activity 279–344 ms and 287–377 ms |

From empty, the first frame waits for a new process, a cold Vulkan driver and NVIDIA's first swap
chain of a device (≈ 0.2 s): about 0.45 s before any scene work. Only a device kept open would
remove it, and a device keeps the driver in memory (≥ 30 MiB private): the empty daemon and the
switch from empty are traded against each other.

## Windows, part E: presentation and shared GPU memory (measured by part E)

Probe outside the repo (RTX 4080 SUPER, driver 621.01, resizable BAR, 2560×1440, 30 fps), per process, GPU
Process Memory counters = `VK_EXT_memory_budget`. Details in `crates/lumenwall-render/README.md`, "Windows: shared
GPU memory and presentation".

| Step | Shared GPU memory |
|---|---|
| Vulkan device, one queue (bare or wgpu, any queue family/priority/API version) | 12.4–12.9 MiB |
| FIFO swap chain (before) / Mailbox swap chain (now) | +26.8 / +0.03 MiB |
| GPU timer readback (`MAP_READ`: a 4 MiB system-memory block kept for the device's life) / now (`MAP_READ \| MAP_WRITE`) | +4.0 / +0 MiB |
| Command buffers: 65 passes in 1 / in 65 command buffers per frame (bare Vulkan, 300 frames); wgpu-core makes 2 per pass | +0 / +5 MiB (wgpu: +2.5–3 paced, +16 back to back on 3463520581) |
| For comparison: D3D11 device + DXGI swap chain / D3D12 device + queue + swap chain | 0.7 / 26.4 MiB in all |

FIFO also costs ≈ 0.69 ms of 3D time per presented frame at idle clocks (probe: 0.737 vs 0.045 ms/frame) and
190–230 ms to create (Mailbox 16 ms).

| Change | Before | After |
|---|---|---|
| Mailbox presentation on Windows (never tearing; daemon in an off-screen 2560×1440 window, process tree, 30 fps, Idle priority, a game running so GPU clocks were high; interleaved) | 3463520581: GPU 3D 1.83/2.08 %, shared 42.7/43.7 MiB, dedicated 266.7 MiB, private 344–350 MiB; 3018340632: 2.23/2.18 %, 42.2/42.7, 217.1, 284–286 | 1.45/1.46/1.50 %, 15.9 MiB, 248.2 MiB, 279–281 MiB; 1.70/1.67 %, 15.4, 198.6, 221–222 |

Projection to the desktop bench (no game): desktop ms/frame = 1.10 × headless ms + 0.945 ms (fit on main's two
scenes; the constant is presentation, absent headless). Without FIFO: 3463520581 ≈ 2.2–2.6 % (WE 3.23),
3018340632 ≈ 1.3–1.4 % (WE 2.89); Perf A's projection × the measured Mailbox/FIFO ratio (0.70–0.80, game clocks):
2.3–3.0 %. Shared memory floor with Vulkan: 12.4–12.9 MiB (WE 3 MiB, D3D11): below it only with a Direct3D 11
renderer (decision for the person).

## Windows, part D: the scene graph recorded directly in Vulkan

### What changed

- On a Vulkan device a scene records its frame into one Vulkan command buffer of its own
  (`lumenwall-render` `hal` module, through wgpu-hal): pipelines, layouts, bind groups and samplers are created
  through wgpu-hal from the same descriptors as the wgpu path (same SPIR-V, same states), and the frame is planned
  once (image barriers, hazard copies, render passes, draws) and replayed every frame. Between frames every
  render target, depth buffer and texture is in the state wgpu-core believes it is in, so the composition, the
  uploads and the readbacks stay on wgpu. Details and the contract with wgpu-core: `crates/lumenwall-render/src/hal/mod.rs`.
- The wgpu path stays for Direct3D 12, `LUMENWALL_DIRECT_VULKAN=0` and any scene whose wgpu-hal objects cannot be
  created or whose frame cannot be planned (automatic, per scene, also after load: layers added by scripts).
- The engine submits the scene's command buffers and the composition at once (`RenderScene::encode_frame`):
  −0.04 to −0.06 ms per frame against a submission of the scene's own (`encode`).
- `render_png --pace <fps>`: headless runs held to real time (measurements on a PC in use).

### Before and after (headless, interleaved)

Windows 11, RTX 4080 SUPER, release, `render_png --size 2560x1440 --frames 300 --pace 30`, Idle priority while a
game was running, one GPU run at a time. Before = the wgpu path of the same build (`LUMENWALL_DIRECT_VULKAN=0`,
what main records), after = the direct path, runs interleaved. Absolute CPU times are about 3× those of unpaced
runs on a quiet PC (wgpu's `finish` on 3463520581: 1.29–1.31 ms here, 0.42–0.47 ms unpaced the same day).

| | 3463520581 wgpu | 3463520581 direct | 3018340632 wgpu | 3018340632 direct |
|---|---|---|---|---|
| Render passes / draws per frame | 65 / 156 | 65 / 156 | 24 / 27 | 24 / 27 |
| Render thread after the scene update (encode + `finish` + submit) | 1.55–1.57 ms | **0.51–0.60 ms** | 0.72–0.88 ms | **0.37–0.41 ms** |
| of which the Vulkan recording | – | 0.25–0.28 ms | – | 0.13–0.14 ms |
| Shared GPU memory (peak, GPU Process Memory) | 22.7 MiB | **15.2 MiB** | 18.7 MiB | **15.2 MiB** |
| Dedicated GPU memory | 199.3 MiB | 199.3 MiB | 122.9–131.5 MiB | 122.9 MiB |
| Scene load, GPU time per frame | 0.73–0.86 s, 1.05–1.20 ms | 0.62–0.84 s, 0.91–1.20 ms | 0.55–1.00 s, 0.37–0.43 ms | 0.67–1.02 s, 0.37–0.73 ms (noise) |

Per render pass, from the two scenes (41 render passes and 129 draws apart): 17–20 µs on the wgpu path,
3.5–4.5 µs on the direct path in these conditions; scaled by the wgpu path's ratio to part A's 8.5 µs per pass,
≈ 1.5–2.2 µs per render pass on a quiet PC. Fixed cost of the direct frame ≈ 0.1 ms (encoders, the first render
pass begun through wgpu-hal on purpose: it resets the encoder's bind point, ≈ 13 µs). Projected on a quiet PC
from the same ratios: 3463520581 ≈ 0.17–0.22 ms per frame instead of 0.53–0.58 ms, 3018340632 ≈ 0.09–0.11 ms
instead of 0.19–0.21 ms (before presentation; the final desktop bench measures it). The shared GPU memory saved
is wgpu-core's two command buffers per render pass (part E's probe: +5 MiB for 65 command buffers per frame).

Visual: headless renders on both paths (2560×1440, particles off, frames 0 and 44) are byte-identical PNGs on the
8 scenes of part A and on the installed default projects, 3D scenes with depth buffers and meshes included
(arsenal, audiophile, fantasticcar, techno, demon_core, dna_fragment, neon_sunset, ricepod, retro). No Vulkan
validation layer is installed on the measuring PC: not run. Linux records through the same code (Vulkan); not
measured there.

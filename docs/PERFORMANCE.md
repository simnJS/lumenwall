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
environment) saves another 25–40 ms on instance creation on this machine (9 ICDs installed).

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
| GPU time | Headless at full clocks: 0.35–0.80 ms/frame for the heaviest scenes (3840×2160). In a 480×270 window the scene still renders at its own size: 5.8–7.3 ms/frame at the low clocks the driver keeps |
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

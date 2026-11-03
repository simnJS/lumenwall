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

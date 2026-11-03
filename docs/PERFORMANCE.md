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
| Static scene (no animation, no effects over time) | Render once, then only on input or property change |
| Animated scene at the target fps | Frame time well below the vsync interval; no CPU frame spikes |
| Steady-state frame | Zero heap allocations on the render thread |
| Memory | Assets resident once; no CPU copy kept after GPU upload unless needed |

## Rules

**Do no work that can be skipped.**
- Stop the render loop when the output is hidden, covered by a fullscreen window, or the session is locked. Do not poll; use OS notifications where they exist.
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

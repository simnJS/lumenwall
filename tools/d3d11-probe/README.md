# d3d11-probe

Prototype that decides with numbers whether Lumenwall should get a Direct3D 11 renderer on
Windows. It runs the **same scene workload** on Direct3D 11 and on Vulkan (wgpu, set up the way
`lumenwall-render` does it) and measures every metric the benchmark against Wallpaper Engine uses.

Why: Lumenwall must use strictly less than Wallpaper Engine on every metric. The one still above is
shared GPU memory per process (15.4–15.9 MiB against 3 MiB). The Vulkan device alone costs
12.4–12.9 MiB of it on NVIDIA, a Direct3D 11 device with its swap chain 0.7 MiB
(`crates/lumenwall-render/README.md`, "Windows: shared GPU memory and presentation").

The crate is **not** a member of the root workspace (it has its own empty `[workspace]`): the root
`Cargo.toml` and `Cargo.lock` are untouched and the workspace check does not build it. Its
`Cargo.lock` started as a copy of the root's, so `wgpu` 30.0.1, `naga` 30.0.1 and `windows` 0.62.2
are the versions of the real build. Wallpaper Engine content is read from the local install
(read-only) and is never copied anywhere.

## Commands

```powershell
$env:CARGO_TARGET_DIR = 'target/agent-<name>'
cargo build --release --manifest-path tools/d3d11-probe/Cargo.toml
cargo clippy --manifest-path tools/d3d11-probe/Cargo.toml -- -D warnings

d3d11-probe plan    [--wallpaper ID]...   # CPU only: what one frame draws
d3d11-probe shaders [--wallpaper ID]...   # CPU only: translation + HLSL + D3DCompile, cold vs cached
d3d11-probe run --backend d3d11|vulkan --wallpaper ID [--seconds 20] [--warmup 5] [--fps 30] [--clear-cache]
pwsh tools\d3d11-probe\run-matrix.ps1 -Exe <exe> -Name <you>   # 3 interleaved runs of each backend
```

`plan` and `shaders` never create a GPU device. `run` does, and `run-matrix.ps1` takes
`%LOCALAPPDATA%\lumenwall-dev\gpu.lock` for its duration (and refuses to start if it exists).
Defaults: `--workshop` / `--assets` from `LUMENWALL_WE_WORKSHOP` / `LUMENWALL_WE_ASSETS`, else the
Steam folders under `C:\Program Files (x86)\Steam`; the bytecode cache is in
`%TEMP%\d3d11-probe-cache\dxbc` (`--cache`).

## Design

```
wallpaper folder ──► lumenwall-assets (Vfs) ──► lumenwall-scene (project.build, image passes)
                                              │
        real .tex ◄── lumenwall-formats ◄─────┤  plan.rs: Workload
        shaders   ◄── lumenwall-shader ◄──────┘  textures, targets, programs (naga modules), draws
                                                    │
                    ┌───────────────────────────────┴──────────────────────────────┐
              d3d11.rs                                                         vulkan.rs
   naga ► HLSL SM 5.0 ► D3DCompile ► bytecode cache                    naga IR ► wgpu (Vulkan)
   flip-model swap chain, Present(0)                                   Mailbox swap chain, latency 1
```

**Workload (`plan.rs`).** Derived on the CPU from the real wallpaper the way `lumenwall-engine`'s
`prepare_image` does it, without the renderer: image sizes and the display scale, composites A/B,
effect `fbos`, the six-level texture slot resolution, targets, `Previous` / `OwnComposite`
bindings, the A/B ping-pong, the last pass of an image on the scene target. The draws that fill
intermediate targets run first, then every draw to the scene target (`lumenwall-render`'s "chains
first, scene draws together"), and consecutive draws on one target form one render pass. The
scene target is copied to a 2560×1440 swap chain (`commands/copy`). Per frame, on both backends:
the frame block and one constant buffer per draw are rewritten (D3D11: `Map` `WRITE_DISCARD`;
wgpu: `Queue::write_buffer`), targets are cleared on their first pass, all draws use the same
alpha blend, one linear clamp sampler, a triangle strip quad that covers the image's part of the
target. 30 fps on a high-resolution waitable timer (no spinning), Idle priority, a 2560×1440
window below the monitors (`WS_EX_NOACTIVATE`, never visible).

**Not reproduced** (same on both backends, so the comparison holds, but the absolute numbers are
not the engine's): puppets, particles, text, 3D models, bloom, scripts and audio; render-target
memory aliasing (the engine shares memory between targets whose frame lifetimes do not overlap,
the probe gives each its own, so dedicated memory is higher than the engine's 248 MiB); render
bundles (wgpu draws are recorded in the pass); the clip-space correction of `lumenwall-render`;
pixel exactness.

**Shaders.** `lumenwall-shader` translates each distinct pass program (shader + combos) to a naga
module. For D3D11 the module goes to `naga::back::hlsl` with shader model 5.0 and the binding
contract mapped to registers (`group 0 binding 0|1` → `b0|b1`, `group 1 binding 2N` → `tN`,
`2N+1` → `sN`), then to `D3DCompile` (`d3dcompiler_47.dll`, `vs_5_0` / `ps_5_0`, optimization
level 3), then to the bytecode cache on disk (key: the program's blake3 key). Vulkan hands the
same naga module to wgpu.

naga 30's HLSL writer declares **every sampler through a sampler heap** (Direct3D 12's model:
`nagaSamplerHeap[2048] : register(s0, space0)` and an index `StructuredBuffer`), which shader model
5.0 cannot compile. `shaders.rs::plain_samplers` rewrites those declarations into plain
`SamplerState name : register(sN);` (four line patterns, covered by the 33 programs below). A
Direct3D 11 renderer in the product would need the same rewrite, or a naga change.

## Shader feasibility (CPU only, measured)

`d3d11-probe shaders`, release build, Idle priority, Ryzen 9 7950X3D, three runs (the second and
third give the same figures to ±2 ms). "Programs" are the distinct (shader, combos) the passes of
the scene use, `commands/copy` included.

| Scene | Programs | Translated (`lumenwall-shader` → naga) | HLSL SM 5.0 → `D3DCompile` | Translation | naga → HLSL | `D3DCompile`, cold | Bytecode | Cached (read from disk) |
|---|---|---|---|---|---|---|---|---|
| 3463520581 | 12 | 12 / 12 | 12 / 12 | 15 ms | 2 ms | 78–80 ms | 39.6 KiB | 0.7–3.1 ms |
| 3018340632 | 21 | 21 / 21 | 21 / 21 | 19–20 ms | 4 ms | 167–172 ms | 71.1 KiB | 1.1–1.2 ms |

No shader of the two scenes fails to translate or to compile to shader model 5.0. Cold compile is
about 7 ms per program; with the cache the whole scene costs one to three milliseconds, then the
driver still turns the bytecode into its own code at `CreateVertexShader` / `CreatePixelShader`
(measured in the GPU runs below as `shaders_ms`). 33 programs is a small sample of the library:
only the shaders these two scenes use.

Workload sizes (`plan`): 3463520581 has 85 textures (63.4 MiB, 69 images in the scene plus
animation frames), 140 targets (232 MiB), 152 draws in 86 render passes (the engine: 156 draws in
65 render passes after Perf A). 3018340632: 11 textures (37.6 MiB), 15 targets (128 MiB), 23
draws in 22 render passes.

## GPU measurements

Scene 3463520581, the workload above, RTX 4080 SUPER (driver 621.01), 2560×1440 window below the
monitors, 30 fps, 5 s warm-up then a 20 s window, Idle priority, `run-matrix.ps1` (runs
interleaved, the backend that goes first alternates). Memory counters are the process's
`GPU Process Memory` instances at the end of the window; CPU % is the process's CPU time over the
window; GPU 3D % is the process's `GPU Engine ... engtype_3D` utilization over the window.
**A game was running on the PC during the runs** (the GPU is at game clocks, other processes
compete): CPU % and GPU 3D % are noisy (see the spread) and only memory and creation times are
reliable. Wallpaper Engine for this scene (docs/benchmarks): GPU 3D 3.23 %, dedicated 323 MiB,
shared 3 MiB.

| Backend | Run | Shared | Dedicated | Private | Working set | CPU | GPU 3D | Device + swap chain | Textures | Shader objects | Pipelines / states | Shared after device |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| D3D11 | 1 | 3.21 MiB | 284.5 MiB | 429.0 MiB | 68.2 MiB | 0.86 % | 2.83 % | 188.5 ms | 26.0 ms | 0.7 ms | 3.2 ms | 0.72 MiB |
| D3D11 | 2 | 3.16 MiB | 284.5 MiB | 438.1 MiB | 64.2 MiB | 3.28 % | 1.37 % | 211.3 ms | 29.4 ms | 0.8 ms | 4.0 ms | 0.72 MiB |
| D3D11 | 3 | 3.17 MiB | 284.5 MiB | 431.7 MiB | 63.1 MiB | 4.53 % | 1.16 % | 204.9 ms | 24.2 ms | 0.7 ms | 3.6 ms | 0.72 MiB |
| Vulkan | 1 | 17.18 MiB | 436.7 MiB | 501.5 MiB | 154.0 MiB | 5.63 % | 3.15 % | 477.0 ms | 41.7 ms | 8.9 ms | 103.6 ms | 13.65 MiB |
| Vulkan | 2 | 17.18 MiB | 436.7 MiB | 481.5 MiB | 119.7 MiB | 8.44 % | 1.80 % | 241.2 ms | 46.8 ms | 2.5 ms | 4.8 ms | 13.65 MiB |
| Vulkan | 3 | 17.18 MiB | 436.7 MiB | 480.1 MiB | 119.3 MiB | 7.65 % | 1.90 % | 277.8 ms | 57.9 ms | 2.5 ms | 7.2 ms | 13.65 MiB |

Shader objects: the bytecode read from the on-disk cache and `CreateVertexShader` /
`CreatePixelShader` (D3D11), naga modules handed to wgpu (Vulkan). Nothing was compiled during the
GPU runs (the cache was filled by `d3d11-probe shaders`: cold `D3DCompile` is in the table above).
Vulkan's 103.6 ms in run 1 is the first pipeline creation of the session (driver shader cache
cold). `late_frames` (frames more than 5 ms late) was 0 for every run except Vulkan run 3 (1).

**Answer.**
- **Shared GPU memory: D3D11 does not quite reach "under 3 MiB", but it nearly does.** 3.16–3.21
  MiB against 17.18 MiB for the same workload on Vulkan (−14 MiB, −81 %), and against Wallpaper
  Engine's 3 MiB (a rounded figure). The bare device + flip swap chain costs 0.72 MiB (Nora's
  0.7); the 2.4–2.5 MiB that come on top appear while the workload is loaded and drawn. Cause not
  pinned down (candidates: the 153 dynamic constant buffers and their `WRITE_DISCARD` renaming,
  the 156 vertex buffers, command-stream memory of the driver for 86 render passes); it is the
  next thing to bisect before deciding, since the person's requirement is strictly below WE.
- **Dedicated:** 284.5 against 436.7 MiB (−35 %). Both are above/below the engine's 248 MiB
  because the probe does not alias targets; WE shows 323 MiB.
- **Private bytes:** 433 against 488 MiB (−11 %). **Working set:** 65 against 131 MiB (−50 %).
- **CPU:** 0.9–4.5 % against 5.6–8.4 % per run (noisy because of the game; D3D11 lower in each
  pair of adjacent runs).
- **GPU 3D:** D3D11 1.2–2.8 % against Vulkan 1.8–3.2 %: lower in the three pairs, inside the noise of
  the game load. Not a conclusion by itself.
- **Device + swap chain creation:** ~200 ms against 240–480 ms. **Texture upload (63 MiB):** 24–29
  ms against 42–58 ms. **Pipelines / state objects:** 3–4 ms against 5–104 ms.
- Shader feasibility is not a blocker: 33 of 33 programs of the two scenes compile to SM 5.0
  (with the sampler rewrite above).

Scene 3018340632 (21 programs): _pending, the GPU lock was held by another agent when the second
scene was due._

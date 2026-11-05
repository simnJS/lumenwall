# lumenwall-d3d11

Direct3D 11 backend of Lumenwall's renderer on Windows. Vulkan stays the renderer on Linux and the
fallback on Windows. This crate holds the **foundation**: the building blocks the scene renderer on
Direct3D 11 is made of. This README is also the **design** of the whole backend: the follow-up tasks
are split from it ("Split proposal", at the end).

Why: Lumenwall must use strictly less than Wallpaper Engine on every metric. The one metric still
above was shared GPU memory per process: 15.4–15.9 MiB on Vulkan against Wallpaper Engine's 3 MiB,
12.4–12.9 MiB of it being the Vulkan device itself on NVIDIA. `tools/d3d11-probe` ran the workload of
scene 3463520581 on both APIs. Direct3D 11 measured:

- shared memory 1.2–2.0 MiB with `IDXGIDevice3::Trim`;
- dedicated memory −32 to −35 %, working set −35 to −50 %;
- lower CPU, faster device creation and uploads;
- the same GPU 3D time.

33 of 33 shader programs compiled to shader model 5.0. The person decided to build it.

## Sources (clean room)

- `tools/d3d11-probe` (README, `src/d3d11.rs`, `src/shaders.rs`): the measurements and the
  recommended design this crate ports.
- `crates/lumenwall-render` (README, public API) and the engine's use of it (the usage map below):
  what the backend has to reproduce.
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `AGENTS.md`, `CLEANROOM.md`.
- Public Microsoft documentation of Direct3D 11, Direct3D 11.1 and DXGI:
  - `D3D11CreateDevice`, `ID3D11DeviceContext1::*SetConstantBuffers1` (256-byte windows);
  - `IDXGIDevice3::Trim` (call `ClearState` first), `IDXGIAdapter3::QueryVideoMemoryInfo`;
  - flip-model swap chains, `D3DCompile`, query types, `ResolveSubresource`.
- naga 30.0.1 (HLSL back end, API and output) and windows 0.62.2 (bindings).

No upstream code, fork or third-party Wallpaper Engine reimplementation was consulted.

## What this crate contains (foundation)

Everything below is tested. Pure parts build and are tested on every platform. Everything that
calls Direct3D is `cfg(windows)`: on Linux the crate builds without any Windows API.

| Module | Content |
|---|---|
| `hlsl` (all platforms) | naga module → HLSL shader model 5.0 under `lumenwall-render`'s binding contract: `group(0) binding(0/1)` → `b0/b1`, `group(1) binding(2N/2N+1)` → `tN/sN`, N < 8. Bindings outside it are an error. The sampler-heap rewrite: naga 30 declares every sampler through a Direct3D 12 heap, rewritten to plain `SamplerState` / `SamplerComparisonState name : register(sN)`, and a leftover heap is an error. Reflection: vertex inputs (location, name, kind, components) and resources (block sizes, texture/volume/sampler slot masks). Vertex inputs use semantic `LOC<location>`. `GENERATOR_VERSION` is part of every cache key. |
| `cache` (all platforms) | Bytecode on disk: `<key>.dxbc` with magic, format version, blake3 checksum, atomic writes (temporary file + rename). A corrupt, truncated or foreign file is a miss. Two kinds of key: `CacheKey::for_hlsl` (from the HLSL itself: robust to any upstream change, costs naga → HLSL at every load, 2–4 ms per scene) and `CacheKey::for_identity` (from the caller's own identity: lumenwall-shader's program key plus transform versions, which skips naga on a hit). The folder is the engine's `cache_dir()` + `dxbc` (`%LOCALAPPDATA%\Lumenwall\cache\dxbc`), passed by the caller. |
| `layout` (all platforms) | Constant arena windows (256-byte aligned, ≤ 64 KiB each, `first_constant` / `num_constants`); linear arenas for static vertex/index data; upload rules (level sizes `max(1, base >> k)`, exact byte counts, BC level 0 a multiple of 4, feature level 11_0 size limits, volumes). |
| `state` (all platforms) | Keys of samplers (address repeat/clamp/border, nearest, mipmaps, anisotropy: the wgpu path's rule, 8 for linear 2D), blending (spec 04 §14 table: `OPAQUE`, `translucent`, `additive`, write masks), rasteriser (cull, front face as wgpu's `FrontFace`, depth clip = the opposite of wgpu's `unclipped_depth`, multisample, scissor) and depth ("less or equal" or "always", write). |
| `error` (all platforms) | `Error::DeviceLost { reason }` (removed / hung / reset / driver internal, with `GetDeviceRemovedReason`) and `OutOfMemory` are values the caller handles. Nothing in the crate panics on a GPU failure. |
| `Device` | Adapter choices: default (first hardware adapter, the software one as last resort), high performance, minimum power, LUID, the monitor of the first output, WARP. Feature level 11_1 else 11_0. Requires Direct3D 11.1 (`ID3D11Device1`, constant buffer offsetting). Optional debug layer, optional single-threaded flag. `SetMaximumFrameLatency(1)`. `check_lost`, `trim` (`ClearState` + `Flush` + `Trim`), `wait_idle` (event query; load time only), `memory()` (`QueryVideoMemoryInfo`, local and non-local), MSAA support. |
| `SwapChain` | One per window: `FLIP_DISCARD`, two `B8G8R8A8_UNORM` buffers, `DXGI_ALPHA_MODE_IGNORE`, no tearing flag, `MakeWindowAssociation(NO_ALT_ENTER \| NO_WINDOW_CHANGES)`. `Present(0, 0)` reports `Presented` or `Occluded` and turns errors into `DeviceLost`. `resize` (`ClearState`, then `ResizeBuffers`). |
| `Texture` | Every `lumenwall-formats` `GpuFormat`: R8, RG8, RGBA8, BC1/2/3/7, R16F, RG16F, RGBA16F, RGB10A2. 2D or volume, all mips, immutable with the pixels given at creation (no CPU copy kept). Writable 2D textures (`write_level0` through `UpdateSubresource`: CPU video frames, cover art). |
| `RenderTarget` | RGBA8, RGBA16F (HDR), R8, RG8. Optional MSAA (2/4/8, checked against the device) resolved into the sampled texture. Optional `D32_FLOAT` depth (cleared to 1). `bind` (target + viewport), `clear`, `resolve`. |
| `ConstantArena`, `StaticBuffer`, `DynamicBuffer` | `ConstantArena`: one dynamic constant buffer for every pass block and the frame block. CPU shadow with change detection; at most one `Map(WRITE_DISCARD)` per frame, none when nothing changed; windows bound with `VSSetConstantBuffers1` / `PSSetConstantBuffers1`. `StaticBuffer`: immutable shared vertex/index data bound with offsets. `DynamicBuffer`: per-frame streams, `WRITE_DISCARD`. |
| `StateCache` | One sampler, blend, rasteriser and depth object per key, created at load time. |
| `compile_program`, `create_input_layout` | `D3DCompile` (`vs_5_0` / `ps_5_0`, optimisation level 3) through the cache; shader objects; input layouts. `complete_elements` feeds every input the pass leaves unfed from a stride-0 buffer (`DEFAULT_ATTRIBUTES`): an unfed input reads `(0, 0, 0, 1)`, as on the wgpu path. |
| `GpuTimer` | Timestamp queries over three slots, read with `DONOTFLUSH`. Never waits: a slot still in flight skips the measurement. |
| `read_subresource`, `read_texture_rgba8` | Staging copy, `Map(READ)`, tightly packed rows (BCn, volumes, BGRA→RGBA). The staging texture is released before returning. |
| `OffscreenWindow` | A window below the monitors, shown but never activated (tests, measurements). |

`examples/clear_present.rs` clears, presents and reads back on an off-screen window. It reads the
process's GPU memory after each step: the `GPU Process Memory` counters (what Task Manager shows) and
`QueryVideoMemoryInfo`. On the hardware adapter it takes `%LOCALAPPDATA%\lumenwall-dev\gpu.lock`, runs
at Idle priority, paced at 30 fps, for at most 60 s.

## Measurements (foundation)

`clear_present --seconds 20 --size 2560x1440`, release, RTX 4080 SUPER (driver 621.01), Windows 11, a
2560×1440 window below the monitors, 30 fps, Idle priority, 3 runs one after the other. MiB:

| Step | Shared (counters) | Dedicated (counters) | Non-local (QVMI) | Local (QVMI) |
|---|---|---|---|---|
| Device (feature level 11_1) | 0.72 | 10.79 | 0.72 | 8.31 |
| Swap chain created (buffers allocated at first use) | 0.72 | 10.79 | 0.72 | 8.31 |
| Frame 1, with a full 2560×1440 readback (14 MiB staging) | 14.93 | 25.84 | 14.93 | 23.32 |
| 30 frames (staging released) | 0.80 | 40.80 | 0.80 | 38.31 |
| Right after `Trim` | 0.66–0.78 | 40.80 | 0.66 | 38.31 |
| 30 frames after `Trim` | **0.80** | 40.80 | 0.80 | 38.31 |
| End of the 20 s window (600 frames) | **0.80** | 40.80 | 0.80 | 38.31 |

- **Shared GPU memory: 0.80 MiB after `Trim`, at steady state, in all three runs** (target ≤ 1 MiB;
  Wallpaper Engine 3 MiB; the Vulkan device alone 12.4–12.9 MiB). Readbacks, which only headless
  renders and screenshots do, take system memory while they last. Releasing their staging texture
  gives the memory back.
- The 30 extra MiB of dedicated memory after the first present are the two swap chain buffers.
- Device creation 135–139 ms; swap chain 3.8–5.5 ms (Vulkan FIFO: 190–230 ms, Mailbox 16 ms).
- 600 of 600 frames presented, none occluded; readbacks exact (0 of 3 686 400 pixels differ).
- GPU time of a 2560×1440 clear: 0.17–0.44 ms per frame at idle clocks.

The GPU tests (`tests/gpu.rs`, below) also pass on this adapter, run with
`LUMENWALL_D3D11_TEST_ADAPTER=hardware` under the lock (11 of 11).

## How the renderer is used today (usage map)

### lumenwall-render's public API

| Type / module | What it does on wgpu today | Direct3D 11 |
|---|---|---|
| `GpuInstance`, `GpuContext`, `ContextOptions`, `GpuCaps` | Instance, adapter ranking, device and queue. Pipeline cache, sampler cache, memory counter, caps (BC, clamp-to-border, unclipped depth, timestamps), video decode queues. | `Device` (+ `StateCache`). BC, border clamp and depth-clip control are always available at feature level 11_0. No pipeline cache: the driver caches shader objects, and the bytecode cache replaces translation. |
| `OutputSurface`, `SurfaceFrame` | One swap chain per output. Mailbox on Windows, two images, opaque, never sRGB. Occlusion or timeout skips the frame. | `SwapChain` (flip model, `Present(0, 0)`: same frames at the same vertical blanks as Mailbox, never tearing). |
| `TextureUpload`, `VolumeUpload`, `TexelFormat`, `TexFlags`, `SamplerKey` | Render-ready uploads and sampler identity; GPU upload in `texture.rs`. | Backend-neutral already (data). `Texture::new` uploads them; `SamplerKey` maps to `state::SamplerKey`. |
| `RenderTargetPool`, `RtId`, `plan_aliasing`, `scaled_size`, `divided_size`, composites, scopes | Names, scopes and sizes. Aliasing plan for transient targets (same size, or flexible up to 1.5×), deferred allocation, resizes. Plus the wgpu textures. | Planning is neutral and reused as is. `RenderTarget` holds the textures; aliased targets share one `RenderTarget`. |
| `ShaderDesc`, `shader::{reflect, apply_clip_space_fix, validate, compile}`, `ShaderInterface` | naga module + entries; reflection; the clip-space fix `(x, −y, (z+w)/2, w)`; validation; module creation. | The naga steps are neutral and shared: Direct3D 11's clip space is wgpu's. `hlsl::translate` + `compile_program` replace module creation. |
| `dynbuf` (`Shadow`, `UniformSlot`, `FrameInputs`, `frame_globals`, `DynamicBuffer`) | One buffer: frame globals, every pass block (dynamic offsets) and the image vertices. CPU shadow; one write per frame; patch lists. | Shadow, slots, patch lists and layouts are neutral. `ConstantArena` takes the uniform blocks (256-byte windows). The vertex region becomes one `DynamicBuffer` (rewritten when dirty), indices and skin records a `StaticBuffer`. |
| `graph` (`SceneBuilder`, `RenderScene`, `PassDesc`, `Op`, `Geometry`, `SlotBinding`, `TextureRef`, `MeshLayout`, `StreamLayout`, `DepthState`, `Extension`, `FrameCommands`) | Load-time builder and compiled frame:<br>• pipelines deduplicated, bind groups per slot-0 variant, hazard copies, merged render passes, render bundles;<br>• reordering (`schedule`), `extend`, `set_ops`, `set_variant`, `set_enabled`, `needs_redraw`;<br>• per-frame staging belt, `profile_frame`, the direct Vulkan path (`hal`). | Descriptors and CPU state are the seam (below). The Direct3D 11 scene renderer turns them into draws on the immediate context:<br>• states from keys, arena windows, SRV/sampler tables per variant;<br>• hazard copies with `CopyResource`;<br>• clears, depth, resolves. |
| `image` (`add_image*`, `ImageHandle::update`, `ImageSpec`, `ImagePass`, `PuppetGeometry`) | Image chain plan → passes, static uniforms, per-frame matrices and vertices. | Neutral: builds `PassDesc`s and writes the shadow and vertices. |
| `chain`, `math`, `compose` (`uv_range`, span), `blend` (`BlendMode`, `WriteMask`, `CullMode`) | Spec formulas and plans (pure); wgpu conversions in `blend`. | Neutral. Conversions to `BlendKey` / `RasterKey` in the backend. |
| `bloom` (`add_bloom`, `BloomShaders`) | Five generic passes; WGSL stand-ins for tests. | Neutral (passes); the WGSL stand-ins go through naga → HLSL. |
| `Compositor`, `OutputComposer`, `ComposeSettings` | One fullscreen triangle per output: UV range, clamp/border/repeat, alpha 1. | A composition program and one draw per output into the swap chain view. |
| `HeadlessOutput`, `read_texture_rgba8`, `write_png` | Off-screen output texture and readback. | A `RenderTarget` + `read_texture_rgba8`; `write_png` is neutral. |
| `stats` (`GpuTimer`, `FrameStats`, `allocator_bytes`) | Timestamps over 3 slots, counters, allocator report. | `GpuTimer`, `Device::memory`, tracked bytes. |
| `yuv` (`YuvConverter`, `YuvPlanes`, `source_from_views`, `conversion_matrix`) | Video frames: planes uploaded or zero-copy views, one conversion draw into RGBA8. | Matrices and layouts are neutral. The conversion shader goes through naga → HLSL; the planes are SRVs of the decoder's NV12/P010 textures (R8 / R8G8 or R16 / R16G16 views). |
| `video_device` (`VideoQueues`) | Vulkan Video decode queues on wgpu's device (wgpu-hal + ash). | Not needed: D3D11VA decodes on the same `ID3D11Device` (`ID3D11VideoDevice`). |
| `hal` (direct Vulkan recording) | One Vulkan command buffer per scene frame, planned once (`plan_barriers`). | Not needed: the immediate context has no wgpu-core layer. The frame plan (segments, draws) is the model the Direct3D 11 renderer follows. |
| `loader` | `VK_LOADER_DRIVERS_SELECT` glob of the adapter. | Not needed (DXGI enumeration loads no driver manifests). |
| `quality` (task "Text styling and quality settings", Jude, on its branch) | `QualitySettings`:<br>• `Msaa` off/x2/x4/x8 on the scene target, with resolve;<br>• `TextureResolution` (top mips dropped at upload);<br>• `HdrMode` (pool format `Rgba16Float`);<br>• `native_resolution`.<br>MSAA and HDR send the scene to the wgpu path. | Neutral settings:<br>• MSAA = `RenderTarget { samples }` + `resolve`, levels checked with `Device::msaa_quality_levels`;<br>• HDR = `TargetFormat::Rgba16Float`;<br>• texture resolution and native resolution stay neutral (upload and sizing). |

### Direct wgpu use outside lumenwall-render

Only `lumenwall-engine` and `lumenwall-video` touch wgpu. `lumenwall-web` produces D3D11 NT handles
and DMA-BUFs without wgpu. `lumenwall-ui` uses its own iced device in another process: out of scope.
Line numbers are those of main at 750f10b.

| Place | Direct GPU use | What must move behind a neutral type |
|---|---|---|
| `engine.rs`:<br>• `open_gpu` / `adopt_gpu` 694–795;<br>• backend order 2463–2479;<br>• surfaces 2492–2511;<br>• frame loop `render_output` 1805–1942, 2197–2233, 2378–2420 | `wgpu::Surface`, `wgpu::Backends`, `is_surface_supported`, `create_surface_unsafe`, `create_command_encoder`, `queue().submit`, `wgpu::Color`. Plus `GpuInstance`/`GpuContext`, `OutputSurface`, `Compositor`, `RenderScene::encode_frame`, `GpuTimer`, screenshots (`read_texture_rgba8`). | The device (`GpuContext` → a backend enum), outputs (surface / swap chain), the frame (begin, scenes, composition, submit, present), screenshots. |
| `engine/release.rs`:<br>• `release_gpu` / `restore` 106–368;<br>• `recreate_for_video` 127–158 | Drops swap chains, then renderers, then the device; keeps the Vulkan instance; recreates the device with decode queues. | Release and restore become backend operations. Direct3D 11 has no instance to keep, and no device recreation is needed for video. |
| `vulkan_driver.rs` 204–297 | Vulkan loader restriction from the adapter. | Vulkan only: skipped on the Direct3D 11 path. |
| `web.rs`:<br>• `FrameCopier` 668–867;<br>• imports 1104–1181;<br>• caches 912–989;<br>• shared-memory upload 992–1061 | Its own wgpu pipeline (copy draw into an RT), a blocking `device.poll`. Import of the CEF frame through wgpu-hal: Vulkan external memory from the D3D11 NT handle, DMA-BUF on Linux. | "External texture → target" import: Direct3D 11 opens the NT handle with `ID3D11Device1::OpenSharedResource1` (no Vulkan interop at all) and copies it into the target with a draw. Plus a "CPU pixels → target" upload. |
| `video.rs` 43–245 | Encoder, submit, `VideoPlayer::open(ctx)`, headless video renders. | Through the neutral frame and the video import below. |
| `headless.rs` 128–371 | `GpuContext::new_headless`, encoder, submit, `profile_frame`, `generate_allocator_report`. | Headless output and frame on either backend (`render_png --backend`). |
| `scene/media.rs` 100–196 | `max_texture_dimension_2d`, `copy_texture_to_texture`, `write_texture` (cover art into `$mediaThumbnail`). | "CPU pixels → scene target" and the size limit as neutral calls. |
| `model3d.rs` 216, 1010 | `wgpu::VertexFormat` in `MeshAttribute`, `wgpu::FrontFace::Ccw` in `PassDesc`. | Plain data. The descriptors keep wgpu's enums as data or get neutral twins: either way no device is involved. |
| `lumenwall-video` `player.rs` 55–510; `ffmpeg/vulkan.rs` 128–385 | Zero copy: FFmpeg's Vulkan Video decoder on wgpu's `VkDevice` (ash, wgpu-hal), timeline semaphores, NV12 views. | On Direct3D 11: FFmpeg D3D11VA on the renderer's own `ID3D11Device`, decoded NV12/P010 array slices sampled directly. |
| `lumenwall-video` `ffmpeg/dxgi.rs` 92+, `hw.rs` 119–131 | Enumerates DXGI adapters to give D3D11VA the wgpu adapter. Today frames come back to the CPU (`av_hwframe_transfer_data`). | Replaced by handing FFmpeg the renderer's device: zero copy. |
| `select.rs` 31–38, examples, tests | `AdapterHint::from_wgpu`, encoders in tests. | Adapter hint by LUID (`Device::info().luid`). |

Errors today: `on_uncaptured_error` only logs. There is no device-lost handling anywhere. Surface
errors are logged per output, and a web copy error restarts the host in shared-memory mode.

## The seam

**Decision: the engine keeps building scenes with the same descriptors.** `SceneBuilder`, `PassDesc`,
`Op`, `Geometry`, `ShaderDesc`, `TextureUpload`, `RenderTargetPool`, `add_image`, `add_bloom` and
`QualitySettings` stay where they are. They become backend-neutral; two backends compile them.

1. **Neutral front in `lumenwall-render`.**
   - `SceneBuilder` keeps what is backend-neutral today: passes and ops, the target plan (names,
     scopes, sizes, aliasing, transient marks), the shadow uniform layout and patch lists,
     vertices/indices/skins, stream and mesh layouts, quality settings.
   - It talks to the GPU only through an object-safe trait for the things that must happen at once:
     upload a texture (the CPU copy is dropped right after), add a prepared shader (clip fix,
     validation and reflection done in the front), add a mesh.
   - `build()` hands the backend a `SceneDesc` and wraps what it returns.
   - `RenderScene` keeps the CPU state: shadow, vertex region, variants, enabled flags, stream data,
     dirty flags, `needs_redraw`. Per frame it calls the backend once with a borrowed view of that
     state: one virtual call per scene per frame, no allocation.
   - The wgpu implementation (bundles, the direct Vulkan path, the staging belt) is the half of
     `graph.rs` that is wgpu-specific today, and stays in `lumenwall-render`.
2. **Two halves joined by a bridge in the engine.**
   - `lumenwall-render` gets the neutral front and its backend trait (task 2), with the wgpu backend
     inside.
   - `lumenwall-d3d11` stays independent of `lumenwall-render`. It takes a plain-data
     **`plan::ScenePlan`** (targets, passes, ops, layouts: what a scene compiles to, with no
     wgpu type) plus incremental uploads (textures, programs, meshes), and runs the frame from a
     borrowed `FrameInput` (shadow bytes, dirty vertex ranges, variants, enabled flags, stream data).
   - The bridge in `lumenwall-engine` (`src/gpu/`, task 5) implements `lumenwall-render`'s backend
     trait by mapping its `SceneDesc` to a `ScenePlan`.

   Why this shape:
   - No dependency cycle, no new crate, no root `Cargo.toml` change.
   - The executor can be written and tested now, in parallel with the seam.
   - On the Direct3D 11 path no wgpu instance or device is ever created: one would cost the 12.4 MiB
     this backend exists to save. The binary still links wgpu for the Vulkan fallback.
3. **What moves behind neutral types** (in the engine and `lumenwall-render`'s front):
   - **Device**: `enum Gpu { Wgpu(GpuContext), D3d11(lumenwall_d3d11::Device) }` in the engine. Caps
     as a neutral struct: BC, MSAA levels per format, max texture size, timestamps.
   - **Outputs**: per-window output (`OutputSurface` / `SwapChain`): acquire or bind, present,
     resize, occlusion. Headless output: a target + readback.
   - **Frame**: `begin` → scenes `render` → composition per output → `submit` → `present`. On wgpu,
     the encoder and `FrameCommands`; on Direct3D 11, the immediate context (commands go out at
     `Present` or `Flush`).
   - **Uploads after load**: CPU pixels into a scene target (cover art, CPU video frames, web
     shared-memory frames) and writable textures.
   - **External textures**: an import yields something a copy draw can sample into a target:
     - `D3D11SharedHandle` (CEF accelerated paint): `OpenSharedResource1` on the device;
     - `D3D11VA` frames (`ID3D11Texture2D` array + slice, NV12 or P010) into the YUV conversion;
     - `DmaBuf` and Vulkan Video frames on the wgpu path, as today.
   - **Readback**: target → RGBA8 rows (screenshots, headless, tests).
   - **Stats**: GPU time per frame and per segment, tracked bytes, process GPU memory (QVMI on
     Direct3D 11, `VK_EXT_memory_budget` / allocator report on Vulkan).
4. **Shaders**: one source of truth. `lumenwall-shader` → naga module → the front's neutral naga
   steps (clip fix, validation, reflection) → `PreparedShader`. Then wgpu takes the naga module, and
   Direct3D 11 runs `hlsl::translate` → `compile_program` with the cache key
   `for_identity(program key, clip-fix version, naga version)`.
5. **Unchanged**: the scene graph code of the engine (`scene.rs`, `layers.rs`, `text.rs`,
   `particles.rs`, `bloom.rs`, `puppet.rs`, `shaders/*`), which only uses lumenwall-render's API.

## Backend selection at run time

- **Windows default**: Direct3D 11 on the adapter that drives the first output
  (`AdapterChoice::Monitor`), the rule the wgpu path follows today with its surface filter. Device
  creation with DXGI costs no driver enumeration (135–139 ms measured, against 240–480 ms for
  Vulkan + device).
- **Fallback order on Windows**: Direct3D 11 → Vulkan (wgpu, direct recording) → Direct3D 12
  (wgpu). Direct3D 11 is skipped when:
  - device creation fails (no feature level 11_0, no Direct3D 11.1 constant buffer offsets,
    `Error::Unsupported`);
  - or a wallpaper fails to build on it with `Unsupported` / `Shader` (a shader that does not
    compile to SM 5.0). In that case that wallpaper is loaded again on Vulkan, through the same
    "drop the device, create another" path as the GPU release. It is logged, and the engine keeps
    using Vulkan until the next wallpaper change.
- **Linux**: Vulkan only (the crate is empty there).
- **Switch for comparisons**: `LUMENWALL_GPU_BACKEND=d3d11|vulkan|dx12` (extends the existing
  variable read in `engine.rs` and `context.rs`). `render_png --backend d3d11|vulkan` for headless
  pixel comparisons. A config setting can come later. For development, task 5 maps
  `LUMENWALL_D3D11_DEBUG=1` to `DeviceOptions::debug` (the Direct3D debug layer).
- **Software**: on a machine whose only adapter is the basic render driver (Windows Sandbox, a broken
  driver), `AdapterChoice::Default` takes it (WARP underneath). It renders correctly at a CPU cost;
  Wallpaper Engine does the same there.

## Coverage of the renderer's features on Direct3D 11

| Feature | Design on Direct3D 11 | Task |
|---|---|---|
| Render targets and aliasing (Perf A) | `plan_aliasing` is neutral and reused. Aliased transient targets share one `RenderTarget`; the first draw into a shared one clears it (`clear_first`, as today). Flexible targets render up to 1.5× larger into a shared texture, with the viewport of the shared size and the nominal size reported to shaders. Direct3D 11 has no placed resources: sharing the texture is the aliasing. Dedicated memory falls from the probe's 285 MiB (no aliasing) toward the engine's 199 MiB. | 3 |
| Frame reordering | `schedule` is neutral (op order plus the 8 MiB transient budget). Consecutive draws on one target need no render pass object on Direct3D 11: `OMSetRenderTargets` only when the target changes. Unbind sampled SRVs that alias the next target (the runtime would unbind them with a warning). | 3 |
| Read/write hazards | When a pass samples its own target, copy it to the scratch target (`CopyResource` / `CopySubresourceRegion`), only for slots the shader samples (`Resources::textures`), as today. | 3 |
| Depth and 3D | `TargetDesc { depth: true }`: `D32_FLOAT` of the target's size and samples, cleared to 1 by the first draw of the frame on that target. `DepthKey` = "less or equal" or "always" + write. 3D passes use the Wallpaper Engine convention (`upstream_clip_space: false`, front face CCW / CW for the mirrored reflection), which is Direct3D's. `depth_clip` on for depth targets, off elsewhere (spec 04 Q4). Static meshes: one `StaticBuffer` per scene for every mesh, offsets per mesh. | 3 |
| Puppets and skinning | CPU skinning rewrites the vertex region (`set_vertices`): one `DynamicBuffer` per scene, rewritten when dirty. GPU skinning writes `g_Bones` (`mat4x3` arrays, 16-byte columns) into the pass windows. naga's cbuffer layout follows WGSL's offsets: the foundation tests a `vec3` + `f32` packing at offset 44, and `mat4x3` arrays are checked by task 3's ported puppet test. Skin records live in a static buffer at slot 1; integer `a_BlendIndices` use `R32G32B32A32_UINT` elements. | 3 |
| Particle streams | One `DynamicBuffer` per stream at its maximum size, rewritten with `WRITE_DISCARD` when written; 32-bit indices in a static buffer; the draw count is set each frame. Inputs are matched by name as today. | 3 |
| Bloom and HDR | Bloom is neutral passes. HDR (`HdrMode::Scene`): the pool format becomes `TargetFormat::Rgba16Float`. Composition reads float targets like any other. The swap chain stays 8-bit UNORM (HDR output to an HDR monitor is not planned). | 3 |
| MSAA | `QualitySettings::msaa` on the scene target: `RenderTarget { samples }`, `RasterKey::multisample`, `resolve` before the target is sampled (composition, effects reading `_rt_FullFrameBuffer`, hazard copies). The level is clamped with `Device::msaa_quality_levels`, as `Msaa::clamp_to` does on wgpu. A copy destination stays single-sampled (Jude's rule). | 3 |
| Texture resolution, native resolution | Neutral: top mips dropped before upload, render scale / full-size chains in the front. | 2 |
| Per-monitor flip-model swap chains | One `SwapChain` per output window on the one device. `FLIP_DISCARD`, two buffers, no tearing, `SetMaximumFrameLatency(1)`, `Present(0, 0)`. Resize with the window. A monitor on another adapter stays presentable through DWM (cross-adapter copy), as on Vulkan today. | 5 |
| Occlusion and pause | Pause stays event-driven (platform: covered, locked, fullscreen; no polling). `Present` returning `Occluded` marks the output paused until the platform says it is visible. Long pause: the GPU release drops swap chains, scene renderers and the device. Direct3D 11 has no instance to keep, so restore = `Device::new` + scene rebuild from the bytecode cache. | 5 |
| Device removed or reset | Every call returns `Error::DeviceLost` (or `OutOfMemory`) instead of aborting. On `DeviceLost`:<br>• the engine runs the GPU release path (drop everything);<br>• it logs the reason and rebuilds with `Device::new` and the cached wallpaper, with back-off (1 s, 5 s, 30 s);<br>• after three failures it falls back to Vulkan.<br>On `OutOfMemory` at load: the wallpaper is dropped (log), `trim`, the daemon goes on (Leo's Windows Sandbox abort was a wgpu out-of-memory; here it is a value). Release builds have `panic = "abort"`, hence no `unwrap` on any GPU result. | 3, 5 |
| `Trim` | After every large load (scene build, wallpaper change, resize of many targets), once the GPU is idle (`wait_idle`) and before the first frame: `Device::trim`. Measured: 0.80 MiB steady after `Trim` (above), 1.2–2.0 MiB on the probe's full scene. | 3, 5 |
| GPU timers and stats | `GpuTimer` per frame (render thread, debug flag). `LUMENWALL_PROFILE_PASSES` on Direct3D 11: a timestamp pair per segment in a profiling frame. `Device::memory()` (local / non-local) and tracked bytes in the stats the engine logs. | 3, 5 |
| Shader and pipeline caches | Bytecode cache in `cache_dir()/dxbc`, key `for_identity` as above. Shader objects are created at load (3–4 ms per scene in the probe); there is no pipeline object on Direct3D 11 (states are separate and cached). | 3, 5 |
| Video zero copy (D3D11VA) | FFmpeg's `AVD3D11VADeviceContext` gets the renderer's `ID3D11Device` and immediate context: `ID3D10Multithread::SetMultithreadProtected(TRUE)`, device created without `SINGLETHREADED`. Decoded frames are slices of an NV12/P010 texture array. The renderer creates SRVs of the slice (R8 + R8G8, or R16 + R16G16) and runs the YUV conversion draw into the video target. No CPU round trip; the decoder's pool is the only copy. CPU frames (software decode) use writable textures. | 6 |
| Web zero copy (CEF accelerated paint) | CEF gives a D3D11 NT handle per frame (duplicated into the engine process). `OpenSharedResource1` gives an `ID3D11Texture2D` on our device: it must be the same adapter, so the browser is started on the renderer's adapter LUID. Copy draw into the web target (the format may be BGRA). The handle is released only after the copy, without the blocking `poll` of today: an event query, or simply ordering on the immediate context before the frame is returned. Opened textures are cached by handle identity (`CompareObjectHandles`, as today). Shared-memory frames use writable textures. | 7 |
| Composition | A composition program (the `Compositor`'s shader through naga → HLSL) and one draw per output into the swap chain view: UV range, clamp modes, alpha 1, vertical flip. | 4 |
| Headless and screenshots | `RenderTarget` output + `read_texture_rgba8`. `render_png --backend d3d11`. | 4, 5 |

## Split proposal

Six follow-up tasks, each one focused worker session, with disjoint owned files. Some own whole
crates, others own **module folders inside this crate** next to the foundation files.

**Order.** 2, 3, 4 and 6 can start now, in parallel with each other and with the end of the
foundation (task 1). 2 only needs "Text styling and quality settings" (Jude) landed, because it
reshapes the same crate. 5 starts when 2, 3 and 4 have landed; 7 after 5; 6's engine wiring (its
`video.rs` part) after 4 and 5.

```
1 foundation ──┬── 3 scene executor (lumenwall-d3d11/src/win/scene/) ──┐
               ├── 4 output passes  (lumenwall-d3d11/src/win/output/) ─┼── 5 engine + bridge ── 7 web
               └── 6 video decode   (lumenwall-video/) ────────────────┘        └── 6 video wiring
Jude lands ─── 2 render seam (lumenwall-render/) ──────────────────────────────┘
```

Files of the foundation (task 1, Luna, until it lands; then whoever the orchestrator names):
- `Cargo.toml`, `README.md`;
- `src/{lib,error,hlsl,cache,layout,state,plan}.rs`;
- `src/win/{mod,device,swapchain,texture,target,buffer,state,compile,query,memory,readback,window,external}.rs`;
- `tests/gpu.rs`, `examples/clear_present.rs`.

A change a follow-up task needs in these files is asked for on the board, not made in place.
`Cargo.lock` may change in several tasks (new path dependencies): the conflicts are trivial and
resolved by merging.

### Interfaces the follow-up tasks code against

All in the foundation:
- **Building blocks**: `Device`, `SwapChain`, `Texture`, `RenderTarget` / `TargetDesc`,
  `ConstantArena` / `ConstantLayout` / `ConstantRegion`, `StaticBuffer`, `DynamicBuffer`,
  `StateCache` with its keys, `hlsl::translate`, `compile_program` / `create_input_layout` /
  `ShaderProgram::complete_elements` / `DEFAULT_ATTRIBUTES`, `BytecodeCache` / `CacheKey`,
  `GpuTimer`, `Device::{memory, trim, check_lost, wait_idle}`, `read_texture_rgba8`,
  `OffscreenWindow`.
- **`plan` (all platforms, plain data)**: the contract between the bridge (task 5, producer) and
  the scene executor (task 3, consumer).
  - `ScenePlan { targets: Vec<TargetPlan>, passes: Vec<PassPlan>, ops: Vec<OpPlan>, constants_len,
    frame_block: Option<ConstantRegion>, vertex_bytes, indices, skins, streams: Vec<StreamPlan>,
    meshes: Vec<MeshPlan> }`.
  - `TargetPlan` { size, nominal size, format, samples, depth, `shares` (aliasing), sampler }.
  - `PassPlan` { label, program, target, blend / raster / depth keys, uniform window, slots `[Option<SlotPlan>; 8]`,
    slot-0 variants, `GeometryPlan`, hazard scratch, `clear_first` }.
  - `OpPlan::{Clear, Copy, Draw}`.
  - Attributes are matched to program inputs **by name**, as on the wgpu path.
  - Uniform windows are byte ranges of the front's shadow buffer, 256-byte aligned like wgpu's
    dynamic offsets. The executor mirrors the shadow into its `ConstantArena` byte for byte.
- **`FrameInput<'a>` (in `plan`)**: what a frame reads, borrowed from `RenderScene`'s CPU state, no
  allocation: shadow bytes, the vertex region and its dirty range, slot-0 variant per pass, enabled
  flags, stream data and index counts.
- **External frames (`src/win/external.rs`)**:
  - `DecodedFrame { texture, slice, format: PlaneFormat::{Nv12, P010}, size }`: what the video
    decoder (task 6) hands over and the YUV pass (task 4) samples;
  - `plane_views(&Device, &DecodedFrame)`: luma and chroma SRVs of one array slice;
  - `Device::open_shared_texture(HANDLE)`: CEF's NT handles, `OpenSharedResource1`;
  - `Device::set_multithread_protected`: FFmpeg's decoder thread.
- **Module stubs** for tasks 3 and 4: `src/win/scene/mod.rs` and `src/win/output/mod.rs`, already
  declared by the foundation. Their signatures, below, return `Error::Unsupported` until the task
  implements them. The owning task may change them; task 5 codes against what lands.
  - Task 3: `SceneBuilder::{new, add_texture, add_program, build}`, `Scene::{render, extend,
    set_ops}`.
  - Task 4: `Compositor::{new, encode}`, `ComposeParams`, `HeadlessOutput`, `YuvConverter`,
    `ExternalCopy`.

### 2/n: Backend-neutral seam in lumenwall-render

- **Owns**: `crates/lumenwall-render/`.
- **Starts**: as soon as Jude's task has landed.
- **Work**:
  - Make the descriptors device-free (wgpu enums allowed as plain data, or neutral twins).
  - Split `graph.rs` into the neutral front and a backend trait:
    - the front: `SceneBuilder` and `RenderScene` CPU state, with a public `SceneDesc` export
      (targets with the aliasing plan, passes, ops, shadow layout, vertices, indices, skins, stream
      and mesh layouts);
    - the trait: texture upload, prepared shader, mesh, build, per-frame render with a borrowed
      state view, `extend` / `set_ops`, readback, profiling.
  - Move the wgpu half (bundles, staging belt, `hal`) behind that trait.
  - Expose:
    - `PreparedShader` (clip fix, validation, reflection) for other backends;
    - the composition shader and its parameters apart from the wgpu pipeline;
    - the YUV conversion shader and its matrices (tasks 4 and 6 compile the same WGSL through
      naga → HLSL).
  - Keep today's concrete API (`GpuContext`, `SceneBuilder::new(&GpuContext, …)`) as the wgpu
    backend's convenience, so the engine compiles unchanged.
- **Acceptance**:
  - The workspace check passes.
  - Headless renders of the 8 installed scenes and the 16 default scenes (2560×1440, particles off,
    frames 0 and 44) are byte-identical to main.
  - `tests/alloc.rs` still allocates nothing per frame.
  - Render-thread CPU per frame unchanged within noise (measured like part D).

### 3/n: Direct3D 11 scene executor

- **Owns**: `crates/lumenwall-d3d11/src/win/scene/`, `crates/lumenwall-d3d11/tests/scene.rs`.
- **Starts**: now.
- **Work**: `SceneBuilder` / `Scene` from a `ScenePlan`.
  - Textures and volumes.
  - Targets: aliasing through `shares`, HDR format, MSAA + resolve, depth.
  - Programs through the bytecode cache.
  - Per pass at load: states, arena window, SRV/sampler tables per slot-0 variant, input layouts by
    attribute name (unfed inputs from the defaults buffer), geometry (vertex region as one
    `DynamicBuffer` rewritten on its dirty range, static indices / skins / meshes, streams).
  - The frame from the ops: target binds, clears (`clear_first`), hazard copies, draws, resolves.
  - Variants, enabled flags, `extend`, `set_ops`; per-segment profiling; `trim` after build; no
    Direct3D object created in steady state.
- **Acceptance**:
  - `lumenwall-render`'s GPU test scenarios rebuilt as plans and passing on WARP and on the
    hardware adapter, with the same assertions as `tests/gpu.rs`, `gpu3d.rs`, `quality_gpu.rs`:
    orientation, padding window, culling, translucent + RGB mask, ping-pong, hazard copy, unfed
    attributes, variants, bloom-shaped chain, volume LUT, BC1, visibility, steady state, depth,
    MSAA, HDR, `mat4x3` bone arrays.
  - At most one constant `Map` per frame.
  - Shared GPU memory of a synthetic 100-pass scene under 1.5 MiB after `trim` (measured like the
    foundation's example).

### 4/n: Direct3D 11 output passes

- **Owns**: `crates/lumenwall-d3d11/src/win/output/`, `crates/lumenwall-d3d11/tests/output.rs`.
- **Starts**: now. It can start from a copy of the WGSL of `lumenwall-render`'s `compose.rs` / `yuv.rs`
  and switch to the public strings once task 2 exposes them.
- **Work**:
  - `Compositor` + `ComposeParams`: UV range, clamp / border / repeat, vertical flip, alpha 1, one
    draw into a swap chain view or a target.
  - `HeadlessOutput`: a target + PNG readback.
  - `YuvConverter`: `DecodedFrame` planes (NV12, P010) or CPU planes (writable textures) → RGBA
    target, matrices from `lumenwall-render`'s `yuv::conversion_matrix`.
  - `ExternalCopy`: a shared BGRA or RGBA texture → target, by draw.
- **Acceptance**:
  - The `compose.rs` modes and the `yuv.rs` conversions reproduce the wgpu path's expected values
    (the same assertions as its tests), on WARP and on the hardware adapter.
  - A synthetic NV12 texture array converted exactly.
  - A shared texture created with `D3D11_RESOURCE_MISC_SHARED_NTHANDLE` on a second device, opened
    with `open_shared_texture` and copied.

### 5/n: Engine on Direct3D 11 (and the bridge)

- **Owns**: `crates/lumenwall-engine/` except `src/web.rs` and `src/video.rs` (new folder
  `src/gpu/`); `crates/lumenwall/` (only if the backend switch needs a CLI flag).
- **Starts**: after 2, 3 and 4.
- **Work**:
  - The bridge: `SceneDesc` → `ScenePlan`, `PreparedShader` → `hlsl::translate` with
    `CacheKey::for_identity`, `lumenwall-render`'s backend trait on top of task 3's `Scene`.
  - Backend selection and fallback (above), the `Gpu` enum, outputs and the frame loop
    (`render_output`), per-monitor swap chains and resize, occlusion.
  - GPU release and restore (`release.rs`), device-lost recovery with back-off.
  - Headless (`headless.rs`, `render_png --backend`), screenshots, cover art (`media.rs`),
    `model3d.rs` types, stats, `trim` after loads.
- **Acceptance**:
  - Headless pixel comparison Direct3D 11 vs Vulkan on the 8 installed scenes and the 16 default
    scenes (2560×1440, particles off, frames 0 and 44): byte-identical, or PSNR ≥ 50 dB with every
    difference explained in the README.
  - Desktop measurement like the probe (off-screen 2560×1440 window, 30 fps, Idle priority, 3
    interleaved runs) on 3463520581 and 3018340632: shared GPU memory < 3 MiB (target ≤ 2), and
    dedicated, private, working set, CPU and GPU 3D all ≤ the Vulkan path and ≤ Wallpaper Engine.
  - GPU release → 0 driver threads; resume to first frame < 300 ms.
  - An injected `DeviceLost` recovers without a panic.
  - `LUMENWALL_GPU_BACKEND=vulkan` byte-identical to main.

### 6/n: Video zero copy on Direct3D 11

- **Owns**: `crates/lumenwall-video/`; then `crates/lumenwall-engine/src/video.rs` for the wiring.
- **Starts**: the decoder part now; the wiring after 4 and 5.
- **Work**:
  - FFmpeg D3D11VA on a foundation `Device`: `AVD3D11VADeviceContext` with our device and immediate
    context, `set_multithread_protected(true)`.
  - Decoded frames handed over as `DecodedFrame` (array texture + slice), no
    `av_hwframe_transfer_data`.
  - Writable-texture fallback for software decode; adapter hint by LUID.
  - The Vulkan Video path unchanged.
  - Wiring: the video wallpaper through task 4's `YuvConverter`.
- **Acceptance**:
  - Decoder alone: frames of a test clip decoded on WARP and on hardware, luma and chroma planes
    read back (`plane_views` + readback) equal to the CPU decode.
  - Wired: 3655057993 (4K60) zero copy on Direct3D 11, with CPU, wake-ups, private bytes, dedicated
    and shared memory measured against the Vulkan zero-copy path and Wallpaper Engine; frames
    PSNR-compared with the Vulkan path.

### 7/n: Web zero copy on Direct3D 11

- **Owns**: `crates/lumenwall-engine/src/web.rs`; `crates/lumenwall-web/` (only if the browser must be
  started on the renderer's adapter LUID).
- **Starts**: after 5.
- **Work**:
  - `open_shared_texture` of the CEF NT handle, a cache by handle identity, task 4's `ExternalCopy`
    into the web target.
  - Release of the frame without a blocking poll.
  - Shared-memory fallback through writable textures; `LUMENWALL_WEB_FRAMES=memory` kept.
- **Acceptance**:
  - 3044696375 renders on Direct3D 11 with accelerated paint (no host restart into memory mode).
  - CPU, wake-ups and PSS/private bytes measured against the Vulkan path.
  - Headless web render compared with the Vulkan path.

## Tests

- **Unit, all platforms (17)**:
  - error classification;
  - binding contract registers, HLSL translation (every resource, inputs, struct members, renamed
    names), refused bindings, missing entry point, sampler rewrite (both kinds, renamed index
    arrays, leftover heap refused);
  - cache round trip, corrupt / truncated / foreign / other-version files, key sensitivity;
  - constant windows, linear arenas, upload rules (levels, bytes, BC, volumes);
  - blend table, anisotropy rule, default attribute bytes.
- **GPU (`tests/gpu.rs`, 11)**. WARP by default, so the workspace check never uses the GPU;
  `LUMENWALL_D3D11_TEST_ADAPTER=hardware` runs them on the default adapter. They cover:
  - the device;
  - clear → readback → present → resize on an off-screen window;
  - every texel format with mips and a volume, read back byte-exact; invalid uploads refused;
  - writable textures;
  - the full program path: WGSL → naga → HLSL → `D3DCompile` → draw. It uses an arena window at a
    non-zero offset, the frame block, a texture and sampler, an unfed input, an input layout from
    an arena offset, and checks the exact pixel values of each quadrant;
  - MSAA partial coverage after resolve (and unsupported sample counts refused);
  - depth "less or equal" in both draw orders;
  - state deduplication;
  - the GPU timer;
  - bytecode cache hit / compile / uncached;
  - dynamic buffer bounds.

## Open questions

- Pixel parity of the HLSL path with the Vulkan path on real scenes is only measured by task 5. The
  foundation checks the binding contract, cbuffer offsets, texture orientation and unfed inputs at
  the pixel. naga's HLSL output matches vertex outputs and fragment inputs by location, with
  `SV_Position` last on both sides.
- `Trim` after a resize or a wallpaper change is not measured yet (task 5). The foundation measured
  it after creation and frames.
- Cross-adapter presentation (a monitor on another GPU) is untested on both backends (the test PC
  has both monitors on one GPU).

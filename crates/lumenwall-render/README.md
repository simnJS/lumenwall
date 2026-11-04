# lumenwall-render

GPU foundation of the Lumenwall scene renderer (wgpu 30): device and outputs, texture uploads, named render targets, the pass graph, image chains, bloom, final composition, headless readback and debug counters.

## Sources (clean room)

- `specs/04-rendu-scene-effets.md` (main source: §0–§17, §21, §24, open questions Q1–Q25).
- `specs/03-shaders.md` §11–§13 (render states, engine uniforms, matrices), §3.4 (vertex attributes), §2.4 (`commands/copy`).
- `specs/05-particules.md` §12 (vertex formats, used for sizing only; particles are not wired yet).
- `specs/01-formats-binaires.md` amendment A.1 (volume textures, as prepared by `lumenwall-formats`).
- `specs/08-puppet-squelette.md` §9.7–§9.8 (skinning attributes, `mat4x3` bone palette).
- `specs/09-scenes-3d.md` §0.3, §3.1, §3.2.2, §3.5, §3.7, §3.9, §4.5, §4.6 (depth buffers, static meshes, Wallpaper Engine clip convention for 3D passes, culling, clip-space images).
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/research/render.md`, `docs/TESTING.md` (reference wallpaper used only to size the benchmark: pass count and target scales).
- Public documentation and API of `wgpu` 30.0.1 and `naga` 30.0.1.

No upstream code, fork or snippet was consulted.

## Coverage

| Module | Content |
|---|---|
| `context` | `GpuInstance` (surfaces from raw-window-handle 0.6) → `GpuContext`: Vulkan on Linux, Vulkan then DX12 on Windows (adapter ranking `adapter_rank`), one device for every output, allocator blocks of 4–8 MiB (`MemoryHints::Manual`: resources above 8 MiB get memory of their exact size; on NVIDIA/Windows every reserved byte is also committed private memory; `LUMENWALL_GPU_BLOCKS`), `LUMENWALL_GPU_BACKEND=vulkan|dx12` for measurements, optional features `TEXTURE_COMPRESSION_BC`, `ADDRESS_MODE_CLAMP_TO_BORDER`, `DEPTH_CLIP_CONTROL`, `PIPELINE_CACHE`, timestamps, `VULKAN_EXTERNAL_MEMORY_DMA_BUF` (Unix) and `VULKAN_EXTERNAL_MEMORY_WIN32` (Windows): they unlock wgpu-hal's import of web frames (DMA-BUF, D3D11 shared textures); the extensions are enabled by wgpu whenever the driver has them, so they cost nothing. On-disk `PipelineCache` (Vulkan), shared sampler cache, GPU memory and resource counters, uncaptured GPU errors logged instead of aborting. |
| `surface` | `OutputSurface`: `PresentMode::Mailbox` on Windows when offered (FIFO elsewhere and as the fallback; `LUMENWALL_PRESENT_MODE`), opaque alpha, two swap-chain images on Windows (frame latency 1; `LUMENWALL_FRAME_LATENCY`), never sRGB (non-sRGB view of an sRGB-only swap chain), occlusion/timeout → skip the frame, lost/outdated → reconfigure. |
| `texture` | `TextureUpload` (RGBA8, RG8, R8, BC1/BC2/BC3 raw, all mips) → `GpuTexture`; `VolumeUpload` (3D, uncompressed, all mips) → volume `GpuTexture` (`is_volume`, `depth`); sampler keys from TEX flags (§16.1); `supports_bc()`; in-place `write_level0` for video frames. |
| `targets` | `RenderTargetPool`: RGBA8 UNORM targets, deferred allocation in scene builders (`materialize` at build), nominal size reported to shaders for targets rendered at the displayed resolution (`create_scaled`, `RenderTarget::resolution`), render-graph aliasing of transient targets (`plan_aliasing`: same size, or flexible targets up to 1.5× larger), scoped names (effect → image → scene, §12.1), aliases, `scaled_size` / `divided_size`, scene targets (§4.3), composites A/B (§8.9), bloom targets, shared scratch copies for read/write hazards (§24.6), resize / sampler changes. |
| `blend` | WE `blending` → `wgpu::BlendState` (§14), RGB-only write mask, culling, `FrontFace::Cw` (verified by test). |
| `shader` | Pass binding contract (2D and 3D texture slots), reflection from the naga IR, the clip-space correction `(x, −y, (z+w)/2, w)` appended to every `return` of the vertex entry point (§24.2), naga validation, GPU module creation under an error scope. |
| `dynbuf` | One buffer for frame globals + every pass uniform block (dynamic offsets) + image vertices; CPU shadow with change detection and one `write_buffer` per frame; patch lists copy `g_Time`, pointer, audio spectra into pass blocks without any name lookup. Writes follow nested layouts (arrays of matrices such as `mat4x3 g_Bones[N]`: 16-byte columns, 64-byte elements). |
| `graph` | `SceneBuilder` → `RenderScene`: pipelines deduplicated, bind group layouts per (slots, volume slots), bind groups per slot-0 variant (animated textures), automatic hazard copies (only for slots the shader samples), consecutive draws on one target merged into one render pass (clear merged too), frozen passes pre-recorded in render bundles, frame reordering when the owner marked transient targets (`schedule`: chains first, scene draws together, conflicting operations kept in order, 8 MiB budget of targets kept alive), per-frame uploads through a `StagingBelt`, `profile_frame` (GPU time per render pass, tools), `needs_redraw()` dirty flag, `set_enabled` (visibility), `set_variant`. After load: `extend` (passes added with the scene's shaders, pipelines and layouts) and `set_ops` (new frame order; only bundles of changed runs are re-recorded). |
| `chain` | §9.8/§10/§11.6: destinations, inputs, `previous`, A/B ping-pong, geometry case, matrix kind, blend transfer (the only place it is applied, see "Blending contract"), RGB-only last pass (pure, unit tested with the spec examples). |
| `image` | `add_image` (chain plan → passes, geometry, static matrices) and `ImageHandle::update` (rect, alignment, rotation pivot, `M_ecran` with parallax, scene geometry) every frame. |
| `math` | `ortho`, `lookAt`, camera `P = ortho · T(eye)`, auto projection, pointer, parallax, hierarchy, alignment, rect, rotation, `M_copie`, `uMax/vMax`, quads in the spec vertex order, text matrix. |
| `compose` | `uv_range` (stretch/fill/fit/default with the exact truncations), span (`bounding_box`, `span_range`), vertical flip, `Compositor`/`OutputComposer` (one fullscreen triangle, blending off, alpha 1, clamp/border/repeat). |
| `bloom` | §17 as five generic passes; `BloomShaders` takes the translated WE asset shaders (what `lumenwall-engine` feeds), `BloomShaders::internal()` provides WGSL stand-ins (tests, installs without the WE shaders). |
| `headless` | `HeadlessOutput`, `read_texture_rgba8`, `write_png`. |
| `stats` | `GpuTimer` (timestamps, 3-slot async readback, never stalls), `FrameStats`, `allocator_bytes`. |
| `yuv` | Video frames: `YuvConverter` (8-bit UNORM planes sampled, 9–16-bit `R16Uint`/`Rg16Uint` planes filtered in the shader), `YuvPlanes` (uploaded planes) or `source_from_views` (zero-copy planes), `conversion_matrix` (BT.601/709/2020/240M/FCC, limited/full range, bit depth, MSB/LSB alignment), chroma siting and decoder padding crop; one draw into an RGBA8 target. |
| `video_device` | Optional device creation through wgpu-hal with Vulkan Video decode queues (`ContextOptions::video_decode`): decode/compute/transfer families other than wgpu's, `synchronization2`, decode extensions; `GpuContext::video_queues` for lumenwall-video's zero-copy decoder. |

### 3D additions (spec 09)

- **Depth**: `SceneBuilder::add_depth(target)` gives a target a `Depth32Float` buffer of its size (`DEPTH_FORMAT`), shared by every pass drawn into it during the frame and cleared to 1 by the first render pass of the frame on that target (whether or not the colour is cleared). `PassDesc::depth` (`DepthState { test, write }`) selects "less or equal" or "always" and the write; it is ignored for targets without depth, whose pipelines keep `depth_stencil: None`. Only scenes that call `add_depth` pay anything (2D scenes are unchanged). Bundles and render passes carry the attachment; depth buffers follow target resizes in `rebuild_bindings`. Passes into a depth target clip against the near/far planes (Wallpaper Engine's rasteriser default); the others keep `unclipped_depth` (spec 04 Q4).
- **Static meshes**: `SceneBuilder::add_mesh(label, MeshLayout, bytes, u16 indices)` uploads an interleaved vertex buffer once (`MeshLayout { stride, attributes: [MeshAttribute { name, format, offset }] }`, the `.mdl` bytes as stored) and `Geometry::Mesh(MeshId)` draws it whole. Shader inputs are fed by name, others read (0, 0, 0, 1); pipelines are shared between meshes of the same layout; mesh passes are bundled.
- **Clip convention**: `ShaderDesc { upstream_clip_space: false }` for 3D passes (Wallpaper Engine/D3D convention, y up, depth [0, 1]: what wgpu expects), with `PassDesc::front_face` = `Ccw` (and `Cw` for the mirrored reflection pass). `math::perspective_we` is the projection of spec 09 §3.2.2; `math::perspective_upstream` = `Sc(1, −1, 1)·P_GL` gives the same pixels and depths to passes that keep the clip fix (images, particles).
- **Images of a perspective scene**: `add_image_with_options(…, ImageOptions { clip_space: true })` feeds every pass clip-space geometry (`[-1, 1]²`, `z = 0`) and compensates the matrices (`M_copie` → identity, `M_ecran · S(W/2, H/2, 1)`), so shaders that ignore the matrices (`cloudsbg.vert`) cover the image and the others draw the same pixels (spec 09 §3.9).

## Integration contract (render-ready input types)

The integration layer converts the other crates' outputs into these types; `lumenwall-render` depends on none of them.

### Device and outputs

```rust
let instance = GpuInstance::new(None);                        // Vulkan (+DX12 on Windows)
let surface = unsafe { instance.create_surface(display, window)? }; // raw-window-handle 0.6
let ctx = instance.into_context(&ContextOptions { pipeline_cache_dir: Some(dir), ..Default::default() }, &[&surface])?;
let mut output = OutputSurface::new(&ctx, surface, (w, h))?;
// after loading a wallpaper: ctx.save_pipeline_cache()?;
```

`ctx.into_instance()` drops the device (after every surface and resource made from it) and
keeps the instance, to create another device with other options without enumerating the drivers
again: lumenwall-engine recreates the device with video decode queues
(`ContextOptions::video_decode`, `ctx.video_decode_requested()`) when a video replaces a scene.

GPU choice with several monitors: one device for every output, on the adapter that presents the
first output (`into_context` filters on its surface, then ranks Vulkan before DX12 and keeps the
driver order between GPUs, the discrete one first in practice). The later outputs reuse that device;
one whose surface the adapter cannot present gets `SurfaceUnsupported` (lumenwall-engine: the start
fails when it is there at start-up, a hot-plugged one is only logged). On Windows a window on a
monitor driven by another adapter is expected to stay presentable, as on hybrid-GPU laptops (DWM
copies the frame across adapters). One device per adapter is rejected: each scene would be rendered
once per GPU, the integrated one at full cost, with a second driver's threads and memory.
Cross-adapter presentation is not measured: the Windows test PC (two 2560×1440 monitors) turned out to
have both on its RTX 4080 SUPER (`EnumDisplayDevices`), and its static second monitor came from the
engine (lumenwall-engine `scene_present_due`), not from the adapter.

`loader::adapter_manifest_glob(adapter)` names the Vulkan driver of an adapter as a
`VK_LOADER_DRIVERS_SELECT` glob of its Linux manifest (`*nvidia*`, `*radeon*`, `*intel*`, …,
from `VkPhysicalDeviceDriverProperties::driverID`); lumenwall-engine remembers it to restrict
the loader at the next start.

### Textures (`lumenwall-formats` → `TextureUpload`)

| `.tex` format | `TexelFormat` |
|---|---|
| RGBA8888 / decoded PNG, JPEG… | `Rgba8` |
| DXT1 / DXT3 / DXT5 | `Bc1` / `Bc2` / `Bc3` if `ctx.supports_bc()`, else decode to `Rgba8` |
| RG88 / R8 | `Rg8` / `R8` |
| RGB565, RGB888, float formats | expand to `Rgba8` on the CPU |

`levels` = every mip of one image (level `i` = `max(1, size0 >> i)`, tightly packed rows), `content_size` = declared ("real") size, `flags` = TEX flags. BC level 0 must be a multiple of 4. An animated texture is one upload per atlas image, bound with `ImagePass::slot0_variants`; `RenderScene::set_variant` selects the frame (§16.3). The CPU copy can be dropped right after `SceneBuilder::add_texture`.

Volume textures (`.tex` flag 0x40, spec 01 A.1, e.g. the 32×32×32 colour LUTs of `materials/lut`): `SceneBuilder::add_volume_texture(&VolumeUpload)`, levels of `width × height × depth` (slices one after the other, uncompressed; `lumenwall-formats` decodes BC volumes). They are referenced by `TextureId` like any texture: a slot the shader declares as `texture_3d` samples the volume, a 2D slot given a volume reads the 2D default instead (and a 3D slot given a 2D texture or a target reads a 1×1×1 `(0, 0, 0, 1)` volume, created only when a shader has a 3D slot). Volume samplers use the texture's flags without anisotropy (`GpuContext::volume_sampler`).

### Shaders (`lumenwall-shader` → `ShaderDesc`)

`ShaderDesc { label, module: naga::Module, vertex_entry, fragment_entry, upstream_clip_space: true }`. The module must follow this binding contract:

- `@group(0) @binding(0)`: optional `var<uniform>` struct holding the pass uniforms. Member names are the WE names (`g_ModelViewProjectionMatrix`, `g_Time`, material constants…); offsets and types are read from the naga struct. Bound with a dynamic offset.
- `@group(0) @binding(1)`: optional frame block, layout `dynbuf::FRAME_GLOBALS_WGSL` (`g_Time`, `g_Daytime`, pointers, texel sizes, audio spectra packed in `vec4` arrays). Using it instead of pass-block copies saves CPU work; both are supported.
- `@group(1) @binding(2N)`: `texture_2d<f32>` of slot N (`g_TextureN`, N < 8), or `texture_3d<f32>` for a volume slot (`sampler3D`, colour LUTs); `@binding(2N+1)`: its filtering sampler.
- Vertex inputs by argument/member name: `a_Position` (3 floats) and `a_TexCoord` (2 floats) are fed; with `Geometry::Skinned`, `a_BlendIndices` (integer vector), `a_BlendWeights`, `a_Normal` and `a_Tangent4` read the static skin buffer (`SkinVertex`, 64 bytes, `SceneBuilder::add_skin`); any other `@location` receives (0, 0, 0, 1).
- The vertex output is in the **GLSL/upstream clip convention**; lumenwall-render appends the wgpu correction. Do not apply it in the translator.
- One fragment output at `@location(0)`.

Reuse the same `ShaderId` for identical translated shaders (same source + combos): pipelines are deduplicated by `(shader, blend, write mask, cull)`.

### Images (`lumenwall-scene` → `ImageSpec` + `ImagePass`)

- `RenderTargetPool::create_scene_targets` before the objects, `create_composites(id, tronc(S), sampler)` per image (or `create_composites_sized` with a size for A and one for B), `new_scope` + `create(name, scaled_size(S, scale))` for effect `fbos`, `create_bloom_targets` / `create_bloom_composites` after the objects. Names are resolved with `resolve(scope, name)` at load time.
- `ImageSpec`: `S`, parsed `Alignment`, main texture and its `(storage, content)` sizes, `animated`, `passthrough`, `fullscreen`, composites, scene target, `final_to_scene`.
- `ImagePass` per pass, in order (material passes, effect passes, tint and `colorBlendMode` passes): shader, the material's **own** blend (see below), cull, resolved `target`, slot sources (`SlotSource::Texture` = highest-priority ready candidate, `SlotSource::Previous` = `bind previous`).
- `add_image` writes static uniforms (`g_ModelMatrix`, copy/identity MVPs, identity engine matrices, texel sizes, ones for alpha/colour/brightness, `g_TextureNResolution` from the bound textures) and appends the draws in render order. Other uniforms: `SceneBuilder::uniform(pass, name)` once → `UniformSlot`, then `shadow_mut().write_floats / write_ints / write_mat4` (at load or any frame).

### Blending contract (spec 04 §9.8, spec 02 §12.6)

The transfer "when the chain has more than one pass, the last pass takes the first pass's blending and the first pass becomes `normal`" (then the puppet rule: first pass `translucent`) is applied **once, by lumenwall-render**: `chain::plan_image_chain`, used by `add_image` / `add_image_with_puppet` and the bloom. It runs on the chain actually drawn (after debug filters, after effect visibility changes). Callers pass each material pass's own `blending` in `ImagePass::blend` and never apply the transfer themselves; `lumenwall-scene` hands over the own modes (`PassDesc::blending`). `crates/lumenwall-engine/tests/blend_contract.rs` fails if the transfer is applied zero or two times.

### Frame loop

```rust
scene.update_frame(&FrameInputs { time, daytime, pointer, pointer_last, audio16, audio32, audio64 });
image.update(&mut scene, &ImageFrame { view_projection, transform, parallax, scene_size }); // per image
if scene.needs_redraw() || output_changed {
    let Some(frame) = output.acquire(&ctx)? else { /* occluded */ };
    let mut enc = ctx.device().create_command_encoder(&Default::default());
    scene.encode(&ctx, &mut enc);                         // ≤ 1 write_buffer, prerecorded segments
    let uv = composer.update(&ctx, frame.size(), logical); // also feeds next frame's pointer
    compositor.encode(&mut enc, &composer, frame.view(), region, Some(clear));
    ctx.queue().submit([enc.finish()]);
    frame.present(&ctx);
}
```

`examples/headless.rs` is a complete minimal integration (texture + base pass + effect + composition → PNG).

## Performance

- Load time only: every texture, target, buffer, sampler, bind group, pipeline and bundle. Steady state creates no GPU object (`ctx.resources_created()` is constant, tested) and the CPU update path performs **zero heap allocations** (`tests/alloc.rs`, counting allocator; wgpu itself allocates ~90 times to create an encoder and record a 6-pass frame).
- Layers added after load (`RenderScene::extend`, scripts' `createLayer`): shader modules, pipelines (cache by shader, blend, mask, cull), bind group layouts and textures are reused; only the new passes' uniform blocks, vertices, bind groups and targets are created. The dynamic buffer grows with headroom (≥ 64 KiB, geometric for vertices); a reallocation recreates the uniform bind group and re-records the bundles once. `set_ops` re-records only the bundles whose run of passes changed (`tests/gpu.rs` `layers_added_after_build`: a reorder creates one bundle, the same order again none).
- Per-frame uploads (the changed uniform byte range, the vertex streams written since the last frame) are copied from a recycled `wgpu::util::StagingBelt` in the frame's encoder: no buffer is created per frame; nothing when nothing changed.
- Frozen passes are replayed from render bundles; consecutive draws to one target share one render pass.
- `needs_redraw()` is false when no uniform, vertex, variant or external texture changed: the caller skips rendering and presenting. Graphs that accumulate across frames (translucent/additive passes into never-cleared targets, §15) keep rendering `ACCUMULATION_SETTLE_FRAMES` (120) frames after the last change; graphs with cross-frame reads keep 2.
- `examples/chain_bench.rs` reproduces the shape of the reference wallpaper (3440×1440, one image with 19 passes: 8 single-pass effects, shine 5 passes at ½, localcontrast 4 passes at ¼, chromatic aberration; bloom 5 passes; composition) with synthetic shaders of similar tap counts:

| Adapter | GPU / frame | CPU update + encode / frame |
|---|---|---|
| RTX 4080 SUPER (Vulkan) | **0.6 ms** | 0.1 ms |
| Ryzen 7950X3D iGPU, 2 CU RDNA2 (RADV) | 22.9 ms (texture-rate bound) | 0.28 ms |
| llvmpipe | 146 ms | 0.15 ms |

  VRAM tracked: 108 MiB (146 MiB before the 1×1 shadow-atlas / bloom-B savings below), dominated by the 3440×1440 RGBA8 main texture and the full-size targets (scene, A/B, bloom A); BC textures shrink the texture share 4–8×. Pipelines + bundles build in 2–50 ms (driver cache warm/cold).

  The 2-CU iGPU exceeds 16 ms because the synthetic shaders are texture-rate bound (≈ 50 full-resolution taps per pixel per frame); engine overhead is the CPU column. The real cost must be re-measured once `lumenwall-shader` translates the wallpaper's own effect shaders; BC main textures would also cut the base-pass bandwidth.

### Windows: shared GPU memory and presentation (NVIDIA)

"Shared GPU memory" (Task Manager, `GPU Process Memory` counters) is the system memory a
process has on the GPU's books. Measured on the Windows test PC (RTX 4080 SUPER, driver
621.01, resizable BAR on, 2560×1440, 30 fps) with a probe outside the repository that reads the
process's own counters and `VK_EXT_memory_budget` after each step (both agree to the KiB):

| Step | Shared | Dedicated |
|---|---|---|
| Vulkan instance | 0.27 MiB | 0.02 MiB |
| Vulkan device, one queue (bare `ash` device or wgpu's: the same; any queue family, priority or API version) | +12.1–12.6 MiB | +8.7 MiB (bare), +18.7 MiB (wgpu) |
| FIFO swap chain, 2 images (`OutputSurface` before this change) | **+26.8 MiB** (+23.9 at creation, +2.5 at the first present) | +78 MiB |
| Mailbox swap chain, 2 images | **+0.03 MiB** | +60 MiB |
| D3D11 device + DXGI flip swap chain, for comparison (what Wallpaper Engine uses) | 0.7 MiB in all | 40.8 MiB |
| D3D12 device + direct queue + DXGI swap chain | 26.4 MiB in all (the queue alone 15.3) | 48.5 MiB |

wgpu's own buffers add nothing there: with resizable BAR its upload and staging buffers
(`MAP_WRITE`) live in device-local, host-visible memory (dedicated). Only buffers mapped for
reading land in system memory, and the allocator then keeps a 4 MiB block of that memory type for
the life of the device: `GpuTimer` did that for 48 bytes (+4.0 MiB, kept after the timer is
dropped); its buffers are now also mappable for writing (`readback_usage`,
`MAPPABLE_PRIMARY_BUFFERS`) and share the upload buffers' block.

NVIDIA's Vulkan driver presents a FIFO swap chain through its own Direct3D 12 presentation queue:
besides the memory, it costs ≈ 0.69 ms of the process's 3D-engine time per presented frame
(probe, clear + present at 30 fps: 0.737 ms per frame in FIFO, 0.045 ms in Mailbox) and 190–230 ms
to create (Mailbox: 16 ms). `OutputSurface` asks for Mailbox on Windows: the wallpaper paces its own
frames below the refresh rate, so every frame is still shown at the next vertical blank, without
tearing (Direct3D 12 backend: `Present(0, 0)` on a flip-model swap chain, no tearing either).

The floor left is the Vulkan device itself, 12.4–12.9 MiB on this driver, against 0.7 MiB for a
Direct3D 11 device with its swap chain: under Wallpaper Engine's 3 MiB only with a Direct3D 11
renderer.

## Decisions on the open questions of spec 04

Rules refer to `docs/PARITY-POLICY.md` (1 perf bugs never reproduced, 2 crashes fixed, 3 visible output reproduced, 4 WE gaps filled, 5 visible bugs follow WE when known, 6 frame-rate independence). Items owned by another crate are recorded here so the integration follows them.

| Q | Decision | Rule |
|---|---|---|
| Q1 | Reproduce the reference where it changes the output and WE's behaviour is unknown (`// parity:` comments); follow WE where a visible defect contradicts its documented behaviour (Q5–Q8, Q12, Q20–Q22); never reproduce perf bugs or crashes. | 1–5 |
| Q2 | Each scene clears its own RT with its own `clearcolor`, and each output clears its background with its scene's colour (identical to the reference with one scene; no leak between scenes). | 5 |
| Q3 | Reference auto-projection formula kept (`math::auto_projection_size`, `// parity:`); to confirm against WE. | 3/5 |
| Q4 | `nearz`/`farz` read where the reference reads them (scene crate), but pipelines use `unclipped_depth` when available: nothing is discarded for its depth (no depth buffer exists), so a scene with `nearz > 0` stays visible as in WE. | 5 |
| Q5 | Default `ParallaxSmoothing::WallpaperEngine`: `delay` is a time constant in seconds (0 = follow instantly), `k = 1 − e^(−dt/delay)`; the reference law stays available (`Reference`). Exact WE curve unconfirmed. | 5, 6 |
| Q6 | Mouse disabled → pointer at the centre of the visible window (neutral parallax, `pointer_disabled`). Lumenwall never reads the output back, so the same `g_PointerPosition` formula holds on X11, Wayland and Windows. | 5 |
| Q7 | Alpha, colour, brightness are live: their `UniformSlot`s are written whenever the property/script value changes (no per-frame cost otherwise). | 5 |
| Q8 | `ChainOptions::final_to_scene = true` for every image; visibility toggles with `RenderScene::set_enabled(image.passes)`. Effect visibility changes are handled by re-planning the image chain (integration rebuilds that image's passes; event-driven). | 5 |
| Q9 | Magenta tint pass kept (it changes the output, WE behaviour unknown): the scene crate appends it as a normal `ImagePass`. | 3 |
| Q10 | `colorBlendMode` is a normal appended pass; when it samples `_rt_FullFrameBuffer` while drawing into it, the graph inserts the copy automatically (§24.6, tested). Mode table comes from the WE shader. | 3 |
| Q11 | Output always opaque: compositor writes alpha 1, surface alpha mode `Opaque` when offered. | 3 |
| Q12 | Not reproduced: a thumbnail that becomes ready later is bound (integration updates the slot and calls `rebuild_bindings`). | 5 |
| Q13 | `g_TextureNResolution` = resolution of the texture actually bound (filled from the bindings at build and on `set_variant`). | 5 |
| Q14 | Missing main texture, parent, or zero-sized texture-less image: the object is dropped, the scene goes on. | 2 |
| Q15 | Dependency cycles detected and the offending edge ignored (scene crate). | 2 |
| Q16 | Texture cache keyed per wallpaper (assets/integration); `TextureId`s are per scene. | 5 |
| Q17 | `TEX0FORMAT` derived from the main texture for every pass, as the reference (combo, shader crate). | 3 |
| Q18 | Scene video textures follow the clock (video crate); render side: `GpuTexture::write_level0` + `RenderScene::mark_dirty`. | 6 |
| Q19 | Windows: `systemfont_<name>` maps to that installed family, fallback Segoe UI, then Arial (text crate, not yet wired). | 5 |
| Q20 | Text ordinate counted from the bottom like images (`math::text_matrix`). | 5 |
| Q21 | Text always drawn into the scene RT. | 5 |
| Q22 | Single-pass puppet: mesh converted to RC and drawn with `M_ecran` (`GeometryCase::PuppetScene`). | 5 |
| Q23 | Perspective camera, zoom, shake, lights, HDR, `fbos.format`/`unique`, global `_rt_*CompoBuffer*`: not implemented yet (orthographic only, RGBA8 targets; effect-declared `_rt_HalfCompoBuffer*` work through effect scopes). Planned under rule 4. | 4 (later) |
| Q24 | Clock kept in f64 on the CPU, converted to f32 only in `FrameInputs::time`; no wrap (a wrap would cause visible jumps). Shader-side f32 precision is the same as WE's. | 3 |
| Q25 | No 250 µs polling: pause is event-driven (platform crate), zero wake-ups. | 1 |

## Deviations

- `blending: normal` disables the blend unit instead of computing `src·1 + dst·0`: identical bits on UNORM targets, cheaper.
- Targets render at the size the scene is displayed at (lumenwall-engine `render_scale`: the scene target, image composites, effect targets and bloom), and report their nominal size in `g_TextureNResolution`, so texel offsets keep their length in texture coordinates. The reference renders at the scene's size. Fine detail is resampled at the display's resolution (docs/PERFORMANCE.md, visual comparison).
- Transient render targets share memory when their frame lifetimes do not overlap; a shared texture is cleared before its first draw of the frame (identical pixels: a transient target is never read before it is written in a frame).
- `_rt_shadowAtlas` is allocated 1×1 (never filled, samples as transparent black; only its resolution vector differs). The bloom image's B composite is 1×1 (never written, unreachable by name). lumenwall-engine does the same for every image composite its chain never writes and no material names (`create_composites_sized`): it would only hold zeros.
- Targets of `tronc(S/scale) = 0` are clamped to 1 pixel (zero-sized textures are invalid; crash fix).
- `ClampToBorder` render targets need `ADDRESS_MODE_CLAMP_TO_BORDER`; without it they clamp to edge (warning). The output `border` clamp mode is emulated in the composition shader, identical result.
- Bloom: the internal WGSL shaders are approximations of the WE asset materials (same passes, inputs, sizes and constants); parity requires the translated WE shaders through `BloomShaders`, which `lumenwall-engine` provides (its output matches a CPU evaluation of the WE shaders to ±2/255 on the reference wallpaper).
- Volume textures in `texture_3d` slots (spec 03 §8.1 binds 2D textures only; the reference fails on volume `.tex` files, spec 01 A.1): colour LUTs grade the image instead of the object failing (policies 2 and 4).
- Accumulating graphs: when the caller skips static frames, rendering continues 120 frames after the last change instead of indefinitely (convergence of 8-bit accumulation).

## Not done yet (wired later)

- Text objects (R8 glyph texture, `text_matrix` is ready). Video wallpapers use `yuv` + `video_device` (lumenwall-video). Web wallpapers (lumenwall-engine `web`) import the browser's DMA-BUFs through wgpu-hal into a `RenderTargetPool` target (`resize` + `Compositor::rebind` on size changes).
- Added for the integration (`lumenwall-engine`): vertex streams for particles (`SceneBuilder::add_stream`, `Geometry::Stream`, `RenderScene::write_stream`, inputs matched by name through `ShaderInterface::vertex_input_names`, never bundled) and puppet meshes (`add_image_with_puppet`, `PuppetGeometry`). A `PuppetGeometry` with one `SkinVertex` per vertex becomes `Geometry::Skinned`: slot 0 and the skin buffer (slot 1) are both bound at the mesh's first vertex, indices drawn with base 0, still bundled. `ImageHandle::puppet_vertices` / `puppet_passes` give the caller the vertex range (CPU skinning through `set_vertices`) and the passes whose `g_Bones` it writes (GPU skinning). The engine supplies the palette; nothing here knows about bones.
- Real WE shaders: depends on `lumenwall-shader` emitting the binding contract above.
- Load-op `Clear` instead of `Load` for passes that fully overwrite their target (saves bandwidth on tiled/integrated GPUs; needs a coverage guarantee to stay parity-safe).

## Tests

- Unit (46): matrices, camera, pointer, parallax laws, hierarchy, alignment, rect, rotation sense, uv padding, quad order/winding, scaling modes with the spec examples and truncation effects, span, blend table, culling/masks, chain plans (spec examples), RT sizes and scopes, uniform layouts and patches, sampler flags, upload validation, surface formats, adapter ranking, naga reflection (2D/3D slots) and clip fix, volume upload validation.
- GPU (`tests/gpu.rs`, 15, headless): orientation end to end, padding window, culling with `FrontFace::Cw`, translucent blend + RGB mask, A/B ping-pong, read/write hazard copy, unfed attributes, animated variants, bloom chain, volume LUT slot (3D texture grading, volume in a 2D slot), composition modes, BC1 upload, visibility toggle, pipeline cache on disk, steady state (no new GPU objects, `needs_redraw`, `g_Time`, GPU timer). They pass on the RTX 4080, RADV and llvmpipe (`LUMENWALL_TEST_SOFTWARE=1`); without any adapter they print "skipped".
- `tests/alloc.rs`: zero allocation in the per-frame update path.
- `tests/gpu3d.rs`: F-3D-O of spec 09 (first row red ≈ 1 with a clip-space quad, top-left UV (0, 0) through the VP-1 camera), depth test across two meshes over two frames, back-face culling with `FrontFace::Ccw`. VP-1/VP-2 vectors in `math::tests`.
- Examples: `headless` (writes a PNG), `chain_bench` (reference-shaped chain timing).

# Research: rendering, shaders, textures (2026-11-03)

Ranked by performance first (see `docs/PERFORMANCE.md`).

## Decisions

| Area | Choice | Fallback | Notes |
|---|---|---|---|
| GPU API | **wgpu 30** (Vulkan on Linux and Windows, DX12 fallback), `MemoryHints::MemoryUsage` | ash (raw Vulkan) if profiling shows wgpu CPU overhead matters | wgpu overhead ~5-10% CPU, irrelevant at 10-300 draws/frame. No internal threads, idle cost = our loop. DX11 backend removed upstream in wgpu. |
| Shader pipeline | Rust preprocessor → **glslang** (crate `glslang` 0.9, builds via `cc`) with HLSL-style implicit conversions → split combined samplers (`spirv-tools` pass) → **naga spv-in** → wgpu | `shaderc` | naga's GLSL frontend is not enough: no implicit conversions, needs explicit bindings. WE shaders rely on HLSL-like implicit float↔vecN, int↔float↔bool conversions. The existing patched fork (Almamu/glslang-WallpaperEngine) is a fork of Khronos glslang with no explicit licence on the patches (GitHub: NOASSERTION). To stay clean, we write our own glslang patches from the behaviour described in `specs/03-shaders.md`, on top of Khronos glslang (BSD/MIT/Apache). |
| Shader cache | On-disk cache keyed by `blake3(preprocessed source + combos)` + wgpu `PipelineCache` (Vulkan) | — | All shader cost is at load time. Use `Dx12Compiler::Auto` (DXC) on Windows. |
| `.pkg` | **memmap2**, zero-copy slices; unmap after load on Windows (file lock blocks Steam updates) | `std::fs::read` | No compression, no checksum. |
| LZ4 | **lz4_flex** 0.14, `decompress_into` straight into upload staging | — | ~5 GB/s. |
| Compressed textures | **Upload BC1/BC2/BC3 directly** (`TEXTURE_COMPRESSION_BC`), free CPU copies after upload | CPU decode with **bcdec_rs** | 4-8× less VRAM, zero decode cost. RGB888/RGB565/RGB16f need CPU expansion to RGBA. |
| Embedded images | **image** 0.25 (zune-jpeg, png, gif), parallel decode with rayon | — | PSD/PIC not supported by `image`. |
| Math | **glam** | — | SIMD, GLM-compatible layout. |
| Particles | CPU SoA simulation with glam, ring buffer, persistent vertex buffer | GPU compute (breaks 1:1 parity of operators/RNG) | Estimate: 10k particles ≈ 0.3-0.5 ms CPU/frame. |
| MSAA | Drop it | — | Only affects the final quad; saves ~100 MB VRAM in 4K. |
| Text | **cosmic-text** with empty fontdb + embedded font only; re-render to R8 texture only when text changes | fontdue | Pixel parity with Windows text rendering is not achievable. |
| Scene JSON | **serde_json** with typed structs | simd-json (runtime SIMD detection) | Parsed once at load; WE values are polymorphic (scalar or `{value,user}`). |

## Architecture

1. wgpu, `PresentMode::Fifo`, FPS cap, no rendering when hidden.
2. mmap `.pkg` → LZ4 into staging → native BCn upload → free CPU copies.
3. Rust shader preprocessor → glslang → sampler split → naga; uniform offsets from naga reflection.
4. Disk shader cache + pipeline cache.
5. One uniform buffer per pass with dynamic offsets; static values written once; time/mouse/audio in a single `write_buffer` per frame.
6. Passes frozen after load → pre-recorded render bundles.
7. Particles on CPU; text texture updated only on change.

## Parity risks

- HLSL-style implicit conversions require a patched glslang (maintenance cost, rebase on upstream glslang).
- Sampler splitting breaks if a `sampler2D` is passed to a user function.
- wgpu follows D3D conventions (like Wallpaper Engine on Windows), so GL-specific hacks (Y flip, negated `ddy`, depth -1..1) must not be reproduced.
- DXC/naga vs the original FXC/D3D11: possible precision and NaN differences.
- Formats beyond the known set (BC7, RGB565, float formats, MP4 inside `.tex`) need specs of their own.
- Rendering on X11 must go straight to a desktop window, not through a CPU readback.

## Sources

- https://github.com/gfx-rs/wgpu/issues/6357, /7102, /4323, PR /9558, CHANGELOG
- https://github.com/SnowflakePowered/glslang-rs
- https://github.com/PSeitz/lz4_flex
- https://github.com/cloudwego/sonic-rs
- https://github.com/KhronosGroup/SPIRV-Tools/pull/6035

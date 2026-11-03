# lumen-video

Video wallpapers: FFmpeg demuxing and decoding on a dedicated thread, a small bounded queue
of frames, and YUV → RGBA conversion on the GPU (`lumen_render::yuv`) into the wallpaper
image that the compositor scales to the outputs (fill/fit/stretch/default, clamp modes).

## Sources (clean room)

- `specs/07-plateforme-audio-video-web.md` §2.2/§2.4 (`--hwdec`, `--fps`, `--volume`,
  `--silent`), §4.5 (wallpaper size = display size of the stream, scaling), §8 (cadence),
  §9.4 (pause), §12 (automute), §14 (video playback), QO-15, QO-16.
- `specs/01-formats-binaires.md` §2.10.2 (videos embedded in `.tex`, read from memory).
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/research/platform.md`, `deny.toml`.
- Public documentation: FFmpeg 8 headers and API docs (`hwcontext_vulkan.h`, `avcodec.h`),
  FFmpeg 8.0 library sources read only to learn the API contract of a caller-provided
  Vulkan device (queue families, frame locking, semaphores), Vulkan specification, wgpu /
  wgpu-hal 30 docs. Reference wallpaper 3655057993 used as test input only.

No upstream code, fork or third-party Wallpaper Engine reimplementation was read.

## Bindings: `ffmpeg-sys-next` 8.1 (not `rsmpeg`)

| | `rsmpeg` 0.18 (MIT) | `ffmpeg-next`/`ffmpeg-sys-next` 8.1 (WTFPL) |
|---|---|---|
| FFmpeg 8 | yes | yes |
| Hardware API | thin helpers (device create, `hw_device_ctx`, transfer); `get_format`, Vulkan/CUDA/D3D11 structs are raw FFI anyway | none; raw FFI |
| Libraries linked | all seven (`avdevice`, `avfilter` too: pkg-config probe of each, `-l` for each) — fails without their dev packages, drags SDL/X11/ALSA/… dependency trees at run time | only the enabled ones: here `avcodec avformat avutil swscale` |
| Static linking | possible through environment variables (`FFMPEG_LINK_MODE`, `FFMPEG_LIBS_DIR` defaults to static) — cargo-deny cannot see it | only through the `static`/`build` features, banned in `deny.toml` |
| Dependencies | `paste` (RUSTSEC-2024-0436, unmaintained: cargo-deny error), `bon` | `libc`; build: `bindgen`, `cc`, `pkg-config` |

Every hardware path needs raw FFI (the safe wrappers of either crate do not cover them), so
Lumen uses the raw `ffmpeg-sys-next` crate alone (not the `ffmpeg-next` wrapper), with RAII
wrappers of its own. FFmpeg is linked **dynamically** (`ldd lumen`: `libavcodec.so.62`,
`libavformat.so.62`, `libavutil.so.60`, `libswscale.so.9`); `build.rs` also rejects
`FFMPEG_STATIC`/`PKG_CONFIG_ALL_STATIC`. The WTFPL exception was already prepared in
`deny.toml` (legal confirmation still pending there).

## Decoding paths (chosen at open, automatic fallback)

`VideoPlayer::open` decodes the first frame on the caller's thread with each candidate in
turn, then hands the decoder to its thread. Candidates for `--hwdec auto`:

1. **Zero copy, Vulkan Video on wgpu's own device** (`ffmpeg::vulkan`). When a video is
   loaded, lumen-render creates the device through wgpu-hal (`video_device`) with
   `VK_KHR_video_queue`/`video_decode_queue`/`decode_{h264,h265,av1}` (+ `video_maintenance1`,
   `push_descriptor`), `synchronization2`, and one extra queue in a decode, a compute and a
   transfer family (never wgpu's family). That `VkDevice` is handed to FFmpeg as a
   user-provided `AVVulkanDeviceContext`; wgpu's family is listed with the graphics flag only,
   so FFmpeg never submits to it but includes it in the images' concurrent sharing. Each
   image of FFmpeg's pool is wrapped once as an NV12 wgpu texture (`texture_from_raw` +
   `create_texture_from_hal`, plane views `R8Unorm`/`Rg8Unorm`). Per displayed frame:
   lock the `AVVkFrame`, wait on its timeline semaphore (wgpu-hal `add_wait_semaphore`), a
   raw-Vulkan command buffer moves the image to `SHADER_READ_ONLY_OPTIMAL` (the state wgpu
   tracks; wgpu forbids mixing raw and wgpu commands in one encoder, so it is a separate
   command buffer of the same submission), signal value + 1 after the conversion and record
   layout/value in the `AVVkFrame`, so FFmpeg's next use of the image waits for our read.
   Works on NVIDIA (proprietary 595) and AMD (RADV, iGPU) here; needs Vulkan 1.3, the
   decode extensions and separate compute/transfer families. NV12 only (other formats,
   e.g. 10-bit P010, use the download path of the same decoder).
2. **Hardware decoder of the rendering GPU, frames copied** (`ffmpeg::hw`): NVIDIA → CUDA
   (NVDEC), then FFmpeg's own Vulkan device; AMD/Intel → VA-API on the render node of the
   rendering GPU (PCI bus id, then vendor), then Vulkan; Windows → D3D11VA. Decoded frames
   are downloaded (`av_hwframe_transfer_data`) into pooled frames, then uploaded.
3. **Software** (libavcodec frame + slice threads), planes uploaded.

`--hwdec nvdec-copy` = CUDA then software; `--hwdec no` = software. A decoder that silently
falls back to software (`get_format` without the hardware format) counts as a failure of its
candidate. `LUMEN_VIDEO_DECODERS=zerocopy,cuda,vulkan,vaapi,d3d11va` overrides the order of
`auto` (measurements); `LUMEN_FFMPEG_LOG=warning|info|verbose|debug` raises FFmpeg's log level
(errors only by default).

DMA-BUF import (VA-API → `texture_from_dmabuf_fd`) was not needed: FFmpeg's Vulkan decoder
gives true zero copy on the same device with AMD's RADV too, without per-frame imports; wgpu
30's DMA-BUF import is also single-plane only.

## YUV → RGB

`lumen_render::yuv`: 8-bit planes sampled as UNORM, 9–16-bit planes as `R16Uint`/`Rg16Uint`
with manual bilinear filtering (no optional 16-bit-norm feature). One 3×4 matrix per stream
folds range (limited/full, scaled to the bit depth), container (MSB-aligned P010 or
LSB-aligned), and matrix (BT.601/709/2020/240M/FCC). Unspecified tags follow player
conventions: BT.709 for ≥1280 wide or >576 high, else BT.601; limited range except `yuvj*`;
MPEG-2 "left" chroma siting (all sitings of `AVChromaLocation` are honoured). The decoder's
padding (coded 3840×2176 for 2160p) is cropped. Formats the converter cannot read (packed,
RGB, gray, big-endian) are converted to 8-bit 4:4:4 by libswscale (warning logged once).

## Behaviour (spec 07 §14)

- Infinite loop: at the end of the file the decoder is drained (last frames shown), flushed,
  the demuxer seeks back; `LoopTimeline` keeps a continuous media time (the first frame of a
  loop follows the last one by one frame duration). Measured: 8 loops in 118 s, 0 late frame.
- Cadence = video frame rate capped to `--fps`: the decoding thread keeps only the frames
  that a display instant `k / fps` would show (one-frame lookahead), so a 60 fps video at 30
  drops every other frame before any copy; the engine sleeps until the next queued frame is
  due (`Wait::Until`) or until the decoder signals a frame landing in an empty queue.
- Memory bound: 3 queued frames (copy paths: 12 MiB each in 4K NV12, buffers recycled;
  zero copy: references to GPU images, `extra_hw_frames = 6`).
- Pause (all outputs of the video paused): the clock stops, the decoder blocks on its full
  queue (no timer, no wake-up), the sound track pauses; on resume playback continues where
  it stopped. Decoding slower than real time resynchronises the clock (plays slower instead
  of bursting).
- Sound: the engine memory-maps the file and plays its audio track in a loop through
  `lumen-audio` (symphonia, `isomp4`/AAC): automute and `--silent` apply; parity with the
  reference's cubic volume curve, gain ≈ (v/128)³ (QO-15, `// parity:` in lumen-engine).
- In-memory input (`VideoInput::Memory`, custom `AVIOContext`, no copy) for videos embedded
  in `.tex` textures (§14.2); tested on the reference MP4 (identical frames).

## Measurements (RTX 4080 SUPER, Vulkan; Ubuntu GNOME Wayland through XWayland; reference wallpaper 3655057993: H.264 Main 3840×2160 60 fps 50 Mb/s BT.709 limited; `--fps 30 --silent`; one 2560×1440 output visible)

| Path | Process CPU (% of one core) | RSS | VRAM (nvidia-smi) | Render thread |
|---|---|---|---|---|
| **Zero copy** (default here) | **2.3–2.6 %** (render 1.2 %, decoding 1.1 %) | 325 MiB (97 MiB anonymous) | 318 MiB | 0.30–0.36 ms/frame, GPU 0.11 ms/frame |
| NVDEC + copy (CUDA) | 9.2 % (render 4.3 %, decoding 3.9 %, CUDA thread 1 %) | 651 MiB | 651 MiB | 1.25 ms/frame (0.9 ms plane upload) |
| FFmpeg Vulkan device + copy | 15.3 % (decoding 9.5 %) | 434 MiB | — | — |
| Software (offline benchmark) | ≈ 170 % (41 s CPU for 24 s of video) | 712 MiB peak | — | 2.25 ms/frame upload |

- Paused by a fullscreen window: zero copy 0.00 % CPU, no wake-up of the engine loop or of
  the decoding thread (the remaining ~250 wake-ups/s are NVIDIA driver threads, present for
  scenes too). With CUDA, its event thread keeps polling (~200 wake-ups/s, 0.5 % CPU) — one
  more reason to prefer zero copy.
- Offline throughput (decode + convert as fast as possible): zero copy and NVDEC 4.0× real
  time (decoder-bound), FFmpeg Vulkan + copy 3.8×, software 2.6×.
- Open time: 65–81 ms (zero copy), 200–240 ms (CUDA context creation).
- Colors: PSNR 58.5 dB against libswscale's accurate BT.709 limited → RGB conversion of the
  same frame (identical for zero copy and copy paths); AMD RADV zero copy 57.5 dB.

## Build and test

| Feature | Default | Notes |
|---|---|---|
| `ffmpeg` | off | lumen-engine enables it on Linux (FFmpeg 8 dev files + libclang for bindgen; CI's setup action installs them). Without it `VideoPlayer::open` returns `VideoError::Unsupported` and video wallpapers are refused with a clear message. |
| lumen-engine `video-ffmpeg` | off | Windows: an FFmpeg 8 shared build; `FFMPEG_DIR` (include + lib). |

- `cargo test -p lumen-video --features ffmpeg`: pacing (loop timeline, decimation, clock),
  queue, decoder selection, pixel-format layouts; lumen-render: conversion matrices
  (BT.709/601, limited/full, P010 vs LSB 10-bit), layouts, queue-family selection.
- `LUMEN_GPU_TESTS=1 LUMEN_WE_WORKSHOP=… cargo test -p lumen-video --features ffmpeg --release -- --ignored gpu`:
  the hardware (zero copy / CUDA / Vulkan copy, via `LUMEN_VIDEO_DECODERS`) and software
  paths give the same frame (mean difference < 0.5/255), the in-memory input is identical,
  no color cast; the loop continues the timeline and the 30 fps cap skips every other frame.
- `cargo run -p lumen-video --features ffmpeg --release --example video_png -- --times 0,6 FILE`
  (PNG to `target/agent-video/shots`), `--bench SECONDS` for throughput, `--low-power` for
  the integrated GPU. Through the engine:
  `cargo run -p lumen-engine --release --example render_png -- --scaling fill 3655057993`.
- Windows: `cargo check/clippy --target x86_64-pc-windows-gnu` pass without FFmpeg, and with
  `--features lumen-engine/video-ffmpeg` when `FFMPEG_DIR` points at FFmpeg 8 headers. Never
  run on Windows.

## Deviations and decisions

- No mpv/libmpv: the reference plays videos through a player library rendering to an RGBA8
  FBO; Lumen decodes with FFmpeg and converts in its own shader (same visible result, no GL
  bridge, zero copy).
- Volume: cubic curve reproduced (QO-15, rule 3). Video audio is played by the scene sound
  player rather than by the video player (same mixing rules, same automute).
- Frames that would never be shown at the fps cap are dropped before any copy (rule 1); the
  reference's player renders at video rate.
- Display size follows the sample aspect ratio; rotation metadata is ignored.

## Limitations / not done

- Video textures inside scenes (§14.2): the decoding side is ready (`VideoInput::Memory`,
  `lumen-formats` exposes `GpuContent::Video`), but lumen-engine still skips them
  (`texture.rs`): binding the player's output to a scene texture slot (copy the RGBA output
  into the texture or bind it directly, start/stop with the use count, always muted) needs
  changes in `scene.rs`, which other work streams are modifying, and no installed scene uses
  one to test against.
- HDR (PQ/HLG) is not tone-mapped and BT.2020 primaries are not converted to BT.709 (matrix
  only). Zero copy is NV12 only (10-bit streams go through the download path).
- Audio and video loops are independent: a sound track a few ms longer than the picture
  drifts slowly over many loops (the reference wallpaper's track is 7 ms longer).
- Zero copy needs Vulkan (Linux, and Windows when wgpu picks Vulkan); on DX12 Windows uses
  D3D11VA with a copy (D3D11 shared-texture import not written, never tested on Windows).
- Untested here: VA-API copy path (the AMD iGPU decodes zero copy through RADV instead),
  Intel GPUs, Windows, Wayland layer-shell compositors.

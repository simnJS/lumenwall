# lumenwall-video

Video wallpapers: FFmpeg demuxing and decoding on a dedicated thread, a small bounded queue
of frames, and YUV → RGBA conversion on the GPU (`lumenwall_render::yuv`) into the wallpaper
image that the compositor scales to the outputs (fill/fit/stretch/default, clamp modes).

## Sources (clean room)

- `specs/07-plateforme-audio-video-web.md` §2.2/§2.4 (`--hwdec`, `--fps`, `--volume`,
  `--silent`), §4.5 (wallpaper size = display size of the stream, scaling), §8 (cadence),
  §9.4 (pause), §12 (automute), §14 (video playback), QO-15, QO-16.
- `specs/01-formats-binaires.md` §2.10.2 (videos embedded in `.tex`, read from memory).
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/research/platform.md`, `deny.toml`.
- Public documentation: FFmpeg 8 headers and API docs (`hwcontext_vulkan.h`, `avcodec.h`),
  FFmpeg 8.0 library sources read only to learn the API contract of a caller-provided
  Vulkan device (queue families, frame locking, semaphores), FFmpeg's `-init_hw_device`
  documentation (D3D11VA device = DXGI adapter index), Vulkan specification, wgpu /
  wgpu-hal 30 docs, Win32 documentation (`LoadLibraryExW` search flags, `dxgi.h`).
  Reference wallpaper 3655057993 used as test input only.

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
Lumenwall uses the raw `ffmpeg-sys-next` crate alone (not the `ffmpeg-next` wrapper), with RAII
wrappers of its own. FFmpeg stays a separate shared library; `build.rs` also rejects
`FFMPEG_STATIC`/`PKG_CONFIG_ALL_STATIC`. The WTFPL exception was already prepared in
`deny.toml` (legal confirmation still pending there).

**Loaded at the first video (Linux, Windows).** FFmpeg's libraries pull ≈ 135 other libraries;
linked to the executable they were mapped at every start, scenes included (24.9 MiB RSS,
12–18 ms of start-up). `ffmpeg::sys` re-exports the types and constants of `ffmpeg-sys-next`
and replaces its 40 functions with wrappers over a table filled on the first call of
`load_decoder` (`VideoPlayer::open`, and lumenwall-engine's support check of a video
wallpaper), for the majors of the headers the bindings come from, never closed:

- Linux: `dlopen`/`dlsym` (libc, no new crate) of `libavutil.so.60`, `libswscale.so.9`,
  `libavcodec.so.62`, `libavformat.so.62`. The executable has no `DT_NEEDED` on FFmpeg
  (`readelf -d lumenwall`: libasound, libpulse, libgcc_s, libm, libc). Loading takes 13–15 ms.
- Windows: `LoadLibraryExW`/`GetProcAddress` (kernel32, declared by hand: no new crate) of
  `avutil-60.dll`, `swscale-9.dll`, `avcodec-62.dll`, `avformat-62.dll` by full path, from
  the first folder holding all four: the executable's (the installer's layout), then the
  absolute folders of `PATH` (`cargo test`/examples, an FFmpeg installed elsewhere), never the
  current directory. `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`:
  their own dependencies (`swresample-6.dll`, system DLLs) come from the same folder, then
  System32. The import libraries are linked but no function is referenced, so `lumenwall.exe`
  has no import entry on FFmpeg (`llvm-readobj --coff-imports`; CI starts it with only the
  Windows folders on `PATH`). Loading takes 1.5–2 ms at the first video with Lumenwall's
  build (10.4 MiB of DLLs, warm file cache; 6–8 ms with BtbN's 128 MiB).

The executable therefore starts and plays scenes where FFmpeg is missing; video wallpapers then
fail with the loader's message (Windows: `the FFmpeg 8 DLLs (avutil-60.dll, swscale-9.dll,
avcodec-62.dll, avformat-62.dll) are neither next to the executable (…) nor in a folder of
PATH`, test `missing_ffmpeg`). The wrappers have the bindings' signatures (checked at compile
time by a test) and a test fails if a call bypasses the table.

## Decoding paths (chosen at open, automatic fallback)

`VideoPlayer::open` decodes the first frame on the caller's thread with each candidate in
turn, then hands the decoder to its thread. Candidates for `--hwdec auto`:

1. **Zero copy, Vulkan Video on wgpu's own device** (`ffmpeg::vulkan`). When a video is
   loaded, lumenwall-render creates the device through wgpu-hal (`video_device`) with
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
   e.g. 10-bit P010, use the download path of the same decoder). Also works on Windows 11
   when wgpu renders through Vulkan (lumenwall-render prefers it to DX12): NVIDIA driver
   621.01 and AMD's driver for a Radeon iGPU, with BtbN's FFmpeg 8.1 build (built with
   `--enable-vulkan`); NVIDIA again with Lumenwall's own build (below).
2. **Hardware decoder of the rendering GPU, frames copied** (`ffmpeg::hw`): NVIDIA → CUDA
   (NVDEC), then FFmpeg's own Vulkan device; AMD/Intel → VA-API on the render node of the
   rendering GPU (PCI bus id, then vendor), then Vulkan; Windows → D3D11VA on the DXGI
   adapter wgpu renders on (`select::pick_dxgi_adapter`: same PCI vendor and device ids,
   else same vendor; FFmpeg's default adapter when wgpu renders on WARP), enumerated with
   raw COM calls (`ffmpeg::dxgi`), except on NVIDIA where CUDA comes first (FFmpeg's D3D11
   download costs ≈ 3× NVDEC's copy there, see the measurements) and D3D11VA second.
   Decoded frames are downloaded (`av_hwframe_transfer_data`) into pooled frames, then
   uploaded.
3. **Software** (libavcodec frame + slice threads), planes uploaded.

`--hwdec nvdec-copy` = CUDA then software; `--hwdec no` = software. A decoder that silently
falls back to software (`get_format` without the hardware format) counts as a failure of its
candidate. `LUMENWALL_VIDEO_DECODERS=zerocopy,cuda,vulkan,vaapi,d3d11va` overrides the order of
`auto` (measurements); `LUMENWALL_FFMPEG_LOG=warning|info|verbose|debug` raises FFmpeg's log level
(errors only by default). The tests and `video_png` honour `WGPU_BACKEND=vulkan|dx12`.

The chosen path is logged at `info` when a video opens, e.g. `video: D3D11VA decoder on device
0 (NVIDIA GeForce RTX 4080 SUPER)` and `video '…': h264 3840x2160 (d3d11 (nv12)), … D3D11VA
hardware decoding, copy through system memory`.

DMA-BUF import (VA-API → `texture_from_dmabuf_fd`) was not needed: FFmpeg's Vulkan decoder
gives true zero copy on the same device with AMD's RADV too, without per-frame imports; wgpu
30's DMA-BUF import is also single-plane only.

## YUV → RGB

`lumenwall_render::yuv`: 8-bit planes sampled as UNORM, 9–16-bit planes as `R16Uint`/`Rg16Uint`
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
  `lumenwall-audio` (symphonia, `isomp4`/AAC): automute and `--silent` apply; parity with the
  reference's cubic volume curve, gain ≈ (v/128)³ (QO-15, `// parity:` in lumenwall-engine).
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
- Open time: 65–81 ms (zero copy), 200–240 ms (CUDA context creation); the first video of a
  process also loads FFmpeg (13–15 ms).
- Colors: PSNR 58.5 dB against libswscale's accurate BT.709 limited → RGB conversion of the
  same frame (identical for zero copy and copy paths); AMD RADV zero copy 57.5 dB.

### Windows 11 (RTX 4080 SUPER, driver 621.01, and an AMD Radeon integrated GPU; BtbN FFmpeg 8.1.3 LGPL; same video)

`lumenwall.exe` (release, `--silent -w 40x40x640x360`, default 30 fps cap, Vulkan), steady
state over 10 s after 4 s of warm-up:

| Path | Process CPU (% of one core) | Working set | Private bytes |
|---|---|---|---|
| **Zero copy** (default here) | **3.0 %** | 233 MiB | 459 MiB |
| D3D11VA + copy (`LUMENWALL_VIDEO_DECODERS=d3d11va`) | 38.9 % | 334 MiB | 705 MiB |

`video_png --bench 12 --fps 30` (dev profile, dependencies optimised), process CPU time for
12 s of video, as % of one core at real time; throughput in parentheses:

| Path | RTX 4080 SUPER | AMD iGPU (`--low-power`) |
|---|---|---|
| Zero copy (Vulkan) | 6.2 % (4.3×) | 8.2 % (3.7×) |
| NVDEC (CUDA) + copy | 12.9 % (3.8×) | — |
| D3D11VA + copy, wgpu on Vulkan | 38.0 % (3.5×) | 13.7 % (2.8×) |
| D3D11VA + copy, wgpu on DX12 | 35.7 % (3.4×) | 13.2 % (3.1×) |
| Software | 140.9 % (7.8×) | — |

The plane upload costs the same on every copy path (1.3–1.5 ms per 4K frame), so NVIDIA's
extra D3D11VA cost is in FFmpeg's D3D11 download (staging copy + `Map`), hence CUDA first on
NVIDIA. Every hardware path gives the software frame (GPU test, mean difference < 0.5/255),
on Vulkan and on DX12 (`WGPU_BACKEND=dx12`).

#### With Lumenwall's FFmpeg build

Checked on the same PC (RTX 4080 SUPER, driver 621.01) with the five DLLs of
`packaging/ffmpeg/build.sh` (built in MSYS2 with gcc 16.2; releases use Ubuntu's gcc 13)
next to release builds of `lumenwall.exe` and `video_png`, headless (`video_png --times
0,1.5`, `LUMENWALL_VIDEO_DECODERS` to force each path; the loader's debug log names the
folder the DLLs came from):

| Video | Zero copy | D3D11VA | CUDA | FFmpeg Vulkan + copy | Software |
|---|---|---|---|---|---|
| 3655057993 (H.264 Main 4K 60 fps + AAC) | ✓ | ✓ (wgpu on Vulkan and DX12) | ✓ | ✓ | ✓ |
| H.264 + AAC, `.mp4` | ✓ | ✓ | ✓ | — | ✓ |
| HEVC Main + AAC, `.mp4` | ✓ | ✓ | ✓ | — | ✓ |
| HEVC Main 10, `.mkv` | P010: same decoder, copy | ✓ | ✓ | — | ✓ |
| VP9 + Opus, `.webm` | no VP9 in the zero-copy device: CUDA | ✓ | ✓ | — | ✓ |
| VP8 + Vorbis, `.webm` | CUDA | none in FFmpeg: software | ✓ | — | ✓ |
| AV1 (SVT-AV1), `.mp4` | ✓ | ✓ | ✓ | — | ✗ no software AV1 decoder in this build |

— not run. The clips are 2 s of `testsrc2` (FFmpeg encoders of the developer's tools;
CI makes the same ones but AV1 with Ubuntu's, `packaging/ffmpeg/test-clips.sh`, and
decodes them in software on WARP). For 3655057993,
the frame at 1.5 s is byte-identical on all six paths; the GPU tests pass
on Vulkan and on DX12 (`gpu_decode_paths_agree`, `gpu_loop_continues_the_timeline`), and
the unit tests of lumenwall-video and lumenwall-engine with the feature on. Through the
engine (`render_png 3655057993`, headless): zero copy, D3D11VA and software. On the AMD
Radeon integrated GPU (`video_png --low-power`): zero copy and D3D11VA. `lumenwall.exe
--version` starts with only the Windows folders on `PATH`.

## Build and test

| Feature | Default | Notes |
|---|---|---|
| `ffmpeg` | off | lumenwall-engine enables it on Linux (FFmpeg 8 dev files + libclang for bindgen; CI's setup action installs them). Without it `VideoPlayer::open` returns `VideoError::Unsupported` and video wallpapers are refused with a clear message. |
| lumenwall-engine `video-ffmpeg` | off | Windows: an FFmpeg 8 shared build; `FFMPEG_DIR` (include + lib), libclang. Release builds turn it on. |

- `cargo test -p lumenwall-video --features ffmpeg`: pacing (loop timeline, decimation, clock),
  queue, decoder selection, pixel-format layouts; lumenwall-render: conversion matrices
  (BT.709/601, limited/full, P010 vs LSB 10-bit), layouts, queue-family selection.
- `LUMENWALL_GPU_TESTS=1 LUMENWALL_WE_WORKSHOP=… cargo test -p lumenwall-video --features ffmpeg --release -- --ignored gpu`:
  the hardware (zero copy / CUDA / Vulkan copy, via `LUMENWALL_VIDEO_DECODERS`) and software
  paths give the same frame (mean difference < 0.5/255), the in-memory input is identical,
  no color cast; the loop continues the timeline and the 30 fps cap skips every other frame.
- `cargo run -p lumenwall-video --features ffmpeg --release --example video_png -- --times 0,6 FILE`
  (PNG to `target/agent-video/shots`), `--bench SECONDS` for throughput, `--low-power` for
  the integrated GPU. Through the engine:
  `cargo run -p lumenwall-engine --release --example render_png -- --scaling fill 3655057993`.
### Windows

`ffmpeg-sys-next` generates its bindings from FFmpeg's headers (bindgen, libclang) and links
the import libraries, so the feature needs an FFmpeg 8 SDK at build time; the workspace check
without it builds and tests everything else. Development, CI and releases use the same
build, Lumenwall's minimal LGPL FFmpeg 8.1.3 (never a GPL or static one), kept outside the
repository: [`packaging/ffmpeg/build.sh`](../../packaging/ffmpeg/README.md) on Linux or in
MSYS2's MINGW64 shell, or the `ffmpeg-win64` artifact of a CI run.

- Source: <https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz>, SHA-256
  `7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3`, unmodified.
- Contents: `bin/` (the five DLLs, nothing else: no `ffmpeg.exe`), `include/`, `lib/`
  (`avcodec.lib`… MSVC import libraries), `licenses/`, `BUILD-INFO.txt`.
- Inside: demuxers `mov` and `matroska`, decoders H.264, HEVC, VP8, VP9 (software and
  hardware) and AV1 (hardware only: no software AV1 decoder), D3D11VA, CUDA/NVDEC and
  Vulkan Video hardware decoding, the usual sound decoders (the full list and why:
  `packaging/ffmpeg/README.md`).

```powershell
# Built in %LOCALAPPDATA%\lumenwall-dev\ffmpeg\lumenwall-8.1.3 (packaging/ffmpeg/README.md)
$env:FFMPEG_DIR = "$env:LOCALAPPDATA\lumenwall-dev\ffmpeg\lumenwall-8.1.3"
$env:PATH = "$env:FFMPEG_DIR\bin;$env:PATH"          # DLLs for tests and examples
$env:CARGO_TARGET_DIR = "target/agent-<name>"
cargo test -p lumenwall-video --features ffmpeg
$env:LUMENWALL_GPU_TESTS = "1"; $env:LUMENWALL_WE_WORKSHOP = "C:\Program Files (x86)\Steam\steamapps\workshop\content\431960"
cargo test -p lumenwall-video --features ffmpeg -- --ignored gpu      # zero copy (Vulkan)
$env:WGPU_BACKEND = "dx12"; cargo test -p lumenwall-video --features ffmpeg -- --ignored gpu   # D3D11VA copy
cargo build -p lumenwall --release --features lumenwall-engine/video-ffmpeg
```

- libclang: bindgen finds `C:\Program Files\LLVM\bin\libclang.dll`; elsewhere set
  `LIBCLANG_PATH`. `ffmpeg-sys-next`'s build script also compiles a small C probe with MSVC.
- sccache as `rustc-wrapper` fails on `ffmpeg-sys-next` (its build script emits ≈ 1 400
  `--cfg` flags; sccache re-spawns rustc with a 40 000-character command line, over
  Windows' 32 767-character limit: os error 206): build the feature with `RUSTC_WRAPPER=`
  (empty).
- CI: job `FFmpeg (Windows DLLs)` builds it, job `video (Windows, FFmpeg)` uses it
  (`docs/CI.md`).

### Windows distribution (for the release workflow and the installer)

- Build: the `ffmpeg` job (`packaging/ffmpeg/build.sh`), then the setup action with
  `ffmpeg: "true"` and
  `cargo build --workspace --release --locked --target x86_64-pc-windows-msvc --features lumenwall-engine/video-ffmpeg`
  (no sccache; `FFMPEG_STATIC`/`PKG_CONFIG_ALL_STATIC` are rejected by `build.rs`). The
  third-party notices must be generated with the same feature, so that `ffmpeg-sys-next`
  (WTFPL) is listed.
- Ship next to `lumenwall.exe`, from the build's `bin/`: `avutil-60.dll`,
  `swresample-6.dll` (needed by avcodec), `swscale-9.dll`, `avcodec-62.dll`,
  `avformat-62.dll` (10.9 MB, measured on a local build: `packaging/ffmpeg/README.md`). BtbN's `lgpl-shared` build, shipped until 0.1.0, was
  128.2 MB; `Lumenwall-Setup.exe` goes from about 182 MB to about 145 MB (estimate), the zip from 251 MB to about 202 MB.
- Licence: LGPL-3.0-or-later (`--enable-version3`, for the Apache-2.0 Vulkan headers
  compiled in). `package.py` ships the LGPLv3 text and the GPLv3 text it incorporates, the
  licences of the rest of what the DLLs contain (nv-codec-headers, Vulkan-Headers, the
  MinGW-w64 runtime) and the build's `BUILD-INFO.txt` under `licenses\`, and FFmpeg's
  section of `THIRD-PARTY-NOTICES.html`: the corresponding source is FFmpeg's release
  tarball (URL, SHA-256), unmodified, and `packaging/ffmpeg/build.sh` at the release's tag
  (`packaging/ffmpeg/README.md`, "Licences of what ships").
- The DLLs stay replaceable by the user (loaded by name at run time, never statically
  linked), which is what the LGPL asks of a combined work: any FFmpeg 8 shared build with
  the same names works (BtbN's included).

## Deviations and decisions

- No mpv/libmpv: the reference plays videos through a player library rendering to an RGBA8
  FBO; Lumenwall decodes with FFmpeg and converts in its own shader (same visible result, no GL
  bridge, zero copy).
- Volume: cubic curve reproduced (QO-15, rule 3). Video audio is played by the scene sound
  player rather than by the video player (same mixing rules, same automute).
- Frames that would never be shown at the fps cap are dropped before any copy (rule 1); the
  reference's player renders at video rate.
- Display size follows the sample aspect ratio; rotation metadata is ignored.

## Limitations / not done

- Video textures inside scenes (§14.2): the decoding side is ready (`VideoInput::Memory`,
  `lumenwall-formats` exposes `GpuContent::Video`), but lumenwall-engine still skips them
  (`texture.rs`): binding the player's output to a scene texture slot (copy the RGBA output
  into the texture or bind it directly, start/stop with the use count, always muted) needs
  changes in `scene.rs`, which other work streams are modifying, and no installed scene uses
  one to test against.
- HDR (PQ/HLG) is not tone-mapped and BT.2020 primaries are not converted to BT.709 (matrix
  only). Zero copy is NV12 only (10-bit streams go through the download path).
- Audio and video loops are independent: a sound track a few ms longer than the picture
  drifts slowly over many loops (the reference wallpaper's track is 7 ms longer).
- Zero copy needs Vulkan (Linux, and Windows when wgpu picks Vulkan, the default when the
  driver has it); on DX12 Windows copies through system memory (NVDEC on NVIDIA, D3D11VA
  elsewhere). Zero copy
  on DX12 (follow-up) would mirror the Vulkan path with FFmpeg's D3D12VA device
  (`AV_HWDEVICE_TYPE_D3D12VA`, FFmpeg ≥ 7) on wgpu's own `ID3D12Device`: lumenwall-render
  would expose the raw device and queue (wgpu-hal `as_hal::<Dx12>`), FFmpeg's
  `AVD3D12VAFrame` NV12 resources would be wrapped once each (`texture_from_raw` +
  `create_texture_from_hal`, plane views `R8Unorm`/`Rg8Unorm`, wgpu's `TEXTURE_FORMAT_NV12`),
  and each frame's fence would be waited on wgpu's queue (`ID3D12CommandQueue::Wait`) and
  signalled back after the conversion. The D3D11VA route (copy into a shared NT-handle
  texture opened by D3D12, keyed mutex or shared fence) also works but keeps a GPU copy.
- Untested here: VA-API copy path (the AMD iGPU decodes zero copy through RADV instead),
  Intel GPUs, Wayland layer-shell compositors, two GPUs of the same vendor on Windows
  (`pick_dxgi_adapter` is unit tested only for that case).

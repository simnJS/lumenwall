# lumenwall-web

Web wallpapers (`type: web`): a separate host process runs an offscreen Chromium (CEF), serves the wallpaper folder through a private scheme, injects the public Wallpaper Engine web API, and hands each frame to `lumenwall-render` as a GPU texture handle.

## Sources (clean room)

- `specs/07-plateforme-audio-video-web.md` §3.5 (ids, `general.properties`), §3.6, §8.2 (web frame rate), §9.4, §10.4 (web mouse), §15 (web), §21.1 (mouse test values), QO-17 to QO-21.
- `specs/02-modele-scene.md` §2.2 (project id), §3.2–§3.3 (property types, text overrides).
- Public Wallpaper Engine documentation (docs.wallpaperengine.io): web user properties, property listener, audio visualizer, media integration, FPS limiter, RGB hardware, debugging.
- Public docs/sources of the `cef` crate 156.3.0 (tauri-apps/cef-rs, bindings and its OSR example) and the CEF C API comments; `cpal` 0.18, `rtrb`, `rustix` 1.1, `windows-sys` 0.61; Win32 documentation (`CreateFileMappingW`/`MapViewOfFile`, `DuplicateHandle`, `CompareObjectHandles`), Vulkan `VK_KHR_external_memory_win32`, wgpu-hal 30 (`texture_from_d3d11_shared_handle`).
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md` (rule 4: the reference injects no JS API; Lumenwall implements the public contract), `docs/research/platform.md`.

## Wallpaper Engine web API coverage

| API (public WE docs) | Status | Notes |
|---|---|---|
| `window.wallpaperPropertyListener.applyUserProperties(props)` | Done | All properties once the page is ready, then only changed ones. `{ type, value }` per key; combos also get `text` (option label). Delivered as soon as the listener (and method) exists, so a late listener does not miss the initial call. |
| ↳ `color` | Done | String as authored (`"r g b"`, floats 0–1). |
| ↳ `slider` / `bool` / `combo` / `textinput` | Done | Number / boolean / option value as authored (string or number) / string. |
| ↳ `file` | Done | Path written so that `'file:///' + value` is a valid URL (Unix: leading `/` dropped; Windows: `\` → `/`). |
| ↳ `directory` | Done | Same path convention, plus `mode`. |
| `…applyGeneralProperties(props)` | Done | `{ fps }` at load and on change. |
| `…setPaused(isPaused)` | Done | On every pause/resume (fullscreen, hidden output…). Before the page is frozen. |
| `…userDirectoryFilesAddedOrChanged(name, files)` / `…Removed` | Done | `fetchall` directories: full listing at load, diff on property change and on `HostCommand::RescanDirectories`. Non-recursive; images: jpeg jpg png pnga bmp gif svg webp; videos: webm ogg ogv. |
| `window.wallpaperRequestRandomFileForProperty(name, cb)` | Done | `ondemand` directories; `cb(name, path)`. Not called when the directory has no matching file. |
| `window.wallpaperRegisterAudioListener(cb)` | Done | 128 values (0–63 left, 64–127 right, bass first, clamped ≥ 0), ~30 calls/s (`AudioPacer`), only while a listener is registered and the wallpaper is not paused. `null` unregisters. |
| `window.wallpaperRegisterMediaStatusListener` | Done | `{ enabled }` |
| `…MediaPropertiesListener` | Done | `{ title, artist, subTitle, albumTitle, albumArtist, genres, contentType }` |
| `…MediaThumbnailListener` | Done | `{ thumbnail (data:image/png;base64), primaryColor, secondaryColor, tertiaryColor, textColor, highContrastColor }`; `MediaEvent::thumbnail_from_png` derives the colours. |
| `…MediaPlaybackListener` | Done | `{ state }`; constants on both documented spellings: `wallpaperMediaIntegration.PLAYBACK_PLAYING` and `wallpaperMediaIntegration.playback.PLAYING`. |
| `…MediaTimelineListener` | Done | `{ position, duration }` (seconds). |
| Media replay | Done | A listener registered after an event immediately receives the latest one of its kind. |
| `window.wallpaperPluginListener.onPluginLoaded`, `window.wpPlugins.led`, `window.cue` (RGB hardware) | Not provided | No RGB plugin in Lumenwall: `onPluginLoaded` is never called, exactly what WE does when no plugin is loaded; pages that follow the docs skip their LED code. |
| Localization, display conditions | n/a | Editor-side features; no runtime API. |
| CEF devtools port | Done | `LaunchParams::devtools_port` (off by default). |

Messages are exchanged as small scripts calling `window.__lumenWE(message)` (host → page) and a native function captured and removed by the bridge (page → host). The bridge is evaluated in `OnContextCreated` of the main frame, before any page script.

## Architecture

```
lumenwall (main process)                         lumenwall-web-host (CEF browser process)        CEF subprocesses
  WebHost::spawn ── socketpair / pipes ──▶     reader thread ── post_task ──▶ UI thread     renderer: bridge injection,
   writer thread  ◀─ HostCommand (JSON) ─        BridgeSession (WE API state)                         V8 page→host function
   reader thread  ─▶ WebEvent + waker()          RenderHandler::OnAcceleratedPaint           GPU, network, …
   SharedFrame (dmabuf fds / NT handle)          IO thread: wp<id>:// files, file:// policy  (same executable, --type=…)
                                                 audio thread: page audio (cpal)
```

- The host is started only for a web wallpaper (`WebHost::spawn`, executable `lumenwall-web-host` next to `lumenwall`; `process::find_host_executable` also looks one folder up for Cargo examples and tests, and honours `LUMENWALL_WEB_HOST`). One host per web wallpaper; it exits when lumenwall closes the channel or sends `Shutdown`. Chromium puts a Unix socket under the temporary directory and aborts when its path exceeds 107 bytes: with a long `TMPDIR`, the host is started with `TMPDIR=/tmp`.
- Channel: envelope `[u32 len][u8 handle count][3×0][JSON]`. Linux: a `socketpair` passed as the host's stdin (the host moves it off fd 0 and points stdin at `/dev/null` so CEF children never inherit it); descriptors travel with `SCM_RIGHTS`. Windows: the host's stdin/stdout pipes; stdout is then redirected to stderr in the host.
- Nothing polls: both sides block on the channel; the main side calls the `waker` closure after queueing each `WebEvent` (hook it to calloop / `MsgWaitForMultipleObjectsEx`).
- Frames (`LaunchParams::transport`):
  - `FrameTransport::Gpu` (default): CEF paints into its own texture pool and lends one texture per frame for the duration of `OnAcceleratedPaint`. The host forwards it (Linux: DMA-BUF plane fds + offsets/strides/modifier; Windows: the shared NT handle value of a D3D11 texture without keyed mutex, which the main process duplicates out of the host with `DuplicateHandle` using its child process handle) and blocks until `FrameReleased` (100 ms timeout, then `FrameRevoked`). A buffer of the pool is recognised across frames so the consumer imports each buffer once: `DmaBufPlane::buffer_id` (device, inode) on Linux; on Windows every frame brings a new duplicate, compared with a kept one by `frame::same_object` (`CompareObjectHandles`).
  - `FrameTransport::Memory`: the browser is created without shared textures; `OnPaint`'s BGRA pixels are copied into one shared buffer (`cef_host::memory`: a `memfd` on Linux, an unnamed paging-file section on Windows, whose handle travels in `FrameInfo::handle` and is duplicated like a texture handle), sent as `SharedTexture::Memory` (`FrameInfo::memory`), mapped read-only by the main process with `MappedMemory` and uploaded. The fallback when the GPU texture cannot be imported, and the Windows path when the render device cannot import D3D11 textures (see below).
  - `SharedFrame` (`ipc::frame`) is the exchange struct; the consumer imports or maps, copies into a texture it owns, waits for the copy, then `release()` — immediately on wake-up, not at the next vsync, because CEF's UI thread waits meanwhile.
- Frame rate: `FramePacing::Internal` (what lumenwall-engine uses) — CEF's scheduler at `windowless_frame_rate = fps`: Chromium only produces a frame when the page changed, so a static page costs no frame and no wake-up; `External` — the main process sends `BeginFrame` for each frame it wants (it would have to tick at the fps even for a static page). No 60 fps floor; `applyGeneralProperties({ fps })` tells the page.
- Pause: `setPaused(true)` to the page, then `WasHidden(true)` (painting stops), DevTools `Page.setWebLifecycleState frozen` (timers and script stop), audio stream paused. Resume reverses it and invalidates the view.
- Audio (QO-21 fixed): an `AudioHandler` takes the page's audio; the host plays it through cpal with `AudioOutput { volume, muted }` from lumenwall (volume, `--silent`, automute, pause). The output stream is paused whenever nothing plays or the page is muted. If no output device is usable, CEF plays the audio itself and only mute applies (`SetAudioMuted`). The stream belongs to the host process: lumenwall's automute detector must ignore `WebEvent::Started { pid }` (the host PID).
- Files: `wp<id>://root/<file>` (scheme standard, secure, CORS and fetch enabled). `FileService` resolves the URL path (host, query, fragment ignored; percent-decoded, raw path as fallback), refuses `..`, absolute/drive paths, NUL, and anything whose canonical path leaves the wallpaper folder (so outgoing symlinks are refused, inner ones work), then serves regular files with their MIME type, `Content-Length`, single `Range` requests (206/416), otherwise 404.
- `file://`: web security is disabled (the WE contract makes pages load user files through `file:///` URLs), but a request handler only lets `file://` reach the wallpaper folder, the files chosen in `file` properties and the folders chosen in `directory` properties (`access::LocalFileAccess`).

### Main process integration (`lumenwall-engine`, `src/web.rs`)

- `WebSlot::open`: `LaunchParams` from the wallpaper (id per spec 02 §2.2, folder, `file`, page size = the first output showing it, fps, `PropertySet::from_project_properties(general.properties)` with `--set-property` applied via `set_from_text`, `AudioOutput { volume: v/128, muted: --silent }`, `FramePacing::Internal`), `WebHost::spawn(find_host_executable(), …)` with the engine waker.
- On wake-up the engine drains `try_event()`: `Frame` → copy into an RGBA8 render target it owns (GPU texture imported once per pool buffer — Linux: wgpu-hal `texture_from_dmabuf_fd`; Windows: wgpu-hal `texture_from_d3d11_shared_handle`, Vulkan `VK_KHR_external_memory_win32` with handle type `D3D11_TEXTURE` — then one draw that crops to the visible rectangle; or `write_texture` from the mapped memory), wait for that submission only, `release()`; the outputs showing the page are then redrawn and compose the target like a scene (fill/fit/stretch). The host starts with `FrameTransport::Gpu` only when the render device can import (Vulkan with `VULKAN_EXTERNAL_MEMORY_DMA_BUF` on Linux, `VULKAN_EXTERNAL_MEMORY_WIN32` on Windows; no DX12 import is written), otherwise with `FrameTransport::Memory`. A failed import restarts the host with `FrameTransport::Memory`.
- Windows zero-copy status: `lumenwall-render` requests `VULKAN_EXTERNAL_MEMORY_WIN32` under `#[cfg(windows)]` in `GpuContext::from_adapter` (`crates/lumenwall-render/src/context.rs`, added by commit d337a2d; wgpu keeps it only when the adapter supports it). `FrameCopier::new` (`crates/lumenwall-engine/src/web.rs`) starts the host with `FrameTransport::Gpu` when the backend is Vulkan and the device has the feature, so on Windows frames go through the D3D11 shared-handle import. Otherwise (another backend such as DX12, or an adapter without the extension, as the WARP software adapter) it logs "no D3D11 shared texture import on this device, shared-memory copies instead" and uses `FrameTransport::Memory`, as `LUMENWALL_WEB_FRAMES=memory` does. The engine does not pass `--use-adapter-luid` to the host yet: on a machine with several GPUs, if Chromium's GPU process paints on another adapter than the render device, the import fails and the host restarts with shared-memory frames. Chromium's `--use-adapter-luid=<HighPart>,<LowPart>` (decimal) given through `WebHost::spawn`'s `extra_args` would select the adapter (see Measurements). `AudioListener(on)` → the spectrum is sent with `audio::audio_frame` when `AudioPacer::should_send` says so, and the engine keeps stepping while a listener exists. An unexpected host exit restarts it (3 times).
- Mouse: platform pointer events → `mouse::web_position` (identity scaling) or the composition window (scaled page) → `MouseTracker` → `HostCommand::Mouse`. Pause: `SetPaused` when every output showing the page is paused (platform or user). `Resize`, `SetFps`, `SetAudio`, `Properties` (only changed values) on change; hot switching scene ↔ web goes through the engine's plan reload (IPC `apply`).

## Build

| Configuration | Command (`CARGO_TARGET_DIR=target/agent-web`) | Status |
|---|---|---|
| default (no CEF) | `cargo test -p lumenwall-web`, `cargo clippy -p lumenwall-web --all-targets -- -D warnings` | built, 40 unit + 12 JS bridge tests + process test pass |
| `cef` (Linux) | see below | **built and run** on Ubuntu 26.04, NVIDIA RTX 4080 SUPER (proprietary driver), GNOME Wayland; clippy `-D warnings` clean |
| Windows, no CEF | `cargo test -p lumenwall-web`, `cargo clippy -p lumenwall-web --all-targets -- -D warnings` (native MSVC) | built and run on Windows 11; `host_process` runs there too (handle duplication, `same_object`, section mapping) |
| `cef-typecheck` | `cargo clippy -p lumenwall-web --features cef-typecheck [--target …] -- -D warnings` | passes on Linux and for Windows: the CEF host is type-checked against the `cef` 156.3.0 bindings in docs.rs mode, without download; cannot link. From Windows, `--target x86_64-unknown-linux-gnu` needs `PKG_CONFIG_ALLOW_CROSS=1` and a `PKG_CONFIG` stub (cpal's `alsa-sys` probes pkg-config even for `clippy`) |
| `cef` for Windows | see "Building the CEF host on Windows" | **built and run** on Windows 11, MSVC 14.44 (VS 2022 Build Tools), CMake 4.4.4, Ninja 1.12.1 from the Build Tools, RTX 4080 SUPER + AMD Radeon iGPU; with the distribution cached, `cargo build --release -p lumenwall-web --features cef --bin lumenwall-web-host` takes 87 s and `cargo build -p lumenwall-web --features cef` (debug) 102 s, each from an empty target directory (the first build also downloads and extracts the 175 MB archive), `lumenwall-web-host.exe` 0.75 MB; clippy `--features cef --all-targets -D warnings` clean |

### Building the CEF host

The `cef` crate downloads the CEF binary distribution from its official index at build time: CEF 156.0.15 *minimal* for linux64 is **333 MB** (the archive stays next to the extracted copy, 1.9 GB in total; `libcef.so` alone is 1.5 GB). Point `CEF_PATH` at a cache folder outside the repository so every target directory reuses it (the crate creates `$CEF_PATH/156.0.15+…/cef_linux_x86_64`):

```sh
export CEF_PATH=$HOME/.cache/lumenwall/cef
# Linux needs cmake, ninja and a C++ compiler for libcef_dll_wrapper. Without a distribution
# package, ninja comes from PyPI: `uv venv ~/.cache/lumenwall/tools/venv && uv pip install ninja`,
# then put ~/.cache/lumenwall/tools/venv/bin in PATH.
cargo build --release -p lumenwall-web --features cef --bin lumenwall-web-host
```

Runtime layout (copied next to the binaries by the `cef-dll-sys` build script): `lumenwall-web-host` next to `lumenwall`, and next to it `libcef.so` / `libcef.dll`, `*.pak`, `icudtl.dat`, `v8_context_snapshot.bin`, `locales/`, and on Linux `libvulkan.so.1`, `libvk_swiftshader.so`, `vk_swiftshader_icd.json` (CEF 156 ships no `libEGL.so`/`libGLESv2.so`). On Linux the host is linked with `rpath=$ORIGIN` (build.rs). The sandbox is disabled (`no_sandbox`, spec 07 §15.1), so `chrome-sandbox` is not needed.

The host executable is also every CEF subprocess (renderer, GPU, utility): it recognises `--type=…`, registers the `wp<id>` scheme from `--lumenwall-scheme=` and injects the bridge in renderers. On Windows it is a GUI-subsystem executable (no console windows).

### Building the CEF host on Windows

The windows64 *minimal* distribution of CEF 156.0.15 is a **175 MB** archive (426 MB extracted; with the archive kept, 593 MB in `CEF_PATH`). The `cef-dll-sys` build script builds CEF's C++ wrapper with CMake and the **Ninja** generator, so it needs the MSVC environment and `ninja` in `PATH`; the Visual Studio 2022 Build Tools ship Ninja (component "C++ CMake tools for Windows"). CEF's wrapper object paths are long: `cl.exe` fails with `C1083: Cannot open compiler generated file: '': Invalid argument` when the target directory sits in a deep folder (a worktree plus `target\agent-<name>` is already too deep), so use a short `CARGO_TARGET_DIR`:

```bat
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
set "PATH=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;%PATH%"
set "CEF_PATH=%LOCALAPPDATA%\lumenwall-dev\cef"
set "CARGO_TARGET_DIR=%LOCALAPPDATA%\lumenwall-dev\target"
cargo build --release -p lumenwall-web --features cef --bin lumenwall-web-host
```

The build script copies the whole distribution next to the binaries (`target\release`), so `lumenwall-web-host.exe`, the engine examples (one folder up) and `lumenwall.exe` built in the same target directory find it. What a release must ship is listed under "Windows runtime files".

### Windows runtime files

Next to `lumenwall.exe` (the host is looked up next to it), measured with the 156.0.15 distribution; the host was run from a folder holding only the "required" rows (GPU and shared-memory frames, `locales\en-US.pak` only, French system locale: Chromium fell back to `en-US`).

| File | Size | Role |
|---|---|---|
| `lumenwall-web-host.exe` | 0.74 MiB | the host (also every CEF subprocess) — required |
| `libcef.dll` | 280.7 MiB | Chromium — required |
| `chrome_elf.dll` | 2.7 MiB | required by `libcef.dll` |
| `icudtl.dat` | 10.3 MiB | ICU data — required |
| `v8_context_snapshot.bin` | 0.7 MiB | V8 startup snapshot — required |
| `resources.pak`, `chrome_100_percent.pak`, `chrome_200_percent.pak` | 23.5 + 0.6 + 0.9 MiB | resources — required |
| `locales\*.pak` (220 files) | 51 MiB (`en-US.pak` 0.6 MiB) | UI locale; `en-US.pak` at least; the page's `navigator.language` follows the locale Chromium loads |
| `d3dcompiler_47.dll` | 4.5 MiB | HLSL compiler for ANGLE (WebGL on D3D11) — recommended |
| `vk_swiftshader.dll`, `vk_swiftshader_icd.json`, `vulkan-1.dll` | 5.1 + 0 + 0.9 MiB | software rendering when the GPU is unusable or blocklisted — recommended |
| `dxcompiler.dll` | 24.8 MiB | WebGPU on D3D12 only — optional |
| `CREDITS.html` | 5.2 MiB | third-party licences of Chromium/CEF — to ship with the binaries |
| `bootstrap.exe`, `bootstrapc.exe`, `libcef.lib`, `CMakeLists.txt`, `cmake\`, `include\`, `libcef_dll\`, `archive.json` | — | build-time only, not shipped |

Required + recommended + all locales + `CREDITS.html`: **385.7 MiB** on disk, **125.1 MiB** as `tar.xz` (LZMA2, what an NSIS/Inno Setup LZMA installer achieves); `dxcompiler.dll` adds 24.8 MiB raw. C runtime: CEF's C++ wrapper is linked statically (`/MT`) into the host, `libcef.dll` only imports `chrome_elf.dll`, and in the Windows release builds the C runtime is also linked statically (`-C target-feature=+crt-static` in `.github/workflows/release.yml`, commit 69e45b6, see `docs/RELEASING.md`), so the host imports no `VCRUNTIME140.dll` or `MSVCP` DLL and starts on a Windows without the Visual C++ redistributable; development builds (no `crt-static`, see `.cargo/config.toml`) still import `VCRUNTIME140.dll` like every Rust binary of the workspace. The sandbox is off (`no_sandbox`), so `bootstrap*.exe` are not needed.

Known limit: in Windows Sandbox (vGPU disabled, so the WARP software adapter) `lumenwall-web-host.exe` starts its GPU and utility processes, never a renderer, and exits with code 0 within seconds; the engine restarts it three times, then stops. It is identical with the static and with the shared C runtime. Try web wallpapers on a PC with a GPU; see `docs/RELEASING.md`.

Debugging: `LUMENWALL_WEB_CONSOLE=1` prints the page's console on standard error; `LUMENWALL_WEB_DEVTOOLS=<port>` opens Chromium's remote debugging on localhost (lumenwall-engine and the probe); `cargo run --release -p lumenwall-web --example host_probe -- <host> <wallpaper folder> [seconds]` runs the host alone and prints what it sends (`LUMENWALL_WEB_FRAMES=memory` for shared-memory frames, `LUMENWALL_PROBE_PAUSE=1` to pause in the middle).

## Tests

- `files`: MIME table sorted and lookups, 404 for missing files and directories, percent-decoding, case-insensitive fallback, traversal (`..`, `%2e%2e`, encoded slashes, backslashes, drive letters, NUL, absolute paths), outgoing symlinks (file and directory) refused, inner symlink served, ranges (206, suffix, open-ended, clamped end, multi-range → 200, 416).
- `bridge_js` (QuickJS via `rquickjs`, dev-dependency): the real `bridge.js` with the real `BridgeSession` in a loop: API installed and native channel hidden, initial properties delivered to a late listener / to a listener defined before the bridge / with top-level `var` / with methods added after assignment, deltas, fps, pause ordering, audio array shape, media replay and constants, random file, fetchall listing and removal, page exceptions isolated, reload.
- `session`, `properties`, `media`, `audio`, `mouse` (spec 07 §21.1 values), `access`, `ipc` unit tests; `ipc::unix` passes a descriptor and a 40 000-value message through a socket pair.
- `host_process` (no harness): the test binary spawns itself as a fake host through `WebHost::spawn` and exchanges Init → Started → Frame with a real descriptor (content checked) → release → shared-memory frame (mapped with `MappedMemory`, content checked) → release → event → Shutdown. Windows: the frame handle is duplicated out of the fake host (its object read back through the duplicate), the same object sent twice is recognised by `same_object`, then a paging-file section frame is mapped and checked.
- With CEF on Windows (manual, 3044696375): `host_probe` (GPU frames: D3D11 NT handles, 2 pool textures; `LUMENWALL_WEB_FRAMES=memory`: two alternating sections), pause (`LUMENWALL_PROBE_PAUSE=1`: 203 frames before the pause, none while paused), host killed → `Exited` at once and no CEF process left; `applyUserProperties` checked through DevTools (`LUMENWALL_WEB_DEVTOOLS`): a listener set by `Runtime.evaluate` after load received `{"schemecolor":{"type":"color","value":"0 0 0"}}` and `applyGeneralProperties({fps: 30})`, and `wallpaperRegisterAudioListener`, `wallpaperRegisterMediaStatusListener`, `wallpaperRequestRandomFileForProperty` are functions; `render_png` composes the page through the engine (shared-memory frames), and `lumenwall-engine`'s `tests/web_content.rs --ignored` passes with `LUMENWALL_WEB_HOST` pointing at the host. The D3D11 → Vulkan import of the engine was run in a throwaway program with the same calls on a device created with `VULKAN_EXTERNAL_MEMORY_WIN32` (see Measurements), because at that time `lumenwall-render` did not request that feature (it does since commit d337a2d).
- With CEF (`lumenwall-engine`'s `tests/web_content.rs`, `--ignored`, and the measurements below): DMA-BUF frames imported and composed, shared-memory frames, `wp<id>://` files (HTML, scripts, CSS images, `<base href=".">` routing), `applyUserProperties` (defaults and `--set-property`), `wallpaperRegisterAudioListener`, mouse moves, pause (no frame while paused, page resumes), `LoadFailed`-free loading of the three installed web wallpapers.
- Not tested: page audio output (none of the test wallpapers plays sound), clicks on a real desktop (the GNOME XWayland desktop windows receive none, spec 07 QO-8), `file://` user files; on Windows also mouse input, a windowed or desktop `lumenwall` run (only headless rendering), and the engine's zero-copy path itself, now that the render device requests the feature (only the import was run, in the throwaway program).

## Measurements

Ubuntu 26.04, Ryzen 9 7950X3D, RTX 4080 SUPER (proprietary driver, Vulkan), GNOME 50.1 Wayland (Lumenwall through XWayland), release build, 30 fps cap, `--silent`, one visible 1920×1080 window (`-w`, so that no covering window pauses the page). CPU = all processes of the instance (lumenwall, host, renderer, GPU, utility, zygotes) over 10 s, in % of one core; PSS summed; VRAM per process from `nvidia-smi pmon`.

| Wallpaper | Running | Paused (IPC `set_paused`) | PSS | VRAM |
|---|---|---|---|---|
| 3044696375 clock (redraws when the clock or the typing changes, 14–29 fps) | 11.7 % (lumenwall 1.7, renderer 5.3, GPU process 3.7) | 0.5 % | 493 MiB (lumenwall 75) | 145 MiB |
| corsair_o_tron (audio listener, 30 fps) | 15.9 % | 0.5 % | 560 MiB | 248 MiB |
| corsair_collection (rainbow pulse, 30 fps) | 8.1 % | 0.6 % | 561 MiB | 305 MiB |
| *scene 3018340632 (hot-switched in the same window)* | *2.2 %* | *0.2 %* | *113 MiB* | *302 MiB* |
| clock, shared-memory frames (`LUMENWALL_WEB_FRAMES=memory`) | 20.1 % | 0.8 % | 525 MiB | 167 MiB |
| corsair_collection, shared-memory frames | 17.2 % | 0.3 % | 638 MiB | 269 MiB |

- Zero-copy path: first sight of a pool buffer (import) ≈ 1 ms (max 1.2 ms), then **0.33 ms per frame** for the copy draw and its wait (lumenwall's main thread, 1920×1080); 2–3 buffers per page, imported again after a pause (the browser rebuilds its pool when shown). Headless: 0.23–0.37 ms per frame. 0 revoked frames in a visible window.
- Shared-memory path: **3.9 ms per frame** (max 19 ms) in lumenwall plus the host's copy out of `OnPaint`: the whole instance costs about twice the CPU of the zero-copy path.
- Pause: the page is hidden and frozen, no frame, no wake-up in lumenwall; what remains is Chromium's idle work (≈ 0.3 % in the GPU process).
- A web page costs 4–7× the CPU of a comparable scene and ≈ 4× its memory: Chromium's renderer and GPU processes dominate; lumenwall's own share (import + copy + composition) is 1.7–3 % of one core at 30 fps.

Windows 11, RTX 4080 SUPER (Vulkan) + AMD Radeon iGPU, release build, 3044696375, audio muted, nothing on the desktop (headless or the host alone):

- Engine (`render_png --size 1920x1080 --frames 60 --fps 30`, shared-memory frames): first frame 3.5 s after the host started (cold CEF start), 90 frames copied, 0 revoked, **0.93 ms per frame** (max 5.2 ms) for the upload and copy, 57 of 60 composed frames showed a new page frame.
- D3D11 → Vulkan import (throwaway program, the engine's calls, 1280×720, 15 s): RTX 4080 SUPER — 314 frames, 2 imports for the whole run (CEF's pool, recognised with `same_object`), **0.54 ms per import, 0.35 ms per frame** (max 0.82 ms) for the copy draw and its wait. wgpu on the AMD iGPU while Chromium painted on the RTX: every import fails (`DeviceError::Unexpected`), nothing crashes — the engine's fallback case; with `--use-adapter-luid=0,71689` (the iGPU's LUID, decimal `HighPart,LowPart`) given to the host, Chromium painted on the iGPU and the imports succeeded (215 frames, 3 imports, 1.2 ms per import, 0.6 ms per frame). Hexadecimal values are not understood (Chromium stays on its default adapter).
- Host alone (`host_probe`, 1280×720, frames released without copy, all host processes + the probe over 10 s, % of one core, private bytes): GPU frames 6.2 %, 250 MiB; shared-memory frames 14.9 %, 245 MiB.

## Deviations

- QO-20: the public WE web API is implemented (parity rule 4); `--set-property` values reach the page (the reference passes none).
- QO-21: page audio follows `--volume`, `--silent`, automute and pause.
- §8.2: no `max(60, fps)` — the browser renders at most at the wallpaper's fps, and only when the page changed.
- §15.4: frames are shared on the GPU and copied once by the GPU (no BGRA upload per frame); the shared-memory upload is only the fallback.
- §9.4: pause freezes the page (hidden + lifecycle frozen + audio paused) instead of only not pumping it.
- §10.4: a mouse move is sent when the position changes, not every frame.
- §15.2: only the wallpaper folder is served (no assets folder, no current directory — the latter would expose arbitrary files); paths are percent-decoded with the raw path as fallback (QO-18); case-insensitive fallback for wallpapers authored on Windows; `Range` supported (QO-18); unknown types are `application/octet-stream` spelled correctly.
- §15.3: remote debugging is off unless `devtools_port` is set, and keeps Chromium's default origin checks (QO-19); `file://` is restricted to the wallpaper folder and user-chosen files/folders. Timers are throttled while paused (the page is hidden) — intentional for power.
- QO-17: without `profile_dir` the profile is a temporary folder removed on exit; with it, localStorage persists.
- Audio values for the page are clamped to ≥ 0 (spec 07 §13 can produce negative values).
- Page background is opaque white (browser default) instead of CEF's transparent offscreen default.
- Quoted `bool` and `slider` values in `project.json` (`"value": "true"`, `"150"`, shipped in corsair_o_tron) are read as Wallpaper Engine reads them.

## Open questions (for the spec writer)

- Numeric values of `wallpaperMediaIntegration.PLAYBACK_*` are not documented publicly; Lumenwall uses STOPPED 0, PLAYING 1, PAUSED 2.
- Exact `project.json` keys WE writes for `directory` properties (mode, file type): Lumenwall reads `mode` (`fetchall`/`ondemand`) and `filetype`/`fileType` (`video…` → videos, else images). Recursive listing or not?
- Does WE call `applyUserProperties` with every property object field (`text`, `order`, …) or only `{ type, value }`? Format of the thumbnail colours (Lumenwall: `#rrggbb`)?
- Does WE send `applyGeneralProperties` fields besides `fps` (e.g. language)?
- Windows: CEF's GPU process and lumenwall's wgpu device must use the same adapter for the D3D11 import. Chromium's `--use-adapter-luid=<HighPart>,<LowPart>` (decimal) selects it (verified, see Measurements); passing the render adapter's LUID needs it from `lumenwall-render` (Vulkan `VkPhysicalDeviceIDProperties`), not done yet. No DX12 import is written (a DX12 device uses shared-memory frames).
- Linux: CEF's shared textures come from its GPU process on the same GPU as lumenwall (verified on NVIDIA, block-linear modifier `0x0300000000606014`); untested on Mesa drivers, multi-GPU machines and Wayland-only sessions without XWayland. CEF hands no fence with the DMA-BUF: the frame is read after `OnAcceleratedPaint`, which showed no tearing on NVIDIA.

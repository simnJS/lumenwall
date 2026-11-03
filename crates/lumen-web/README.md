# lumen-web

Web wallpapers (`type: web`): a separate host process runs an offscreen Chromium (CEF), serves the wallpaper folder through a private scheme, injects the public Wallpaper Engine web API, and hands each frame to `lumen-render` as a GPU texture handle.

## Sources (clean room)

- `specs/07-plateforme-audio-video-web.md` §3.5 (ids, `general.properties`), §3.6, §8.2 (web frame rate), §9.4, §10.4 (web mouse), §15 (web), §21.1 (mouse test values), QO-17 to QO-21.
- `specs/02-modele-scene.md` §2.2 (project id), §3.2–§3.3 (property types, text overrides).
- Public Wallpaper Engine documentation (docs.wallpaperengine.io): web user properties, property listener, audio visualizer, media integration, FPS limiter, RGB hardware, debugging.
- Public docs/sources of the `cef` crate 156.3.0 (tauri-apps/cef-rs, bindings and its OSR example) and the CEF C API comments; `cpal` 0.18, `rtrb`, `rustix` 1.1, `windows-sys` 0.61.
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md` (rule 4: the reference injects no JS API; Lumen implements the public contract), `docs/research/platform.md`.

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
| `window.wallpaperPluginListener.onPluginLoaded`, `window.wpPlugins.led`, `window.cue` (RGB hardware) | Not provided | No RGB plugin in Lumen: `onPluginLoaded` is never called, exactly what WE does when no plugin is loaded; pages that follow the docs skip their LED code. |
| Localization, display conditions | n/a | Editor-side features; no runtime API. |
| CEF devtools port | Done | `LaunchParams::devtools_port` (off by default). |

Messages are exchanged as small scripts calling `window.__lumenWE(message)` (host → page) and a native function captured and removed by the bridge (page → host). The bridge is evaluated in `OnContextCreated` of the main frame, before any page script.

## Architecture

```
lumen (main process)                         lumen-web-host (CEF browser process)        CEF subprocesses
  WebHost::spawn ── socketpair / pipes ──▶     reader thread ── post_task ──▶ UI thread     renderer: bridge injection,
   writer thread  ◀─ HostCommand (JSON) ─        BridgeSession (WE API state)                         V8 page→host function
   reader thread  ─▶ WebEvent + waker()          RenderHandler::OnAcceleratedPaint           GPU, network, …
   SharedFrame (dmabuf fds / NT handle)          IO thread: wp<id>:// files, file:// policy  (same executable, --type=…)
                                                 audio thread: page audio (cpal)
```

- The host is started only for a web wallpaper (`WebHost::spawn`, executable `lumen-web-host` next to `lumen`, see `process::default_host_executable`). One host per web wallpaper; it exits when lumen closes the channel or sends `Shutdown`.
- Channel: envelope `[u32 len][u8 handle count][3×0][JSON]`. Linux: a `socketpair` passed as the host's stdin (the host moves it off fd 0 and points stdin at `/dev/null` so CEF children never inherit it); descriptors travel with `SCM_RIGHTS`. Windows: the host's stdin/stdout pipes; stdout is then redirected to stderr in the host.
- Nothing polls: both sides block on the channel; the main side calls the `waker` closure after queueing each `WebEvent` (hook it to calloop / `MsgWaitForMultipleObjectsEx`).
- Frames: CEF paints into its own texture pool and lends one texture per frame for the duration of `OnAcceleratedPaint`. The host forwards it (Linux: DMA-BUF plane fds + offsets/strides/modifier; Windows: the shared NT handle value, which the main process duplicates out of the host with `DuplicateHandle` using its child process handle) and blocks until `FrameReleased` (100 ms timeout, then `FrameRevoked`). `SharedFrame` (`ipc::frame`) is the exchange struct; `lumen-render` must import, copy into a texture it owns, wait for the copy, then `release()` — immediately on wake-up, not at the next vsync, because CEF's UI thread waits meanwhile.
- Frame rate: `FramePacing::External` (default) — the main process sends `BeginFrame` at the wallpaper's fps / compositor frame callbacks and the browser produces at most one frame per request, none while hidden; `Internal` — CEF's timer at `windowless_frame_rate = fps`. No 60 fps floor; `applyGeneralProperties({ fps })` tells the page.
- Pause: `setPaused(true)` to the page, then `WasHidden(true)` (painting stops), DevTools `Page.setWebLifecycleState frozen` (timers and script stop), audio stream paused. Resume reverses it and invalidates the view.
- Audio (QO-21 fixed): an `AudioHandler` takes the page's audio; the host plays it through cpal with `AudioOutput { volume, muted }` from lumen (volume, `--silent`, automute, pause). The output stream is paused whenever nothing plays or the page is muted. If no output device is usable, CEF plays the audio itself and only mute applies (`SetAudioMuted`). The stream belongs to the host process: lumen's automute detector must ignore `WebEvent::Started { pid }` (the host PID).
- Files: `wp<id>://root/<file>` (scheme standard, secure, CORS and fetch enabled). `FileService` resolves the URL path (host, query, fragment ignored; percent-decoded, raw path as fallback), refuses `..`, absolute/drive paths, NUL, and anything whose canonical path leaves the wallpaper folder (so outgoing symlinks are refused, inner ones work), then serves regular files with their MIME type, `Content-Length`, single `Range` requests (206/416), otherwise 404.
- `file://`: web security is disabled (the WE contract makes pages load user files through `file:///` URLs), but a request handler only lets `file://` reach the wallpaper folder, the files chosen in `file` properties and the folders chosen in `directory` properties (`access::LocalFileAccess`).

### Main process integration (expected from `lumen` / `lumen-render`)

1. Build `LaunchParams` (id per spec 02 §2.2, folder, `file`, size, fps, `PropertySet::from_project_properties(general.properties)` with `--set-property` applied via `set_from_text`, `AudioOutput { volume: v/128, muted }`), call `WebHost::spawn(&default_host_executable()?, &[], params, waker)`.
2. On wake-up drain `try_event()`: `Frame` → import + copy + `release()`; `AudioListener(true)` → send `HostCommand::Audio` built with `audio::audio_frame` when `AudioPacer::should_send` says so; `MediaListeners` → forward `HostCommand::Media` for those kinds; `Started { pid }` → exclude from automute.
3. Per web frame (external pacing): `BeginFrame`. Mouse: `mouse::web_position` + `MouseTracker` → `HostCommand::Mouse`. Resize, `SetPaused`, `SetAudio`, `Properties`, `SetFps` on change.

## Build

| Configuration | Command (`CARGO_TARGET_DIR=target/agent-web`) | Status |
|---|---|---|
| default (no CEF) | `cargo test -p lumen-web`, `cargo clippy -p lumen-web --all-targets -- -D warnings` | built, 40 unit + 12 JS bridge tests + process test pass |
| Windows, no CEF | `cargo check -p lumen-web --target x86_64-pc-windows-gnu` (also clippy) | passes, never run |
| `cef-typecheck` | `cargo clippy -p lumen-web --features cef-typecheck --all-targets -- -D warnings` | passes: the CEF host is type-checked against the `cef` 156.3.0 bindings in docs.rs mode, without download; cannot link |
| `cef` | see below | **not built here**: the CEF download exceeds the 300 MB budget |

### Building the CEF host

The `cef` crate downloads the CEF binary distribution at build time: CEF 156.0.15 *minimal* is **333 MB** for linux64 (extracted ≈ 1 GB) and 175 MB for windows64. It was not downloaded on the development machine.

```sh
# Optional: keep one extracted distribution for all builds instead of one per target dir.
export CEF_PATH=$HOME/.cache/cef
# Linux needs cmake, ninja and a C++ compiler for libcef_dll_wrapper.
CARGO_TARGET_DIR=target/agent-web cargo build --release -p lumen-web --features cef --bin lumen-web-host
```

Runtime layout: `lumen-web-host` next to `lumen`, and next to it the CEF runtime files from the distribution (`libcef.so` / `libcef.dll`, `*.pak`, `icudtl.dat`, `v8_context_snapshot.bin`, `locales/`, and on Linux `libEGL.so`, `libGLESv2.so`, `libvk_swiftshader.so`, `vk_swiftshader_icd.json`). On Linux the host is linked with `rpath=$ORIGIN` (build.rs). The sandbox is disabled (`no_sandbox`, spec 07 §15.1), so `chrome-sandbox` is not needed.

The host executable is also every CEF subprocess (renderer, GPU, utility): it recognises `--type=…`, registers the `wp<id>` scheme from `--lumen-scheme=` and injects the bridge in renderers. On Windows it is a GUI-subsystem executable (no console windows).

## Tests

- `files`: MIME table sorted and lookups, 404 for missing files and directories, percent-decoding, case-insensitive fallback, traversal (`..`, `%2e%2e`, encoded slashes, backslashes, drive letters, NUL, absolute paths), outgoing symlinks (file and directory) refused, inner symlink served, ranges (206, suffix, open-ended, clamped end, multi-range → 200, 416).
- `bridge_js` (QuickJS via `rquickjs`, dev-dependency): the real `bridge.js` with the real `BridgeSession` in a loop: API installed and native channel hidden, initial properties delivered to a late listener / to a listener defined before the bridge / with top-level `var` / with methods added after assignment, deltas, fps, pause ordering, audio array shape, media replay and constants, random file, fetchall listing and removal, page exceptions isolated, reload.
- `session`, `properties`, `media`, `audio`, `mouse` (spec 07 §21.1 values), `access`, `ipc` unit tests; `ipc::unix` passes a descriptor and a 40 000-value message through a socket pair.
- `host_process` (no harness): the test binary spawns itself as a fake host through `WebHost::spawn` and exchanges Init → Started → Frame with a real descriptor (content checked) → release → event → Shutdown.
- Not tested: anything that needs CEF at run time (painting, scheme handler and request policy inside Chromium, audio capture, pause freezing, Windows handle duplication).

## Deviations

- QO-20: the public WE web API is implemented (parity rule 4); `--set-property` values reach the page (the reference passes none).
- QO-21: page audio follows `--volume`, `--silent`, automute and pause.
- §8.2: no `max(60, fps)` — the browser renders at the wallpaper's fps (and only on demand with external pacing).
- §9.4: pause freezes the page (hidden + lifecycle frozen + audio paused) instead of only not pumping it.
- §10.4: a mouse move is sent when the position changes, not every frame.
- §15.2: only the wallpaper folder is served (no assets folder, no current directory — the latter would expose arbitrary files); paths are percent-decoded with the raw path as fallback (QO-18); case-insensitive fallback for wallpapers authored on Windows; `Range` supported (QO-18); unknown types are `application/octet-stream` spelled correctly.
- §15.3: remote debugging is off unless `devtools_port` is set, and keeps Chromium's default origin checks (QO-19); `file://` is restricted to the wallpaper folder and user-chosen files/folders. Timers are throttled while paused (the page is hidden) — intentional for power.
- QO-17: without `profile_dir` the profile is a temporary folder removed on exit; with it, localStorage persists.
- Audio values for the page are clamped to ≥ 0 (spec 07 §13 can produce negative values).
- Page background is opaque white (browser default) instead of CEF's transparent offscreen default.

## Open questions (for the spec writer)

- Numeric values of `wallpaperMediaIntegration.PLAYBACK_*` are not documented publicly; Lumen uses STOPPED 0, PLAYING 1, PAUSED 2.
- Exact `project.json` keys WE writes for `directory` properties (mode, file type): Lumen reads `mode` (`fetchall`/`ondemand`) and `filetype`/`fileType` (`video…` → videos, else images). Recursive listing or not?
- Does WE call `applyUserProperties` with every property object field (`text`, `order`, …) or only `{ type, value }`? Format of the thumbnail colours (Lumen: `#rrggbb`)?
- Does WE send `applyGeneralProperties` fields besides `fps` (e.g. language)?
- Windows: CEF's GPU process and lumen's wgpu device must use the same adapter for `OpenSharedHandle`; choosing the adapter for CEF (Chromium `--use-adapter-luid`?) is not done yet.
- Linux: CEF's shared textures need its GPU process to run with EGL/Vulkan on the same GPU as lumen; untested on Wayland-only sessions.

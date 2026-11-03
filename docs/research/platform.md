# Research: platform, audio, video, web, scripting (2026-11-03)

Ranked by performance first (see `docs/PERFORMANCE.md`). Versions from crates.io on 2026-11-03.

## Upstream behaviours we must NOT reproduce (performance bugs)

| Upstream behaviour | Cost | Lumenwall |
|---|---|---|
| X11: GPU→CPU readback of the full frame every frame, then pushed to the root pixmap without shared memory | ~1 GB/s of copies in 4K at 30 fps | Render directly on the GPU (desktop window or DRI3 pixmap) |
| Pause loop sleeps 250 µs (not ms) and re-queries the window server each time | ~4000 wake-ups/s while paused | Block on events; zero wake-ups when paused |
| Fullscreen detection: blocking window-server query every frame | Per-frame round-trip | Event-driven |
| Audio automute: synchronous stream-list query every frame, up to 100 ms | Frame stalls | Subscribe to stream events |
| Web: CPU frame copy, minimum 60 fps | CPU copies + forced 60 fps | GPU texture import, fps = wallpaper target |
| Wayland: integer scale only | Renders 2× at 125% | Fractional scale + viewporter |

## Decisions

| Area | Linux | Windows | Notes |
|---|---|---|---|
| Wallpaper surface | `smithay-client-toolkit` 0.21 + `wayland-protocols` (layer-shell BOTTOM, fractional-scale, viewporter); `x11rb` 0.14 | `windows` 0.62: Progman/WorkerW (`0x052C`); on 24H2+ child of Progman with `WS_EX_LAYERED`, under `SHELLDLL_DefView`; DX12 via DirectComposition | winit only for windowed mode. No layer-shell on GNOME/Weston. Windows 24H2+ layout is undocumented: needs testing on 24H2 and 25H2. PerMonitorV2 DPI awareness. |
| X11 output | Desktop-type window rendered directly by Vulkan (fastest) ; root pixmap via DRI3 for compatibility | — | Root-pixmap parity mode as an option. |
| Pause / occlusion | x11rb events + `_NET_ACTIVE_WINDOW`; `wlr-foreign-toplevel` where available, else rely on compositor stopping frame callbacks | `SetWinEventHook` + `DWMWA_CLOAKED` + `SHQueryUserNotificationState` | Extra: per-monitor pause; pause on battery / screen off / locked session (`zbus` logind, `RegisterPowerSettingNotification`). |
| Event loop | `calloop` | `MsgWaitForMultipleObjectsEx` | Blocking, no polling. Frame pacing from compositor on Wayland, timer elsewhere. |
| Audio capture | `pipewire` 0.10 native, fallback `libpulse-binding` | `cpal` 0.18 WASAPI loopback (input stream on output device) | Open only if the wallpaper declares audio processing. Silent output = no wake-ups. |
| FFT | `realfft` | same | Reproduce upstream formula exactly (spec 07). Parity needs resampling to the upstream format (8-bit mono 44.1 kHz). |
| Sound playback | `symphonia` + `kira` (or `rodio`) | same | Stop output stream when paused/muted. |
| Automute | Pulse/PipeWire event subscription | `IAudioSessionManager2` notifications | |
| Video | `ffmpeg-next` + hardware decode, zero-copy: VA-API → dmabuf → wgpu (`texture_from_dmabuf_fd`); Vulkan Video on NVIDIA | D3D11VA → shared NV12 texture → wgpu DX12 | YUV→RGB in our shader. Decode at wallpaper fps, stop when paused. `libmpv2` as parity fallback (GL bridge cost). |
| Web | `cef` 156 with accelerated off-screen rendering (dmabuf / D3D11 → wgpu) in a separate process, launched only for web wallpapers | same | `wry` (WebView2/WebKitGTK) is lighter but has no off-screen rendering and is not Chromium. |
| Scripting | `rquickjs` 0.14 (QuickJS-ng, same engine family as upstream, <1 MB RAM) | same (MSVC support marked experimental: test early) | `boa` as pure-Rust fallback; V8 too heavy. |
| Mouse | X11 `QueryPointer` per rendered frame; Wayland pointer events on the surface | `GetCursorPos` per frame; Raw Input with `RIDEV_INPUTSINK` for clicks | Avoid `WH_MOUSE_LL` (system-wide latency). |
| Steam paths | `steamlocate` 2.1 (registry, Flatpak, Snap, multiple libraries) | same | Workshop: `<library>/steamapps/workshop/content/431960`. |
| CLI / logs / config | `clap`, `tracing`, `toml`, `dirs`, `notify` | same | Keep upstream option names. |
| Now playing (media) | `zbus` MPRIS signals | `GlobalSystemMediaTransportControlsSessionManager` | Upstream polls every 2 s. |

## Architecture

1. `lumenwall-platform`: `WallpaperSurface` trait, backends Wayland (sctk), X11 (x11rb), Win32; provides a raw window handle for wgpu plus events (size, scale, pause, pointer).
2. One blocking event loop per process; nothing runs between frames.
3. Central pause state: when paused, no GPU submission, audio capture closed, video and web suspended.
4. One wgpu device for all monitors; no CPU readback in the hot path.
5. Video (ffmpeg) and web (CEF) behind cargo features and loaded only when needed; web in its own process.

## Parity and platform risks

- Windows 24H2+/25H2 desktop window layout (undocumented, changing).
- GNOME Wayland unsupported (no layer-shell); KDE/COSMIC lack wlr-foreign-toplevel.
- Zero-copy video means three code paths (VA-API, D3D11, Vulkan Video).
- WebView2/WebKit differ from Chromium; CEF is heavy.
- FFT values need resampling to match upstream.

## Prior art (reference only, do not copy)

- kirie (Rust + wgpu, renders WE wallpapers on Linux/macOS): **AGPL-3.0**. Implementers must not read its code.
- wqLouis/linux-wallpaperengine (Rust port): GPL-derived. Implementers must not read its code.

# lumen-platform

Wallpaper surfaces, monitors, pause detection and pointer input. The crate hands out raw handles
(`raw-window-handle` 0.6) for the renderer's wgpu surfaces and never touches the GPU.

```rust
let mut s = lumen_platform::open(&Config::default())?;      // Box<dyn WallpaperSurface>
s.wait(Wait::Block)?;                                         // sleeps in the OS, never polls
while let Some(ev) = s.next_event() { /* OutputAdded, Frame, Paused, Pointer... */ }
s.request_frame(id);  /* render */  s.pre_present(id);  /* present */
```

`examples/surface_info.rs` opens the surfaces and prints every event (`--window`, `--x11`, `--output NAME`,
`--fps N`, `--clicks`, `--idle`, `--seconds N`; `RUST_LOG=lumen_platform=debug` for logs).

## Specs used

- `specs/07-plateforme-audio-video-web.md`: §1.2 (stop), §2.2 (`-w`, `-r`, `--layer`, `--fps`, fullscreen
  options, `--disable-mouse`), §4.1–4.5 (modes, outputs, span slices, UV scaling), §5 (X11), §6 (Wayland),
  §7 (window), §8.2 (pacing), §9 (fullscreen pause), §10 (mouse), §21.1 test 3, §22 (Windows).
- `docs/research/platform.md`, `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`.
- Public documentation only: Wayland protocol XML (layer-shell, fractional-scale, viewporter,
  wlr-foreign-toplevel), EWMH/ICCCM, RandR/DPMS/MIT-SCREEN-SAVER, MSDN, docs.rs, and the public
  "Progman 0x052C / WorkerW" technique.

## Coverage

| | Wayland (sctk 0.21) | X11 (x11rb 0.14, XCB) | Windows (windows 0.62) | Window (winit 0.30) |
|---|---|---|---|---|
| Surface | layer-shell, one per output, `--layer` | `_NET_WM_WINDOW_TYPE_DESKTOP` window per RandR output | child of the WorkerW behind the icons; 24H2+: `WS_EX_LAYERED` child of Progman below `SHELLDLL_DefView` | 640×480 or `-w` geometry |
| Scale | fractional-scale + viewporter (integer fallback) | 1.0 | PerMonitorV2, DPI/96 | winit |
| Hot-plug | yes | RandR notify | `WM_DISPLAYCHANGE`, Explorer restart (`TaskbarCreated`) | — |
| Pacing | frame callbacks + fps cap | timer | high-resolution waitable timer | timer |
| Fullscreen | wlr-foreign-toplevel (only-active, app-id filter) | root children cache + `_NET_ACTIVE_WINDOW`/`_NET_WM_STATE` | `SetWinEventHook` + `DWMWA_CLOAKED` + `SHQueryUserNotificationState` | none (debug) |
| Covered / hidden | no frame callback for 500 ms | `VisibilityNotify` (no compositor) | maximised / work-area-covering window | occluded, minimised |
| Locked / screen off | via missing frame callbacks | MIT-SCREEN-SAVER, DPMS 1.2 | WTS lock, `GUID_CONSOLE_DISPLAY_STATE` | — |
| Pointer | surface events | pipelined `QueryPointer` per frame; clicks on our window | `GetCursorPos` per frame; Raw Input `RIDEV_INPUTSINK` for clicks on the desktop | winit |

Pure logic is OS-independent and unit-tested: UVs and span slices (`uv`), mouse projection and focus rules
(`mouse`), frame pacing (`sched`), toplevel relevance and the X11 root-window cache (`fullscreen`), Windows
parent choice, child rectangles, click targets and pause decision (`win_logic`).

## Performance

- Paused or idle outputs arm no timer. Measured on X11 (XWayland) and in windowed mode: **0** wake-ups of
  the event thread over 3 s with no frame requested; the X11 backend renders at 30.2 frames/s with a cap of 30.
- No foreign window ever gets a `PropertyChange` mask on X11 (applications rewrite their properties
  constantly); EWMH states are read only when a focus or geometry event makes them relevant.
- No allocation per pointer event in this crate (events are `Copy`, queues reused). libxcb/libwayland
  allocate internally; Wayland's `wl_surface.frame` creates one protocol object per frame (inherent).
- Windows Raw Input wakes the thread on every mouse report, so it is registered only in
  `PointerMode::MotionAndButtons` (web wallpapers) and only while an output runs.

## Notes for the renderer and the binary

- Wayland: call `pre_present` before presenting (it attaches the frame callback and the viewport state to
  that commit) and prefer a non-blocking present mode (Mailbox/Immediate): pacing comes from our callbacks,
  and FIFO can block in the driver while the surface is hidden.
- Drop the GPU surface of an output when `OutputRemoved` arrives, before the next `wait`.
- Features: `wayland`, `x11`, `windowed` (all default). `windowed` brings winit 0.30 and its own
  sctk 0.19 / x11rb 0.13 (duplicate crates, `cargo deny` warns only). winit must run on the main thread.
- Windows: embed a manifest with Windows 8+ `supportedOS` entries (layered child windows) and, ideally,
  PerMonitorV2 awareness (the backend also requests it at runtime).

## Not tested

- **Windows**: only `cargo check`/`clippy --target x86_64-pc-windows-gnu`. Nothing ran on Windows.
- **Wayland**: the dev machine runs GNOME (no layer-shell); only the connection and the "no layer-shell"
  error path ran. Needs sway / Hyprland / niri / KDE / COSMIC.
- X11 ran on XWayland only (GNOME), not on a real Xorg with a reparenting WM or a compositor (picom).

### To check on Windows 11 24H2 / 25H2 (and once on Windows 10)

1. Wallpaper visible behind the icons, icons still clickable; with several monitors and a monitor left of
   / above the primary (negative virtual origin); after changing scale, resolution and arrangement.
2. 24H2 path: layered child of Progman really composes under DirectX 12 (wgpu DX12 HWND swap chain vs
   DirectComposition), and Explorer does not later reorder its children above ours. The executable needs a
   manifest declaring Windows 8+ compatibility, or layered child windows are refused.
3. Kill and restart `explorer.exe`: `OutputRemoved` then `OutputAdded` without restarting Lumen.
4. Pause: borderless fullscreen game, exclusive D3D fullscreen, F11 browser, maximised window (only its
   monitor, `OCCLUDED`), minimise/restore, virtual desktop switch (cloaking), Win+L, screen off; and that
   moving the mouse over a fullscreen game causes no wake-ups (the location hook is limited to the
   foreground thread: verify cursor moves are not reported through it).
5. Clicks (`--clicks`): reported on the desktop only, never on other windows; no stuck buttons.
6. Out-of-context WinEvents wake `MsgWaitForMultipleObjectsEx(QS_ALLINPUT)` promptly.

## Deviations

- No root-pixmap read-back on X11: the GPU renders into the desktop windows (`vflip` is then true on every
  backend). A root-pixmap compatibility mode is left to a later change.
- Fractional scaling on Wayland; hot-plug on every backend (the reference handles neither).
- Event-driven pause (no 250 µs loop, no per-frame round-trip); outputs that are covered, hidden, locked or
  powered off also pause (`PauseReasons`). `PauseScope::AllOutputs` (default) keeps the reference rule that a
  fullscreen window pauses every output.
- X11: windows flagged `_NET_WM_STATE_HIDDEN` never count (Mutter keeps minimised XWayland windows mapped),
  and a maximised window matching an output only pauses that output (`OCCLUDED`), as Wallpaper Engine
  separates maximised from fullscreen. Exact-geometry windows still count as fullscreen (§9.2).
- Clicks are delivered on X11 and Windows too (on the visible desktop only); the reference never sees them
  on X11 (QO-8).
- Missing `XDG_SESSION_TYPE` falls back on `WAYLAND_DISPLAY` / `DISPLAY` instead of aborting.
- Identity (layer-shell namespace, `WM_CLASS`, title) is `Config::identity`, default `lumen` (QO-4 open).
- Windowed mode has no fullscreen detector and only tracks the pointer over the window.

## Questions for the spec writer

- §4.5 "c = nl/2": float or integer division when `nl` is odd? Implemented as float (half-pixel difference).
- Windows monitor names for `-r` (QO-27): implemented as the GDI device name without `\\.\` (`DISPLAY1`).

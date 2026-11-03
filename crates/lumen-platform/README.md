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
  wlr-foreign-toplevel), EWMH/ICCCM, RandR/DPMS/MIT-SCREEN-SAVER, the Present extension protocol, MSDN,
  docs.rs, and the public "Progman 0x052C / WorkerW" technique. GNOME/XWayland behaviour below was measured
  on the dev machine, not read from Mutter or XWayland sources.

## Coverage

| | Wayland (sctk 0.21) | X11 (x11rb 0.14, XCB) | Windows (windows 0.62) | Window (winit 0.30) |
|---|---|---|---|---|
| Surface | layer-shell, one per output, `--layer` | `_NET_WM_WINDOW_TYPE_DESKTOP` window per RandR output | child of the WorkerW behind the icons; 24H2+: `WS_EX_LAYERED` child of Progman below `SHELLDLL_DefView` | 640×480 or `-w` geometry |
| Scale | fractional-scale + viewporter (integer fallback) | 1.0 | PerMonitorV2, DPI/96 | winit |
| Hot-plug | yes | RandR notify | `WM_DISPLAYCHANGE`, Explorer restart (`TaskbarCreated`) | — |
| Pacing | frame callbacks + fps cap | timer | high-resolution waitable timer | timer |
| Fullscreen | wlr-foreign-toplevel (only-active, app-id filter) | root children cache + `_NET_ACTIVE_WINDOW`/`_NET_WM_STATE` | `SetWinEventHook` + `DWMWA_CLOAKED` + `SHQueryUserNotificationState` | none (debug) |
| Covered / hidden | no frame callback for 500 ms | `VisibilityNotify` (no compositor); XWayland: Present frame timing (any window, Wayland-native included) | maximised / work-area-covering window | occluded, minimised |
| Locked / screen off | via missing frame callbacks | MIT-SCREEN-SAVER, DPMS 1.2; XWayland: Present frame timing | WTS lock, `GUID_CONSOLE_DISPLAY_STATE` | — |
| Pointer | surface events | pipelined `QueryPointer` per frame; clicks on our window | `GetCursorPos` per frame; Raw Input `RIDEV_INPUTSINK` for clicks on the desktop | winit |

Pure logic is OS-independent and unit-tested: UVs and span slices (`uv`), mouse projection and focus rules
(`mouse`), frame pacing (`sched`), toplevel relevance and the X11 root-window cache (`fullscreen`), XWayland
frame-callback starvation (`starvation`), Windows parent choice, child rectangles, click targets and pause
decision (`win_logic`).

## GNOME (Wayland session, X11 windows through XWayland)

Mutter has no layer-shell, so the binary falls back on the X11 backend, whose window tree only contains
XWayland clients: a native Wayland window (Firefox, games, terminals, video players) is invisible to the
root-children cache, `_NET_WM_STATE` and `VisibilityNotify`. Lumen instead watches whether the compositor
still paints its windows:

- Each X11 window is a Wayland surface of XWayland, and Mutter sends no frame callback to a surface it does not
  paint. XWayland then completes `PresentPixmap` requests on a fallback timer (1 s) instead of at the next
  frame. The backend selects `PresentCompleteNotify` on its windows (the renderer's own presents are reported
  to every selecting client) and dates each completion by its UST (`CLOCK_MONOTONIC` µs).
- Running: a present unanswered for 750 ms ⇒ `Paused { OCCLUDED }` for that output. Hidden: one probe per
  second (the window's own top-left pixel presented back in copy mode: no visible change); two answers in
  under 250 ms ⇒ `Resumed`. No probe while the output is paused for another reason.
- Measured (GNOME 50.1, Ubuntu 26.04, NVIDIA): answers take 2–6 ms when visible and exactly 1000 ms when
  hidden. A GTK4 or raw xdg-shell fullscreen window pauses the output it covers ≈1.3 s after it maps
  (FIFO presents block for the first fallback period) and resumes it ≈30 ms after it closes; while hidden the
  event thread wakes ≈2×/s, 0 frames are rendered, process CPU 0.2 % (was ≈1 frame/s with a blocked render
  thread and no `Paused` event, so audio kept playing).
- It follows what Mutter paints, not window states: a maximised Wayland window does **not** pause (measured:
  Mutter keeps painting the desktop under it, presumably the strip behind the top bar), unlike the geometry
  rule for maximised XWayland windows. The lock screen, powered-off monitors and the overview should pause
  like a fullscreen window (not tested, so as not to lock the session).
- Off with `pause_when_covered = false`, outside XWayland (`XWAYLAND` extension or `WAYLAND_DISPLAY`) and
  without Present (warning logged). If an output's probes are never answered (5 × 2 s) the detection gives
  up for it with a warning.

Options evaluated (public GNOME/freedesktop interfaces, checked live on GNOME 50.1):

| Option | Result |
|---|---|
| `org.gnome.Shell.Introspect.GetWindows` | `AccessDenied` outside unsafe mode / portal backends; no fullscreen field anyway |
| `org.gnome.Mutter.DisplayConfig` | monitors and `PowerSaveMode` only |
| `org.gnome.SessionManager` inhibitors | heuristic: video players inhibit idle, games and terminals usually do not |
| `org.gnome.ScreenSaver.ActiveChanged` | lock only (covered by frame timing already) |
| X11 `VisibilityNotify`, `_NET_WM_STATE`, `_NET_ACTIVE_WINDOW` | Wayland windows are not in the X11 tree; measured: no event |
| xdg-desktop-portal | no portal exposes other applications' windows |
| Wayland globals of Mutter | no layer-shell, no wlr/ext foreign-toplevel; `xdg_toplevel.suspended` only for own toplevels |
| Present frame timing (chosen) | exact per-output visibility, no extension, 1 wake-up/s while hidden |
| Own GNOME Shell extension (`global.display` `in-fullscreen-changed` over D-Bus) | exact fullscreen with 0 wake-ups, but needs install + re-login and per-release maintenance: not needed |

## Performance

- Paused or idle outputs arm no timer (except the XWayland probe of a hidden output, below). Measured on X11
  (XWayland) and in windowed mode: **0** wake-ups of
  the event thread over 3 s with no frame requested; the X11 backend renders at 30.2 frames/s with a cap of 30.
- No foreign window ever gets a `PropertyChange` mask on X11 (applications rewrite their properties
  constantly); EWMH states are read only when a focus or geometry event makes them relevant.
- No allocation per pointer event in this crate (events are `Copy`, queues reused). libxcb/libwayland
  allocate internally; Wayland's `wl_surface.frame` creates one protocol object per frame (inherent).
- Windows Raw Input wakes the thread on every mouse report, so it is registered only in
  `PointerMode::MotionAndButtons` (web wallpapers) and only while an output runs.
- XWayland: one Present probe per second per hidden output (nothing while paused for another reason); while
  running, the completions of the renderer's presents are read (no measurable change in event-thread
  wake-ups, which are dominated by the item below).
- X11 root `SubstructureNotify` (needed for the children cache) also delivers every `ClientMessage` sent to the
  root with that mask. On the dev machine a session client answers `_NET_WM_PING` to the root at 60–200
  messages/s, which wakes the event thread (batched to ≈2–8 wake-ups/s while paused, more while running).

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
- XWayland frame timing: tested with Mutter 50 and the NVIDIA driver only; not with Mesa, KWin's or other
  compositors' XWayland, nor on lock / DPMS off.

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
- XWayland: native Wayland windows are seen as covering (`OCCLUDED`, per output), never as `FULLSCREEN`: on
  two monitors a fullscreen Wayland window pauses only the output it hides, whatever `PauseScope`.
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

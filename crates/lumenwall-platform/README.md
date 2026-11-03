# lumenwall-platform

Wallpaper surfaces, monitors, pause detection and pointer input. The crate hands out raw handles
(`raw-window-handle` 0.6) for the renderer's wgpu surfaces and never touches the GPU.

```rust
let mut s = lumenwall_platform::open(&Config::default())?;      // Box<dyn WallpaperSurface>
s.wait(Wait::Block)?;                                         // sleeps in the OS, never polls
while let Some(ev) = s.next_event() { /* OutputAdded, Frame, Paused, Pointer... */ }
s.request_frame(id);  /* render */  s.pre_present(id);  /* present */
```

`examples/surface_info.rs` opens the surfaces and prints every event (`--window`, `--x11`, `--output NAME`,
`--fps N`, `--clicks`, `--idle`, `--seconds N`; `RUST_LOG=lumenwall_platform=debug` for logs).

## Specs used

- `specs/07-plateforme-audio-video-web.md`: §1.2 (stop), §2.2 (`-w`, `-r`, `--layer`, `--fps`, fullscreen
  options, `--disable-mouse`), §4.1–4.5 (modes, outputs, span slices, UV scaling), §5 (X11), §6 (Wayland),
  §7 (window), §8.2 (pacing), §9 (fullscreen pause), §10 (mouse), §21.1 test 3, §22 (Windows).
- `docs/research/platform.md`, `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`.
- Public documentation only: Wayland protocol XML (layer-shell, fractional-scale, viewporter,
  wlr-foreign-toplevel), EWMH/ICCCM, RandR/DPMS/MIT-SCREEN-SAVER, the Present extension protocol, MSDN,
  docs.rs, the public "Progman 0x052C / WorkerW" technique, the D-Bus specification (wire format, SASL) and
  the GNOME Shell/Mutter GObject introspection data (for the optional extension). GNOME/XWayland behaviour
  below was measured on the dev machine, not read from Mutter or XWayland sources.

## Coverage

| | Wayland (sctk 0.21) | X11 (x11rb 0.14, XCB) | Windows (windows 0.62) | Window (winit 0.30) |
|---|---|---|---|---|
| Surface | layer-shell, one per output, `--layer` | `_NET_WM_WINDOW_TYPE_DESKTOP` window per RandR output | child of the WorkerW behind the icons; 24H2+: `WS_EX_LAYERED` child of Progman below `SHELLDLL_DefView` | 640×480 or `-w` geometry |
| Scale | fractional-scale + viewporter (integer fallback) | 1.0 | PerMonitorV2, DPI/96 | winit |
| Hot-plug | yes | RandR notify | `WM_DISPLAYCHANGE`, Explorer restart (`TaskbarCreated`) | — |
| Pacing | frame callbacks + fps cap | timer | high-resolution waitable timer | timer |
| Fullscreen | wlr-foreign-toplevel (only-active, app-id filter) | root children cache + `_NET_ACTIVE_WINDOW`/`_NET_WM_STATE`; GNOME: Shell extension (optional) | `SetWinEventHook` + `DWMWA_CLOAKED` + `SHQueryUserNotificationState` | none (debug) |
| Maximised (`MAXIMIZED`) | — (covered surfaces stop getting frame callbacks) | `_NET_WM_STATE_MAXIMIZED_*` of windows over ≥ ½ an output (client inside WM frames); GNOME: Shell extension for Wayland windows | counted as covering (`OCCLUDED`) | — |
| Covered / hidden | no frame callback for 500 ms | `VisibilityNotify` (no compositor); XWayland: Present frame timing (any window, Wayland-native included) | maximised / work-area-covering window | occluded, minimised |
| Locked / screen off | via missing frame callbacks | MIT-SCREEN-SAVER, DPMS 1.2; XWayland: Present frame timing | WTS lock, `GUID_CONSOLE_DISPLAY_STATE` | — |
| Pointer | surface events | pipelined `QueryPointer` per frame; clicks on our window; GNOME: Shell extension (position above every window, clicks on the desktop and on desktop icons) | `GetCursorPos` per frame; Raw Input `RIDEV_INPUTSINK` for clicks on the desktop | winit |

Pure logic is OS-independent and unit-tested: UVs and span slices (`uv`), mouse projection and focus rules
(`mouse`), frame pacing (`sched`), toplevel relevance, the X11 root-window cache and the maximised rule
(`fullscreen`), XWayland frame-callback starvation (`starvation`), the D-Bus wire format and extension
protocol (`linux::gnome_shell`), Windows parent choice, child rectangles, click targets and pause decision
(`win_logic`).

## GNOME (Wayland session, X11 windows through XWayland)

Mutter has no layer-shell, so the binary falls back on the X11 backend, whose window tree only contains
XWayland clients: a native Wayland window (Firefox, Chrome, games, terminals, video players) is invisible to
the root-children cache, `_NET_WM_STATE` and `VisibilityNotify`. Three signals are combined (union of reasons):

1. **X11 state** of XWayland windows (Proton/Wine games, Steam, Discord, `--ozone-platform=x11` browsers):
   exact-geometry windows ⇒ `FULLSCREEN` (§9.2); `_NET_WM_STATE_MAXIMIZED_VERT`+`_HORZ` ⇒ `MAXIMIZED` on the
   output the window overlaps most (`pause_when_maximized`). The state is read on the client window, not on
   Mutter's `mutter-x11-frames` frame (which has none), only for windows covering at least half an output, and
   only on map/unmap/configure/focus events.
2. **Present frame timing** for whatever Mutter stops painting (native fullscreen windows, overview, lock
   screen, powered-off monitor): `OCCLUDED`, below.
3. **Optional GNOME Shell extension** (`packaging/gnome-extension/lumenwall@simnjs.github.io`): Mutter's own
   per-monitor "in fullscreen" state and maximised normal windows of the active workspace, published on the
   session bus (see `linux::gnome_shell` for the interface). Detected automatically (also when it is enabled
   or re-enabled later, e.g. after each unlock); without it the binary logs once that maximised Wayland
   windows cannot pause. Version 2 also sends the pointer (below).

### Pointer and clicks on GNOME

XWayland only knows where the pointer is while it is above an X11 window, and Ubuntu's Desktop Icons NG (DING)
covers the desktop with a Wayland window that takes every click: without help, `QueryPointer` returns a stale
position as soon as the pointer is above a Wayland window or the icons window, and our desktop windows never
get a click (Dino Run cannot be played, parallax and cursor effects freeze). Version 2 of the extension
publishes, only to the Lumenwall connection that asks for it (`WatchPointer`, unicast signals):

- `PointerMotion(x, y, connector)`: the global position (Shell logical coordinates), above any window, at most
  once per compositor frame and at most the frame cap of Lumenwall per second (`--fps`), only when it changed;
- `PointerButton(x, y, connector, button, pressed)`: the presses that land on the desktop (no window under the
  pointer, or a desktop-type window: Desktop Icons NG, icons included, and our own windows) and their
  releases. Never presses on application windows, the top bar, docks, notifications, the overview, nor while
  a Shell grab is active (a Shell menu or an application popup closing on that click). Left, middle and right
  buttons; the right click also opens the desktop menu of DING (Wallpaper Engine seldom uses it).

Input is only observed, never consumed: icons, rubber-band selection and the desktop menu keep working. The
extension cannot see the events Mutter hands to Wayland clients (event filters and `captured-event` handlers
are not called for them, measured on GNOME 50.1), so it reads the pointer state (`MetaCursorTracker`) when
Mutter's idle monitor reports user activity: once per burst of input, then once per frame while input
continues, then once more. A press is classified by picking the reactive actor under the pointer.

The X11 backend converts the positions into output coordinates (connector, then geometry; scaled when the
X11 output size differs from the logical one), queues `Motion` events for the outputs that run right away
(a scene that only redraws on input wakes up), and `Button` events for the output under the pointer; the
release goes to the output that got the press. Meanwhile it sends no `QueryPointer` and ignores the
`ButtonPress` events of its own windows (the extension reports those clicks too). It asks for the pointer
while at least one output runs and the user did not pause the wallpapers
(`WallpaperSurface::set_input_paused`); otherwise it unsubscribes and the Shell stops watching the input
altogether. If the extension stops (lock screen, disabled), held buttons are released and `QueryPointer`
takes over; an extension older than version 2 is detected (`UnknownMethod`) and logged once.

Measured in a nested headless GNOME Shell 50.1 (two 1280×720 virtual monitors, DING enabled, virtual pointer
device, Dino Run on both outputs):

| Check | Result |
|---|---|
| Click on the dinosaur | the dinosaur jumps on that output only (`cursorDown`); timed on a coin: coin picked up (pickup particles), debug and release builds |
| Pointer above a native Wayland window (GTK 4) | the dinosaur follows it (global position) |
| Click on an application window, on the top bar, while a Shell menu is open, the click that closes DING's menu | not delivered |
| Click and drag from one monitor to the other | press and release on the first output (local x beyond its width) |
| 25 clicks held 5 to 80 ms (no motion) | 25 presses and 25 releases, durations kept to ±0.5 ms |
| DING with the pointer watched | icons, rubber-band selection and the right-click menu work; the same press/release are reported |
| Right button | reported as 3: Mutter orders the button bits like evdev (`BUTTON2_MASK` is the right button) |
| Motion, 30 fps cap | one signal per 33–35 ms while moving, none when still |
| Lumenwall exits, fullscreen window (every output pauses), `lumenctl pause` | unsubscribed (name watcher, `WatchPointer(0)`): no observation in the Shell, no signal; back on resume |

In that nested Shell the X11 desktop windows of Lumenwall (desktop type, `_NET_WM_STATE_BELOW`) stacked above
DING's Wayland windows, hiding its icons; clicks then land on our windows, which the extension reports the same
way (desktop type). Which one is on top in a real session was not checked.

A maximised **Wayland** window has no other reliable signal: Mutter keeps painting the strip of desktop under
the top bar (transparent with Blur my Shell), so the frame timing never starves, and no public interface lists
other applications' windows (table below). Install the extension for Wallpaper Engine's "pause when another
application is maximised"; the strip under the panel then keeps the last frame.

Present frame timing:

- Each X11 window is a Wayland surface of XWayland, and Mutter sends no frame callback to a surface it does not
  paint. XWayland then completes `PresentPixmap` requests on a fallback timer (1 s) instead of at the next
  frame. The backend selects `PresentCompleteNotify` on its windows (the renderer's own presents are reported
  to every selecting client) and dates each completion by its UST (`CLOCK_MONOTONIC` µs).
- Running: a present unanswered for 750 ms ⇒ `Paused { OCCLUDED }` for that output. Hidden: one probe per
  second (the window's own top-left pixel presented back in copy mode: no visible change); two answers in
  under 250 ms ⇒ `Resumed`. No probe while the output is paused for another reason.
- Off with `pause_when_covered = false`, outside XWayland (`XWAYLAND` extension or `WAYLAND_DISPLAY`) and
  without Present (warning logged). If an output's probes are never answered (5 × 2 s) the detection gives
  up for it with a warning.

Measured on DP-4 (GNOME 50.1, Ubuntu 26.04, RTX 4080 proprietary driver, 2 × 2560×1440 at 280/240 Hz,
`fps = 144`, FIFO, Blur my Shell + Dash to Dock + DING enabled). Running: 6–8 % CPU, ≈10 % GPU. Paused: 0 frames,
0–0.7 % CPU, 0 % GPU, event thread ≈3 wake-ups/s.

| Case | Signal | Pause after map | Resume after close |
|---|---|---|---|
| GTK4 window fullscreen, native Wayland | frame timing | 1.2–1.25 s (FIFO blocks the renderer until the 1 s fallback) | 65–70 ms |
| Chrome `--kiosk`, native Wayland, VP9 video playing | frame timing | 1.7–3.3 s after launch (start-up included) | 75–190 ms |
| GTK4 window fullscreen, XWayland | X11 state | 30–250 ms (mapped windowed, then resized) | 35 ms |
| Simulated Proton game: managed fullscreen / override-redirect, XWayland | X11 state | < 1 ms | < 1 ms |
| Chrome `--ozone-platform=x11 --kiosk` | X11 state | ≈0.7 s after launch | 85 ms |
| GTK3 window maximised, XWayland (2560×1400 under the 40 px top bar) | X11 state | < 1 ms (was: never) | 1 ms |
| GTK4 window maximised, native Wayland | none without the extension | never | — |
| Same, with the extension (nested headless GNOME 50.1 Shell) | extension | 17 ms after the window reaches its monitor; signal 1–2 ms after each state change | signal 2 ms after unmaximise / close |
| Overview (nested Shell) | frame timing | ≈1 s | on close |

With the extension a fullscreen Wayland window is announced 25–35 ms after it maps, often after Mutter already
stopped painting the desktop: with FIFO the renderer may then still block for the 1 s fallback first.

Present modes at 144 fps (test build of the renderer): the NVIDIA driver offers only FIFO and Immediate on
XWayland (no Mailbox). The frame timing works with both: FIFO pauses 1.2 s after a native fullscreen window
maps (the renderer blocks until the fallback answers), Immediate 0.95 s (750 ms threshold, nothing blocks);
both resume in ≈70 ms, with the same running cost (≈8 % CPU) and 0 % GPU while paused.

Options evaluated (public GNOME/freedesktop interfaces, checked live on GNOME 50.1):

| Option | Result |
|---|---|
| `org.gnome.Shell.Introspect.GetWindows` | `AccessDenied` outside unsafe mode / portal backends; no fullscreen field anyway |
| `org.gnome.Mutter.DisplayConfig` | monitors and `PowerSaveMode` only |
| `org.gnome.SessionManager` inhibitors | heuristic: video players inhibit idle, games and terminals usually do not; a small video window inhibits too |
| `org.gnome.ScreenSaver.ActiveChanged` | lock only (covered by frame timing already) |
| X11 `VisibilityNotify`, `_NET_WM_STATE`, `_NET_ACTIVE_WINDOW` | Wayland windows are not in the X11 tree; measured: no event |
| `_GTK_WORKAREAS_D0` on the root | per-monitor work areas only, says nothing about windows |
| xdg-desktop-portal | no portal exposes other applications' windows |
| Wayland globals of Mutter | no layer-shell, no wlr/ext foreign-toplevel; `xdg_toplevel.suspended` only for own toplevels |
| Present frame timing (kept) | exact per-output visibility, no extension, 1 wake-up/s while hidden; blind to maximised windows |
| Own GNOME Shell extension (added, optional) | exact fullscreen and maximised state, 0 wake-ups; needs install + re-login and per-release care (feature-detected Mutter calls, tested on 50.1 only) |

Install or update the extension (once per user; a Wayland session only loads new or changed extension code
at login, so an update needs a new login too):

```sh
mkdir -p ~/.local/share/gnome-shell/extensions
rm -rf ~/.local/share/gnome-shell/extensions/lumenwall@simnjs.github.io
cp -r packaging/gnome-extension/lumenwall@simnjs.github.io ~/.local/share/gnome-shell/extensions/
# log out and back in, then (first install only; an update keeps it enabled):
gnome-extensions enable lumenwall@simnjs.github.io
gnome-extensions info lumenwall@simnjs.github.io   # "Version: 2", "State: ACTIVE"
```

Lumenwall needs no restart: it subscribes again when the extension reappears.

## Performance

- Paused or idle outputs arm no timer (except the XWayland probe of a hidden output, below). Measured on X11
  (XWayland) and in windowed mode: **0** wake-ups of
  the event thread over 3 s with no frame requested; the X11 backend renders at 30.2 frames/s with a cap of 30.
- No foreign window ever gets a `PropertyChange` mask on X11 (applications rewrite their properties
  constantly); EWMH states are read only when a focus or geometry event makes them relevant (pipelined, one
  round-trip per batch of events).
- GNOME extension link: one Unix socket to the session bus in the event loop, no thread and no timer. The bus
  only forwards `NameOwnerChanged` for the extension's name and its `MonitorsChanged` signal, which is sent
  only when a monitor's state changes: 0 wake-ups otherwise, with or without the extension. The handshake
  (≈1 ms) runs once at start-up.
- GNOME pointer feed (extension version 2), measured in the nested headless Shell above with a 500 Hz virtual
  mouse moving continuously over the desktop (release build, 8 s windows, `/proc/<pid>/task/*/schedstat` and
  context switches):

  | | gnome-shell | Lumenwall |
  |---|---|---|
  | Mouse still, watched | 0.09 → 0.12 % CPU, 3 wake-ups/s (unchanged) | same as `--disable-mouse`: Dino Run 344 wake-ups/s, static 3044659344 282 wake-ups/s (driver threads), 0 from the feed |
  | Mouse moving, nobody watching (baseline) | 6.1–6.7 % (virtual device and cursor) | — |
  | Mouse moving, watched at 30/s (`--fps 30`) | +0.4 to +1.4 % of a core; bus daemon +0.1 %, 30 wake-ups/s | +28 to +30 wake-ups/s, +0.1 to +0.3 % CPU (static scene that ignores the pointer: no frame drawn) |
  | Mouse moving, watched once per frame (60 Hz monitors) | +1.5 % of a core; bus daemon +0.3 %, 61 wake-ups/s | — |

  Inside the Shell one look at the pointer costs 1.7 µs, one signal ≈ 33 µs (variant + emission), one click
  classification (pick) 12 µs. Lumenwall asks for at most its frame cap per second, so a 280 Hz monitor does
  not multiply the signals; nothing is sent while the pointer is still, while every output is paused or while
  the user paused the wallpapers.
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
- X11 on XWayland: with FIFO a hidden window blocks the render thread in `acquire` until XWayland's 1 s
  fallback (and indefinitely on DRM-syncobj waits in a headless nested Mutter); no platform event, D-Bus
  message included, is processed meanwhile. Immediate (the only other mode NVIDIA offers there) avoids it;
  the frame cap already paces the frames.
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
- GNOME extension: GNOME 50.1 only, in a nested headless Shell with a virtual pointer (the real session
  needs a re-login to load it); GNOME 45–49 are declared and use feature-detected calls (connectors fall back
  on geometry). Not tested: workspace switches with maximised XWayland windows on the other workspace,
  mirrored monitors, fractional scaling (geometry fallback only), real mice and touchpads (tap-to-click),
  tablets and touch screens (touch is not reported), other desktop-icons extensions than DING.

### To check on Windows 11 24H2 / 25H2 (and once on Windows 10)

1. Wallpaper visible behind the icons, icons still clickable; with several monitors and a monitor left of
   / above the primary (negative virtual origin); after changing scale, resolution and arrangement.
2. 24H2 path: layered child of Progman really composes under DirectX 12 (wgpu DX12 HWND swap chain vs
   DirectComposition), and Explorer does not later reorder its children above ours. The executable needs a
   manifest declaring Windows 8+ compatibility, or layered child windows are refused.
3. Kill and restart `explorer.exe`: `OutputRemoved` then `OutputAdded` without restarting Lumenwall.
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
- XWayland: without the GNOME extension, native Wayland windows are seen as covering (`OCCLUDED`, per
  output), never as `FULLSCREEN`: on two monitors a fullscreen Wayland window pauses only the output it hides,
  whatever `PauseScope`. With the extension they are `FULLSCREEN` and follow `PauseScope`.
- X11: windows flagged `_NET_WM_STATE_HIDDEN` never count (Mutter keeps minimised XWayland windows mapped),
  and a maximised window matching an output only pauses that output (`OCCLUDED`), as Wallpaper Engine
  separates maximised from fullscreen. Exact-geometry windows still count as fullscreen (§9.2).
- Maximised windows pause the output they are on (`MAXIMIZED`, `Config::pause_when_maximized`, on by default;
  the binary ties it to the fullscreen-pause switch): Wallpaper Engine's "other application maximised" rule,
  which the reference does not have. A strip of desktop left visible by a transparent panel keeps the last
  frame.
- Clicks are delivered on X11 and Windows too (on the visible desktop only); the reference never sees them
  on X11 (QO-8). On GNOME the Shell extension also delivers clicks on desktop icons, like Wallpaper Engine on
  Windows. A press released before the next frame is still reported by `PointerTracker::state` for one
  frame, so scripts get their `cursorDown`/`cursorClick` for short clicks.
- Missing `XDG_SESSION_TYPE` falls back on `WAYLAND_DISPLAY` / `DISPLAY` instead of aborting.
- Identity (layer-shell namespace, `WM_CLASS`, title) is `Config::identity`, default `lumenwall` (QO-4 open).
- Windowed mode has no fullscreen detector and only tracks the pointer over the window.

## Questions for the spec writer

- §4.5 "c = nl/2": float or integer division when `nl` is odd? Implemented as float (half-pixel difference).
- Windows monitor names for `-r` (QO-27): implemented as the GDI device name without `\\.\` (`DISPLAY1`).

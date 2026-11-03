# lumenwall-ui

Desktop app (iced, native Rust, no webview) to browse, apply and configure wallpapers, plus the tray icon. Design and measurements: [`docs/UI.md`](../../docs/UI.md); screenshots: [`docs/ui-screenshots/`](../../docs/ui-screenshots/).

```sh
lumenwall-ui                 # the window
lumenwall-ui --tray          # tray icon only (separate process, no GPU)
lumenwall-ui --background    # start the daemon if needed, then the tray (what autostart runs)
lumenwall-ui --demo-library --fake-daemon   # generated library + in-process fake daemon over real IPC
```

Other developer options: `--fake-steam`, `--no-key`, `--ws-select`, `--screenshot-delay`, `--page`, `--select`, `--search`, `--lang`, `--size`, `--screenshot <png>` (saves the window once loaded, then exits), `--bench-animate`. Environment: `LUMENWALL_UI_RENDERER=software`, `LUMENWALL_UI_REDUCE_MOTION=1|0`, `LUMENWALL_LOG`, `LUMENWALL_IPC_ENDPOINT`, `LUMENWALL_WE_WORKSHOP`.

## Layout

| Module | Role |
|---|---|
| `app.rs` | State, messages, update, conditional subscriptions, keyboard, drag and drop |
| `view/` | Pages (library, details panel, screens, settings), status bar, reusable widgets, `drawn` (frame-drawn flag) |
| `theme.rs` | Design system: colour tokens, spacing, radii, elevation, type scale, widget styles, UI font choice |
| `icons.rs` | Lumenwall's own SVG icons, empty-state illustrations, and the embedded brand assets (symbol, logo, window icon, tray glyphs) |
| `chrome.rs` | Linux: the title band, window buttons, move / resize and the opaque 1 px outline the app draws itself; rounded, see-through corners only on Wayland with the GPU renderer (square elsewhere) |
| `build.rs` | Windows: icon and version resources (`assets/brand/icon/lumenwall.ico`) |
| `catalog/` | Library scan, lenient `project.json` reading, Steam / Workshop / Wallpaper Engine discovery (`Discovery` trait, native, Flatpak, Snap, Windows registry), resolution from `scene.pkg` / MP4 headers |
| `thumbs.rs`, `previews.rs` | Preview decoding (16:9 crop, LZ4 frames, disk cache) and animation |
| `daemon.rs` | IPC worker thread and the UI's view of the daemon |
| `workshop/` | Steam Workshop: public Web API client (allow-listed hosts, TTL disk cache), BBCode renderer, API key storage (0600), folder watcher (`notify`), page state, fake Steam server (`--fake-steam`) |
| `tray/` | StatusNotifierItem + dbusmenu over zbus (Linux), `tray-icon` (Windows) |
| `config.rs`, `i18n.rs`, `picker.rs`, `platform.rs` | Preferences and autostart, EN/FR strings, file chooser portal / dialog, OS helpers |
| `fake.rs`, `demo.rs` | Fake daemon and generated demo library for development and screenshots |

The catalog lives here behind `catalog::Discovery` until `lumenwall-assets` / `lumenwall-scene` are integrated.

## Specs

- `specs/02-modele-scene.md` §2–3: `project.json` keys and user property kinds.
- `specs/01-formats-binaires.md` §1: `.pkg` layout (to read `scene.json` for the resolution filter).
- `specs/07-plateforme-audio-video-web.md` §3.1: Workshop id or path.

## Network

Only the Workshop page talks to the network, only while it is displayed, only to Steam's hosts, with a generic `Lumenwall/<version>` User-Agent and the user's own key (never bundled, never logged). Details in [`docs/UI.md`](../../docs/UI.md#workshop).

## Sound

The status bar holds the global volume. The details panel's Customize section adds a level and a mute button for the selected wallpaper (global × wallpaper, saved by the daemon in `[wallpaper-audio]`), shown only when the wallpaper can make sound (a scene with sound objects, a video with an audio track or that could not be read, any web wallpaper) and the daemon keeps such levels (`state.wallpaper_audio`). Slider steps queued while dragging are merged: only the latest position is sent.

## Deviations

- `project.json` is read leniently: a missing or odd field never hides a wallpaper (the engine stays strict). Workshop items of category `Asset` are listed as unsupported.
- Previews are shown cropped to their centre 16:9 band.

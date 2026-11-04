# lumenwall-ui

Desktop app (iced, native Rust, no webview) to browse, apply and configure wallpapers, plus the tray icon. Design and measurements: [`docs/UI.md`](../../docs/UI.md); screenshots: [`docs/ui-screenshots/`](../../docs/ui-screenshots/).

```sh
lumenwall-ui                 # the window
lumenwall-ui --tray          # tray icon only (separate process, no GPU)
lumenwall-ui --background    # start the daemon if needed, then the tray (what autostart runs)
lumenwall-ui --setup         # the window with the installation dialog open
lumenwall-ui --stop-engine   # ask the engine to quit and wait (installers: exit 0 stopped, 3 none, 1 error)
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
| `setup/` | Installation on this computer: first-run dialog and the Installation card of the Settings (below) |
| `updater.rs` | Self-update through `lumenwall-update`: "Lumenwall X.Y.Z is available" banner above the pages (release notes, progress, errors), Updates card in the Settings, engine and app restart |
| `config.rs`, `i18n.rs`, `picker.rs`, `platform.rs` | Preferences and autostart, EN/FR strings, file chooser portal / dialog, OS helpers |
| `fake.rs`, `demo.rs` | Fake daemon and generated demo library for development and screenshots |

The catalog lives here behind `catalog::Discovery` until `lumenwall-assets` / `lumenwall-scene` are integrated.

## Installation dialog

Released programs are often started from a downloaded folder. At the first start the app checks, on a worker thread, what is in place for the current user and, if something is missing, opens a dialog that lists it; each item can be ticked off, and nothing is written before "Set up" is clicked (no keyboard shortcut does it). "Not now" is remembered (`ui.json`: `"setup": {"prompted": true}`); the dialog comes back only when the app carries a newer GNOME extension than the installed one, once per version. Settings › Installation shows the same items with Repair and Uninstall. The dialog never opens by itself in development builds, screenshots or demo runs, nor for a copy made by the Windows installer, which already asked (`--setup` forces it).

| Item | Linux | Windows |
|---|---|---|
| Programs (only when started from an extracted release archive elsewhere) | `lumenwall`, `lumenwall-ui` copied to `~/.local/bin` | the whole folder copied to `%LOCALAPPDATA%\Programs\Lumenwall` |
| Menu | `~/.local/share/applications/lumenwall-ui.desktop` (absolute `Exec`, `TryExec`) and the `hicolor` icons | `Lumenwall.lnk` in the Start menu (`IShellLinkW`) |
| Start with the session | `~/.config/autostart/lumenwall.desktop` (`"<absolute path>/lumenwall-ui" --background`) | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value `Lumenwall` |
| GNOME Shell extension (GNOME sessions) | copied to `~/.local/share/gnome-shell/extensions/lumenwall@simnjs.github.io` (whole folder replaced when the embedded `version-name` is newer), then `gnome-extensions enable`, or the `org.gnome.shell enabled-extensions` key when the running Shell does not know it yet; the dialog then says to log out and back in | — |

- The desktop entry, icons and extension are compiled into the binary from `packaging/` (`setup/assets.rs`; a test fails if a file there is not embedded), so a bare `lumenwall-ui` can set itself up.
- Programs are replaced atomically (written beside, then renamed); on Windows a running `.exe` is renamed to `<name>.lumenwall-old` first, as the updater does. Copies in `~/.local/bin` and `%LOCALAPPDATA%\Programs\Lumenwall` keep updating themselves (`lumenwall-update`).
- State: `Ok`, `Missing`, `Outdated` (other version of the programs, an entry whose program is gone, a lost icon, an older extension), `Disabled` (extension turned off), `System` (from a package: `/usr/share/applications`, `/usr/share/gnome-shell/extensions`). A package-managed copy (`/usr/bin`) only offers what belongs to the user: start-up and turning the extension on.
- Uninstall stops the engine (IPC `shutdown`), then removes the entries, icons, autostart entry, the user's copy of the extension (turned off first) and the programs in `~/.local/bin`; settings and the library stay. On Windows, a copy installed by the installer is removed by its `uninstall.exe`; a folder Lumenwall still runs from is reported for deletion after closing.
- Tests: `setup::linux`, `setup::windows` (registry and shortcuts behind a fake), `setup::files`, `setup::desktop` run on temporary folders; GNOME Shell is reached through the `Shell` trait, faked in tests. Never test `apply` by hand outside a throw-away session: the GSettings fallback writes the session's real `enabled-extensions` list.

## Specs

- `specs/02-modele-scene.md` §2–3: `project.json` keys and user property kinds.
- `specs/01-formats-binaires.md` §1: `.pkg` layout (to read `scene.json` for the resolution filter).
- `specs/07-plateforme-audio-video-web.md` §3.1: Workshop id or path.

## Network

Only the Workshop page talks to the network, only while it is displayed, only to Steam's hosts, with a generic `Lumenwall/<version>` User-Agent and the user's own key (never bundled, never logged). Details in [`docs/UI.md`](../../docs/UI.md#workshop).

Update checks ask `api.github.com` at most every 12 hours (at start or when the window gets focus, only if due; "Check now" in the Settings), with the same generic User-Agent and nothing else; downloads happen only when the user clicks "Update and restart". Off in the Settings (`"updates": {"auto_check": false}` in `ui.json`), with `LUMENWALL_NO_UPDATE=1`, and automatically for package-managed copies. Details in [`crates/lumenwall-update`](../lumenwall-update/README.md).

## Playlists

Each tile of the Screens page lists the Wallpaper Engine playlists the daemon reads (fetched again when the page opens, so playlists made in Wallpaper Engine meanwhile show up); choosing one plays it on that screen (`apply_playlist`, saved by the daemon). A screen that plays a playlist shows its name, the item shown ("3 of 7"), how it changes (every N minutes, random order, or only with Next), and Previous / Next / Stop the playlist (Stop keeps the wallpaper shown, through `apply`). A daemon without playlists gets no controls; one that cannot read Wallpaper Engine's settings says why.

## Sound

The status bar holds the global volume. The details panel's Customize section adds a level and a mute button for the selected wallpaper (global × wallpaper, saved by the daemon in `[wallpaper-audio]`), shown only when the wallpaper can make sound (a scene with sound objects, a video with an audio track or that could not be read, any web wallpaper) and the daemon keeps such levels (`state.wallpaper_audio`). Slider steps queued while dragging are merged: only the latest position is sent.

## Deviations

- `project.json` is read leniently: a missing or odd field never hides a wallpaper (the engine stays strict). Workshop items of category `Asset` are listed as unsupported.
- Previews are shown cropped to their centre 16:9 band.

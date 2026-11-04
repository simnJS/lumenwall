# Lumenwall desktop app (`lumenwall-ui`)

The app browses installed wallpapers, applies them per monitor, edits their user properties live, and configures the daemon. It is a client of the daemon (`lumenwall`) over `lumenwall-ipc`; it never renders wallpapers itself. Audience: players and desktop users on Linux and Windows who want an animated desktop without reading a manual.

Screenshots (generated demo library, no Workshop content): [`docs/ui-screenshots/`](ui-screenshots/).

## Principles

- **Costs nothing when idle.** No redraw unless something changes: input, a daemon event, or a visible animated preview reaching its next GIF frame. Every subscription (animation tick, frame clock, pointer tracking, housekeeping timer) exists only while it is needed. The tray is a separate tiny process; closing the window frees everything.
- **Never stuck.** Without the daemon the app opens in *demo mode*: the library stays browsable, a banner and the status bar say the engine is not running and offer to start it.
- **Explain, don't fail.** Every empty or error state has an illustration, says what happened and what to do next.
- **Native and permissive.** 100 % Rust drawing its own widgets (iced): no webview, no HTML. Only licences accepted by `deny.toml`.
- **Own look.** Generic UX conventions (grid library, details panel, monitor map); no Wallpaper Engine icons, images or visual identity. Icons and illustrations are drawn for Lumenwall (`crates/lumenwall-ui/src/icons.rs`).

## Layout

```
╭─────────────────────────────────────────────────────────────────────────────╮
│ ◓ Lumenwall              Library (title band, Linux)                 – □ ✕ │
├─────────┬──────────────────────────────────────────────┬────────────────────┤
│         │ Library              Sort [Name▾] [⌕ search…] │ Details          ✕ │
│         │ 32 wallpapers ready                            │ ┌────────────────┐ │
│ Library │ (All)(Scene)(Video)(Web) [Any tag▾] [Any res▾] │ │  big preview   │ │
│ Screens │ ┌──────────────┐ ┌──────────────┐              │ └────────────────┘ │
│ Settings│ │ 16:9 preview │ │ ✓ On screen 1│              │ Title, type · res  │
│         │ │▓▓ Title ▓▓▓▓▓│ │▓▓ Title ▓▓▓▓▓│              │ (tag)(tag)         │
│         │ └──────────────┘ └──────────────┘              │ APPLY  [map 1][2]  │
│ Ctrl F  │        … only the rows on screen are built     │ Fill Fit Stretch   │
│ ← → ↵ ⎋ │                                                │ [   ✓ Apply    ]   │
│         │                                                │ CUSTOMIZE …        │
├─────────┴──────────────────────────────────────────────┴────────────────────┤
│ ● Engine running   ◔ 30 fps  CPU 0.8 %  GPU 2.9 %  Engine RAM 92 MB   ⏸ 🔈 ──○ │
└──────────────────────────────────────────────────────────────────────────────┘
```

- **Library** (the heart): large 16:9 previews edge to edge with rounded corners, a dark gradient with the title and resolution/tags over the image, a type badge (scene, video, web), an accent "On screen 1, 2" badge, warning badges when it cannot play, a soft hover wash and an accent glow on the selection. Previews animate on hover by default. Instant search (title, tag, Workshop id), type chips, tag and resolution filters, sort.
- **Details panel** (slides in when a wallpaper is selected): big preview, title, metadata, tag chips (click to filter), *Apply* card with the monitor map, *All screens*, scaling (fill / fit / stretch, with tooltips) and one primary button, then *Customize*: sliders with their value, colour swatches with an inline editor (presets, RGB, hex), switches, lists, text fields, file pickers, *Reset*. Changes show live on the desktop and are saved 0.7 s after the last change.
- **Workshop**: community wallpapers for Wallpaper Engine on Steam (see [Workshop](#workshop) below): browse with the user's own Steam Web API key, subscribe through the Steam client, new subscriptions appear by themselves with a *New* badge.
- **Screens**: the monitors drawn to scale with the wallpaper of each, then one card per screen (status, scaling, fps, *Change*, *Remove*). A click on a screen opens the library with only that screen targeted.
- **Settings**: cards for performance (fps limit with presets, pause on fullscreen / battery / lock), sound, startup, installation (what is set up for this account, Repair, Uninstall), library folders and detected Steam / Wallpaper Engine paths, appearance (language, theme, preview animation), about.
- **Installation dialog**: at the first start, over the window (dimmed backdrop, elevation 3), when something is missing for this account: one tickable row per item (programs, menu entry and icons, start with the session, GNOME extension) with its state as a pill, "Log out and back in" note when the extension is ticked, "Not now" (ghost) and "Set up" (primary). Opens by itself once, again only for a newer embedded extension; never in development builds nor after the Windows installer. Details: [`crates/lumenwall-ui/README.md`](../crates/lumenwall-ui/README.md#installation-dialog).
- **Status bar** (always visible): engine state, real fps (per screen × number of screens), CPU, GPU, engine RAM, the app's own RAM, pause, mute, volume; *Start engine* when it is off. Performance is the product's selling point.
- **Tray** (`lumenwall-ui --tray`, separate process, ~10 MB, no GPU): open, pause/resume, mute, start engine, quit. Linux: StatusNotifierItem + dbusmenu over D-Bus (KDE, most panels, GNOME with the AppIndicator extension, which Ubuntu ships). Without a tray host everything stays reachable from the window. The glyph is the brand's monochrome sun, outlined while paused or stopped: on Linux `IconName` `lumenwall-symbolic` / `lumenwall-paused-symbolic` when both are in the `hicolor` theme (the panel recolours them), else the embedded `assets/brand/tray/png` glyphs as `IconPixmap` (white on GNOME-family and dark panels, black when the portal says light elsewhere); on Windows the white or black PNG for the taskbar theme (`SystemUsesLightTheme`, followed live through a registry change notification) at the system DPI (16/20/24/32 px).
- **Window frame (Linux)**: the window opens undecorated and transparent and draws its own title band in the sidebar's colour (`chrome.rs`): brand mark, page title, and the window buttons the desktop shows on its side (`org.gnome.desktop.wm.preferences button-layout`, else GTK's `gtk-decoration-layout`). A press on the band moves the window, a double press (400 ms) maximises or restores it, a right press opens the window menu; 4 px edges and 12 px corners resize it; 12 px rounded corners and a hairline outline, dropped while maximised or full screen. Windows and macOS keep the native title bar.
- **App icon**: the window icon comes from `assets/brand/icon/png/lumenwall-{32,64}.png`; on Windows `build.rs` embeds `lumenwall.ico` (with a version resource, "Lumenwall" in Task Manager) in `lumenwall-ui.exe` and `lumenwall.exe`, and the window then takes the hinted sizes at its DPI. Linux: `packaging/linux/lumenwall-ui.desktop` (menu entry, `Icon=lumenwall`, app id `lumenwall-ui`) and the `hicolor` icons in `packaging/linux/icons`.

## Workshop

Lumenwall is **not a Steam application**: no Steamworks, no `steam_api`, and it never subscribes on the user's behalf nor presents itself as Wallpaper Engine. The Workshop of app 431960 is only *read* through Steam's public Web API.

- **Subscribe** opens the item in the Steam client (`steam://url/CommunityFilePage/<id>`), or in the browser (`https://steamcommunity.com/sharedfiles/filedetails/?id=<id>`) when no Steam installation was found. The UI says next to the button that owning Wallpaper Engine is required and that Steam does the download.
- **New subscriptions**: the `workshop/content/431960` folders are watched through file events (`notify`: inotify, ReadDirectoryChangesW), never polled. A new item folder gets its own watch; once it has been quiet for 2 s with a readable `project.json` it joins the library with a *New* badge, a "Just arrived" strip and a toast. Items newer than the previous session are *New* too. A deleted folder (unsubscribed) leaves the library.
- **Browsing** (`IPublishedFileService/QueryFiles`, needs the user's **personal** key): trending (7 days), most popular (total subscribers), most recent, text search (debounced 450 ms), type chips, genre tag, resolution tag, cursor pagination as the user scrolls ("Load more" too). Only "Everyone" items by default; "Questionable" and "Mature" are separate opt-ins in Settings and get a visible badge. Cards: 16:9 preview animated on hover, title, author, rating, subscribers, type, *Installed* (or *New*) badge.
- **Details panel**: big preview and up to 3 screenshots (click to enlarge), author and profile link, 5-star rating with vote count, subscribers, favourites, size, last update, tags (click to filter), *Subscribe in Steam* or, when installed, *Apply* and *Show in library*, *Open in browser*, and the description: Steam BBCode rendered as plain rich text (headings, bold/italic/underline/strike, lists, quotes, safe `http(s)`/`steam:` links; images, embeds and unknown tags dropped, no HTML).
- **Without a key** the page stays useful: an explanation with *Add my key* / *Get a key* / *Open in browser* / *Browse in Steam*, the "Just arrived" strip, and the installed Workshop wallpapers enriched by `ISteamRemoteStorage/GetPublishedFileDetails` (POST, no key): subscribers, size, update date, description. Names and ratings need the key.
- **Key**: entered in Settings (masked field, *Get a key* opens https://steamcommunity.com/dev/apikey, explanation of where it goes). Stored only in `<config>/lumenwall/steam-web-api-key` with mode 0600 on Unix (tightened if a hand-made file is looser), never in `ui.json`, never logged (requests log the endpoint only; HTTP errors keep only their status), never part of a cache file name. Lumenwall ships no key.

**Network rules.** Requests go only to `api.steampowered.com`, `steamcommunity.com` and Steam's image CDNs (`*.steamusercontent.com`, `*.steamstatic.com`, `steamuserimages-a.akamaihd.net`), over HTTPS with the system certificate store (rustls + `rustls-platform-verifier`, ring provider); any other URL, including image URLs returned by the API, is refused. User-Agent `Lumenwall/<version>`, nothing else about the user. Timeouts 6 s (connect) / 20 s (total), 8 MB JSON and 16 MB image limits. The network worker exists only while the Workshop page is displayed (its subscription is dropped when the user leaves, and queued downloads are cancelled): no request can start from another page. Disk cache with TTL: query results 15 min, item details 6 h, author names 24 h, images 7 days. Offline, rate limit (HTTP 429), rejected key (401/403) and other errors each have their own message and *Retry*; what was loaded stays visible.

Requests per action, counted on the fake server (cold cache, 1360×860 window):

| Action | Requests |
|---|---|
| Open the page (key) | 1 `QueryFiles` + 1 `GetPlayerSummaries` + 1 preview per card on screen and one row ahead (9 here; installed items use their local preview) + 1 `GetPublishedFileDetails` per 100 installed items (then cached 6 h) |
| Open it again within 15 min | 0 |
| Open an item | its screenshots (2–3); the preview is already cached |
| Change sort, filter or search / reach the end of the list | 1 `QueryFiles` + 1 `GetPlayerSummaries` + previews of the new cards on screen |
| Open the page without a key | 1 `GetPublishedFileDetails` (POST) per 100 installed items, no image (local previews) |
| Any other page | 0 |

Cost of the page (same machine, wgpu): idle 33 MB private / 201 MB RSS, the same as the library (+1 MB); +2.2 % CPU while the selected item's GIF plays; private memory flat (35 MB) over 45 s with every visible Workshop GIF animated. One worker thread while the page is shown, none otherwise.

**Testing without a key or the Internet.** `lumenwall-ui --fake-steam` (with `--no-key` for the keyless page) starts a local HTTP server that answers the three endpoints in their documented JSON shapes, with 48 made-up items, generated GIF previews and screenshots, and a made-up key (in memory only). `cargo test` runs the client end to end against it (pagination, filters, caching, rejected key, host allow-list, downloads) and the folder watcher against real file events. The screenshots in `docs/ui-screenshots/workshop-*.png` come from it.

## Practical

- Apply in one click (*Apply*), a double-click on a card, `Enter`, or by **dragging a card onto a screen** of the monitor map.
- Keyboard: `Ctrl+F` search, arrows move the selection (grid-aware, scrolls into view), `Enter` applies (also from the search field: first result selected, then applied), `Esc` cancels a drag, clears the search, closes the colour editor, then the panel.
- Remembered between runs: window size, targeted screens, filters, sort, scaling, preview mode, theme, language, extra folders (`<config>/lumenwall/ui.json`).

## Design system (`crates/lumenwall-ui/src/theme.rs`)

- Tokens for dark (default) and light, values from [BRAND.md](BRAND.md): background, sidebar, surface, raised, overlay, border, text / muted / faint, accent (warm amber) for fills, accent-hover, accent-soft, `accent_text` (the accent as text or icon: darker in the light theme), `on_accent`, success, warning and `on_warning`, danger and `on_danger` (error toasts, red badges). Every text pair is at least 4.5:1 (WCAG AA) in both themes; `cargo test -p lumenwall-ui text_contrast -- --nocapture` checks and prints them.
- Spacing 4 / 8 / 12 / 16 / 24 / 32; radii 6 / 10 / 14 / pill; three elevation levels of soft shadows; type scale 28 / 20 / 16 / 14 / 12.5 / 11 in regular, medium and semibold; icon sizes 14 / 16 / 20 / 24 (`theme::icon`), 28 for the brand mark.
- Motion (`theme::motion`): 100 / 200 / 220 / 320 ms (press, hover wash, details panel, toasts). A tick subscription exists only while something moves. The system's "reduce animations" setting (GNOME `enable-animations` and KDE's animation speed through the settings portal, else `gsettings` / `kdeglobals`; Windows "Animation effects") is read at start and when the window regains focus; when on, every duration is zero. `LUMENWALL_UI_REDUCE_MOTION=1|0` overrides it. Preview animation stays the user's choice in Settings.
- Font: the system UI font that has real static weights (Inter if installed, Segoe UI on Windows, then Ubuntu, Lato, Noto Sans…). Variable-only fonts are skipped: iced's text engine sees one face and would fake bold with another family.

## States worth designing

| Situation | What the user sees |
|---|---|
| Daemon not running | Banner + status bar *Start engine*; library browsable; *Apply* disabled with the reason |
| `lumenwall` binary not found / started but silent | Status bar explains where it is looked for / that it does not answer |
| No Steam found | Illustration, explanation, *Add a folder*, list of the places checked (`~`-shortened) |
| Steam found, no wallpapers | Illustration, *Open the Workshop*, *Add a folder* |
| Wallpaper Engine not installed | Warning notice; scene cards badged "Needs Wallpaper Engine"; *Apply* disabled with the reason |
| Unsupported (application, Workshop asset, unknown type) | Red badge and an explanation in the panel |
| Search/filters match nothing | Illustration + *Clear filters* |
| Daemon reports an error for a screen | Red status and message on the screen card |
| Workshop without key | Explanation, *Add my key*, *Browse in Steam*, installed items with their Workshop details |
| Steam unreachable / rate limited / key refused | Illustration or notice naming the cause, *Retry*, loaded results kept |

## Performance design

- **Virtualized grid**: only the rows inside the viewport (plus one) are built; spacers stand for the rest. 1000 wallpapers cost the same as 12.
- **Previews** are decoded on two background threads, cropped to 16:9 (cards and panel never clip at draw time, 44 % less memory than square previews), at most 480 px wide (320 for GIFs), cached on disk (`<cache>/lumenwall/thumbs`, LZ4). Animated frames stay LZ4-compressed in memory under a 96 MB budget (least recently seen evicted, reloaded from disk).
- **Animation**: a tick subscription exists only while a preview animates, at a period quantized from the GIF delays; a frame is swapped only after the previous one was actually drawn. Two iced 0.14 behaviours made this necessary (see `view/drawn.rs` and `previews.rs`): images are cached when *measured* (every update) but freed only when a frame is *drawn*, and re-creating a timer subscription with a new period on each tick leaked memory. Measured: anonymous memory flat over 45 s with every visible GIF animating.
- **Daemon link**: one blocking reader thread; events arrive through an iced subscription. No reconnect timer: the app retries when its window gains focus or when the user asks. Engine stats are subscribed while the window exists; the daemon samples them only while a client asks.

## Framework

The user chose **iced** (MIT); the first prototype used egui. Constraints: permissive licence, Linux Wayland + X11 and Windows, 100 % native Rust drawing, light, idle without redraw, smooth previews, keyboard accessible.

iced 0.14 (latest release, December 2025) is configured as: `wgpu-bare` with only the Vulkan backend (and DX12 on Windows), so no OpenGL driver is loaded; `tiny-skia` (CPU renderer) as automatic fallback when no GPU adapter works (the "Vulkan ICD of the iGPU only" case that failed in the first prototype now falls back instead of exiting); `smol` executor (one runtime thread instead of one per core). `LUMENWALL_UI_RENDERER=software` forces the CPU renderer. iced 0.14 is built on **wgpu 27** while the engine uses wgpu 30: two wgpu copies in the workspace until iced moves on (the UI is a separate binary, so no runtime conflict).

Measurements (release build, 1360×860 window, after 6 s, CPU over the next 10 s; Ubuntu GNOME Wayland, RTX 4080 SUPER, NVIDIA driver; the "file" part of RSS is the GPU driver's shared libraries):

| Build / situation | RSS | private (anon) | threads | CPU |
|---|---|---|---|---|
| iced, wgpu Vulkan, idle, 32 real wallpapers | 228 MB | 61 MB | 18 | 0.0 % |
| iced, wgpu Vulkan, 1 GIF playing (selection) | 211 MB | 44 MB | 18 | 2.5 % |
| iced, wgpu Vulkan, every visible GIF playing (demo, 6) | 201–239 MB | 35–72 MB | 18–23 | 1.9–3.8 % |
| iced, tiny-skia (CPU), idle | 42–72 MB | 15–46 MB | 9–10 | 0.0 % |
| iced, tiny-skia (CPU), 1 GIF playing | 57 MB | 31 MB | 10 | 24–29 % |
| tray process (`--tray`) | 10 MB | 1 MB | 5 | 0.0 % |
| *earlier prototypes, empty window:* egui/glow · egui/wgpu · iced default features | 128 · 192 · 214 MB | 19 · 39 · 31 MB | 4 · 11 · 45 | 0 · 0.1 · 0.1 % |

The CPU renderer is the lightest at rest but redraws too much for animations, so wgpu is the default. Binary: 16.1 MB (release, fat LTO, stripped).

**Keyboard and accessibility gaps (iced 0.14):** no screen-reader support (no AccessKit yet); `Tab` only moves between text fields, not buttons or cards; no visible focus ring on cards (the selection outline plays that role); no `Ctrl+click` multi-select on the monitor map (a click toggles a screen instead).

## Licence and advisory notes for `deny.toml`

- Licences: `cargo deny check licenses` passes with all UI dependencies.
- No font is embedded. Shipping Inter (OFL-1.1) would need `OFL-1.1` in `[licenses] allow` (or an exception) and a third-party notice; the system font is used meanwhile.
- `cargo deny check advisories` reports, through iced 0.14 and zbus 5 (to decide in `deny.toml`):
  - RUSTSEC-2026-0253 `lru` 0.16 (unsound `pop()` if a key's `Drop` panics), via cryoglyph → iced_wgpu. Its keys are glyph-cache keys whose `Drop` never panics. Fixed by iced moving to `lru` ≥ 0.18.2.
  - RUSTSEC-2026-0206 `rustybuzz`, RUSTSEC-2026-0192 `ttf-parser` (unmaintained), via cosmic-text / fontdb / usvg: the text stack of every iced 0.14 renderer.
  - RUSTSEC-2026-0370/0371/0372 `zvariant`, `zbus_names`, `zvariant_derive` 5.x (unmaintained, superseded by zbus 6), via `lumenwall-ui`'s tray/portal code and iced's Linux theme detection (`mundy`). Moving to zbus 6 and dropping iced's `linux-theme-detection` (reading the XDG `color-scheme` setting directly) would remove them.

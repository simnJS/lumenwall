<p align="center">
  <img src="assets/brand/banner/lumen-banner.png" alt="Lumen: animated wallpapers that sleep when you can't see them" width="100%">
</p>

<p align="center">
  <a href="https://github.com/simnJS/lumen/actions/workflows/ci.yml"><img src="https://github.com/simnJS/lumen/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-ffb347?labelColor=20232c" alt="License: MIT OR Apache-2.0"></a>
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20Windows%20%28preview%29-ffb347?labelColor=20232c" alt="Platforms: Linux, Windows (preview)">
  <img src="https://img.shields.io/badge/rust-1.90%2B-ffb347?labelColor=20232c" alt="Rust 1.90 or later">
</p>

<p align="center"><b>English</b> · <a href="README.fr.md">Français</a></p>

**Lumen plays Wallpaper Engine Workshop scenes as your desktop wallpaper on Linux and Windows, with a native Rust engine that draws a frame only when something on screen changes, and nothing at all when the desktop is hidden.**

> [!NOTE]
> Lumen is in early development and has no release yet. Scene wallpapers run on Linux today. Video and web wallpapers, the desktop app's connection to the engine, and Windows at runtime are not done: see [Features](#features).

## Contents

- [Screenshots](#screenshots)
- [Performance](#performance)
- [Features](#features)
- [Installation](#installation)
- [Usage](#usage)
- [FAQ](#faq)
- [Architecture](#architecture)
- [Contributing and clean-room rules](#contributing-and-clean-room-rules)
- [License](#license)
- [Legal](#legal)

## Screenshots

The desktop app, `lumen-ui`, with its generated demo library (`lumen-ui --demo-library --fake-daemon`). No Workshop content is shown.

<p align="center">
  <img src="docs/ui-screenshots/library-light-en.png" alt="Library page in the light theme: grid of wallpaper previews, details panel with the screen map and the Apply button" width="49%">
  <img src="docs/ui-screenshots/screens-dark-en.png" alt="Screens page in the dark theme: two monitors drawn to scale with the wallpaper of each" width="49%">
</p>

More in [`docs/ui-screenshots/`](docs/ui-screenshots/); design notes in [`docs/UI.md`](docs/UI.md).

## Performance

Performance comes first: a wallpaper that looks the same but costs more is treated as a regression ([`docs/PERFORMANCE.md`](docs/PERFORMANCE.md)). The figures below come from the crate READMEs, where each one is recorded with its method.

**Machine and conditions.** AMD Ryzen 9 7950X3D, NVIDIA GeForce RTX 4080 SUPER (proprietary driver, Vulkan), Ubuntu 26.04, GNOME 50.1 Wayland session (Lumen draws through XWayland there). Release build, 30 fps cap. Reference wallpaper: a 3440×1440 Workshop scene with 11 effects, 4 particle systems and a sound ([`docs/TESTING.md`](docs/TESTING.md)).

| Situation | Measured | Source |
|---|---|---|
| Desktop wallpaper on two 2560×1440 screens | 30.0 fps, **1.6 % of one CPU core**, 240 MiB RSS (49 MiB private, the rest is driver libraries and the mapped archive), 268 MiB VRAM | [lumen-engine](crates/lumen-engine/README.md#measurements-rtx-4080-super-vulkan-release-reference-wallpaper-34401440-scene) |
| One frame, 1720×720 window | CPU 0.4–0.6 ms; GPU **0.5 ms** at full clocks (headless), 0.8–4.7 ms when the GPU stays at its low idle clocks | lumen-engine |
| Loading the scene (13 textures, 23 shader passes from the cache, 4 particle systems) | **80–230 ms** | lumen-engine |
| Fullscreen window on top | Paused: 0 frames, 0.2 % of one core (sound thread) | lumen-engine |
| Screen covered by a fullscreen Wayland window (GNOME) | Pauses ≈1.3 s after the window appears, resumes ≈30 ms after it closes; while covered 0 frames, ≈2 wake-ups per second | [lumen-platform](crates/lumen-platform/README.md#gnome-wayland-session-x11-windows-through-xwayland) |
| Idle, no frame requested (X11, window mode) | **0 wake-ups** of the event thread over 3 s | [lumen-platform](crates/lumen-platform/README.md#performance) |
| Steady-state frame | **0 heap allocations** in Lumen's per-frame paths: renderer update, particles, puppets, scripts (counting-allocator tests; wgpu allocates internally when recording commands) | [lumen-render](crates/lumen-render/README.md), [lumen-particles](crates/lumen-particles/README.md), lumen-engine |
| Puppet animation, 16 skeletons and 78 bones | 8.5–9 µs of CPU per frame | lumen-engine |
| SceneScript, 20 scripts and 7 text layers | 10–18 µs per frame | lumen-engine |
| Translating the 23 shader passes of the scene | 1.9 ms on 32 threads; ≈30 µs per pass from the disk cache | [lumen-shader](crates/lumen-shader/README.md) |
| Desktop app idle, 32 wallpapers / tray process | 0.0 % CPU / 10 MB RSS | [docs/UI.md](docs/UI.md) |

Measure on your machine with `LUMEN_STATS=1 lumen <id>`: fps, CPU and GPU time per frame and memory, every 5 seconds.

## Features

✅ works and was tested on the development machine · 🚧 partial or in progress · ⏳ not started, or not tested yet

**Wallpapers**

| | Feature | Notes |
|---|---|---|
| ✅ | Scene wallpapers | Images with their effect chains and the wallpaper's own shaders, bloom, colour LUTs, camera parallax, user properties (`--set-property`, live reload) |
| ✅ | Particles | Sprites, trails, ropes, child systems |
| ✅ | SceneScript | Property scripts, timers, events, cursor. 🚧 Creating layers at run time is not supported yet |
| ✅ | Puppet animation | Skeletons, animation layers, GPU skinning, attachments. 🚧 No physics, inverse kinematics or blend shapes |
| ✅ | Text layers | The wallpaper's font, scripted text such as clocks. 🚧 No outline, shadow or background box |
| ✅ | Sound and audio-reactive effects | Scene sounds, audio spectrum captured from PulseAudio (or PipeWire's Pulse server), automatic mute when another app plays |
| 🚧 | Video wallpapers | In progress: hardware decoding with zero-copy upload is designed ([`docs/research/platform.md`](docs/research/platform.md)); the `lumen-video` crate is still empty |
| 🚧 | Web wallpapers | Chromium (CEF) host and the Wallpaper Engine web API written, not built or connected yet |
| 🚧 | Playlists | Only the first item plays |
| 🚧 | One wallpaper spanning several screens | Each screen shows the whole wallpaper |

**Desktop**

| | Feature | Notes |
|---|---|---|
| ✅ | GNOME (Wayland session), no extension | X11 desktop windows through XWayland; pauses under fullscreen and covering windows (measured on GNOME 50.1) |
| ✅ | X11 | Tested through XWayland only, not yet on a native Xorg session |
| 🚧 | Wayland layer-shell: KDE Plasma, sway, Hyprland, niri, COSMIC | Implemented, not yet tested on these compositors |
| ⏳ | Windows 10 and 11 | Builds and passes the tests in CI (DX12 on the WARP software adapter); never run on a real Windows desktop |
| ✅ | Pause when hidden | Fullscreen and covered screens measured; screen lock and screen off implemented, not verified |
| ✅ | Configuration file with live reload | fps and properties apply without restarting |
| 🚧 | Desktop app (`lumen-ui`) and tray | Library, screens, settings, live property editing; works with the demo daemon only, because `lumen` does not answer the app's protocol yet |
| 🚧 | Workshop browser in the app | In progress |
| ⏳ | Packages (Flatpak, AUR, Windows installer) | None yet: build from source |

## Installation

**You need:**

- **Wallpaper Engine**, bought and installed through Steam, with the wallpapers you want subscribed in the Workshop. Lumen reads Workshop items and Wallpaper Engine's `assets` folder in place and ships none of their files. On Linux, install Wallpaper Engine with Steam Play (Proton) enabled for it; Lumen never runs it.
- **Rust 1.90 or later** ([rustup](https://rustup.rs)).
- **A Vulkan driver** on Linux (Mesa or NVIDIA), DirectX 12 on Windows.
- **Linux build libraries**, from the dependency tree (C compiler for QuickJS, ALSA, PulseAudio):
  - Debian, Ubuntu: `sudo apt install build-essential pkg-config libasound2-dev libpulse-dev`
  - Fedora: `sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel pulseaudio-libs-devel`
  - Arch: `sudo pacman -S base-devel alsa-lib libpulse`

  CI installs a larger set for every crate and feature: [`.github/actions/setup-rust/action.yml`](.github/actions/setup-rust/action.yml).

**Install from the repository** (the `lumen` crate on crates.io is an unrelated project):

```bash
cargo install --locked --git https://github.com/simnJS/lumen lumen
cargo install --locked --git https://github.com/simnJS/lumen lumen-ui   # optional: desktop app preview
```

**Start with the session (Linux).** Choose the wallpaper in the configuration file, then copy the autostart entry and the icons:

```bash
git clone https://github.com/simnJS/lumen && cd lumen
mkdir -p ~/.config/lumen ~/.config/autostart ~/.local/share/icons
printf 'wallpaper = "<workshop-id>"\n' > ~/.config/lumen/config.toml
cp packaging/linux/lumen.desktop ~/.config/autostart/
cp -r packaging/linux/icons/hicolor ~/.local/share/icons/
```

GNOME, KDE, XFCE and other desktops start `lumen` at login from `~/.config/autostart`. Remove the file to stop. Editing `config.toml` while Lumen runs applies fps and property changes immediately.

**Build from source:**

```bash
git clone https://github.com/simnJS/lumen && cd lumen
cargo build --release --locked          # target/release/lumen and target/release/lumen-ui
```

The full set of checks (format, clippy, tests, GPU tests on a software adapter, `cargo deny`) is in [`docs/CI.md`](docs/CI.md).

## Usage

```bash
lumen <workshop-id>                         # on every screen; a Workshop id or a wallpaper folder
lumen ~/wallpapers/my-scene                 # a folder that contains project.json
lumen -r DP-1 -b <id-1> -r HDMI-A-1 -b <id-2>   # one wallpaper per screen
lumen --fps 60 --scaling fill <id>
lumen --list-properties <id>                # the wallpaper's user properties
lumen --set-property rain=0 <id>            # override one (repeatable)
lumen -w 0x0x1280x720 <id>                  # in a window, for testing
LUMEN_STATS=1 lumen <id>                    # fps, CPU/GPU time per frame, memory
```

| Option | Effect |
|---|---|
| `-r`, `--screen-root <output>` | Desktop mode on this screen; following options apply to it. Repeatable |
| `-b`, `--bg <id or path>` | Wallpaper of the current screen |
| `--scaling stretch\|fit\|fill\|default` | How the wallpaper fills the screen |
| `-f`, `--fps <n>` | Frame-rate cap (default 30) |
| `-v`, `--volume <0–128>` / `-s`, `--silent` | Sound volume (default 15) / mute |
| `--noautomute` | Keep sound while another application plays audio |
| `--no-fullscreen-pause` | Keep rendering under fullscreen windows |
| `--disable-particles`, `--disable-mouse`, `--disable-parallax` | Turn features off |
| `--assets-dir <dir>` | Wallpaper Engine `assets` folder, when it is not found automatically |
| `--screenshot <file.png>` | Save a frame after a few frames |
| `--config <file>`, `--no-config` | Use another configuration file, or none |

Every option, the configuration file format (`~/.config/lumen/config.toml`, `%APPDATA%\Lumen\config.toml` on Windows) and the exit codes are in [`crates/lumen/README.md`](crates/lumen/README.md). The desktop app's options are in [`crates/lumen-ui/README.md`](crates/lumen-ui/README.md).

## FAQ

**Do I need to own Wallpaper Engine?**
Yes. Workshop wallpapers are downloaded by Steam into Wallpaper Engine's Workshop folder, and scenes use shaders and materials from Wallpaper Engine's `assets` folder. Lumen reads those files where Steam put them and contains none of them.

**Is Lumen affiliated with Wallpaper Engine or Valve?**
No. Lumen is an independent open-source project, not endorsed by the developers of Wallpaper Engine or by Valve.

**Does it work on GNOME without an extension?**
Yes. GNOME has no layer-shell, so Lumen uses X11 desktop windows through XWayland and detects covered screens from the compositor's frame timing. Measured on GNOME 50.1 with an NVIDIA GPU.

**KDE Plasma, sway, Hyprland?**
The layer-shell backend is written but has not run on those compositors yet. Reports are welcome (use the bug template).

**Windows?**
Lumen builds and passes its tests on Windows in CI, but has never run on a Windows desktop. The checklist to verify is in [`crates/lumen-platform/README.md`](crates/lumen-platform/README.md#to-check-on-windows-11-24h2--25h2-and-once-on-windows-10).

**What does it cost while I play a game?**
A fullscreen window pauses rendering: 0 frames, 0.2 % of one core measured (the sound thread). `--no-fullscreen-pause` turns that off.

**Why does the app say the engine is not running?**
The desktop app talks to the engine over a local socket, and the engine side of that protocol is not written yet. Until then the app is a preview that runs against its demo daemon.

**Is Lumen based on another Wallpaper Engine reimplementation?**
No. It is written from behavioural specifications under a clean-room process, which is what allows the MIT OR Apache-2.0 licence ([`CLEANROOM.md`](CLEANROOM.md)).

## Architecture

```mermaid
flowchart TB
    ui["lumen-ui<br/>desktop app and tray"] --> ipc["lumen-ipc<br/>local socket protocol"]
    ipc -. "engine side not written yet" .-> cli["lumen<br/>command line, config, lifecycle"]
    cli --> engine["lumen-engine<br/>frame loop, integration"]
    engine --> assets["lumen-assets<br/>files, Steam discovery"]
    assets --> formats["lumen-formats<br/>.pkg, .tex, .mdl"]
    engine --> scene["lumen-scene<br/>project, scene, materials"]
    engine --> shader["lumen-shader<br/>wallpaper shaders to GPU modules"]
    shader --> render["lumen-render<br/>wgpu renderer"]
    engine --> render
    engine --> particles["lumen-particles<br/>particle simulation"]
    engine --> script["lumen-script<br/>SceneScript runtime"]
    engine --> audio["lumen-audio<br/>sound, spectrum"]
    engine --> platform["lumen-platform<br/>Wayland, X11, Windows surfaces"]
    engine -. "not connected yet" .-> video["lumen-video"]
    engine -. "not connected yet" .-> web["lumen-web<br/>CEF host"]
```

Each crate's README lists the specifications it was built from, its measurements and its deviations from the reference behaviour ([`docs/PARITY-POLICY.md`](docs/PARITY-POLICY.md)).

## Contributing and clean-room rules

Contributions are welcome. Read [`CONTRIBUTING.md`](CONTRIBUTING.md) first; the essentials:

- **Clean room.** Code is written from the specifications in `specs/` and public documentation only. Never read, copy or adapt code from GPL or AGPL Wallpaper Engine reimplementations or their forks ([`CLEANROOM.md`](CLEANROOM.md)).
- **No Wallpaper Engine content in the repository**: no Workshop files, previews or assets, not even as test fixtures ([`docs/TESTING.md`](docs/TESTING.md)).
- **Performance is a feature**: no per-frame allocation, no polling, no work while hidden ([`docs/PERFORMANCE.md`](docs/PERFORMANCE.md)).
- **Permissive dependencies only**, enforced by `cargo deny` ([`deny.toml`](deny.toml)).
- Brand, colours, icons and screenshots follow [`docs/BRAND.md`](docs/BRAND.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Lumen by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

## Legal

Lumen is not affiliated with, endorsed by or sponsored by the developers of Wallpaper Engine or by Valve Corporation. Wallpaper Engine is a trademark of its owner. Steam and the Steam logo are trademarks and/or registered trademarks of Valve Corporation in the U.S. and/or other countries. Workshop wallpapers are the work of their authors; Lumen does not distribute or download them.

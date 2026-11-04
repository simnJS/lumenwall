# Lumenwall vs Wallpaper Engine: gap inventory

Report only, no code. Written from the repository at commit `9c6379c` (main), 2026-11-04.

**How to read it**

- **Status**: `done` · `partial` · `missing` · `different` (deliberately, see `docs/PARITY-POLICY.md`) · `not verified` (could not be checked from files or docs; nothing is guessed).
- **Effort** (a rough estimate, not a plan): S ≤ 2 days · M ≤ 2 weeks · L more.
- **Evidence** names a file, a spec section (`specs/NN` §x, written in French from the reference), a crate README section, or a test with a wallpaper id. Tests marked *(ignored)* need the person's Workshop content (`docs/TESTING.md`) and were **not run** for this report.
- **WE behaviour** comes from the specs and the public docs. `docs.wallpaperengine.io` and `help.wallpaperengine.io` describe mostly authoring; where a row's WE side is not in a spec or a public page it says "not verified".
- **Windows** column wording: the README says Windows was never run on a real desktop, while the task brief says it now runs on the person's machine (two bugs in progress). This report did not run anything, so Windows runtime is `not verified` unless a row says otherwise.
- Some crate README "Not done yet" rows are **stale** (they contradict the git log and other rows); they are flagged. See [Documentation drift](#documentation-drift).

**Already known, in progress (not re-analysed, not in the top 10):**
Windows video wallpapers refused (FFmpeg not built in; `crates/lumenwall-video/README.md` "Features", `crates/lumenwall-engine/README.md` "Not done yet"); Windows web wallpapers refused (CEF not built for Windows, no Direct3D shared-texture import; `crates/lumenwall-web/README.md`); static wallpaper on monitor 2 on the other GPU; NVIDIA overlay prompt.

## 1. The person's installed content

`C:\Program Files (x86)\Steam\steamapps\workshop\content\431960` holds 14 folders: **10 wallpapers** and 4 Workshop *assets* (shared effect/material/font packs: 2499516781, 2652493753, 2978738836, 2981960200; they are not wallpapers and Lumenwall lists them as unsupported on purpose, `lumenwall-scene` Deviations). The scenes bundle the assets they use inside their own `scene.pkg` (checked: the `workshop/<id>/…` effects of 3372348904 and 3463520581 are in the package), so no cross-item lookup is needed.

Features were read from `project.json` and from `scene.json` unpacked to a scratch folder outside the repository (nothing committed).

| Id | Title | Type | Features used | Linux | Windows |
|---|---|---|---|---|---|
| 1712475860 | Dino Run | scene (2D, 343×193) | 32 images, `scroll` ×21, `godrays`; 2 sound objects (jump, coin); 3 scripts incl. `createLayer`/`registerAsset` (coins, pickup particles) and `applyUserProperties`; props: 2 combos, 2 bools, colour | **partial**: layers and clock tested *(ignored)* `tests/runtime_layers.rs`, `tests/scene_clock.rs`; script `play()`/`stop()` of sound layers are neutral, so jump/coin sounds are likely silent (engine README "Sound layers") | not verified |
| 1960765686 | Desert at night 4k | scene (2D, 3840×2160) | 3 images, 23 particle systems, no effect, no script | **done, not verified individually**: every scene wallpaper loads and builds (`lumenwall-scene` *(ignored)* test over `$LUMENWALL_WE_WORKSHOP`); no render test names it | not verified |
| 2234978675 | Patrouille de France | scene (2D, 4288×2848) | 1 image, `clouds` effect | **done, not verified individually** (same as above) | not verified |
| 3018340632 | Boykisser, Blade Runner [reference scene] | scene (2D, 3440×1440) | 11 effects on one image (depthparallax, iris, shake ×3, foliagesway ×2, pulse, shine, localcontrast, chromaticaberration), 4 particle systems, 1 sound (5 mp3, `random`), bloom with `hdr: true`; props ash, rain, colour | **done**: reference wallpaper, measured (README "Performance", `tests/real_content.rs`). HDR flag effect not verified | not verified |
| 3044659344 | Porsche 911 GT3 | scene (2D, 4K) | 2 images, 1 text layer (clock script), 3 scripts (scale, clock `scriptProperties`); props: 24 h bool, invert, tints | **done**: `tests/text_content.rs` *(ignored)* | not verified |
| 3089056751 | Porsche (RUF) 911 930 | scene (2D, 4K) | same pattern: 2 images, 1 clock text, 2 scripts | **done**: `tests/text_content.rs` *(ignored)*; text position matches the WE preview within 2 px (engine README, Text) | not verified |
| 3372348904 | Soren <3 | scene (2D, portrait 3024×4032), `supportsaudioprocessing` | 9 images, 7 text layers, 1 particle system, 20 scripts, 6 bundled effects (`blurprecise`, `blend`, `blendgradient`, `spin`, `opacity`), audio visualizer (63 bars built with `createLayer`, `registerAudioBuffers`), media thumbnail/properties events | **partial**: visualizer and text tested *(ignored)* (`render_png --audio synthetic`, `tests/text_content.rs`); media events have no source (no MPRIS) so cover art/title stay empty | not verified (WASAPI capture "never run", `lumenwall-audio` README) |
| 3463520581 | Kirito x Asuna (SAO) 4K | scene (2D, 3840×2160) | 69 images, 64 `waterwaves` + opacity, shake, tint, `perspective`, 4 bundled Workshop effects (bokeh blur, bloom, scroll, waterripple), 16 puppets with animation layers (78 bones, 58 attached objects), 4 particle systems (leaves, gusts), wind/gravity, camera shake, 1 sound, 12 user props | **done**: 8 `.mdl` puppets skinned on the GPU, measured (engine README "Puppets", `tests/real_content.rs` *(ignored)*) | not verified |
| 3044696375 | JavaScript Clock | web | `clock.html`/`clock.js`, props: colour | **done** (CEF host built separately): `tests/web_content.rs` *(ignored)*, README measures 11.7 % of one core | **refused** (known) |
| 3655057993 | Boykisser Winter [4K] | video | 72 MB H.264 4K60 + AAC | **done**: 2.3–2.6 % of one core (engine README) | **refused** (known) |

Content counts used for ranking below: 8 of 10 are scenes, 1 web, 1 video. Scenes: 5 have scripts, 3 have text, 3 particles-heavy (1960765686, 3018340632, 3463520581), 1 puppets, 1 audio-reactive, 1 media events, 1 layer-creating script.

## 2. Wallpaper types

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Scene (2D) | Image/particle/sound/text objects, effect chains | done | README Features; engine README | — |
| Scene (3D / perspective) | Models, perspective camera, lights, reflections | partial (P1 done; P2/P3 not) | engine README "3D", "Not done yet"; `tests/scene3d_content.rs` *(ignored)*; spec 09 | L |
| Video | MP4/WebM looped, with sound | done on Linux; missing on Windows (known) | `lumenwall-video` README | in progress |
| Web | HTML page in an embedded browser | done on Linux; missing on Windows (known) | `lumenwall-web` README | in progress |
| Application (`.exe`) | Runs a program as wallpaper | missing, refused on purpose | `lumenwall-scene` README Deviations (`LoadError::Unsupported`) | not planned |
| Workshop assets / editor templates | Shared packs, not wallpapers | different: listed as "not a wallpaper" | `lumenwall-scene`, `lumenwall-ui` READMEs | — |
| Presets (WE "preset" items) | Saved property sets of another wallpaper | not verified (no spec mentions them) | — | — |

## 3. Scene rendering and effects

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Effect chains and wallpaper's own shaders (GLSL → WGSL) | Per-image passes, material constants | done | `lumenwall-shader` README; 3018340632 (11 effects, 23 passes) | — |
| Built-in effects used by the 10 wallpapers | scroll, godrays, shake, waterwaves, clouds, perspective, foliagesway… | done for those tested; `godrays`, `clouds`, `perspective` have no named test | 3018340632, 3463520581 tests; others not verified | S (verify) |
| Bloom | Threshold/strength/tint | done (WE shaders, ±2/255 vs CPU evaluation) | `lumenwall-engine` `bloom`, README "Performance" | — |
| HDR bloom (`hdr`, `bloomhdr*`) | HDR targets | not verified for 2D (render README Q23: "HDR… not implemented yet"); 3018340632 sets `hdr: true` | `lumenwall-render` README Q23; scene.json of 3018340632 | M |
| Colour LUTs / volume textures | Colour grading | done (deviation: no failure on volume `.tex`) | `lumenwall-render` Deviations | — |
| Camera parallax (mouse) | Depth shift | done, option `--disable-parallax` | README Features | — |
| Camera shake and fade | Scene setting | done for 3D scenes; 2D scenes with `camerashake` not verified (none of the 10 enable it except 3463520581's user-bound flag) | engine README "3D" (commit `bbe1125`) | S (verify) |
| Text layers | Fonts, scripted text | partial: no outline, shadow, background box, colour fonts, `blockalign`; string cannot grow past the reserved size | engine README "Text styling", "Text layer growth" | M |
| Video textures in scenes | `.mp4` as texture | missing (texture skipped) | engine README "Video textures"; spec 07 §14.2 | M |
| Render targets format (`fbos.format`, `unique`, global `_rt_*CompoBuffer*`) | HDR/other formats | partial (RGBA8 only for global ones) | `lumenwall-render` README Q23 | M |
| Image scaling modes (fill/fit/stretch) | Per output | done | `lumenwall` README `--scaling` | — |

## 4. Particles

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Emitters, operators, initialisers, sprites, trails, ropes | Full system | done | `lumenwall-particles` README coverage table; 3018340632 (4 systems), 3463520581 | — |
| Child systems, control points, boids, collisions | Documented | done (`collisionmodel` parsed and ignored, as the spec says) | `lumenwall-particles` README A6.3 | — |
| World-space particles in 3D | — | different (deviation 3D-33) | engine README | — |
| Particles attached to bones | Follow a skeleton bone | missing (images only) | engine README "Puppet extras" | M |
| Script effects on particle `origin`/`scale`/`angles` after creation | Live | partial: read at creation only | engine README "Script effects on fixed state" | S |
| Particle quality toggle | `--disable-particles` | done (all or nothing) | `lumenwall` README | — |

## 5. Puppets and 3D

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Puppet skeleton, animation layers, GPU skinning, attachments | Animated characters | done | engine README "Puppets" (16 skeletons, 78 bones, 8.5–9 µs/frame); 3463520581 | — |
| Bone physics, inverse kinematics, blend rules, blend shapes (`MORPHING`) | Used by some Workshop characters | missing ("no known format") | engine README "Puppet extras"; spec 08 §11 | L (format unknown) |
| Script bone/layer API | Scripts drive bones | partial: `ILayer` layer members yes, bones not wired | engine README "Puppet extras" | M |
| Puppet meshes of created layers | `createLayer` with puppet | missing | engine README "Created layers" | M |
| 3D models, lights (point), planar reflection, camera paths | WE 3D scenes | done (28 installed models in the corpus; not the person's content) | engine README "3D"; `tests/scene3d_content.rs` *(ignored)* | — |
| 3D fog, shadows, volumetrics, HDR, spot/tube/directional lights, model parallax, skeletal animation of models | WE 3D scenes | missing | engine README "3D: P2/P3 of spec 09" | L |
| 3D integer colour properties (`"1 0 0"`) | Reads as 0–1 per previews | partial: read as 0–255, so some 3D bodies come out black | engine README "Not done yet" (spec 02 Q1) | S |

None of the person's 10 wallpapers is 3D (all `orthogonalprojection` is set).

## 6. Scripting (SceneScript)

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Property scripts (`update`, `init`, `applyUserProperties`), timers, vectors, `scriptProperties`, text scripts | Documented API | done | `lumenwall-script` README; 5 of the person's scenes; µs costs in engine README | — |
| `createLayer` / `destroyLayer` / `sortLayer` / `registerAsset` | Runtime layers | done (pools; commit `752d39d`) | engine README "Layers created by scripts"; `tests/runtime_layers.rs` *(ignored)* (1712475860, 3372348904) | — |
| Created layers: puppet meshes, `.mdl`, scripts in the configuration | Supported | missing | engine README "Created layers" | M |
| Sound layers `play()`/`stop()` | Scripts trigger sounds | missing (neutral) | engine README "Sound layers"; Dino Run uses `jump` and `coin` | S |
| Camera/bloom/clear-colour and effect-`visible` writes, particle live writes | Live | partial (accepted, no visible effect) | engine README "Script effects on fixed state" | S–M |
| `engine.registerAudioBuffers` | FFT bands | done | `lumenwall-script` README host table; 3372348904 | — |
| Media events (`mediaThumbnailChanged`, `mediaPropertiesChanged`…) | Fed by the OS player | partial: API present, no source | engine README "Media events" | M (see §12) |
| `localStorage` | Persisted | partial: in memory only | engine README | S |
| Time limits | None | different: 100 ms tick/call, 1 s load | `lumenwall-script` README Deviations | — |

## 7. Web API (web wallpapers)

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Page loading from the wallpaper folder, `wp<id>://` scheme | CEF page | done (Linux) | `lumenwall-web` README | — |
| `wallpaperPropertyListener` (user properties, `applyUserProperties`), `wallpaperRegisterAudioListener`, pause/`setPaused` | Documented | done (Linux) | README Features; `tests/web_content.rs` *(ignored)*, 3044696375 | — |
| Mouse and click events | Page receives input | partial: clicks on GNOME XWayland desktop windows not delivered without the Shell extension (spec 07 QO-8) | `lumenwall-web` README "Not tested"; platform README | M |
| Media listeners (`wallpaperRegisterMediaStatusListener` and co.) | OS media info | partial: registered, never fed | engine README "Web media integration" | M (see §12) |
| Page audio under `--volume`/`--silent` | Yes | done; automute does not apply | engine README "Web page audio and automute" | M |
| Windows | — | missing (known) | — | in progress |
| `file://` user files, page audio output | — | not verified | `lumenwall-web` README "Not tested" | — |

## 8. Video

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| H.264 etc. playback, loop, sound | Yes | done on Linux (Vulkan Video zero copy, NVDEC, VA-API, D3D11VA, software) | `lumenwall-video` README; 3655057993 | — |
| Windows playback | Yes | missing (known) | — | in progress |
| HDR (PQ/HLG) tone mapping, 10-bit zero copy, BT.2020 primaries | Not verified in WE | missing | `lumenwall-video` README "Limitations" | M |
| Video inside scenes (§3 above) | — | missing | — | M |
| Audio/video loop drift | — | partial: independent loops drift over many loops | `lumenwall-video` README | S |
| Intel GPU, VA-API copy path, layer-shell compositors | — | not verified | `lumenwall-video` README | — |

## 9. Audio

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Scene sound objects (loop, random, volume) | Plays scene sounds | done | `lumenwall-audio` README; 3018340632 (`random`, 5 mp3) | — |
| Default volume | 15 | different: 64 | `lumenwall` README Deviations | — |
| Audio visualizer (spectrum capture, bands, smoothing) | Pulse monitor | done (Pulse/PipeWire-Pulse); Windows WASAPI loopback written, never run | `lumenwall-audio` README | M (verify Windows) |
| Automute (another app plays) | On by default | different: off by default; detection only counts media being played | `lumenwall` README Deviations; `lumenwall-audio` README | — |
| Automute on Windows | Yes | partial: WASAPI sessions written, "ducking notifications never run" | `lumenwall-audio` README "Not tested" | M |
| PipeWire-native backends | — | not verified (not compiled) | `lumenwall-audio` README | M |
| Silence while hidden | Sounds keep playing unless automute | different: suspended while every output is paused | `lumenwall-audio` README | — |
| Per-wallpaper volume | None | different (extension): `[wallpaper-audio]` | `lumenwall-ui` README "Sound" | — |
| Microphone input | Not verified in WE | missing | — | — |

## 10. User properties

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Property kinds (bool, slider, combo, color, text, file…) | All edited in WE | done in the model (`lumenwall-scene`); `file`, `directory`, `textinput`, `usershortcut` stored as text | spec 02 §3; `lumenwall-scene` README | — |
| Live editing, per-wallpaper saving | Properties panel | done | README Features; `lumenwall-ui` README; `--set-property` | — |
| Conditions, groups (e.g. 3463520581 `extra`, `vfxvramheavy`) | Show/hide rows | done in the model; UI rendering of groups/conditions not verified | `lumenwall-scene` Q8, Q30; UI not checked | S (verify) |
| Scheme colour (`schemecolor`, every one of the 10) | UI theme colour | not verified | `project.json` of all 10 | S |
| `usershadervalues` | Bound constants | done (3D passes; images not yet, would turn `retro` black) | `lumenwall-scene` README | S |

## 11. Playlists, slideshow, profiles

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Playlist object: order, timer, time-of-day, `updateonpause` | Spec 07 §17 | partial: parser and `PlaylistRunner` exist and are unit-tested but nothing calls the runner, so only the first item plays | `crates/lumenwall/src/playlist.rs` (runner used only in its own tests); README Features | M |
| Playlist transitions, slideshow GUI | Not verified in WE | missing (no UI for playlists) | `lumenwall-ui` README | M |
| Wallpaper per application / application rules | Exists (help.wallpaperengine.io index: "Select wallpapers per application", "Fix issues with specific apps") | missing; not specified in `specs/` | no spec, no code | L |
| Profiles / schedules | Not verified | missing | — | — |

## 12. Multi-monitor

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| One wallpaper per monitor, all monitors | Yes | done | `lumenwall` README `-r/-b`; `lumenwall-ui` Screens page | — |
| Hot-plug, per-monitor scale, fractional scaling | Not in the reference | done (Linux backends; Windows `WM_DISPLAYCHANGE` written) | `lumenwall-platform` README | — |
| One wallpaper spanning several monitors | Span | partial: each output shows the whole wallpaper | engine README "Span groups" | M |
| Negative virtual origin, monitor left/above primary on Windows | — | not verified | platform README Windows checklist item 1 | S (verify) |
| Static wallpaper on monitor 2 (other GPU, Windows) | — | bug in progress (known) | — | in progress |
| Wayland compositors other than GNOME (KDE, sway, Hyprland, niri, COSMIC) | — | not verified (written, not run) | README Features | M (verify) |

## 13. Pause rules

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Pause on fullscreen app | Yes (all outputs) | done, measured on GNOME 50.1; Windows `SetWinEventHook` written, not verified | `lumenwall-platform` README; README Performance | M (verify Windows) |
| Pause when a window is maximised | Yes | done (`MAXIMIZED`, per output); GNOME Wayland needs the Shell extension | platform README Coverage | — |
| Pause when covered/hidden, locked, screen off | Not in the reference | done (extra) | platform README | — |
| Pause on battery | Yes (a setting) | **missing**: IPC field exists, UI shows it, the daemon answers "pausing on battery is not supported yet" | `crates/lumenwall/src/daemon.rs:612`, `lumenwall-ipc/src/proto.rs:393` | S |
| Pause when not focused / other apps (`--fullscreen-pause-only-active`) | Yes | partial: Wayland only | `lumenwall` README | S |
| GPU released after a long pause | Not in the reference | done (extra) | `docs/PERFORMANCE.md`; engine README | — |
| NVIDIA overlay prompt | — | bug in progress (known) | — | in progress |

## 14. Performance settings

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| FPS cap | 15/24/30/60/… | done, default 30, live reload | `lumenwall` README `--fps` | — |
| Render only when something changes | Not in the reference | done (extra) | `docs/PERFORMANCE.md` | — |
| Quality presets: anti-aliasing (MSAA), texture resolution, reflection quality, post-processing | Per-user settings in WE's UI | missing: no knob; textures use anisotropy 8 and full resolution. WE's exact option list is not verified (public pages found cover troubleshooting only) | `lumenwall-formats` README; `lumenwall` README option table | M |
| Particles / mouse / parallax switches | Yes | done (CLI) | `lumenwall` README | S (UI exposure not verified) |
| Vulkan/GPU selection | Not verified | partial: remembers last driver; GPU choice for multi-GPU on Windows is a known bug | engine README `vulkan_driver` | — |
| Memory and wake-up budgets | — | done, documented | README Performance | — |

## 15. Media integration

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Now-playing info and cover for scripts/web (MPRIS on Linux, Windows media session) | Yes | **missing**: scene side (`media_changed`, `$mediaThumbnail` VFS source) and web listeners exist, no source feeds them | engine README "Media events", "Web media integration"; spec 07 §16 | M |
| Playback control (play/pause/next) from a wallpaper | Not verified | missing | — | M |

Used by 3372348904 (12 `mediaThumbnailChanged` references) and by the asset packs 2499516781 and 2978738836 (media player widgets).

## 16. Workshop and the UI

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Library (grid, search, tags, sort) | Yes | done | `docs/UI.md`; `lumenwall-ui` README | — |
| Workshop browse and search | In-app | done with the person's own Steam Web API key | `docs/UI.md` "Workshop" | — |
| Subscribe | In-app | different: opens the Steam client (Lumenwall is not a Steam app); new subscriptions appear by themselves | `docs/UI.md` | — |
| Properties editor | Yes | done (live) | `lumenwall-ui` README | — |
| Apply per monitor, scaling, volume | Yes | done | `docs/UI.md` | — |
| Tray, autostart, installer, self-update | Yes | done on Linux; Windows installer built in CI, never run on the person's machine per README (not verified) | README Installation; `lumenwall-update` | — |
| Playlist and schedule UI | Yes | missing | §11 | M |
| Windows UI | — | partial: "not yet tested on Windows" (README) | README Features | S (verify) |
| Upload / publish to Workshop | Yes | missing (needs Steamworks, deliberately outside scope) | `docs/UI.md` "not a Steam application" | out of scope |

## 17. Editor

Wallpaper Engine's editor (creating scenes, importing models, publishing) is **out of scope**: Lumenwall plays existing wallpapers (`CLEANROOM.md`, README). No code or spec for it exists. Status: `missing`, not planned. Effort: L+.

## 18. Windows versus Linux

| Area | Linux | Windows |
|---|---|---|
| Scene wallpapers | done, measured | not verified here; CI renders on WARP only (README Features). The person's running Windows build is evidence the task brief refers to |
| Desktop surface | GNOME/X11 done; Wayland layer-shell written, not run | WorkerW/Progman child written; checklist of 6 points open (platform README "To check on Windows 11") |
| Video | done | missing (FFmpeg not built in), in progress |
| Web | done | missing (no CEF build, no D3D import), in progress |
| Audio capture and automute | Pulse done; PipeWire-native not compiled | WASAPI written, never run |
| Pause rules | measured | written, not verified |
| UI, tray | done | built, "not yet tested on Windows" |
| Install and update | one-line installer, `.deb` | NSIS installer in CI, not run on a real machine (not verified) |
| Multi-GPU | not applicable here | known bug (monitor 2) |
| IPC daemon ↔ app (named pipe `\\.\pipe\lumenwall-ipc-<user>`, single instance, forwarding) | Unix socket, tests pass | **suspect**: per the orchestrator, 3 tests in `crates/lumenwall-ipc/tests/socket.rs` fail and 1 hangs on Windows (pre-existing; task "Make the workspace tests pass on Windows" in progress). Not reproduced here: my full `cargo test --workspace` timed out at 10 min, consistent with the hang |

## Top 10 to do next

Ranked by user impact = how many of the person's 10 wallpapers (W) and how much common Workshop content needs it. Counts are of the person's 10; "common Workshop content" is judgement, no catalogue statistics were collected (not verified). Windows items rank high because 9 of 10 wallpapers run through code that has not been verified on Windows.

| # | Item | Needs it (of the 10) | Why now | Effort |
|---|---|---|---|---|
| 1 | **Verify scenes end to end on the person's Windows machine**: run the platform checklist (§13 of `lumenwall-platform` README), the 8 scenes, UI, installer, and the named-pipe IPC (app ↔ daemon, single instance; tests failing, see §18); file every defect found | 8 scenes + UI | Everything else is moot if the main desktop does not show the 8 scenes correctly. Status today is "not verified" | M |
| 2 | **Windows audio**: run WASAPI loopback and automute on real playback | 3372348904 (visualizer) + any audio-reactive scene | Written, never run | S–M |
| 3 | **Sound layers from scripts** (`play`/`stop`) | 1712475860 (Dino Run: jump, coin); common in interactive/game wallpapers | Scripts already run; only the hook to the audio player is missing | S |
| 4 | **Wire playlists** (`PlaylistRunner` into the daemon/engine, then UI) | 0 of 10, but a headline WE feature for most users | Runner and parser exist and are tested; nothing calls them | M |
| 5 | **Media integration source** (MPRIS, Windows media session) feeding scenes and web | 3372348904 (+ asset packs 2499516781, 2978738836; web clocks/players) | API is ready on both sides, only the source is missing | M |
| 6 | **Pause on battery** (the daemon refuses it today) | 0 of 10, every laptop user | UI toggle exists but is rejected, which is a visible bug | S |
| 7 | **Text styling** (outline, shadow, background box, growth) | 3044659344, 3089056751, 3372348904 (all use text, clocks and titles); very common in Workshop clocks | Visible difference to WE on any styled text; unchecked on these 3 beyond position | M |
| 8 | **One wallpaper spanning several monitors** | 0 of 10, multi-monitor users | Each output shows the whole picture today | M |
| 9 | **Quality settings** (MSAA, texture resolution, reflection quality) and an HDR/`hdr` check | 3018340632 (`hdr: true`, bloom); low-end GPU users | No knob exists besides fps; HDR 2D effect not verified | M |
| 10 | **Video textures in scenes + created-layer puppets/`.mdl`** | 0 of 10, a minority of Workshop scenes (not measured) | Textures are skipped silently; decoding side is ready | M |

Not in the top 10 on purpose: 3D P2/P3, puppet IK/physics/blend shapes (no known format, L), per-application wallpaper rules (L, no spec), Wayland compositors beyond GNOME (verify when a user reports), editor, application wallpapers.

## Documentation drift

Rows in `crates/lumenwall-engine/README.md` "Not done yet" disagree with the code and other docs; fix them when touching that file (not done here, outside `docs/parity/`):

- "Web wallpapers: Load error (`lumenwall-web` not integrated)" and the README architecture diagram showing video and web as "not connected yet": video and web run in the engine (commits `e09dafc`, `c4e9c36`).
- "Playlists: only the first item is shown" is accurate; "`thisScene.createLayer`… neutral results" is stale (commit `752d39d`).
- Root `README.md` FAQ "Why does the app say the engine is not running?" says the engine side of IPC is not written; `crates/lumenwall/src/daemon.rs` implements it.
- Root `README.md` Windows row says "never run on a real Windows desktop"; the task brief says it now runs there.

## What was not checked

No Lumenwall or Wallpaper Engine process was run; no test was run (`*(ignored)*` tests need the content and a GPU); WE's own settings screen was not consulted (public pages reachable offered troubleshooting indexes only). Anything the sources do not state is marked "not verified".

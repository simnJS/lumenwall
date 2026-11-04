# Lumenwall vs Wallpaper Engine: gap inventory

Report only, no code. First written from the repository at commit `9c6379c` (main), 2026-11-04; brought up to date the same day with main at `101d8ea`, after Windows video, web (CEF), playlists, pause on battery, the media source, the album cover in scenes, the engine process, the minimal FFmpeg and the one-window installer landed. A row changed by that update names the commit, code or README that proves it; rows not touched were re-read and still hold.

**How to read it**

- **Status**: `done` · `partial` · `missing` · `different` (deliberately, see `docs/PARITY-POLICY.md`) · `not verified` (could not be checked from files or docs; nothing is guessed).
- **Effort** (a rough estimate, not a plan): S ≤ 2 days · M ≤ 2 weeks · L more.
- **Evidence** names a file, a spec section (`specs/NN` §x, written in French from the reference), a crate README section, or a test with a wallpaper id. Tests marked *(ignored)* need the person's Workshop content (`docs/TESTING.md`) and were **not run** for this report.
- **WE behaviour** comes from the specs and the public docs. `docs.wallpaperengine.io` and `help.wallpaperengine.io` describe mostly authoring; where a row's WE side is not in a spec or a public page it says "not verified".
- **Windows** column wording: Windows now runs on the person's machine (desktop mode, two monitors: commit `4f027ea`; Windows 11 measurements in the `lumenwall-engine`, `lumenwall-video` and `lumenwall-web` READMEs). A cell says what a README or a commit message records; where none does it stays `not verified`. This update ran nothing: it reads code, READMEs and the git log.
- Some crate README "Not done yet" rows are **stale** (they contradict the git log and other rows); they are flagged. See [Documentation drift](#documentation-drift).

**Landed since the first version** (rows below updated): Windows video (`338bdaf`), Windows web with CEF (`9f7def1`, `d337a2d`), the second monitor that stayed static (`4f027ea`), Wallpaper Engine playlists (`43aaf16`), pause on battery and the now-playing source (`aea5fe0`), `$mediaThumbnail` / `$mediaPreviousThumbnail` (`d7c8758`), the engine in a process of its own on Windows (`0ce1ea2`), Lumenwall's minimal LGPL FFmpeg (`d61f64b`), the one-window `Lumenwall-Setup.exe` (`4e6d274`, `996b6e4`). **Still open from the original list:** the NVIDIA overlay prompt (documented, not fixed: `4f027ea`, `lumenwall-platform` README).

## 1. The person's installed content

`C:\Program Files (x86)\Steam\steamapps\workshop\content\431960` holds 14 folders: **10 wallpapers** and 4 Workshop *assets* (shared effect/material/font packs: 2499516781, 2652493753, 2978738836, 2981960200; they are not wallpapers and Lumenwall lists them as unsupported on purpose, `lumenwall-scene` Deviations). The scenes bundle the assets they use inside their own `scene.pkg` (checked: the `workshop/<id>/…` effects of 3372348904 and 3463520581 are in the package), so no cross-item lookup is needed.

Features were read from `project.json` and from `scene.json` unpacked to a scratch folder outside the repository (nothing committed).

| Id | Title | Type | Features used | Linux | Windows |
|---|---|---|---|---|---|
| 1712475860 | Dino Run | scene (2D, 343×193) | 32 images, `scroll` ×21, `godrays`; 2 sound objects (jump, coin); 3 scripts incl. `createLayer`/`registerAsset` (coins, pickup particles) and `applyUserProperties`; props: 2 combos, 2 bools, colour | **partial**: layers and clock tested *(ignored)* `tests/runtime_layers.rs`, `tests/scene_clock.rs`; script `play()`/`stop()` of sound layers are neutral, so jump/coin sounds are likely silent (engine README "Sound layers") | not verified |
| 1960765686 | Desert at night 4k | scene (2D, 3840×2160) | 3 images, 23 particle systems, no effect, no script | **done, not verified individually**: every scene wallpaper loads and builds (`lumenwall-scene` *(ignored)* test over `$LUMENWALL_WE_WORKSHOP`); no render test names it | not verified |
| 2234978675 | Patrouille de France | scene (2D, 4288×2848) | 1 image, `clouds` effect | **done, not verified individually** (same as above) | not verified |
| 3018340632 | Boykisser, Blade Runner [reference scene] | scene (2D, 3440×1440) | 11 effects on one image (depthparallax, iris, shake ×3, foliagesway ×2, pulse, shine, localcontrast, chromaticaberration), 4 particle systems, 1 sound (5 mp3, `random`), bloom with `hdr: true`; props ash, rain, colour | **done**: reference wallpaper, measured (README "Performance", `tests/real_content.rs`). HDR flag effect not verified | **done on the person's desktop**: 30.0 fps on both monitors (commit `4f027ea`); switch times in the engine README's Windows measurements |
| 3044659344 | Porsche 911 GT3 | scene (2D, 4K) | 2 images, 1 text layer (clock script), 3 scripts (scale, clock `scriptProperties`); props: 24 h bool, invert, tints | **done**: `tests/text_content.rs` *(ignored)* | not verified |
| 3089056751 | Porsche (RUF) 911 930 | scene (2D, 4K) | same pattern: 2 images, 1 clock text, 2 scripts | **done**: `tests/text_content.rs` *(ignored)*; text position matches the WE preview within 2 px (engine README, Text) | not verified |
| 3372348904 | Soren <3 | scene (2D, portrait 3024×4032), `supportsaudioprocessing` | 9 images, 7 text layers, 1 particle system, 20 scripts, 6 bundled effects (`blurprecise`, `blend`, `blendgradient`, `spin`, `opacity`), audio visualizer (63 bars built with `createLayer`, `registerAudioBuffers`), media thumbnail/properties events | **partial**: visualizer and text tested *(ignored)* (`render_png --audio synthetic`, `tests/text_content.rs`); media events and the cover now have a source (OS media session, `aea5fe0`) and `$mediaThumbnail` is bound (`d7c8758`; its render test uses a fake source *(ignored)*, MPRIS glue never run, `lumenwall-audio` README) | media session run against a real player (`now_playing`, audio README), timeline events not tested; WASAPI capture still "never run" (`lumenwall-audio` README) |
| 3463520581 | Kirito x Asuna (SAO) 4K | scene (2D, 3840×2160) | 69 images, 64 `waterwaves` + opacity, shake, tint, `perspective`, 4 bundled Workshop effects (bokeh blur, bloom, scroll, waterripple), 16 puppets with animation layers (78 bones, 58 attached objects), 4 particle systems (leaves, gusts), wind/gravity, camera shake, 1 sound, 12 user props | **done**: 8 `.mdl` puppets skinned on the GPU, measured (engine README "Puppets", `tests/real_content.rs` *(ignored)*) | measured off the desktop: switches 202–241 ms (engine README, Windows measurements); no desktop run recorded |
| 3044696375 | JavaScript Clock | web | `clock.html`/`clock.js`, props: colour | **done**: `tests/web_content.rs` *(ignored)*, README measures 11.7 % of one core | **done, headless and host runs**: first frame 3.5 s after a cold start, 0.93 ms per frame to copy (web README "Measurements"); mouse input and a desktop run not tested |
| 3655057993 | Boykisser Winter [4K] | video | 72 MB H.264 4K60 + AAC | **done**: 2.3–2.6 % of one core (engine README) | **done**: Vulkan zero copy 3.0 % of one core (`lumenwall-video` README, "Windows 11"; commit `338bdaf`) |

Content counts used for ranking below: 8 of 10 are scenes, 1 web, 1 video. Scenes: 5 have scripts, 3 have text, 3 particles-heavy (1960765686, 3018340632, 3463520581), 1 puppets, 1 audio-reactive, 1 media events, 1 layer-creating script.

## 2. Wallpaper types

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Scene (2D) | Image/particle/sound/text objects, effect chains | done | README Features; engine README | — |
| Scene (3D / perspective) | Models, perspective camera, lights, reflections | partial (P1 done; P2/P3 not) | engine README "3D", "Not done yet"; `tests/scene3d_content.rs` *(ignored)*; spec 09 | L |
| Video | MP4/WebM looped, with sound | done on Linux and Windows (Windows needs the FFmpeg DLLs of the installer; without them the wallpaper fails with a message naming them) | `lumenwall-video` README ("Windows 11", "Windows distribution"); `338bdaf` | — |
| Web | HTML page in an embedded browser | done on Linux and Windows (the CEF host is in the installer; Windows: D3D11 shared textures into Vulkan, shared-memory frames with DX12; mouse input and a desktop run not tested there) | `lumenwall-web` README ("Not tested", "Measurements"); `9f7def1`, `d337a2d` | — |
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
| Media events (`mediaThumbnailChanged`, `mediaPropertiesChanged`…) and the cover textures `$mediaThumbnail`, `$mediaPreviousThumbnail` | Fed by the OS player | done: GSMTC on Windows (run against a real session), MPRIS on Linux (bus glue never run); followed only while a script handles the events or a pass samples a cover; cover bound and swapped on the GPU | engine README "Media integration"; `script/media.rs`, `scene/media.rs`; commits `aea5fe0`, `d7c8758` | — |
| `localStorage` | Persisted | partial: in memory only | engine README | S |
| Time limits | None | different: 100 ms tick/call, 1 s load | `lumenwall-script` README Deviations | — |

## 7. Web API (web wallpapers)

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Page loading from the wallpaper folder, `wp<id>://` scheme | CEF page | done (Linux) | `lumenwall-web` README | — |
| `wallpaperPropertyListener` (user properties, `applyUserProperties`), `wallpaperRegisterAudioListener`, pause/`setPaused` | Documented | done (Linux) | README Features; `tests/web_content.rs` *(ignored)*, 3044696375 | — |
| Mouse and click events | Page receives input | partial: clicks on GNOME XWayland desktop windows not delivered without the Shell extension (spec 07 QO-8); Windows mouse input not tested | `lumenwall-web` README "Not tested"; platform README | M |
| Media listeners (`wallpaperRegisterMediaStatusListener` and co.) | OS media info | done: the slot follows the system's media sessions while the page has a listener, and sends only what changed | `crates/lumenwall-engine/src/web.rs` (`set_media_listeners`, `forward_media`); `lumenwall-web` README API table; `aea5fe0` | — |
| Page audio under `--volume`/`--silent` | Yes | done; automute does not apply | engine README "Web page audio and automute" | M |
| Windows | — | done (CEF 156 host in the zip and the installer); automute does not apply to the page; the engine's D3D11 → Vulkan copy path is requested by `lumenwall-render` (`d337a2d`) | `crates/lumenwall-engine/src/web.rs`; web README "Windows runtime files" | — |
| `file://` user files, page audio output | — | not verified | `lumenwall-web` README "Not tested" | — |

## 8. Video

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| H.264 etc. playback, loop, sound | Yes | done on Linux (Vulkan Video zero copy, NVDEC, VA-API, software) and on Windows (Vulkan zero copy, NVDEC, D3D11VA, software) | `lumenwall-video` README; 3655057993 | — |
| Windows playback | Yes | done: FFmpeg's DLLs loaded at the first video; Lumenwall's own minimal LGPL build (10.9 MB, FFmpeg 8.1.3) ships in the zip and the installer | `lumenwall-video` README "Windows distribution"; `packaging/ffmpeg/README.md`; `338bdaf`, `d61f64b` | — |
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
| Playlist object: order, timer, time-of-day, `updateonpause` | Spec 07 §17 | done for `timer` mode, `random`/sorted order, `updateonpause`, failures, next/previous: one runner per target in the daemon, thread `lumenwall-playlists` sleeping to the earliest deadline. "Time of day" and "Log in" modes are in no spec: such playlists stay on their item | `crates/lumenwall/src/daemon/playlists.rs`; `lumenwall` README "Playlists"; `43aaf16` | — |
| Playlist transitions, slideshow GUI | Not verified in WE | partial: Screens page lists, starts and stops playlists, shows the item, Previous / Next; `transition` is read and shown as a cut | `lumenwall-ui` README "Playlists"; engine README "Playlist transitions" | M |
| Wallpaper per application / application rules | Exists (help.wallpaperengine.io index: "Select wallpapers per application", "Fix issues with specific apps") | missing; not specified in `specs/` | no spec, no code | L |
| Profiles / schedules | Not verified | missing | — | — |

## 12. Multi-monitor

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| One wallpaper per monitor, all monitors | Yes | done | `lumenwall` README `-r/-b`; `lumenwall-ui` Screens page | — |
| Hot-plug, per-monitor scale, fractional scaling | Not in the reference | done (Linux backends; Windows `WM_DISPLAYCHANGE` written) | `lumenwall-platform` README | — |
| One wallpaper spanning several monitors | Span | partial: each output shows the whole wallpaper | engine README "Span groups" | M |
| Negative virtual origin, monitor left/above primary on Windows | — | not verified | platform README Windows checklist item 1 | S (verify) |
| Second monitor static (Windows, shared scene) | — | done: every output of a shared scene now presents each frame (cause: one timer paces all outputs, the first cleared the redraw flag; not the iGPU); test `outputs_sharing_a_scene_present_every_frame`; 30.0 fps on both monitors measured | `crates/lumenwall-engine/src/engine.rs`; commit `4f027ea` | — |
| Wayland compositors other than GNOME (KDE, sway, Hyprland, niri, COSMIC) | — | not verified (written, not run) | README Features | M (verify) |

## 13. Pause rules

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Pause on fullscreen app | Yes (all outputs) | done, measured on GNOME 50.1; Windows `SetWinEventHook` written, not verified | `lumenwall-platform` README; README Performance | M (verify Windows) |
| Pause when a window is maximised | Yes | done (`MAXIMIZED`, per output); GNOME Wayland needs the Shell extension | platform README Coverage | — |
| Pause when covered/hidden, locked, screen off | Not in the reference | done (extra) | platform README | — |
| Pause on battery | Yes (a setting) | done: `pause-on-battery` (off by default), a Settings switch, "Paused: on battery" in the app; Windows `PowerSettingRegisterNotification`, Linux UPower, followed only while the setting is on. The switch to battery itself was not tested (desktop PC on mains) | `crates/lumenwall/src/daemon.rs` (`Battery`), `lumenwall-ui` `view/settings.rs`; `lumenwall-audio` README "Not tested"; `aea5fe0` | — |
| Pause when not focused / other apps (`--fullscreen-pause-only-active`) | Yes | done on Wayland and Windows (Windows: only the foreground fullscreen window or its root owner pauses; QUNS states unchanged); X11 not covered | `lumenwall` README; `crates/lumenwall-platform/src/win_logic.rs` (`decide_pause`, `is_active_window`), commit `eca72f8` (task 2f29cdf1, pending landing) | — |
| GPU released after a long pause | Not in the reference | done (extra) | `docs/PERFORMANCE.md`; engine README | — |
| NVIDIA overlay prompt | — | known issue, documented not fixed: the driver's capture module counts unknown executables as games | `lumenwall-platform` README; `4f027ea` | — |

## 14. Performance settings

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| FPS cap | 15/24/30/60/… | done, default 30, live reload | `lumenwall` README `--fps` | — |
| Render only when something changes | Not in the reference | done (extra) | `docs/PERFORMANCE.md` | — |
| Quality presets: anti-aliasing (MSAA), texture resolution, reflection quality, post-processing | Per-user settings in WE's UI | missing: no knob; textures use anisotropy 8 and full resolution. WE's exact option list is not verified (public pages found cover troubleshooting only) | `lumenwall-formats` README; `lumenwall` README option table | M |
| Particles / mouse / parallax switches | Yes | done (CLI) | `lumenwall` README | S (UI exposure not verified) |
| Vulkan/GPU selection | Not verified | partial: remembers the last driver (on Windows from its registry key), one backend per instance, Vulkan first then DX12; passing the render adapter's LUID to the web host on multi-GPU machines is not done | engine README `vulkan_driver`; `lumenwall-web` README; `b950698` | S |
| Memory and wake-up budgets | — | done, documented | README Performance | — |

## 15. Media integration

| Feature | WE behaviour | Lumenwall | Evidence | Effort |
|---|---|---|---|---|
| Now-playing info and cover for scripts/web (MPRIS on Linux, Windows media session) | Yes | done: `lumenwall-audio` media hub (GSMTC, MPRIS) runs only while something subscribes; artwork decoded off the render thread, kept within 512 px, http(s) artwork never downloaded; scenes get the events and the cover textures, web pages the listeners | engine README "Media integration"; `lumenwall-audio` README (`media`); `aea5fe0`, `d7c8758`; spec 07 §16 | — |
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
| Tray, autostart, installer, self-update | Yes | done on Linux; Windows: one-window `Lumenwall-Setup.exe` built in CI (per user, "Démarrer avec Windows" ticked, desktop shortcut optional, ends the engine and web host before replacing files), not Authenticode-signed (SmartScreen warns). No README records a run of it on the person's machine (not verified) | README Installation; `docs/RELEASING.md`; `4e6d274`, `996b6e4`; `lumenwall-update` | — |
| Playlist and schedule UI | Yes | partial: playlists yes (§11), schedule no | `lumenwall-ui` README "Playlists" | M |
| Windows UI | — | partial: "not yet tested on Windows" (README) | README Features | S (verify) |
| Upload / publish to Workshop | Yes | missing (needs Steamworks, deliberately outside scope) | `docs/UI.md` "not a Steam application" | out of scope |

## 17. Editor

Wallpaper Engine's editor (creating scenes, importing models, publishing) is **out of scope**: Lumenwall plays existing wallpapers (`CLEANROOM.md`, README). No code or spec for it exists. Status: `missing`, not planned. Effort: L+.

## 18. Windows versus Linux

| Area | Linux | Windows |
|---|---|---|
| Scene wallpapers | done, measured | measured on Windows 11 (engine README); desktop mode runs on the person's machine (`4f027ea`); CI renders on WARP (README Features) |
| Desktop surface | GNOME/X11 done; Wayland layer-shell written, not run | WorkerW/Progman child runs on the person's desktop with two monitors (`4f027ea`); the 6-point checklist stays open for the other cases (platform README "To check on Windows 11") |
| Video | done | done, measured (3.0 % of one core, 4K60) |
| Web | done | done in headless and host runs; mouse input and a desktop run not tested |
| Audio capture and automute | Pulse done; PipeWire-native not compiled | WASAPI written, never run |
| Pause rules | measured | written, not verified; battery source registered and its callback received (audio README `power`) |
| UI, tray | done | built, "not yet tested on Windows" |
| Install and update | one-line installer, `.deb` | one-window NSIS installer built in CI (`Lumenwall-Setup.exe`), updater rewrites changed files; no recorded run on a real machine (not verified) |
| Multi-GPU | not applicable here | monitor 2 fixed (`4f027ea`); LUID for the web host not done |
| IPC daemon ↔ app (named pipe `\\.\pipe\lumenwall-ipc-<user>`, single instance, forwarding) | Unix socket, tests pass | works: the Windows measurements drive the daemon through a private endpoint, and the pipe fixes of `0c8545d` are on main; the full workspace test run on Windows was not repeated for this update |

## Top 10 to do next, and where each stands

The ranking is the first version's (user impact = how many of the person's 10 wallpapers (W) and how much common Workshop content needs it; "common Workshop content" is judgement, no catalogue statistics were collected). It was not re-ranked: only the status column was added.

| # | Item | Status now | Effort |
|---|---|---|---|
| 1 | **Verify scenes end to end on the person's Windows machine** (platform checklist, the 8 scenes, UI, installer, named-pipe IPC) | partly: scenes and desktop mode run on two monitors (`4f027ea`), IPC works; UI, installer run and the checklist's other points still unverified | M |
| 2 | **Windows audio**: WASAPI loopback and automute on real playback | open: "never run" (`lumenwall-audio` README) | S–M |
| 3 | **Sound layers from scripts** (`play`/`stop`) | open: neutral (engine README "Not done yet") | S |
| 4 | **Wire playlists** | **done** (`43aaf16`), §11 | — |
| 5 | **Media integration source** (MPRIS, Windows media session) | **done** (`aea5fe0`, `d7c8758`), §15 | — |
| 6 | **Pause on battery** | **done** (`aea5fe0`), §13 | — |
| 7 | **Text styling** (outline, shadow, background box, growth) | open | M |
| 8 | **One wallpaper spanning several monitors** | open | M |
| 9 | **Quality settings** (MSAA, texture resolution, reflection quality) and an HDR/`hdr` check | open | M |
| 10 | **Video textures in scenes + created-layer puppets/`.mdl`** | open | M |

Not in the top 10 on purpose: 3D P2/P3, puppet IK/physics/blend shapes (no known format, L), per-application wallpaper rules (L, no spec), Wayland compositors beyond GNOME (verify when a user reports), editor, application wallpapers.

## Documentation drift

Fixed by this update: the stale rows of `crates/lumenwall-engine/README.md` "Not done yet" (web wallpapers "not integrated", `createLayer` "neutral", the media cover "not bound", web on Windows "refused"), the architecture diagram of the root `README.md` (video and web "not connected yet"), its FAQ entries on the engine socket and on Windows, and the Windows status lines of `README.md` and `crates/lumenwall/README.md`.

Still stale, outside the files this update owns:

- `crates/lumenwall-web/README.md` ("Windows zero-copy status", "Not tested") says `lumenwall-render` does not request `VULKAN_EXTERNAL_MEMORY_WIN32` yet; commit `d337a2d` added it and `crates/lumenwall-engine/src/web.rs` uses it.
- `README.fr.md` is the French copy of the root README and was not touched.

## What was not checked

No Lumenwall or Wallpaper Engine process was run; no test was run (`*(ignored)*` tests need the content and a GPU); WE's own settings screen was not consulted (public pages reachable offered troubleshooting indexes only). Anything the sources do not state is marked "not verified".

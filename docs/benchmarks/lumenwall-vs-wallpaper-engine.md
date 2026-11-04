# Lumenwall vs Wallpaper Engine on Windows

Measured on 4 November 2026 on the maintainer's Windows desktop, with `tools/bench-vs-we/bench-vs-we.ps1`
(see its [README](../../tools/bench-vs-we/README.md)). Raw data: [`lumenwall-vs-wallpaper-engine/`](lumenwall-vs-wallpaper-engine/).

> **This is a short run.** 4 wallpapers, **1 repetition**, 10 s warm-up and **30 s** of one-second
> samples per wallpaper and engine. A "±" is the standard deviation of the 30 samples of that single
> run: it shows the noise inside the run, not how far a second run would land. With one run per
> cell there is no run-to-run spread at all, so treat every difference below about 25 % on CPU,
> and below about 2 W or 5 points on the NVIDIA board figures, as unproven. On top of that, other
> agents' Rust builds were running on the same machine (see [Conditions](#conditions)). The numbers
> show the order of magnitude and the large gaps. Rerun the full method on an idle machine
> (`-Repetitions 2 -WarmupSeconds 20 -MeasureSeconds 60`) to confirm small differences.

## In short

1. **Lumenwall 0.1.0 for Windows runs only 2 of the 4 wallpapers.** It refuses the video
   (no FFmpeg in this build) and the web wallpaper (web runs on Linux only for now). Wallpaper
   Engine (WE) runs all 4. [Errors](#wallpapers-that-could-not-run).
2. **Bug: Lumenwall leaves the second monitor static** (DISPLAY2 at 0 fps while reported
   "running"), on the bench daemon and on the regular daemon alike.
   [Details](#bug-found-lumenwall-static-on-monitor-2). The main results therefore cover
   monitor 1 only, for both engines. The two-monitor run is in the
   [appendix](#appendix-both-monitors-lumenwall-static-on-monitor-2).
3. **GPU: Lumenwall uses more 3D time than WE on both scenes** (8.5 % vs 4.5 % on 3463520581,
   ×1.9; 6.6 % vs 5.3 % on 3018340632, ×1.2), although WE runs its "ultra" preset with MSAA x2.
4. **Memory: Lumenwall is heavier.** Its working set is 2.8–2.9× WE's (186–191 MiB against
   64–69 MiB) and its private bytes are 22–32 % higher. Its dedicated VRAM is 1.9× WE's on the
   heavy scene (608 vs 323 MiB) and the same on the light one (266 vs 275 MiB). Empty and
   resident, Lumenwall keeps 54–72 MiB private against 3 MiB for WE.
5. **CPU and board power: no difference above the noise** on the scenes (Lumenwall 4.6 % vs
   3.1 % and 1.6 % vs 1.4 % of one core; 36–40 W for both, 33–38 W with no engine at all).
   **Switching wallpapers is slower in Lumenwall**: first GPU activity 0.6–0.9 s after the
   command, against 0.2–0.5 s for WE.

## Bug found: Lumenwall static on monitor 2

- **Seen on the desktop**: with Lumenwall covering both monitors, the wallpaper on monitor 2 does
  not move.
- **Measured**: during the two-monitor run, Lumenwall's own statistics (IPC `get_stats`) reported
  `DISPLAY1=30 fps; DISPLAY2=0 fps`, with both outputs in status `running` and no pause reason.
  Its log confirms it: `rendered 1218 frames in 40.9 s (29.8 fps)` and
  `rendered 1225 frames in 41.1 s (29.8 fps)` for two outputs, which is one output's worth (two
  outputs at the 30 fps cap would give about 60). The regular daemon (`lumenwall.exe` with its normal
  configuration, both monitors) logged `rendered 472 frames in 15.9 s (29.6 fps)` the same
  afternoon, so this is everyday behaviour, not a bench artefact. Nothing is logged as a warning
  or an error.
- **The GPU still works for it**: Lumenwall's log gives 4.12 ms of GPU time per frame with both
  outputs, against 2.13 ms (3018340632) and 2.97 ms (3463520581) on DISPLAY1 alone. Its 3D share
  goes from 6.6–8.5 % to 12.1 %. So it seems to render for the second output without a new image
  ever reaching it.
- **Monitor wiring**: monitor 2 was suspected to be plugged into the AMD iGPU. Windows does
  not see it that way. DXGI lists both outputs under the RTX 4080 SUPER, EnumDisplayDevices
  reports DISPLAY2 (HKC2731) driven by the RTX, and neither engine did any work on the AMD
  adapter (0 % 3D, 0 MiB, both runs). Only `Win32_VideoController` lists a 2560×1440 @ 239 Hz mode
  on the AMD adapter, which contradicts the rest; the cable is worth checking. What does differ is
  the refresh rate: **DISPLAY1 runs at 280 Hz, DISPLAY2 at 240 Hz**.
- **WE drives both monitors** on the same setup: its 3D share on 3018340632 goes from 5.3 % (one
  monitor) to 20.3 % (both).
- Not investigated further (out of scope). Candidates: per-output frame pacing when the outputs
  have different refresh rates, and presentation to the second surface (Vulkan on Windows).

## Setup

| | |
|---|---|
| Machine | AMD Ryzen 9 7950X3D (16 cores, 32 threads), NVIDIA GeForce RTX 4080 SUPER (driver 621.01, LUID 0x0000EBF6), AMD Radeon iGPU (LUID 0x00011809, unused by both engines), Windows 11 Pro 10.0.26300 |
| Monitors | DISPLAY1: GBT273C, 2560×1440 @ 280 Hz, primary, on the right; DISPLAY2: HKC2731, 2560×1440 @ 240 Hz, on the left. Both driven by the RTX according to DXGI and EnumDisplayDevices |
| Lumenwall | 0.1.0 release `x86_64-pc-windows-msvc`, the copy installed on this desktop (not a build of this repo). Renders with Vulkan on the RTX (`GPU: NVIDIA GeForce RTX 4080 SUPER (Vulkan, DiscreteGpu)`) |
| Wallpaper Engine | 2.8.42 (Steam), settings as the owner left them after the welcome wizard: fps 30, preset ultra (MSAA x2, post-processing ultra, shadows ultra, volumetrics ultra, reflections on), resolution full, pause on maximized and fullscreen windows, video through Media Foundation with hardware acceleration, normal priority. Renders with Direct3D on the RTX |
| Same for both | Frame-rate cap 30 (WE's setting, passed to Lumenwall with `--fps 30`); sound off (WE `-control mute` before and after every wallpaper, Lumenwall `--silent`); the same monitor (main run: DISPLAY1 only, WE `-monitor 0`, Lumenwall `-r DISPLAY1`; appendix: both) |

| Wallpaper | Type | Title |
|---|---|---|
| 3463520581 | scene | Kirito x Asuna / Sword Art Online [4K] (67 images, 156 passes) |
| 3018340632 | scene | Boykisser — Ryan Gosling │ Blade Runner theme │ 4k (the scene usually shown on this desktop, 27 passes) |
| 3044696375 | web | JavaScript Clock & Motivation |
| 3655057993 | video | Boykisser │ Winter theme [4K] (H.264 3840×2160 60 fps) |

### Method

Both engines stay resident for the whole run and are driven the same way: a command hands one
wallpaper to the running engine, the bench waits 10 s, samples once per second for 30 s, then
removes the wallpaper. For WE the commands are `wallpaper64.exe -control openWallpaper -file
<project.json> -monitor 0` and then `-control closeWallpaper`. For Lumenwall, a second
`lumenwall.exe --no-config --silent --fps 30 -r DISPLAY1 --bg <dir>` forwards the wallpaper to a
bench daemon over IPC, then IPC `clear`. The engine that goes first alternates per wallpaper:
WE first on 3463520581 and 3044696375, Lumenwall first on 3018340632 and 3655057993. Three idle
baselines of 15 s: neither engine running (start and end), and both resident with no wallpaper.
WE is started through Explorer and closed through the Windows Restart Manager (no force-kill was
needed). The Lumenwall bench daemon uses a private IPC endpoint and `--no-config`.

Metrics, all black-box (details in the tool's README):

- CPU of the engine's whole process tree (children included: WE's `webwallpaper64.exe` CEF
  processes count; the UIs `wallpaperui.exe` and `lumenwall-ui.exe` are excluded, neither ran),
  100 % = one logical core.
- Private bytes and working set of the tree.
- GPU engine time per adapter from the `GPU Engine` counters, summed over the tree's pids.
- Dedicated and shared GPU memory from `GPU Process Memory`.
- Board power, utilization and clocks of the RTX through NVML, the library nvidia-smi reads (same
  values as nvidia-smi, checked).
- Time from the command to the first millisecond of GPU time of the tree, polled every 50 ms.

### Conditions

- **Background load (not the engines)**: other agents' Rust builds (`rustc`, up to about 20 cores)
  ran during the measurement. Whole-machine CPU (100 % = 32 threads) averaged 16 % and 10 % during
  Lumenwall's two scene runs and 10 % and 37 % during WE's. It averaged 35 % during WE's web run
  and 55 % during its video run, with peaks at 100 % in several runs. The per-process counters
  are not shared with those builds, but contention can raise an engine's CPU time and delay its
  frames. The NVML board figures include everything else using the RTX: browsers, Discord and the
  compositor kept it at 25–36 % utilization and 33–38 W with no engine running.
- **Maximized windows**: none during 239 of the 240 samples. One sample (WE, 3463520581, t = 26 s)
  saw a maximized window; WE's 3D share at that sample (4.1 %) stayed in line, so it was kept.
- **Quality settings differ by nature**: WE's ultra preset and MSAA x2 have no Lumenwall
  counterpart, and Lumenwall renders each scene at its authored size (3840×2160 for 3463520581,
  3440×1440 for 3018340632, per its log) before fitting it to the 2560×1440 output. WE's internal
  render size is unknown (black box).
- **Sound**: Lumenwall with `--silent` does not load scene sounds at all (`sound disabled
  (--silent): no output device, scene sounds not loaded`); WE muted may still decode them. The
  audio session meters of both engines stayed at 0 for the whole run.
- **Resident engines**: each wallpaper is loaded into an engine process that showed other
  wallpapers before. Both release everything when they are empty (WE 3 MiB private, 0 VRAM;
  Lumenwall closes its GPU device), so each Lumenwall run includes the creation of a GPU device.

## Results: monitor 1 (DISPLAY1) only

Mean over the 30 samples ± their standard deviation. "–": not applicable or not run.

### CPU of the process tree (% of one logical core)

| Wallpaper | Wallpaper Engine | Lumenwall | Lumenwall / WE |
|---|---|---|---|
| 3463520581 (scene) | 3.07 ± 2.15 | 4.62 ± 1.50 | ×1.50 |
| 3018340632 (scene) | 1.44 ± 1.55 | 1.62 ± 1.45 | ×1.12 |
| 3044696375 (web) | 11.44 ± 8.77 | not run | – |
| 3655057993 (video) | 7.72 ± 3.78 | not run | – |

Windows charges CPU time in 15.6 ms ticks, so one-second samples of a light process jump between
0 and 1.56 %. That explains the large "±". Lumenwall's own frame counter gives 1.30 ms (3463520581)
and 0.55 ms (3018340632) of render-thread CPU per frame at 30.0 fps.

### Private bytes of the process tree (MiB)

| Wallpaper | Wallpaper Engine | Lumenwall | Lumenwall / WE |
|---|---|---|---|
| 3463520581 (scene) | 521 | 686 | ×1.32 |
| 3018340632 (scene) | 282 | 345 | ×1.22 |
| 3044696375 (web) | 224 ± 5 | not run | – |
| 3655057993 (video) | 369 ± 25 | not run | – |

### Working set of the process tree (MiB)

| Wallpaper | Wallpaper Engine | Lumenwall | Lumenwall / WE |
|---|---|---|---|
| 3463520581 (scene) | 69 | 191 | ×2.77 |
| 3018340632 (scene) | 64 | 186 | ×2.92 |
| 3044696375 (web) | 361 ± 3 | not run | – |
| 3655057993 (video) | 317 ± 25 | not run | – |

### GPU 3D time on the RTX (% of time, process tree)

| Wallpaper | Wallpaper Engine | Lumenwall | Lumenwall / WE |
|---|---|---|---|
| 3463520581 (scene) | 4.54 ± 0.80 | 8.47 ± 1.00 | ×1.87 |
| 3018340632 (scene) | 5.30 ± 0.36 | 6.58 ± 0.43 | ×1.24 |
| 3044696375 (web) | 0.11 ± 0.08 | not run | – |
| 3655057993 (video) | 1.04 ± 0.38 | not run | – |

Lumenwall's own GPU timestamps give 2.97 ms (3463520581) and 2.13 ms (3018340632) per frame.
Video decode on the RTX: only WE's video uses it, 28.6 ± 4.6 % (NVML decoder: 31.4 ± 17.9 %).
Copy engines: 0 for both. AMD iGPU: 0 % on every engine type and 0 MiB for both engines, in both
runs.

### Dedicated GPU memory on the RTX (MiB, process tree)

| Wallpaper | Wallpaper Engine | Lumenwall | Lumenwall / WE |
|---|---|---|---|
| 3463520581 (scene) | 323 | 608 | ×1.88 |
| 3018340632 (scene) | 275 | 266 | ×0.97 |
| 3044696375 (web) | 68 ± 5 | not run | – |
| 3655057993 (video) | 434 | not run | – |

### Shared GPU memory (MiB, process tree)

| Wallpaper | Wallpaper Engine | Lumenwall | Lumenwall / WE |
|---|---|---|---|
| 3463520581 (scene) | 3 | 43 | ×17 |
| 3018340632 (scene) | 4 | 43 | ×12 |
| 3044696375 (web) | 3 | not run | – |
| 3655057993 (video) | 188 ± 15 | not run | – |

### NVIDIA board (NVML, whole GPU, every process)

| Wallpaper | Power WE (W) | Power Lumenwall (W) | Utilization WE (%) | Utilization Lumenwall (%) | Graphics clock WE (MHz) | Graphics clock Lumenwall (MHz) |
|---|---|---|---|---|---|---|
| 3463520581 (scene) | 36.3 ± 0.6 | 39.0 ± 1.4 | 33.5 ± 6.0 | 36.0 ± 4.8 | 979 ± 111 | 1,084 ± 95 |
| 3018340632 (scene) | 39.5 ± 1.4 | 37.8 ± 0.4 | 36.5 ± 4.7 | 37.9 ± 2.1 | 1,002 ± 74 | 990 ± 30 |
| 3044696375 (web) | 33.7 ± 0.9 | not run | 35.1 ± 5.4 | not run | 866 ± 175 | not run |
| 3655057993 (video) | 42.4 ± 9.0 | not run | 34.1 ± 14.9 | not run | 1,344 ± 976 | not run |

With no engine running the board drew 32.7–38.2 W at 25–36 % utilization: the differences
between engines are inside that noise.

### Command → first GPU activity (ms)

| Wallpaper | Wallpaper Engine | Lumenwall |
|---|---|---|
| 3463520581 (scene) | 474 (command 425) | 857 (command 802) |
| 3018340632 (scene) | 227 (command 182) | 594 (command 548) |
| 3044696375 (web) | 5,807 (command 3,352; first web wallpaper, CEF starts) | – |
| 3655057993 (video) | 300 (command 248) | – |

"Command" is how long the client command took to return. Lumenwall's includes the creation of
a GPU device, because the daemon closes it when the previous wallpaper is removed
(`no wallpaper left: surfaces and GPU device closed`).

### Processes

Scenes and video: one process for both engines. WE's web wallpaper: `wallpaper64.exe` plus 5
`webwallpaper64.exe` (CEF), 11.4 % CPU in total, most of it in two of them (4.6 % and 5.7 %).

### Wallpapers that could not run

| Wallpaper | Engine | Error (from the hand-off client and the daemon log) |
|---|---|---|
| 3044696375 (web) | Lumenwall | `the running daemon refused …\3044696375: daemon error: UnsupportedWallpaper: 'JavaScript Clock & Motivation' is a web wallpaper: web wallpapers run on Linux only for now (no Direct3D frame import yet)`. The release folder also has no `lumenwall-web-host.exe`. |
| 3655057993 (video) | Lumenwall | `the running daemon refused …\3655057993: daemon error: UnsupportedWallpaper: 'Boykisser │ Winter theme [4K]' is a video wallpaper: this build has no video support (lumenwall-video without FFmpeg)`. Started directly on this wallpaper, the daemon exits with the same error. |

## Idle baselines

| Phase | Whole-machine CPU | NVIDIA power | NVIDIA utilization | NVIDIA memory used | Engine itself |
|---|---|---|---|---|---|
| Neither engine, start | 9.9 % | 32.7 W | 32.5 % | 1,847 MiB | – |
| Both resident, no wallpaper | 7.3 % | 33.3 W | 35.9 % | 1,850 MiB | WE: 0.00 % CPU, 3 MiB private, 0 VRAM. Lumenwall: 0.00 % CPU, 59 MiB private (54 at the end of the window), 0 VRAM |
| Neither engine, end | 26.6 % (builds) | 38.2 W | 25.1 % | 1,601 MiB | – |

The two-monitor run gave the same picture: 4.3 % / 2.8 % / 48.5 % whole-machine CPU and 27–31 W.
Resident and empty, WE took 0.11 % CPU and 3 MiB private and Lumenwall 0.00 % and 72 MiB.

## Performance problems found (not fixed here)

1. Second monitor never updated (above), with the regular daemon too.
2. GPU 3D time 1.2–1.9× WE's on scenes, although WE renders with MSAA x2 and its ultra
   preset. Rendering at the scene's authored size (3840×2160 for a 2560×1440 output) is a
   candidate.
3. Working set 2.8–2.9× WE's, private bytes +22–32 %, dedicated VRAM 1.9× on the heavy scene
   (608 MiB; 514 MiB was measured on Linux in `docs/PERFORMANCE.md`), 43 MiB of shared GPU memory
   against 3–4 MiB.
4. 54–72 MiB private kept by the empty daemon, against 3 MiB for WE.
5. Wallpaper switch to first GPU activity 0.6–0.9 s, against 0.2–0.5 s for WE, since the GPU
   device is recreated after `clear`.
6. Video and web wallpapers unsupported in the Windows release.

## Limits of the method

- One short repetition (see the top of the page), under background load from other builds.
- No frame rate or frame time for WE: that needs PresentMon (not installed, not asked for in this
  short run) or an ETW consumer. Lumenwall's own counter says it held 30.0 fps on DISPLAY1; WE's
  frame rate is not known.
- The GPU Engine counters give each process's busy time per engine type. The compositor's work to
  show the wallpaper is in `dwm.exe` and is not counted for either engine.
- NVML covers the whole RTX; the iGPU and the CPU package have no power reading here.
- The main run covers one monitor, half of the real use, because of the bug above.
- Settings cannot be made identical: WE's quality preset has no Lumenwall equivalent.
- Only 2 wallpapers could be compared; web and video are WE-only figures.

## Appendix: both monitors (Lumenwall static on monitor 2)

**Flag: in this run Lumenwall rendered DISPLAY1 only; DISPLAY2 showed a static image (see the
bug above). WE rendered both.** These numbers do not compare the same work. Same method, both
monitors (WE `-monitor 0` and `-monitor 1`, Lumenwall without `-r`), run just before the main one.

| Wallpaper | Engine | CPU (% of a core) | Private (MiB) | Working set (MiB) | 3D RTX (%) | Dedicated (MiB) | Shared (MiB) | NVIDIA W | First GPU (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 3463520581 | WE | 3.38 ± 2.27 | 995 | 108 | 10.49 ± 0.65 | 647 | 5 | 32.7 | 729 |
| 3463520581 | Lumenwall | 5.94 ± 2.80 | 736 | 245 | 12.16 ± 1.18 | 691 | 61 | 31.0 | 750 |
| 3018340632 | WE | 2.39 ± 1.73 | 529 | 97 | 20.26 ± 0.98 | 549 | 7 | 33.7 | 292 |
| 3018340632 | Lumenwall | 1.62 ± 1.72 | 389 | 239 | 12.07 ± 1.79 | 347 | 61 | 28.2 | 654 |
| 3044696375 | WE | 26.35 ± 12.58 | 447 | 713 | 0.39 ± 0.19 | 130 | 6 | 24.6 | 434 |
| 3655057993 | WE | 23.03 ± 8.68 | 759 | 615 | 1.22 ± 0.30 | 868 | 431 | 52.7 | 653 |

WE's video decoded at 55.4 % of the RTX's video engine with two monitors (two decodes), against
28.6 % with one. Whole-machine CPU during this run: 2.5–10.2 % for the scene runs, 60 % and 79 %
during WE's web and video runs (builds again). The two Lumenwall runs of web and video were
refused with the same errors as above.

## Reproduce

```powershell
# Main run (monitor 1 only), about 6 minutes:
pwsh -File tools\bench-vs-we\bench-vs-we.ps1 -WeMonitors 0 -LumenwallOutputs DISPLAY1 -RelaunchLumenwall
# Both monitors (appendix):
pwsh -File tools\bench-vs-we\bench-vs-we.ps1 -RelaunchLumenwall
```

Raw files: [`display1/`](lumenwall-vs-wallpaper-engine/display1/) (main run) and
[`both-monitors/`](lumenwall-vs-wallpaper-engine/both-monitors/) (appendix), each with
`samples.csv` (one row per second), `runs.csv`, `results.csv`, `processes.csv` and `machine.json`.
`pwsh -File tools\bench-vs-we\summarize.ps1 -OutDir <folder>` rebuilds a summary from them.

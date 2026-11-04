# bench-vs-we

Measures Lumenwall and Wallpaper Engine (WE) on the same Workshop wallpapers, on the real Windows
desktop, under the same conditions, and writes raw CSVs plus a Markdown summary. WE is measured as
a black box: processes, Windows performance counters, NVML. Nothing is installed or downloaded.

The report produced with it: [`docs/benchmarks/lumenwall-vs-wallpaper-engine.md`](../../docs/benchmarks/lumenwall-vs-wallpaper-engine.md).

## Files

| File | Role |
|---|---|
| `bench-vs-we.ps1` | The bench: saves the user's state, runs the phases, restores the state, calls `summarize.ps1`. |
| `summarize.ps1` | Turns a run folder into `results.csv` and `summary.md`. Can be rerun on its own: `pwsh -File summarize.ps1 -OutDir <folder>`. |
| `BenchNative.cs` | Win32 helpers compiled at run time by `Add-Type`: process snapshot and times, memory counters, DXGI adapters and LUIDs, maximized windows, audio session peaks (read-only), NVML, Windows Restart Manager. |

## Prerequisites

- Windows 10 or 11, PowerShell 7.2 or later (`pwsh`). No administrator rights.
- A Lumenwall release (`-LumenwallExe`, default `%USERPROFILE%\Desktop\lumenwall-0.1.0-x86_64-pc-windows-msvc\lumenwall.exe`).
- Wallpaper Engine installed through Steam, its welcome wizard finished (the script refuses to run otherwise), the wallpapers subscribed in the Workshop folder.
- An NVIDIA GPU for the board power, utilization and clock columns (`nvml.dll`, installed with the driver). Without it those columns stay empty; everything else works on any GPU.
- `lumenwall-ui` closed (it would restart the daemon). Nobody using the PC during the run, and no maximized or fullscreen window: WE pauses for them (`playbackmaximized = pause`) and Lumenwall pauses the covered output, which would skew the comparison. The script waits up to `-CoverWaitSeconds` for such windows to go, then refuses to start (unless `-AllowCoveringWindows`, for smoke tests).

## Running it

```powershell
# The short bench (4 wallpapers, 1 repetition, 10 s warm-up, 30 s measurement), about 6 minutes,
# on the main monitor only (Lumenwall 0.1.0 leaves a second monitor static, see the report):
pwsh -File tools\bench-vs-we\bench-vs-we.ps1 -WeMonitors 0 -LumenwallOutputs DISPLAY1 -RelaunchLumenwall

# The same on every monitor:
pwsh -File tools\bench-vs-we\bench-vs-we.ps1 -RelaunchLumenwall

# The full method: 10 wallpapers, 2 repetitions, 20 s warm-up, 60 s measurement, about 70 minutes:
pwsh -File tools\bench-vs-we\bench-vs-we.ps1 -RelaunchLumenwall -Repetitions 2 -WarmupSeconds 20 -MeasureSeconds 60 -IdleSeconds 60 `
    -Wallpapers 1712475860,1960765686,2234978675,3018340632,3044659344,3089056751,3372348904,3463520581,3044696375,3655057993
```

The script prints its time estimate first. Results go to `-OutDir` (default
`%TEMP%\lumenwall-bench-vs-we\<date-time>`).

| Parameter | Default | Meaning |
|---|---|---|
| `-Wallpapers` | `3463520581, 3018340632, 3044696375, 3655057993` | Workshop IDs (folders of `-WorkshopDir`) or wallpaper folders. |
| `-Repetitions` | 1 | Each repetition runs every wallpaper on both engines; the engine that goes first alternates per wallpaper and flips on every repetition. |
| `-WarmupSeconds` / `-MeasureSeconds` | 10 / 30 | Per run: time between the command and the first sample, then the sampled window (one sample per second). |
| `-IdleSeconds` | 15 | Length of each idle baseline (3 of them). |
| `-SettleSeconds` | 3 | Pause after a wallpaper is removed. |
| `-Fps` | 0 | Frame-rate cap for both engines; 0 reads WE's `fps` and passes it to Lumenwall with `--fps`. The script never changes WE's settings. |
| `-LumenwallExe`, `-WeDir`, `-WorkshopDir`, `-WeUser` | see the script | Paths, and the user section of WE's `config.json`. |
| `-WeMonitors` | `0,1` | WE monitor indexes passed to `-control openWallpaper -monitor N`. The log line `WE resident: … monitor 0 = GBT273C = DISPLAY1` gives the mapping, read from WE's monitor map. |
| `-LumenwallOutputs` | all | Lumenwall outputs (`DISPLAY1`, …) given as `-r <output> --bg <wallpaper>`; empty = the wallpaper alone, every monitor. Keep the same monitors as `-WeMonitors`. |
| `-IpcEndpoint` | `lumenwall-bench-vs-we` | Private named pipe of the bench daemon (`LUMENWALL_IPC_ENDPOINT`). |
| `-RelaunchLumenwall` | off | Start the user's daemon (`lumenwall.exe`, no arguments) at the end even when it was not running at the start. |
| `-KeepWallpaperEngine` | off | Leave WE running at the end. By default it is closed, and the last idle baseline is taken with neither engine running. |
| `-WeConfigReference` | none | WE `config.json` to put back at the end instead of the copy taken at the start (useful when an earlier start of WE already rewrote it). |
| `-CoverWaitSeconds`, `-AllowCoveringWindows` | 180, off | See the prerequisites. |
| `-AudioAbortPeak` | 0.001 | The bench removes every wallpaper and stops when an engine's audio session peak exceeds this. |

## Method

1. **Checks and state**: paths, WE wizard done, `lumenwall-ui` not running; backups of WE's
   `config.json` (and `distribution\config.json`), Lumenwall's `config.toml` and `ui.json`, with
   their SHA-256. A running Lumenwall daemon is stopped with WM_CLOSE.
2. **Idle, neither engine**: WE is closed if it runs, then `-IdleSeconds` of samples.
3. **Both engines resident**: WE started through Explorer (a normal launch), `-control mute`, then
   `-control closeWallpaper`; the Lumenwall bench daemon started with
   `--no-config --silent --fps <N> <a scene>` on the private endpoint, then IPC `clear` (a scene,
   because a build may refuse video or web wallpapers). Its state must report `muted`.
4. **Idle, both resident and empty**: `-IdleSeconds` of samples, one row per engine.
5. **Runs**: per wallpaper and engine, the command (WE: `-control mute`, `openWallpaper` on each
   monitor of `-WeMonitors`, `mute` again; Lumenwall: a second
   `lumenwall.exe --no-config --silent --fps <N> <dir>`, or `… -r <output> --bg <dir>` for each of
   `-LumenwallOutputs`, which forwards it to the daemon over IPC exactly as a user would; the
   daemon itself starts with the same options), the warm-up (GPU counters
   polled every 50 ms for the first GPU activity), the measurement, then `closeWallpaper` / IPC
   `clear`. A wallpaper the engine refuses is recorded with its error and not measured. After a
   Lumenwall window the script asks the daemon for its own statistics (`get_stats`, outside the
   window, because it turns on Lumenwall's frame counters for one second).
6. **End**: the bench daemon stops (IPC `shutdown`, WM_CLOSE as a fallback), WE closes through
   the Windows Restart Manager, a last idle baseline with neither engine running, then the restore.

Metrics, per one-second sample:

| Column | Source |
|---|---|
| `cpu_pct` | Kernel + user CPU time of the engine's process tree (`GetProcessTimes`), 100 = one logical core. The tree is followed by parent pid from the engine's main process, so CEF or WebView2 children count wherever they live; `wallpaperui.exe`, `lumenwall-ui.exe` and their children are excluded (their CPU is in `ui_cpu_pct`). |
| `private_mib`, `ws_mib` | Private bytes (commit) and working set summed over the tree (`GetProcessMemoryInfo`). |
| `gpu3d_pct_<adapter>`, `gpu_vdec_pct_*`, `gpu_copy_pct_*`, `gpu_all_pct_*` | `GPU Engine` counters, running time summed over the tree's pids, per adapter LUID and engine type. |
| `vram_ded_mib_*`, `vram_shared_mib_*` | `GPU Process Memory` dedicated and shared usage, summed over the tree, per adapter. |
| `nv_*` | NVML (the library nvidia-smi reports from): board power, GPU and decoder utilization, clocks, memory used, P-state. Whole GPU, every process. |
| `system_cpu_pct` | Whole machine (`GetSystemTimes`), 100 = every thread busy. |
| `other_cpu_pct` | The other resident engine, which should stay near zero. |
| `covering`, `audio_peak` | Maximized or fullscreen windows seen at the sample; the highest audio session peak of both engines. |

Adapters are named from DXGI (`nvidia`, `amd`, `intel`) with their LUID, so the summary shows
which GPU each engine renders on.

## The user's state it touches, and how it restores it

| State | During the run | Restored |
|---|---|---|
| Lumenwall daemon | Stopped with WM_CLOSE if running. The bench daemon runs with `--no-config` on a private IPC endpoint, so no client can reach it and nothing is saved. | Relaunched with no arguments through Explorer (so it is not a child of the script) when it was running at the start or with `-RelaunchLumenwall`. |
| `%APPDATA%\Lumenwall\config.toml`, `ui.json` | Not written (`--no-config`). | Hash compared with the backup; copied back if it differs. |
| `%LOCALAPPDATA%\Lumenwall\logs` | The bench daemon appends to the day's log. | Not restored (log only). |
| Wallpaper Engine | Closed through the Restart Manager at the start if it runs, started through Explorer, muted, given wallpapers, closed at the end. A force-kill happens only if the Restart Manager fails, and is written in the notes. | Closed at the end (or left running with `-KeepWallpaperEngine`). |
| WE `config.json` and `distribution\config.json` | WE rewrites its monitor map at start-up and records the wallpapers it shows. | Once WE is closed, compared with the backup (or `-WeConfigReference`) and copied back if they differ. Files WE writes on its own (`config_backups\`, its logs, Workshop cache) are left as they are. |
| Desktop | The wallpapers appear on both monitors, one engine at a time. | When both engines are closed Windows shows its own wallpaper again, until the relaunched daemon shows the user's. |
| Audio | Both engines muted; the audio session meters are only read. | Nothing to restore: no volume or mute setting is changed. |

`restore.json` in the output folder records the result of each check, the highest audio peak and
the notes.

## Output folder

| File | Content |
|---|---|
| `bench.log` | Every step with timestamps. |
| `machine.json` | Machine, adapters, versions, WE settings, parameters. |
| `samples.csv` | One row per second and per phase. |
| `runs.csv` | One row per run: window, command time, first GPU activity, Lumenwall's self-report, error. |
| `processes.csv` | Per process of the tree over each window (to see which process does the work). |
| `results.csv`, `summary.md` | Written by `summarize.ps1`. |
| `restore.json`, `backup\` | The restore checks and the backups. |

## Limits

- No frame-rate or frame-time capture for WE (it would need PresentMon or an ETW consumer). Only
  Lumenwall's own counter (`get_stats`) says whether it holds the cap.
- Windows charges CPU time in 15.6 ms ticks: one-second samples of a light process jump between
  0 and 1.56 %; only the window mean is meaningful.
- NVML and `system_cpu_pct` include everything else running on the machine; compare them with the
  idle baselines.
- Each engine runs inside a resident process that already showed other wallpapers: memory can
  carry what earlier wallpapers left.

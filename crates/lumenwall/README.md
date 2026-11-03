# lumenwall (application)

The `lumenwall` binary and its application layer: command line, configuration file and hot
reload, execution plan, logging, process lifecycle and exit codes, Wallpaper Engine
playlists, screenshot scheduling. Linux and Windows.

## Inputs (clean room)

Built from `specs/07-plateforme-audio-video-web.md` only: §1 (lifecycle), §2 (command line),
§3.1 (Workshop ID or path), §3.3 (assets message), §3.6 (property overrides), §4.1/§4.3
(modes, output → wallpaper mapping, alphabetical order), §8.2 (fps), §9.1, §17 (playlists),
§18.1–§18.2 (screenshot validation and trigger), §19 (logging), §20 (`--render-debug`),
§22–§23 (Windows, open questions); plus `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`,
`docs/research/platform.md`, and the public documentation of `clap`, `tracing`,
`tracing-appender`, `toml`, `dirs`, `notify`, `signal-hook`, `windows-sys` and
`embed-manifest`. No upstream code was read.

## Flow

```
argv ─ cli::parse ─► CliArgs ─┐
config::load ─► Config ───────┼─ plan::build ─► ExecutionPlan ─► Engine::load / start / run
WallpaperResolver, PlaylistCatalog ┘            (pure data)        ▲
watch (notify) ─► config changed ─► plan::build + plan::diff ─► Control::Reload
signals (SIGINT/SIGTERM, console events) ─────────────────────► Control::Stop
```

| Module | Content |
|---|---|
| `cli` | Option table (clap derive, used for `--help` and option metadata) and the ordered parser |
| `config` | TOML file, validation, default location |
| `plan` | `ExecutionPlan` (what runs where, how), CLI/config merge, `diff` for reloads |
| `services` | Traits to other crates: `WallpaperResolver`, `PlaylistCatalog`, `Engine`, `Waker`; control channel |
| `playlist` | Wallpaper Engine `config.json` playlists (§17): parsing, path normalisation, timer |
| `screenshot` | One-shot trigger after N frames (§18.2) |
| `logging` | `tracing` setup: stdout/stderr split, env filter, rotated file, reloadable level |
| `watch` | Event-driven file watcher (`notify`) |
| `signals` | SIGINT/SIGTERM (`signal-hook`), Windows console control events |
| `console` | Windows GUI subsystem: reattach to the parent terminal |
| `daemon` | IPC daemon (`lumenwall-ipc` `Handler`): reads from engine snapshots, client choices written atomically to the configuration file (`toml_edit`, comments kept) and applied live, statistics sampled only while subscribed |
| `app` | Lifecycle (§1.1 order), exit codes, single instance |

`main.rs` plugs placeholder services until the other crates are wired: paths resolve as is,
Workshop IDs, the Wallpaper Engine installation lookup and the engine report "not integrated
yet" (exit code 1).

## Command-line options

Same names, aliases and defaults as spec 07 §2.2. Options are processed in order; the ones
marked *target* apply to the output or span group named by the last `-r`/`--screen-span`
(§2.3). Unknown arguments are ignored.

| Option | Alias | Argument | Default | Effect |
|---|---|---|---|---|
| *(positional)* | | ID or path | — | Default wallpaper (window, outputs without `--bg`). No `/` = Workshop ID |
| `--window` | `-w` | `XxYxWxH` | normal 640×480 window | Explicit borderless window. Once; exclusive with `-r`/`--screen-span` |
| `--screen-root` | `-r` | output | — | Desktop mode on this output; becomes the target. Repeatable |
| `--screen-span` | | `A,B[,C…]` | — | One wallpaper across ≥ 2 outputs; target key `span:<raw>`. Repeatable |
| `--bg` | `-b` | ID or path | — | Wallpaper of the target **and** new default wallpaper. Repeatable |
| `--playlist` | | name | — | Target playlist (its first item becomes the target's wallpaper), or window playlist without target; fills an empty default. Repeatable |
| `--scaling` | | `stretch` `fit` `fill` `default` | `default` | *target*; without target, global default for the window and for outputs named **afterwards** |
| `--clamp` | `--clamping` | `clamp` `border` `repeat` | `clamp` | Same rule as `--scaling` |
| `--layer` | | `background` `bottom` `top` `overlay` | `bottom` | Wayland layer (no effect on Windows) |
| `--fps` | `-f` | integer ≥ 1 | 30 | Frame-rate cap |
| `--hwdec` | | `auto` `nvdec-copy` `no` | `auto` | Hardware video decoding |
| `--no-fullscreen-pause` | | | pause on | Keep rendering under fullscreen windows |
| `--fullscreen-pause-only-active` | | | off | Wayland: only active fullscreen windows pause |
| `--fullscreen-pause-ignore-appid` | | text | — | Ignore app_ids containing it; empty values ignored. Repeatable |
| `--volume` | `-v` | integer | 64 | Clamped to 0..=128 (gain = volume / 128); see [Sound](#sound). Exclusive with `--silent` |
| `--silent` | `-s` | | off | Mute wallpapers |
| `--noautomute` | | | automute off | Keep sound while another application plays media (forces `automute = false`) |
| `--no-audio-processing` | | | on | No spectrum capture |
| `--screenshot` | | file | — | `.bmp`, `.png`, `.jpg`, `.jpeg` (case-sensitive) |
| `--screenshot-delay` | | integer ≥ 0 | 5 | Frames before the capture, clamped to 5 |
| `--assets-dir` | | dir | auto | Wallpaper Engine `assets` folder |
| `--disable-particles` | | | off | No particle systems |
| `--disable-mouse` | | | off | No mouse interaction |
| `--disable-parallax` | | | off | No parallax |
| `--list-properties` | `-l` | | off | Print user properties, exit 0 |
| `--set-property` | `--property` | `key[=value]` | — | Override; no `=` means `"1"`; later key wins. Repeatable |
| `--dump-structure` | `-z` | | off | Print the parsed wallpapers |
| `--render-debug` | | `base-only` `no-solid-final` `pass-log` `object=<id>` `skip-object=<id>` `skip-effect=<id>` | — | Repeatable; ids are `u32` |
| `--help` | `-h` | | | Help (`-h` short, `--help` long), exit 0 |
| `--config` | | file | default file | **Lumenwall**: configuration file (must exist) |
| `--no-config` | | | | **Lumenwall**: ignore the configuration file |
| `--version` | | | | **Lumenwall**: print the version, exit 0 |

Syntax accepted: `--opt value`, `--opt=value`, `-x value`, `-xvalue`, `-x=value`, flag
clusters (`-sz`, `-sf60`), `--` ends the options. A value-taking option always consumes the
next argument, even if it starts with `-` (`-v -5`). Non-UTF-8 paths are preserved.

Fatal errors (exit 1, message on stderr followed by `Run '<argv0> --help' for usage.`):
`--window` twice, malformed, or combined with `-r`/`--screen-span` in either order; `-r`
repeated or inside a span; span with < 2 names, a duplicate, or an output already used;
values outside the lists; invalid `--render-debug`; non-integer numbers; `--volume` with
`--silent`; missing value; value given to a flag; two positionals; no wallpaper at all;
unresolvable wallpaper or unknown playlist; bad screenshot extension.

## Configuration file (Lumenwall extension)

`<config dir>/lumenwall/config.toml` (`~/.config/lumenwall/config.toml`; `%APPDATA%\Lumenwall\config.toml`
on Windows), or `--config FILE`. Missing default file: ignored. Unknown keys are errors.

```toml
wallpaper = "2317494988"           # default wallpaper (like the positional)
playlist = "evening"               # window playlist
fps = 30
volume = 64                        # 0..=128 like --volume; 64 = 50 % in the window
silent = false
automute = false                   # true: pause sounds while music, video or a game plays
audio-processing = true
fullscreen-pause = true
fullscreen-pause-only-active = false
fullscreen-pause-ignore-appids = ["mpv"]
gpu-release-delay = 300            # seconds every output stays paused before the GPU device,
                                   # video memory and driver threads are released; 0 = never
scaling = "default"                # global default
clamp = "clamp"
layer = "bottom"
hwdec = "auto"
mouse = true
parallax = true
particles = true
assets-dir = "/path/to/assets"
screenshot = "/tmp/wall.png"
screenshot-delay = 5

[properties]                       # booleans become "1"/"0"
bloom = true

[wallpaper-audio."2317494988"]     # per wallpaper: Workshop ID or folder path
volume = 40                        # percent of the global volume, 0..=100 (default 100)
muted = false

[window]                           # explicit window (exclusive with screen/span)
x = 0
y = 0
width = 800
height = 600

[[screen]]                         # like -r
name = "HDMI-1"
wallpaper = "./my_wallpaper"       # else playlist's first item, else the default
playlist = "day"
scaling = "fill"
clamp = "border"

[[span]]                           # like --screen-span
screens = ["DP-1", "DP-2"]
wallpaper = "123"

[log]
level = "info,lumen=debug"         # LUMENWALL_LOG / RUST_LOG win
file = true                        # default: true on Windows, false elsewhere
directory = "/var/tmp/lumenwall"
max-files = 7
```

Merge with the command line (command line first):

- **Layout**: any `-w`/`-r`/`--screen-span` replaces `[window]`/`[[screen]]`/`[[span]]`.
- **Default content**: positional/`--bg`/untargeted `--playlist` replace `wallpaper` and
  `playlist` together.
- **Scaling/clamp**: the target's own value, then the command-line global value, then the
  file's global value, then the default. Command-line targets keep the §2.3 semantics.
- **Scalars**: command line, then file, then defaults. Flags (`--silent`, `--noautomute`…)
  can only force their setting; `--volume` turns a configured `silent` off.
- **Lists**: `--fullscreen-pause-ignore-appid` replaces the file's list.
- **Properties**: merged by key, command line wins.
- **Per-wallpaper sound**: file only (`[wallpaper-audio]`); a path to a Workshop item folder
  is read as its ID.

## Sound

- **Volume units.** `--volume` and `volume` keep the reference's 0..=128 scale (gain =
  volume / 128). The window, the tray and the IPC protocol use a fraction shown as a
  percentage: percent = volume × 100 / 128, volume = round(percent × 128 / 100). 64 ↔ 50 %,
  128 ↔ 100 %, the reference's default 15 ↔ 12 %. Every whole percent survives the round
  trip (`settings::volume_fraction`, `volume_from_fraction`).
- **Default 64 (50 %).** The reference's 15 is a gain of 0.12 (−18.6 dB) and was reported as
  "no sound". 64 is −6 dB: audible, with headroom on loud wallpapers.
- **Automute off by default.** When on (`automute = true`, or the window's setting), wallpaper
  sounds pause while another program plays music, video or a game; calls, notifications,
  speech, paused or muted streams and Lumenwall itself are ignored (rules in the
  `lumenwall-audio` README). It stays off by default because a wrong "something is playing"
  is silent and unexplained (the reported bug: a Discord call muted every wallpaper), while a
  missed one is heard and fixed with one click; and because on Linux the PulseAudio protocol
  that Lumenwall uses cannot tell an idle open stream from a playing one (Speech Dispatcher's
  idle stream is reported as not paused). Fullscreen windows still pause the wallpapers
  through the fullscreen pause.
- **Per wallpaper.** `[wallpaper-audio."<id or path>"]` sets a wallpaper's level (percent of
  the global volume) and mute; the engine mixes at global × level. Clients change it with
  `set_wallpaper_audio`; the daemon writes the entry (removed when back to 100 %, unmuted)
  and applies it live. The engine has one mixer for every output: when different wallpapers
  are shown at once, the loudest of their levels applies to all of them until the engine
  mixes each wallpaper separately (`ExecutionPlan::engine_volume`).

**Hot reload**: the file's directory is watched with OS notifications (inotify,
ReadDirectoryChangesW; no polling), so editor rename-replace and late creation are seen.
After 150 ms of quiet the file is re-read; unchanged bytes are ignored. A valid file
rebuilds the plan and sends `Control::Reload { plan, changes }` to the engine with the
`PlanChange` list (output mode, target added/removed/content/scaling, fps, audio, pause,
input, particles, hwdec, layer, assets, properties, screenshot). An invalid file is logged
and the running settings stay; a removed file means defaults. The log level is reloaded;
log file settings apply at next start. The directory must exist at startup to be watched.

## IPC daemon (Lumenwall extension)

- `lumenwall` serves `lumenwall-ipc` on `$XDG_RUNTIME_DIR/lumenwall/ipc.sock` (`LUMENWALL_IPC_ENDPOINT` overrides).
  Single instance: a second `lumenwall` sends its wallpaper (`apply`) to the running one, or prints
  "Lumenwall is already running"; exit 0.
- `lumenwall` with no argument and no configured wallpaper starts idle (no surface, no GPU device)
  and waits for a client (deviation: the reference exits with "no wallpaper").
- Client choices (wallpaper per output or for all, scaling, saved properties, fps, fullscreen
  pause, automute, mute, volume, per-wallpaper sound, GPU release delay `gpu_release_delay_s`)
  go into the configuration file (`wallpaper`, `scaling`, `[[screen]]`, `[properties]`, `fps`,
  `silent`, `volume`, `[wallpaper-audio]`, `gpu-release-delay`…), written atomically, and replace the command-line values they concern
  for the rest of the session. Pause and property previews are
  not saved. Property overrides stay keyed by name (shared by wallpapers with the same name).
- Reads never wake the engine; statistics (fps, CPU ms/frame, GPU busy %, process CPU, RSS,
  GPU memory) are measured only while a client subscribes. `examples/lumenctl.rs` drives a daemon.

## Logging and exit codes

- Information → stdout, warnings/errors → stderr, one line each (§19). The "running with"
  line is printed on stdout unconditionally (§1.1) and recorded in the log file.
- Filter: `LUMENWALL_LOG`, else `RUST_LOG`, else `[log] level`, else `info` (`info,lumen=debug`
  in debug builds).
- Log file: daily rotation, 7 files kept, non-blocking writer; default on for Windows
  (`%LOCALAPPDATA%\Lumenwall\logs\lumenwall.<date>.log`), off elsewhere
  (`~/.local/state/lumenwall/logs` when enabled).
- Exit codes: 0 normal stop, `--help`, `--version`, `--list-properties`; 1 any fatal error.
- Stop: SIGINT/SIGTERM (Unix) or console Ctrl+C/Break/close/logoff/shutdown (Windows) send
  `Control::Stop(StopReason::Signal(n))`; the engine leaves its loop, "stopping" is
  logged, exit 0. A second signal exits immediately with code 1.

## Windows

- Release builds are GUI-subsystem executables (`windows_subsystem = "windows"`): no console
  window at login. Debug builds keep the console subsystem.
- Logs go to the rotated file. When started from a terminal, the process attaches to the
  parent console (`AttachConsole`, `CONOUT$` fallback) so `--help` and `--list-properties`
  are visible; redirected output (`> file`) works as is.
- `build.rs` embeds an application manifest with `embed-manifest` (no external tool, works for
  MSVC and GNU): DPI awareness `PerMonitorV2` (fallback `PerMonitor`), UTF-8 code page,
  long paths, `asInvoker`, Common Controls 6.
- A value with `/`, `\` or a drive prefix (`C:`) is a path; Workshop IDs otherwise.
- Without a console there are no console control events: session end and window close are
  window messages that the platform layer must report as `StopReason::Driver`.

## Integration points (traits)

| Trait | Provided by | Contract |
|---|---|---|
| `WallpaperResolver` | `lumenwall-assets` | Workshop ID/path → wallpaper folder (§3.1); errors are shown as is |
| `PlaylistCatalog` | `playlist::WeConfigCatalog` + a `config.json` reader from `lumenwall-assets` (§3.2) | Name → first item folder; unknown names list the available ones |
| `Engine` | platform/render/audio crates | `load` (no window/audio/web for `--list-properties`), `write_properties`, `start`, `write_structure`, `run(Receiver<Control>)` blocking on OS events |
| `Waker` | engine event loop | Wakes the loop after a `Control` message is queued |

The engine is also expected to use `PlaylistRunner` (deadline-driven, `pause_ended` for
`updateonpause`) and `ScreenshotTrigger`.

## Deviations

| Topic | Reference | Lumenwall | Reason |
|---|---|---|---|
| QO-1 `--fps` ≤ 0 | Accepted (hang / uncapped loop) | Fatal error | PARITY 1–2 |
| Default volume | 15 (gain 0.12) | 64 (gain 0.5, 50 % in the window) | Reported as inaudible; see [Sound](#sound) |
| Automute default | On (`--noautomute` turns it off) | Off (`automute = true` turns it on); `--noautomute` still accepted | A false detection silences every wallpaper; see [Sound](#sound) |
| Per-wallpaper sound level | None | `[wallpaper-audio]` | Lumenwall extension |
| QO-2 `--scaling`/`--clamp` order trap | Observable behaviour | Reproduced; `--clamping` accepted as alias | Parity; alias from the reference's user docs |
| QO-25 playlist on a span | Ignored (synthetic key) | Works | PARITY 4/5 |
| QO-28 two positionals | Undetermined | Fatal error | Avoid silently picking one |
| Fatal messages | Printed twice | Printed once | Defect |
| Hot control (§1.2) | None | Config file reload, second signal forces exit | Lumenwall extension |
| `--version`, `--config`, `--no-config` | Absent | Added | Lumenwall extension |
| Help vs errors | Order-dependent | `-h`/`--help`/`--version` anywhere win | Usability |
| Integers (`--fps`, `--volume`, `--screenshot-delay`) | Not specified | Strict decimal `i64` | Only `--window` documents prefix tolerance |
| `--window` size ≤ 0 | Accepted | Fatal error | Cannot render |
| Empty `-r`, `--bg`, `--playlist`, `--screenshot`, `--assets-dir`, `--config`, empty property key | Not specified | Fatal error | Typos |
| Negative `--screenshot-delay` | Not specified | Fatal error | Domain is ≥ 0 |
| Path classification on Windows | Linux only | `\` and `C:` also mean path | §22 |
| Playlist paths on Windows (QO-24) | Linux mapping | Native paths kept | §22 |
| Playlist timer | Checked every iteration | Exposes `deadline()` for the event loop | PERFORMANCE |
| `XDG_SESSION_TYPE`, D-Bus fatal (QO-22) | Fatal | Not handled here (platform/media crates) | PARITY 2 |

## Open questions (non-blocking)

1. §17: the key of the item array in a playlist object is not named; `items` is assumed
   (`settings` is named by §17.3).
2. §2.2: are `--fps`, `--volume`, `--screenshot-delay` parsed with numeric-prefix tolerance
   like `--window`? Does the reference accept `--opt=value`, attached short values, flag
   clusters, abbreviated long options?
3. §1.1: does the "running with" line include `argv[0]`? (Lumenwall includes it.)
4. §20: valid range of `object=`/`skip-object=`/`skip-effect=` ids (`u32` assumed).
5. QO-27: output names for `-r` on Windows (owned by `lumenwall-platform`).

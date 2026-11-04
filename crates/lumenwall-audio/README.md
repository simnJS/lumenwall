# lumenwall-audio

Scene sound playback, audio capture and spectrum analysis for audio-reactive wallpapers, and the two system sources wallpapers and the daemon follow: what is playing (media integration) and the power source (pause on battery).

## Sources (clean room)

- `specs/07-plateforme-audio-video-web.md` §11 (scene sounds), §12 (automute), §13 (capture, FFT, bands, smoothing), §2.2/§2.4 (`--volume`, `--silent`, `--noautomute`, `--no-audio-processing`), §9.4, §15.5 and §16 (media, MPRIS), §21 (tests), §22 (Windows), QO-12 to QO-15, QO-22, QO-23.
- `specs/06-scripting.md` §8.4 (media events), `specs/02-modele-scene.md` §7.6 (sound object keys).
- `docs/research/platform.md`, `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/parity/lumenwall-vs-wallpaper-engine.md` §13 and §15.
- Public sources/docs of `realfft`, `symphonia` 0.6, `cpal` 0.18, `rtrb`, `libpulse-binding` 2.30, `pipewire` 0.10, `windows` 0.62 (`Windows.Media.Control`, `PowerSettingRegisterNotification`), `zbus` 6.2, `image` 0.25; the MPRIS 2.2 and UPower D-Bus interface documentation; Wallpaper Engine's public web wallpaper documentation (`wallpaperRegisterMedia*Listener`).
- Reference wallpapers 3018340632 and 3372348904, used only as test input (`docs/TESTING.md`).

## Coverage

- `spectrum`: §13.3 window accumulation (`WindowAccumulator`), §13.4 formula (realfft, even bins, `0.35·log10`, weights, upper bound 1, 32/16 bands written by the last bin), §13.5 smoothing, §13.6 left = right. `SpectrumAnalyzer::update(dt)` advances `0.3 × dt × reference_fps` (`update_reference_step()` = exactly one reference iteration). No allocation after construction.
- `convert`: native capture (any rate, channels, f32/i16/i32/u8/u16) → reference format: channel mean → polyphase Kaiser-sinc resampler (`resample`) → 8-bit quantisation. `Fragmenter` regroups the stream into the reference's 4410-byte fragments so targets update at the reference cadence (~10 Hz).
- `capture`: `AudioCapture` trait (`start` non-blocking, `stop` closes everything), `CaptureSink`, `default_capture()`.
  - `pulse` (feature): monitor of the default sink in the reference format (U8 mono 44.1 kHz, fragsize 4410, maxlength fragment + 7.5 s); subscription to sink/source/server events and reconnection to the new default monitor.
  - `pipewire` (feature): `stream.capture.sink`, reference format requested from PipeWire, `node.latency = 4410/44100`; the session manager follows the default output.
  - `wasapi` (Windows): cpal input stream on the default render device (loopback), rebuilt when cpal reports a default-device change.
- `automute`: `AutomuteDetector` (§12.1 rules: disabled → never, fullscreen → playing, else watcher), `PlayingFlag` with change callback. Every backend describes its streams as a `Stream` and `classify` decides; only media being played counts (see "Automute rule" below). Watchers: Pulse sink-input subscription, PipeWire registry + node `info` (state) + `Props` (volume, mute) events, Windows `IAudioSessionManager2` + `IAudioSessionEvents` + `IAudioVolumeDuckNotification` + `IMMNotificationClient`. One blocking thread each, connection timeout 2 s, retry every 5 s, loss logged once. Each change of the decision is logged at `info` with the stream that caused it (`automute: 'Spotify' is playing media…`); every stream's verdict is logged at `debug` (`LUMENWALL_LOG=lumenwall_audio=debug`).
- `sound`: `SoundObject::from_json` (`sound`, `playbackmode`, plus WE `volume`, `mintime`, `maxtime`, `startsilent`).
- `playback` (feature): `Player` / `PlayerHandle`. symphonia decoding (mp3, ogg/vorbis, wav, flac, aac/m4a, adpcm) from shared bytes (zero copy from a mapped `.pkg`), resampling to the device, lock-free ring per voice, callback mixing `out = clamp(out + s·volume/128, −1, 1)` per stream in creation order. Preferred device format 48 kHz f32 stereo, 4096 frames; otherwise the device default is adopted.

### Automute rule

A stream mutes the wallpapers only if it is media being played by another program. It does **not** count when it is:

| Case | How it is recognised |
|---|---|
| This process | `application.process.id` (Windows: session process id) |
| No owning process | no `application.process.id` (server internals) and no media role, system sounds session; Spotify's stream has no process id but declares `music` and counts |
| Another Lumenwall process | application name or binary containing `lumenwall` (also `PipeWire ALSA [lumenwall]`, the ALSA plug-in's stream, which has no process id) |
| Paused | Pulse `corked`; PipeWire node state not `running`; Windows session not active |
| Muted or inaudible | mute flag, or average gain below −60 dB (Pulse volumes are cubic) |
| A call | `media.role` `phone` / `Communication`; a call program by binary or name (Discord and its builds, Vesktop, WebCord, Teams, Zoom, Skype, Mumble, TeamSpeak, Slack, Signal, Element, Jitsi, Linphone, Jami…); any application name containing `WEBRTC` (Discord's voice stream is an untagged `WEBRTC VoiceEngine` stream of the `Discord` binary); Windows sessions the system reports as communications (ducking notifications) |
| Speech synthesis | Speech Dispatcher (`speech-dispatcher-*`, `sd_*`), Orca, eSpeak, Festival, RHVoice; role `a11y` / `Accessibility` |
| Another declared role | any `media.role` other than `music`, `video`/`Movie`, `game`, `production` (notifications, alarms, tests…) |

Measured on the development machine (GNOME 50, PipeWire 1.6.2, a Discord call): pipewire-pulse lists Discord's voice stream as `WEBRTC VoiceEngine` / `Discord` without a role (verdict `Call`), Speech Dispatcher's idle stream as **not corked** although its node is idle (`Accessibility`), a paused Spotify as corked with role `music` and no process id (`Paused`), and Lumenwall's own output as `PipeWire ALSA [lumenwall]` without a process id (`Lumenwall`). With automute on, Lumenwall's stream stayed running during the call. The PulseAudio protocol cannot tell an idle open stream from a playing one; the PipeWire backend can (node state). Hence the name rules, and the application's default of automute off (`lumenwall` README).

### Why no kira/rodio

Both sit on cpal like this crate, but keep their output stream (and its wake-ups) alive while idle, sum voices without the reference's per-stream saturation, and add a mixer graph/clock we do not need. The custom path (about 1 000 lines with tests) has an allocation-free callback, closes the device whenever nothing is audible, and reproduces "suspended, not advanced" exactly.

### Engine integration

- §13.1: `SpectrumAnalyzer::new(fps)` if a wallpaper declares `supportsaudioprocessing` and `--no-audio-processing` is absent, else `disabled()`. `default_capture(analyzer.feed())`, `start()` on resume, `stop()` when every output is paused or on user pause. Call `update(dt)` once per rendered frame; upload `left*/right*`.
- `AutomuteDetector::new(settings, Some(cb))` with `cb` forwarding to `PlayerHandle::set_other_app_playing` (reacts even when no frame is rendered); per frame `update(fullscreen)` and forward changes.
- `Player::new(PlayerConfig { volume: clamp_volume(v), silent })`. `set_hidden(true)` while every output is paused (fullscreen, covered, locked, display off), `false` when one shows the wallpaper again; independent of the user pause (`set_paused`). The engine creates the player hidden and un-hides it on its first resume (see Deviations).

### Wake-ups

Capture measured with `voluntary_ctxt_switches` of `/proc/<pid>/task/*/status` over 10 s (release, `--silent`, 3372348904, user pause over IPC); the other rows follow from the code (no audible test is run), the sound row is covered by `hidden_player_keeps_the_device_closed_and_sounds_unconsumed`.

| Thread | Running | Every output paused / user pause |
|---|---|---|
| `lumenwall-capture` (Pulse on pipewire-pulse 1.6.2) | 10.2/s: one per 100 ms fragment pushed by the server, blocking `poll` without timeout | thread joined: 0 (a new one starts on resume, 10.2/s again) |
| `lumenwall-sound` + device thread | only while something is audible | stream closed, control thread blocked on its channel: 0 |
| automute watcher (when enabled) | one per server event | one per server event |

- PipeWire backend: `node.latency = 4410/44100` asks for the same ~10 quanta per second (not measured, not compiled here).
- WASAPI loopback (cpal): event-driven; shared mode signals once per device period (`GetDevicePeriod`, usually 10 ms), so ~100 wake-ups/s while another program plays sound and none while the endpoint is silent or the capture is stopped. Shared mode cannot lengthen that period; a 100 ms timer-driven reader would be polling. Not measured (never run on Windows).
- Not ours: with every output paused, the only remaining wake-ups of the process on the development machine (NVIDIA 595.99.02, Vulkan) come from driver threads created inside `vkCreateDevice` (`libnvidia-glcore`): an unnamed thread at ~100/s (`pthread_cond_timedwait`, 10 ms), `[vkrt]` ~10/s, `[vkps]` ~4/s, 0.00 % CPU. They live as long as the wgpu device does, with or without audio.
- `DEFAULT_VOLUME` is 64 (50 % in the UI, gain 0.5); `REFERENCE_DEFAULT_VOLUME` is the reference's 15 (12 %, gain 0.12, −18.6 dB). The `lumenwall` application sets the volume; per-wallpaper levels are applied by it on top of this global volume.

## Power and media sources

### Power source (`power`, pause on battery)

`PowerWatch::start(callback)` reports `on_battery` once known, then each change; dropping it stops. The `lumenwall` daemon starts it only while its `pause-on-battery` setting is on (default off, Wallpaper Engine's "on battery: keep running" default) and pauses the engine while the callback says "on battery".

- Windows: `PowerSettingRegisterNotification(GUID_ACDC_POWER_SOURCE, DEVICE_NOTIFY_CALLBACK)`: a callback, no window, no thread of ours. First value from `GetSystemPowerStatus` (`ACLineStatus`); the system also calls back right after the registration. `PoDc` (battery) and `PoHot` (short-term source such as a UPS) count as battery. The callback's context is a registration number looked up in a table, so a notification racing with the unregistration finds nothing.
- Linux: UPower `OnBattery` on the system bus, read with `Properties.Get`, then followed through `PropertiesChanged` on `/org/freedesktop/UPower`; read again when `org.freedesktop.UPower` reappears (`NameOwnerChanged`). One `lumenwall-power` thread inside `zbus::block_on`. No system bus or no UPower: logged once, the state stays "on mains".

### Now playing (`media`)

`MediaHub::system()` shares one source between every consumer. `subscribe(waker)` starts it on the first subscriber, the last `MediaSubscription` dropped stops it (thread ends, registrations removed). After each change the source wakes the subscribers; `poll()` returns the new `MediaState` (`available`, and the session's title, artist, subtitle, album, album artist, genres, content type, playback state, position, duration, artwork). Unchanged states (same fields, position moving only with the playback) are not published.

- Windows: `GlobalSystemMediaTransportControlsSessionManager` on a `lumenwall-media` thread in the multithreaded apartment: the system's current session (the one the volume flyout shows), followed through `CurrentSessionChanged`, `MediaPropertiesChanged`, `PlaybackInfoChanged`, `TimelinePropertiesChanged`. The handlers only set a flag; the thread then reads what changed. The thumbnail stream is read on a media properties change only and decoded only when its bytes changed. The timeline position is brought to "now" from `LastUpdatedTime` while playing.
- Linux: MPRIS on the session bus (`lumenwall-media` thread in `zbus::block_on`): `ListNames`, the owner and `PlaybackStatus` of each `org.mpris.MediaPlayer2.*`, `GetAll` of the player followed; then `PropertiesChanged` of `org.mpris.MediaPlayer2.Player`, `Seeked` and `NameOwnerChanged` only. Metadata per §16.3 (`xesam:title`, first `xesam:artist`, `xesam:album`, first `xesam:albumArtist`, `xesam:genre`, `mpris:length` in µs, `mpris:artUrl`). The position is read again only when the playback state changes.
- Artwork (`decode_artwork`): PNG, JPEG, WebP, GIF, BMP or TGA up to 32 MiB and 8192 px per side, reduced to 512 px per side, re-encoded as PNG (fast compression), RGBA8 pixels kept for the palette. Decoded on the source thread, never on the render thread.
- Consumers (`lumenwall-engine`): web pages subscribe while they have a media listener (`web.rs`) and get `wallpaperRegisterMedia*Listener` events with the colours of `lumenwall_web::media::MediaEvent::thumbnail_from_png`; scenes whose scripts handle a media event get `mediaStatusChanged` and the four events of spec 06 §8.4 with the same colours (`script/media.rs`, `MediaFeed`, wired by the engine loop).

### Idle cost (measured)

Windows 11 (10.0.26300), release build of `examples/now_playing.rs` (`cargo run -p lumenwall-audio --release --example now_playing -- 90 <mode>`), one media session open (a paused music player), on mains. Process CPU time and per-thread context switches (`\Thread(now_playing*)\Context Switches/sec`) over 60 s, sampled 15 to 20 s after start; the worker's own count comes from its debug log at stop.

| Mode | Threads once settled | Process CPU over 60 s | Context switches | Lumenwall's thread woken |
|---|---|---|---|---|
| none (baseline) | 1 | 0.0 ms | 0/s | — |
| power | 1 (none of ours) | 0.0 ms | 0/s | no thread |
| media | 5 | 0.0 ms | 0.05/s, one thread | not counted in that run |
| both | 5 | 0.0 ms | 0.12–0.32/s (3 runs), one thread | 0 times in 80 s (counted in the last run) |

- 0.0 ms is below the 15.6 ms accounting granularity of `TotalProcessorTime`.
- The remaining switches are on one thread that appears with the media session client (the system's COM/RPC plumbing). Lumenwall's `lumenwall-media` thread waits on a condition variable without timeout and was not woken once while the session did not change.
- First state 30–50 ms after the subscription (session manager, properties, a 512×512 artwork decoded and re-encoded: about 500 kB of PNG).
- While a player plays, the media thread wakes as often as that player updates its timeline (its own events); not measured here (nothing is played on the test machine).
- Linux (UPower, MPRIS): by construction one thread each, blocked in the zbus runtime's reactor on the bus socket with no timer armed between signals; signals come only on a change (UPower `OnBattery`, a player's properties, a seek). Not measured: no Linux machine was available for this change.

## Build and test matrix

| Feature | Default | Status on the dev machine |
|---|---|---|
| `playback` | yes | built, tested (offline pipeline; ignored test opens the real device with silence) |
| `wasapi` | yes (no-op off Windows) | `cargo check`/`clippy` for `x86_64-pc-windows-gnu` only, never run |
| `pulse` | no | built, tested: no server, mute server, live pipewire-pulse capture (~10 windows/s) and watcher. libpulse dev files are absent: `libpulse-sys` links its `libpulse.so.0` fallback |
| `pipewire` | no | **not compiled**: `libpipewire-0.3`/`libspa-0.2` dev files are not installed. Written against pipewire 0.10.1; run `cargo check -p lumenwall-audio --features pipewire` where they are |
| `power` | yes | Windows: built, tested (notification decoding), run (`now_playing`: on mains, registration callback received). Linux (UPower, zbus): `cargo clippy --target x86_64-unknown-linux-gnu --no-default-features --features power,media --lib -- -D warnings` from Windows only, never run |
| `media` | yes | Windows: built, tested (hub, artwork), run against a real session (`now_playing`). Linux (MPRIS, zbus): MPRIS interpretation tested on every system; the bus glue only type-checked as above, never run |

Linux release builds should enable `pipewire,pulse` (runtime fallback: PipeWire if its socket exists, else Pulse).

`cargo test -p lumenwall-audio [--features pulse]`; `cargo test -p lumenwall-audio --release -- --ignored` for real-data and live tests. Bench: `cargo bench -p lumenwall-audio` (≈1.0 µs per analysis, ≈0.15 µs per frame without new data, ≈45 µs to convert 100 ms of 48 kHz stereo).

## Deviations

- Automute subscribes to server events instead of a synchronous per-frame stream-list query (rule 1); same result, no frame stalls.
- Capture never blocks without a sound server (QO-14, rule 2): bands stay at 0, retry every 5 s.
- The output stream is closed while paused, hidden, muted, automuted, at volume 0 or when every sound ended; the reference outputs silence. Sounds are not advanced either way.
- Scene sounds are suspended (not advanced, output stream closed) while every output is paused, with or without automute: a hidden wallpaper costs no wake-up (`docs/PERFORMANCE.md`), as with Wallpaper Engine's "Pause" playback setting. The reference keeps them playing during the fullscreen pause unless automute is on (§9.4).
- Smoothing uses real elapsed time scaled to the reference frame rate (rule 6).
- No capture data for 0.5 s → targets 0. WASAPI loopback and suspended PipeWire sinks deliver nothing, where the reference's Pulse monitor delivers silence (which gives 0).
- Conversion to the reference format is ours (mean downmix, rounding quantiser); the server's exact algorithms are unknown. Tones differ by ≤ 1 LSB and < 0.01 on their band.
- Rule 4 (QO-12): `volume` scales the object; `random` plays one random file then pauses `[mintime, maxtime]` s; `startsilent` waits for `Player::set_object_playing`. `SoundObject::reference_only()` gives the reference reading. The reference wallpaper's sound object is `random` with 5 mp3: the reference starts all five at once.
- Automute only reacts to media being played (rule 5: Wallpaper Engine mutes while other applications *play* audio): paused, muted or inaudible streams, calls, speech synthesis, notification sounds and Lumenwall's own streams do not count (table above). The reference counts any stream with a PID and a non-zero volume, paused or muted (QO-13), so a voice call or an idle speech stream muted the wallpapers.
- `PlayerConfig::default()` uses volume 64 instead of the reference's 15 (barely audible).
- Mono/surround devices: mono files are duplicated, extra device channels get the matching source channel or silence.
- Media: no polling (rule 1, QO-23): the reference queries the MPRIS position every frame; here the position is extrapolated from the last sample while playing and read again on a state change or a `Seeked` signal. The timeline is therefore sent with each change, not at a fixed rate.
- Media: a missing session bus or media session manager is not fatal (rule 2, QO-22): `available` stays false.
- MPRIS: only the followed player's signals are applied, and another player becomes the followed one when it starts playing (QO-23; the reference applies every player's signals). Without a playing player, the first paused one is followed (the reference follows none until one plays), as Windows' current session keeps a paused player's track.
- MPRIS: `contentType` is empty (MPRIS does not say); artwork behind an `http(s)://` URL is not downloaded (privacy: nothing leaves the machine), only `file://` URLs are read.
- Artwork is reduced to 512 px per side and handed to pages as PNG, whatever the player gave.

## Not tested

- PipeWire backends (not compiled; the automute watcher's node `info` and mute handling were written against pipewire 0.10.1 and are unchecked), Windows backends (not run; the ducking notifications in particular), default-device change following on every backend, audible output and device reopen after a device change.
- No captured reference output is available: spectrum parity is checked against a literal f64 transcription of the formula and hand-computed values. Bands whose power is exactly zero mathematically (perfectly periodic quantised test tones) depend on FFT rounding and cannot match bit for bit; real audio never has them.
- Power: the switch to battery itself (the test machine is a desktop on mains); UPower on a real system. Media: a playing session's timeline events on Windows, session changes between players, every MPRIS player.

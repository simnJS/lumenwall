//! Scene sound playback (spec 07 §11).
//!
//! Architecture (no kira/rodio, see README): one control thread decodes with symphonia into a
//! lock-free ring per voice and owns the cpal output stream; the device callback only sums the
//! rings with the reference's saturating formula. The output stream exists only while
//! something is audible: it is dropped when paused, muted, automuted, at volume 0, or when
//! every sound has ended, so an idle player costs no wake-ups. Voices are not consumed while
//! the stream is down, which matches "suspended, not advanced" (§11.1).

mod decode;
mod mix;

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::sound::{PlaybackMode, SoundObject};
use decode::{DeviceFormat, VoiceFeeder};
use mix::{MixState, VoiceOut};

/// Bytes of an audio file, shared without copying (for example a slice of a memory-mapped
/// package).
pub type SoundData = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// One resolved file of a sound object.
#[derive(Clone)]
pub struct SoundFile {
    /// Asset path; its extension is used as a format hint.
    pub name: String,
    /// File contents.
    pub data: SoundData,
}

/// Player settings from the command line.
#[derive(Clone, Copy, Debug)]
pub struct PlayerConfig {
    /// Global volume `[0, 128]` (`--volume`, default 15).
    pub volume: u8,
    /// `--silent`: no device is opened and no sound is loaded.
    pub silent: bool,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            volume: crate::DEFAULT_VOLUME as u8,
            silent: false,
        }
    }
}

/// Identifies a sound object added to a [`Player`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectId(u32);

// parity: the reference requests 48 kHz float stereo with 4096-frame buffers (§11.1).
const PREFERRED_RATE: u32 = 48_000;
const PREFERRED_CHANNELS: u16 = 2;
const PREFERRED_BUFFER_FRAMES: u32 = 4096;
const COMMAND_QUEUE: usize = 64;
/// Scratch for non-f32 devices, in samples; larger callbacks are mixed in chunks.
const SCRATCH_SAMPLES: usize = 16_384;

/// Flags shared with handles and the device callback.
struct Control {
    paused: AtomicBool,
    muted: AtomicBool,
    other_playing: AtomicBool,
    volume: AtomicU8,
    wake_pending: AtomicBool,
}

impl Control {
    fn suspended(&self) -> bool {
        self.paused.load(Ordering::Acquire)
            || self.muted.load(Ordering::Acquire)
            || self.other_playing.load(Ordering::Acquire)
            || self.volume.load(Ordering::Acquire) == 0
    }

    fn master_gain(&self) -> f32 {
        // parity: gain = volume/128 (§11.1).
        f32::from(self.volume.load(Ordering::Relaxed)) / 128.0
    }
}

enum Cmd {
    Wake,
    Add {
        id: ObjectId,
        object: SoundObject,
        files: Vec<SoundFile>,
    },
    ObjectPlaying(ObjectId, bool),
    ObjectVolume(ObjectId, f32),
    Clear,
    StreamError,
    Shutdown,
}

/// Thread-safe control of a [`Player`] (automute callbacks, fullscreen detector…).
#[derive(Clone)]
pub struct PlayerHandle {
    tx: Option<SyncSender<Cmd>>,
    ctl: Arc<Control>,
}

impl PlayerHandle {
    fn wake(&self) {
        if let Some(tx) = &self.tx
            && !self.ctl.wake_pending.swap(true, Ordering::AcqRel)
            && let Err(TrySendError::Full(_)) = tx.try_send(Cmd::Wake)
        {
            // Queue full: the control thread is awake and will read the flags anyway.
        }
    }

    /// "Another application plays" (§12): sounds are suspended without being consumed.
    pub fn set_other_app_playing(&self, playing: bool) {
        self.ctl.other_playing.store(playing, Ordering::Release);
        self.wake();
    }

    /// Wallpaper paused: the output stream is closed.
    pub fn set_paused(&self, paused: bool) {
        self.ctl.paused.store(paused, Ordering::Release);
        self.wake();
    }

    /// User mute.
    pub fn set_muted(&self, muted: bool) {
        self.ctl.muted.store(muted, Ordering::Release);
        self.wake();
    }

    /// Global volume, clamped to `[0, 128]`.
    pub fn set_volume(&self, volume: i32) {
        self.ctl
            .volume
            .store(crate::clamp_volume(volume), Ordering::Release);
        self.wake();
    }
}

/// Scene sound player.
pub struct Player {
    handle: PlayerHandle,
    thread: Option<JoinHandle<()>>,
    next_id: u32,
}

impl Player {
    /// Creates the player. With `silent`, nothing is opened or loaded (§11.1). Never fails:
    /// device problems are logged and the player stays silent.
    pub fn new(config: PlayerConfig) -> Self {
        let ctl = Arc::new(Control {
            paused: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            other_playing: AtomicBool::new(false),
            volume: AtomicU8::new(config.volume.min(crate::MAX_VOLUME as u8)),
            wake_pending: AtomicBool::new(false),
        });
        if config.silent {
            tracing::info!("sound disabled (--silent): no output device, scene sounds not loaded");
            return Self {
                handle: PlayerHandle { tx: None, ctl },
                thread: None,
                next_id: 0,
            };
        }
        let (tx, rx) = sync_channel(COMMAND_QUEUE);
        let engine_ctl = ctl.clone();
        let engine_tx = tx.clone();
        let thread = thread::Builder::new()
            .name("lumen-sound".into())
            .spawn(move || Engine::new(engine_ctl, engine_tx).run(rx));
        match thread {
            Ok(t) => Self {
                handle: PlayerHandle { tx: Some(tx), ctl },
                thread: Some(t),
                next_id: 0,
            },
            Err(e) => {
                tracing::error!(
                    "sound: cannot start the playback thread: {e}; continuing without sound"
                );
                Self {
                    handle: PlayerHandle { tx: None, ctl },
                    thread: None,
                    next_id: 0,
                }
            }
        }
    }

    /// Whether sounds are loaded and played (not `--silent`, thread running).
    pub fn is_enabled(&self) -> bool {
        self.handle.tx.is_some()
    }

    /// Thread-safe handle for state changes.
    pub fn handle(&self) -> PlayerHandle {
        self.handle.clone()
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.handle.tx {
            let _ = tx.send(cmd);
        }
    }

    /// Adds a scene sound object; its files start at once (§11.2) unless `start_silent`.
    /// `files` are the object's `sound` entries resolved in the wallpaper file space, in order.
    pub fn add_object(&mut self, object: &SoundObject, files: Vec<SoundFile>) -> Option<ObjectId> {
        if !self.is_enabled() {
            return None;
        }
        let id = ObjectId(self.next_id);
        self.next_id += 1;
        self.send(Cmd::Add {
            id,
            object: object.clone(),
            files,
        });
        Some(id)
    }

    /// Starts/resumes (`true`) or pauses (`false`) one object (SceneScript, `startsilent`). An
    /// object whose single play ended restarts from the beginning.
    pub fn set_object_playing(&self, id: ObjectId, playing: bool) {
        self.send(Cmd::ObjectPlaying(id, playing));
    }

    /// Changes one object's gain (Wallpaper Engine `volume`).
    pub fn set_object_volume(&self, id: ObjectId, volume: f32) {
        self.send(Cmd::ObjectVolume(id, volume));
    }

    /// Removes every sound (scene unloaded).
    pub fn clear(&self) {
        self.send(Cmd::Clear);
    }

    /// See [`PlayerHandle::set_paused`].
    pub fn set_paused(&self, paused: bool) {
        self.handle.set_paused(paused);
    }

    /// See [`PlayerHandle::set_muted`].
    pub fn set_muted(&self, muted: bool) {
        self.handle.set_muted(muted);
    }

    /// See [`PlayerHandle::set_other_app_playing`].
    pub fn set_other_app_playing(&self, playing: bool) {
        self.handle.set_other_app_playing(playing);
    }

    /// See [`PlayerHandle::set_volume`].
    pub fn set_volume(&self, volume: i32) {
        self.handle.set_volume(volume);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct ObjectState {
    id: ObjectId,
    voices: Vec<VoiceFeeder>,
}

/// Control thread state.
struct Engine {
    ctl: Arc<Control>,
    tx: SyncSender<Cmd>,
    host: Option<cpal::Host>,
    stream: Option<cpal::Stream>,
    format: DeviceFormat,
    mix: Arc<Mutex<MixState>>,
    objects: Vec<ObjectState>,
    seed: u64,
    failed_logged: bool,
}

impl Engine {
    fn new(ctl: Arc<Control>, tx: SyncSender<Cmd>) -> Self {
        let format = DeviceFormat {
            rate: PREFERRED_RATE,
            channels: usize::from(PREFERRED_CHANNELS),
        };
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x5EED, |d| d.as_nanos() as u64);
        Self {
            ctl,
            tx,
            host: None,
            stream: None,
            format,
            mix: Arc::new(Mutex::new(MixState::new(format.channels))),
            objects: Vec::new(),
            seed,
            failed_logged: false,
        }
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        // Blocks while idle: no timer, no polling.
        while let Ok(cmd) = rx.recv() {
            if !self.handle(cmd) {
                break;
            }
            while let Ok(cmd) = rx.try_recv() {
                if !self.handle(cmd) {
                    self.stream = None;
                    return;
                }
            }
            self.refresh();
        }
        self.stream = None;
    }

    /// Applies one command; `false` to exit.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Wake => {}
            Cmd::Add { id, object, files } => self.add(id, &object, files),
            Cmd::ObjectPlaying(id, playing) => self.set_playing(id, playing),
            Cmd::ObjectVolume(id, volume) => {
                for o in self.objects.iter().filter(|o| o.id == id) {
                    for v in &o.voices {
                        v.shared.set_gain(volume.max(0.0));
                    }
                }
            }
            Cmd::Clear => {
                self.stream = None;
                let old = self
                    .mix
                    .lock()
                    .map(|mut m| std::mem::take(&mut m.voices))
                    .unwrap_or_default();
                drop(old);
                self.objects.clear();
            }
            Cmd::StreamError => {
                // Device invalidated (unplugged, default output changed): rebuild on refresh.
                self.stream = None;
            }
            Cmd::Shutdown => return false,
        }
        true
    }

    fn add(&mut self, id: ObjectId, object: &SoundObject, files: Vec<SoundFile>) {
        let gain = object.volume;
        let times = (object.min_time, object.max_time);
        let active = !object.start_silent;
        let mut voices = Vec::new();
        let mut outs = Vec::new();
        let mut push = |files: Vec<SoundFile>, seed: u64| {
            let (feeder, out) =
                VoiceFeeder::new(files, object.mode, gain, times, active, self.format, seed);
            voices.push(feeder);
            outs.push(out);
        };
        match object.mode {
            // One voice drawing a random file each time (Wallpaper Engine "random").
            PlaybackMode::Random => push(files, self.seed ^ u64::from(id.0)),
            // parity: every file of the array starts simultaneously (§11.2).
            PlaybackMode::Loop | PlaybackMode::Single => {
                for (i, f) in files.into_iter().enumerate() {
                    push(vec![f], self.seed ^ ((u64::from(id.0) << 32) | i as u64));
                }
            }
        }
        for v in &mut voices {
            v.fill();
        }
        if let Ok(mut m) = self.mix.lock() {
            m.voices.extend(outs);
        }
        self.objects.push(ObjectState { id, voices });
    }

    fn set_playing(&mut self, id: ObjectId, playing: bool) {
        let format = self.format;
        let mut replaced: Vec<(usize, VoiceOut)> = Vec::new();
        let mut index = 0;
        for o in &mut self.objects {
            for v in &mut o.voices {
                if o.id == id {
                    if playing && v.shared.finished.load(Ordering::Acquire) {
                        let out = v.restart(format);
                        v.fill();
                        replaced.push((index, out));
                    }
                    v.shared.active.store(playing, Ordering::Release);
                }
                index += 1;
            }
        }
        self.swap_outs(replaced);
    }

    /// Replaces callback-side voices; the old ones are dropped outside the lock.
    fn swap_outs(&self, replaced: Vec<(usize, VoiceOut)>) {
        if replaced.is_empty() {
            return;
        }
        let mut old = Vec::with_capacity(replaced.len());
        if let Ok(mut m) = self.mix.lock() {
            for (i, out) in replaced {
                if let Some(slot) = m.voices.get_mut(i) {
                    old.push(std::mem::replace(slot, out));
                }
            }
        }
        drop(old);
    }

    fn audible(&self) -> bool {
        self.objects
            .iter()
            .any(|o| o.voices.iter().any(VoiceFeeder::audible))
    }

    /// Refills rings and opens/closes the output stream to match the state.
    fn refresh(&mut self) {
        self.ctl.wake_pending.store(false, Ordering::Release);
        for o in &mut self.objects {
            for v in &mut o.voices {
                if v.audible() {
                    v.fill();
                }
            }
        }
        let want = !self.ctl.suspended() && self.audible();
        if !want {
            // Silent output = no device wake-ups.
            self.stream = None;
        } else if self.stream.is_none() {
            self.open_stream();
        }
    }

    fn open_stream(&mut self) {
        let host = self.host.get_or_insert_with(cpal::default_host);
        let Some(device) = host.default_output_device() else {
            self.log_failure("no default output device");
            return;
        };
        let preferred = StreamConfig {
            channels: PREFERRED_CHANNELS,
            sample_rate: PREFERRED_RATE,
            buffer_size: BufferSize::Fixed(PREFERRED_BUFFER_FRAMES),
        };
        let mut attempts = vec![
            (preferred, SampleFormat::F32),
            (
                StreamConfig {
                    buffer_size: BufferSize::Default,
                    ..preferred
                },
                SampleFormat::F32,
            ),
        ];
        // The system may impose another format, which is then adopted (§11.1).
        if let Ok(def) = device.default_output_config() {
            attempts.push((def.config(), def.sample_format()));
        }
        for (config, format) in attempts {
            let stream = match format {
                SampleFormat::F32 => self.build::<f32>(&device, &config),
                SampleFormat::I16 => self.build::<i16>(&device, &config),
                SampleFormat::U16 => self.build::<u16>(&device, &config),
                SampleFormat::I32 => self.build::<i32>(&device, &config),
                SampleFormat::F64 => self.build::<f64>(&device, &config),
                SampleFormat::U8 => self.build::<u8>(&device, &config),
                _ => continue,
            };
            let Ok(stream) = stream else { continue };
            let new_format = DeviceFormat {
                rate: config.sample_rate,
                channels: usize::from(config.channels),
            };
            self.adopt_format(new_format);
            if let Err(e) = stream.play() {
                self.log_failure(&format!("cannot start output stream: {e}"));
                return;
            }
            tracing::debug!(
                "sound: output {} Hz, {} channels, {:?}",
                config.sample_rate,
                config.channels,
                format
            );
            self.stream = Some(stream);
            self.failed_logged = false;
            return;
        }
        self.log_failure("cannot open the output device");
    }

    fn log_failure(&mut self, msg: &str) {
        // §11.1: initialisation failure → logged once, playback continues without sound.
        if !self.failed_logged {
            tracing::error!("sound: {msg}; continuing without sound");
            self.failed_logged = true;
        }
    }

    /// Rebuilds the rings when the device format differs from the one they were built for.
    fn adopt_format(&mut self, format: DeviceFormat) {
        if self.format == format {
            return;
        }
        self.format = format;
        let mut outs = Vec::new();
        for o in &mut self.objects {
            for v in &mut o.voices {
                if v.format() != format {
                    // Restarting loses at most the buffered half second; only on device change.
                    outs.push(v.restart(format));
                    v.fill();
                }
            }
        }
        if let Ok(mut m) = self.mix.lock() {
            m.channels = format.channels;
            if outs.len() == m.voices.len() {
                let old = std::mem::replace(&mut m.voices, outs);
                drop(m);
                drop(old);
            }
        }
    }

    fn build<T: OutputSample>(
        &self,
        device: &cpal::Device,
        config: &StreamConfig,
    ) -> Result<cpal::Stream, cpal::Error> {
        let mix = self.mix.clone();
        let ctl = self.ctl.clone();
        let tx = self.tx.clone();
        let err_tx = self.tx.clone();
        let mut scratch = if T::NEEDS_SCRATCH {
            vec![0.0f32; SCRATCH_SAMPLES]
        } else {
            Vec::new()
        };
        let data = move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            let master = ctl.master_gain();
            let wake = match mix.lock() {
                Ok(mut m) => T::mix_into(out, &mut m, master, &mut scratch),
                Err(_) => {
                    out.fill(T::EQUILIBRIUM);
                    false
                }
            };
            if wake && !ctl.wake_pending.swap(true, Ordering::AcqRel) {
                let _ = tx.try_send(Cmd::Wake);
            }
        };
        let error = move |e: cpal::Error| match e.kind() {
            ErrorKind::DeviceNotAvailable
            | ErrorKind::StreamInvalidated
            | ErrorKind::DeviceChanged => {
                tracing::info!("sound: output device changed ({e}); reopening");
                let _ = err_tx.try_send(Cmd::StreamError);
            }
            _ => tracing::debug!("sound: stream error: {e}"),
        };
        device.build_output_stream::<T, _, _>(*config, data, error, None)
    }
}

/// Device sample types: f32 is mixed in place, others through an f32 scratch.
trait OutputSample: SizedSample + Send + 'static {
    const NEEDS_SCRATCH: bool;
    fn mix_into(out: &mut [Self], mix: &mut MixState, master: f32, scratch: &mut [f32]) -> bool;
}

impl OutputSample for f32 {
    const NEEDS_SCRATCH: bool = false;
    fn mix_into(out: &mut [Self], mix: &mut MixState, master: f32, _: &mut [f32]) -> bool {
        mix.mix(out, master)
    }
}

macro_rules! converted_output {
    ($($t:ty),*) => {$(
        impl OutputSample for $t {
            const NEEDS_SCRATCH: bool = true;
            fn mix_into(out: &mut [Self], mix: &mut MixState, master: f32, scratch: &mut [f32]) -> bool {
                let step = scratch.len() - scratch.len() % mix.channels;
                let mut wake = false;
                for block in out.chunks_mut(step) {
                    let s = &mut scratch[..block.len()];
                    wake |= mix.mix(s, master);
                    for (d, &x) in block.iter_mut().zip(s.iter()) {
                        *d = <$t as FromSample<f32>>::from_sample_(x);
                    }
                }
                wake
            }
        }
    )*};
}

converted_output!(i16, u16, i32, f64, u8);

#[cfg(test)]
mod tests;

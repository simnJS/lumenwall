//! Decoding of scene sounds into device-format ring buffers (control thread side).

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rtrb::{Producer, RingBuffer};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use super::SoundFile;
use super::mix::{VoiceOut, VoiceShared};
use crate::resample::{Quality, Resampler};
use crate::sound::PlaybackMode;

/// Seconds of audio buffered per voice.
const RING_SECONDS: f32 = 0.5;
/// Bytes of silence emitted for a packet that fails to decode (§11.1).
const DECODE_ERROR_SILENCE_BYTES: usize = 1024;

static ZEROS: [f32; 1024] = [0.0; 1024];

/// Output format of the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeviceFormat {
    pub(crate) rate: u32,
    pub(crate) channels: usize,
}

struct SharedBytes(super::SoundData);

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

enum Next {
    Data,
    DecodeError,
    End,
}

/// One open file.
struct Decoder {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    rate: u32,
    channels: usize,
    buf: Vec<f32>,
}

impl Decoder {
    fn open(file: &SoundFile) -> Result<Self, String> {
        let source = Cursor::new(SharedBytes(file.data.clone()));
        let mss = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        if let Some(ext) = Path::new(&file.name).extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let reader = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| format!("unrecognised audio file: {e}"))?;
        let track = reader
            .default_track(TrackType::Audio)
            .ok_or("no audio track")?;
        let Some(CodecParameters::Audio(params)) = &track.codec_params else {
            return Err("no audio codec parameters".into());
        };
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported codec: {e}"))?;
        let rate = params.sample_rate.unwrap_or(44_100);
        let channels = params.channels.as_ref().map_or(2, |c| c.count()).max(1);
        let track = track.id;
        Ok(Self {
            reader,
            decoder,
            track,
            rate,
            channels,
            buf: Vec::with_capacity(8192),
        })
    }

    /// Decodes the next packet of the track into `self.buf` (interleaved f32).
    fn next(&mut self) -> Next {
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Next::End,
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(_) => return Next::End,
            };
            if packet.track_id != self.track {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    self.rate = buf.spec().rate();
                    self.channels = buf.spec().channels().count().max(1);
                    buf.copy_to_vec_interleaved::<f32>(&mut self.buf);
                    return Next::Data;
                }
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                    return Next::DecodeError;
                }
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                }
                Err(_) => return Next::End,
            }
        }
    }
}

/// Native sample rate and channel count of a sound file.
#[cfg(test)]
pub(crate) fn probe_format(file: &SoundFile) -> Result<(u32, usize), String> {
    Decoder::open(file).map(|d| (d.rate, d.channels))
}

/// Maps one frame layout to the device's (mono ↔ stereo ↔ surround).
fn map_channels(src: &[f32], sc: usize, dc: usize, out: &mut Vec<f32>) {
    if sc == dc {
        out.extend_from_slice(src);
        return;
    }
    for frame in src.chunks_exact(sc) {
        if dc == 1 {
            out.push(frame.iter().sum::<f32>() / sc as f32);
            continue;
        }
        let l = frame[0];
        let r = if sc >= 2 { frame[1] } else { frame[0] };
        out.push(l);
        out.push(r);
        // Extra device channels take the matching source channel, else silence.
        let extra = frame.iter().skip(2).copied().chain(std::iter::repeat(0.0));
        out.extend(extra.take(dc - 2));
    }
}

/// Decoder side of a voice: keeps its ring buffer filled.
pub(crate) struct VoiceFeeder {
    pub(crate) shared: Arc<VoiceShared>,
    producer: Producer<f32>,
    files: Vec<SoundFile>,
    mode: PlaybackMode,
    min_time: f32,
    max_time: f32,
    rng: u64,
    current: usize,
    decoder: Option<Decoder>,
    resampler: Option<Resampler>,
    resampler_rate: u32,
    pending: Vec<f32>,
    pending_pos: usize,
    silence_samples: u64,
    dev: DeviceFormat,
    done: bool,
}

impl VoiceFeeder {
    /// New voice for `files` (one file for loop/single, all files for random) and its
    /// callback side.
    pub(crate) fn new(
        files: Vec<SoundFile>,
        mode: PlaybackMode,
        gain: f32,
        (min_time, max_time): (f32, f32),
        active: bool,
        dev: DeviceFormat,
        seed: u64,
    ) -> (Self, VoiceOut) {
        let shared = VoiceShared::new(gain, active);
        let (producer, out) = Self::ring(&shared, dev);
        let feeder = Self {
            shared,
            producer,
            files,
            mode,
            min_time,
            max_time,
            rng: seed | 1,
            current: 0,
            decoder: None,
            resampler: None,
            resampler_rate: 0,
            pending: Vec::with_capacity(16_384),
            pending_pos: 0,
            silence_samples: 0,
            dev,
            done: false,
        };
        (feeder, out)
    }

    fn ring(shared: &Arc<VoiceShared>, dev: DeviceFormat) -> (Producer<f32>, VoiceOut) {
        let cap = ((dev.rate as f32 * RING_SECONDS) as usize).max(1024) * dev.channels;
        let (producer, consumer) = RingBuffer::new(cap);
        (
            producer,
            VoiceOut {
                consumer,
                shared: shared.clone(),
                low_water: cap / 2,
            },
        )
    }

    /// Starts over from the beginning with a fresh ring (new device format or restart).
    /// Returns the new callback side, which replaces the old one.
    pub(crate) fn restart(&mut self, dev: DeviceFormat) -> VoiceOut {
        let (producer, out) = Self::ring(&self.shared, dev);
        self.producer = producer;
        self.dev = dev;
        self.decoder = None;
        self.resampler = None;
        self.pending.clear();
        self.pending_pos = 0;
        self.silence_samples = 0;
        self.done = false;
        self.shared.finished.store(false, Ordering::Release);
        self.shared.drained.store(false, Ordering::Release);
        out
    }

    /// Device format the ring is built for.
    pub(crate) fn format(&self) -> DeviceFormat {
        self.dev
    }

    /// Whether this voice still has (or will have) samples to play.
    pub(crate) fn audible(&self) -> bool {
        self.shared.active.load(Ordering::Relaxed) && !self.shared.drained.load(Ordering::Acquire)
    }

    fn next_random(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// Decodes until the ring is full or the voice ends. Never blocks.
    pub(crate) fn fill(&mut self) {
        let dc = self.dev.channels;
        loop {
            if self.pending_pos < self.pending.len() {
                let (pushed, rest) = self
                    .producer
                    .push_partial_slice(&self.pending[self.pending_pos..]);
                self.pending_pos += pushed.len();
                if !rest.is_empty() {
                    return;
                }
            }
            self.pending.clear();
            self.pending_pos = 0;
            if self.producer.slots() == 0 {
                return;
            }
            if self.silence_samples > 0 {
                let n = (self.silence_samples as usize).min(ZEROS.len() - ZEROS.len() % dc);
                let (pushed, _) = self.producer.push_partial_slice(&ZEROS[..n]);
                self.silence_samples -= pushed.len() as u64;
                if pushed.len() < n {
                    return;
                }
                continue;
            }
            if self.done {
                return;
            }
            self.produce();
        }
    }

    /// Produces the next block into `pending`, handling end of file per playback mode.
    fn produce(&mut self) {
        if self.decoder.is_none() {
            if self.files.is_empty() {
                self.finish();
                return;
            }
            if self.mode == PlaybackMode::Random {
                self.current = (self.next_random() % self.files.len() as u64) as usize;
            }
            match Decoder::open(&self.files[self.current]) {
                Ok(d) => self.decoder = Some(d),
                Err(e) => {
                    tracing::warn!("scene sound {}: {e}", self.files[self.current].name);
                    if self.mode == PlaybackMode::Random && self.files.len() > 1 {
                        // Another file may work; insert the pause and try again later.
                        self.start_pause();
                    } else {
                        self.finish();
                    }
                    return;
                }
            }
        }
        let dc = self.dev.channels;
        let Some(dec) = self.decoder.as_mut() else {
            return;
        };
        match dec.next() {
            Next::Data => {
                let sc = dec.channels;
                if dec.rate != self.resampler_rate
                    || self.resampler.as_ref().is_none_or(|r| r.channels() != sc)
                {
                    self.resampler = Some(Resampler::new(
                        dec.rate,
                        self.dev.rate,
                        sc,
                        Quality::Playback,
                    ));
                    self.resampler_rate = dec.rate;
                }
                let Self {
                    resampler,
                    pending,
                    decoder,
                    ..
                } = self;
                if let (Some(r), Some(d)) = (resampler.as_mut(), decoder.as_ref()) {
                    r.process(&d.buf, &mut |block: &[f32]| {
                        map_channels(block, sc, dc, pending)
                    });
                }
            }
            Next::DecodeError => {
                // parity: a packet that fails to decode yields 1024 bytes of silence (§11.1).
                let samples = DECODE_ERROR_SILENCE_BYTES / std::mem::size_of::<f32>();
                self.silence_samples += (samples - samples % dc) as u64;
            }
            Next::End => match self.mode {
                PlaybackMode::Loop => {
                    // parity: loop restarts at the beginning as soon as the file ends (§11.2).
                    // The resampler keeps its history so the loop point is seamless.
                    self.decoder = None;
                }
                PlaybackMode::Single => {
                    self.flush_resampler();
                    self.finish();
                }
                PlaybackMode::Random => {
                    self.flush_resampler();
                    self.start_pause();
                }
            },
        }
    }

    fn flush_resampler(&mut self) {
        let dc = self.dev.channels;
        let Self {
            resampler, pending, ..
        } = self;
        if let Some(r) = resampler.as_mut() {
            let sc = r.channels();
            r.flush(&mut |block: &[f32]| map_channels(block, sc, dc, pending));
        }
    }

    /// Random mode: pause of `[mintime, maxtime]` seconds before the next random file.
    fn start_pause(&mut self) {
        self.decoder = None;
        let span = (self.max_time - self.min_time).max(0.0);
        let unit = (self.next_random() >> 11) as f64 / (1u64 << 53) as f64;
        let seconds = f64::from(self.min_time) + unit * f64::from(span);
        let frames = (seconds * f64::from(self.dev.rate)) as u64;
        self.silence_samples = frames * self.dev.channels as u64;
    }

    fn finish(&mut self) {
        self.decoder = None;
        self.done = true;
        self.shared.finished.store(true, Ordering::Release);
    }
}

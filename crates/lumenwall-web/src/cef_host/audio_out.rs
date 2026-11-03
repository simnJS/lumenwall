//! Page audio played by the host with Lumenwall's volume, mute and automute (spec 07 QO-21).
//!
//! The browser hands its audio to `AudioHandler` instead of playing it; packets go through a
//! lock-free ring to an output stream owned by a dedicated thread. The stream is paused whenever
//! nothing plays or the page is muted, so a silent page costs no audio wake-ups.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::ipc::AudioOutput;

/// Half a second of stereo audio at 48 kHz.
const RING_SAMPLES: usize = 48_000;

enum Control {
    Play,
    Pause,
}

struct Levels {
    gain: AtomicU32,
    muted: AtomicBool,
}

struct Active {
    producer: rtrb::Producer<f32>,
    control: mpsc::Sender<Control>,
    rate: u32,
    playing: bool,
}

/// Output for the page's audio. `Send + Sync`; the stream itself lives on its own thread.
pub struct AudioOut {
    levels: Arc<Levels>,
    active: Mutex<Option<Active>>,
    /// The browser currently delivers a stream.
    streaming: AtomicBool,
    failed: AtomicBool,
}

impl AudioOut {
    pub fn new(state: AudioOutput) -> Self {
        let out = Self {
            levels: Arc::new(Levels {
                gain: AtomicU32::new(0),
                muted: AtomicBool::new(false),
            }),
            active: Mutex::new(None),
            streaming: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        };
        out.set_state(state);
        out
    }

    /// Volume and mute (silent, automute, pause).
    pub fn set_state(&self, state: AudioOutput) {
        self.levels
            .gain
            .store(state.volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        self.levels.muted.store(state.muted, Ordering::Relaxed);
        self.update_playback();
    }

    /// Sample rate to request from the browser; `None` if no output device is usable (the
    /// browser then plays the audio itself, only mute applies).
    pub fn device_rate(&self) -> Option<u32> {
        if self.failed.load(Ordering::Relaxed) {
            return None;
        }
        let mut active = self.active.lock().ok()?;
        if active.is_none() {
            match start_output(self.levels.clone()) {
                Ok(a) => *active = Some(a),
                Err(e) => {
                    tracing::warn!("web audio output unavailable: {e}");
                    self.failed.store(true, Ordering::Relaxed);
                    return None;
                }
            }
        }
        active.as_ref().map(|a| a.rate)
    }

    pub fn stream_started(&self) {
        self.streaming.store(true, Ordering::Relaxed);
        self.update_playback();
    }

    pub fn stream_stopped(&self) {
        self.streaming.store(false, Ordering::Relaxed);
        self.update_playback();
    }

    /// Queues one packet of planar float samples (`channels[c][frame]`).
    pub fn push_planar(&self, channels: &[&[f32]]) {
        if self.levels.muted.load(Ordering::Relaxed) || channels.is_empty() {
            return;
        }
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        let Some(active) = active.as_mut() else {
            return;
        };
        let left = channels[0];
        let right = channels.get(1).copied().unwrap_or(left);
        let frames = left.len().min(right.len());
        if active.producer.slots() < frames * 2 {
            return; // Output stalled: drop rather than grow latency.
        }
        for i in 0..frames {
            let _ = active.producer.push(left[i]);
            let _ = active.producer.push(right[i]);
        }
    }

    fn update_playback(&self) {
        let play =
            self.streaming.load(Ordering::Relaxed) && !self.levels.muted.load(Ordering::Relaxed);
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        if let Some(active) = active.as_mut()
            && active.playing != play
        {
            active.playing = play;
            let _ = active
                .control
                .send(if play { Control::Play } else { Control::Pause });
        }
    }
}

fn start_output(levels: Arc<Levels>) -> Result<Active, String> {
    let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(RING_SAMPLES);
    let (control, control_rx) = mpsc::channel::<Control>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
    std::thread::Builder::new()
        .name("lumenwall-web-audio".into())
        .spawn(move || {
            let setup = || -> Result<(cpal::Stream, u32), String> {
                let host = cpal::default_host();
                let device = host.default_output_device().ok_or("no output device")?;
                let config = device
                    .default_output_config()
                    .map_err(|e| e.to_string())?
                    .config();
                let channels = usize::from(config.channels).max(1);
                let rate = config.sample_rate;
                let stream = device
                    .build_output_stream::<f32, _, _>(
                        config,
                        move |out: &mut [f32], _| {
                            let gain = f32::from_bits(levels.gain.load(Ordering::Relaxed));
                            let muted = levels.muted.load(Ordering::Relaxed);
                            for frame in out.chunks_mut(channels) {
                                let l = consumer.pop().unwrap_or(0.0);
                                let r = consumer.pop().unwrap_or(l);
                                let (l, r) = if muted {
                                    (0.0, 0.0)
                                } else {
                                    (l * gain, r * gain)
                                };
                                match frame {
                                    [mono] => *mono = 0.5 * (l + r),
                                    [a, b, rest @ ..] => {
                                        *a = l;
                                        *b = r;
                                        rest.fill(0.0);
                                    }
                                    [] => {}
                                }
                            }
                        },
                        |e| tracing::warn!("web audio output: {e}"),
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                stream.pause().map_err(|e| e.to_string())?;
                Ok((stream, rate))
            };
            let stream = match setup() {
                Ok((stream, rate)) => {
                    let _ = ready_tx.send(Ok(rate));
                    stream
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // Blocks between commands: no wake-ups while idle.
            for command in control_rx {
                let result = match command {
                    Control::Play => stream.play(),
                    Control::Pause => stream.pause(),
                };
                if let Err(e) = result {
                    tracing::warn!("web audio output: {e}");
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let rate = ready_rx.recv().map_err(|e| e.to_string())??;
    Ok(Active {
        producer,
        control,
        rate,
        playing: false,
    })
}

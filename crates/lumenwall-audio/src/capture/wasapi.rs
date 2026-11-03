//! Windows capture: WASAPI loopback of the default render device (spec 07 §22).
//!
//! cpal opens an input stream on an output device in loopback mode. The mix format (usually
//! float 48 kHz stereo) is converted to the reference format by [`CaptureSink::push_native`].
//! cpal reports a change of default device as a stream error; the stream is then rebuilt on
//! the new default device. The thread blocks on a channel between events.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, SampleFormat, SizedSample, StreamConfig};

use super::{AudioCapture, CaptureSink, CaptureState, SharedState};
use crate::convert::CaptureSample;
use crate::spectrum::SpectrumFeed;

/// Retry delay when no render device exists.
const RETRY_DELAY: Duration = Duration::from_secs(5);

enum Msg {
    Rebuild,
    Stop,
}

/// WASAPI loopback capture backend.
pub struct WasapiLoopbackCapture {
    feed: SpectrumFeed,
    state: SharedState,
    worker: Option<(Sender<Msg>, JoinHandle<()>)>,
}

impl WasapiLoopbackCapture {
    /// Backend publishing to `feed`.
    pub fn new(feed: SpectrumFeed) -> Self {
        Self {
            feed,
            state: SharedState::new(),
            worker: None,
        }
    }
}

impl AudioCapture for WasapiLoopbackCapture {
    fn start(&mut self) {
        if self.worker.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let feed = self.feed.clone();
        let state = self.state.clone();
        let err_tx = tx.clone();
        self.state.set(CaptureState::Connecting);
        match thread::Builder::new()
            .name("lumenwall-capture".into())
            .spawn(move || run(feed, state, err_tx, rx))
        {
            Ok(t) => self.worker = Some((tx, t)),
            Err(e) => {
                tracing::error!("audio capture: cannot spawn thread: {e}");
                self.state.set(CaptureState::Unavailable);
            }
        }
    }

    fn stop(&mut self) {
        if let Some((tx, t)) = self.worker.take() {
            let _ = tx.send(Msg::Stop);
            let _ = t.join();
        }
        self.state.set(CaptureState::Stopped);
    }

    fn state(&self) -> CaptureState {
        self.state.get()
    }

    fn backend_name(&self) -> &'static str {
        "wasapi-loopback"
    }
}

impl Drop for WasapiLoopbackCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(feed: SpectrumFeed, state: SharedState, tx: Sender<Msg>, rx: Receiver<Msg>) {
    let host = cpal::default_host();
    let mut warned = false;
    loop {
        let stream = open(&host, &feed, &tx);
        let wait = match &stream {
            Some(_) => {
                state.set(CaptureState::Running);
                warned = false;
                None
            }
            None => {
                state.set(CaptureState::Unavailable);
                if !warned {
                    tracing::warn!(
                        "audio capture: no loopback device; retrying every {} s",
                        RETRY_DELAY.as_secs()
                    );
                    warned = true;
                }
                Some(RETRY_DELAY)
            }
        };
        let msg = match wait {
            None => rx.recv().unwrap_or(Msg::Stop),
            Some(d) => match rx.recv_timeout(d) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => Msg::Rebuild,
                Err(RecvTimeoutError::Disconnected) => Msg::Stop,
            },
        };
        drop(stream);
        if let Msg::Stop = msg {
            return;
        }
    }
}

fn open(host: &cpal::Host, feed: &SpectrumFeed, tx: &Sender<Msg>) -> Option<cpal::Stream> {
    let device = host.default_output_device()?;
    let supported = device.default_output_config().ok()?;
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, feed, tx),
        SampleFormat::I16 => build::<i16>(&device, &config, feed, tx),
        SampleFormat::I32 => build::<i32>(&device, &config, feed, tx),
        SampleFormat::U8 => build::<u8>(&device, &config, feed, tx),
        SampleFormat::U16 => build::<u16>(&device, &config, feed, tx),
        SampleFormat::F64 => build::<f64>(&device, &config, feed, tx),
        other => {
            tracing::warn!("audio capture: unsupported loopback format {other:?}");
            None
        }
    }?;
    stream.play().ok()?;
    Some(stream)
}

fn build<T: SizedSample + CaptureSample + Send + 'static>(
    device: &cpal::Device,
    config: &StreamConfig,
    feed: &SpectrumFeed,
    tx: &Sender<Msg>,
) -> Option<cpal::Stream> {
    let mut sink = CaptureSink::new(feed.clone());
    let rate = config.sample_rate;
    let channels = usize::from(config.channels);
    let tx = tx.clone();
    // An input stream on a render device enables WASAPI loopback.
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _| sink.push_native(data, rate, channels),
            move |e: cpal::Error| match e.kind() {
                ErrorKind::DeviceNotAvailable
                | ErrorKind::StreamInvalidated
                | ErrorKind::DeviceChanged => {
                    let _ = tx.send(Msg::Rebuild);
                }
                _ => tracing::debug!("audio capture: stream error: {e}"),
            },
            None,
        )
        .map_err(|e| tracing::warn!("audio capture: cannot open loopback stream: {e}"))
        .ok()
}

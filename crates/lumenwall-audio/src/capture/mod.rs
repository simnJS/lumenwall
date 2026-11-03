//! Capture of what the machine plays, for audio-reactive wallpapers (spec 07 §13.1–§13.3, §22).
//!
//! Backends:
//! - Linux, feature `pipewire`: PipeWire stream capturing the default sink's monitor; follows
//!   default-output changes through the session manager.
//! - Linux, feature `pulse`: PulseAudio protocol (also served by pipewire-pulse), monitor of the
//!   default sink in the reference format; follows default-sink changes by subscription.
//! - Windows, feature `wasapi`: WASAPI loopback on the default render device (cpal), rebuilt
//!   when the default device changes.
//!
//! Capture is opened only on [`AudioCapture::start`] and fully closed by
//! [`AudioCapture::stop`] (call it when the wallpaper pauses). Starting never blocks: the
//! connection happens on the backend thread, and a missing sound server leaves the bands at
//! zero while retrying every 5 s (spec §12.2 robustness, applied to capture per §13.1).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::convert::{CaptureSample, ReferenceConverter};
use crate::spectrum::{Fragmenter, SpectrumFeed, WindowAccumulator};

#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub mod pipewire;
#[cfg(all(target_os = "linux", feature = "pulse"))]
pub mod pulse;
#[cfg(all(windows, feature = "wasapi"))]
pub mod wasapi;

/// Lifecycle state of a capture backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureState {
    /// Not started, or stopped.
    Stopped,
    /// Connecting to the sound server / opening the device.
    Connecting,
    /// Stream open; data flows when something plays.
    Running,
    /// No sound server or device; retrying in the background.
    Unavailable,
}

impl CaptureState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Connecting,
            2 => Self::Running,
            3 => Self::Unavailable,
            _ => Self::Stopped,
        }
    }
}

/// Capture state shared between a backend thread and its owner.
#[derive(Clone, Debug)]
pub struct SharedState(Arc<AtomicU8>);

impl SharedState {
    /// New state, `Stopped`.
    pub fn new() -> Self {
        Self(Arc::new(AtomicU8::new(0)))
    }

    /// Current state.
    pub fn get(&self) -> CaptureState {
        CaptureState::from_u8(self.0.load(Ordering::Acquire))
    }

    /// Updates the state.
    pub fn set(&self, s: CaptureState) {
        self.0.store(s as u8, Ordering::Release);
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

/// A source of captured audio feeding a [`SpectrumFeed`].
pub trait AudioCapture: Send {
    /// Opens the capture (non-blocking). No effect if already started.
    fn start(&mut self);
    /// Closes the stream and releases the device; no thread keeps running.
    fn stop(&mut self);
    /// Current state.
    fn state(&self) -> CaptureState;
    /// Backend name for logs.
    fn backend_name(&self) -> &'static str;
}

/// Backend used when capture is disabled or unavailable on this build: never produces data.
#[derive(Default)]
pub struct NullCapture {
    started: bool,
}

impl AudioCapture for NullCapture {
    fn start(&mut self) {
        self.started = true;
    }
    fn stop(&mut self) {
        self.started = false;
    }
    fn state(&self) -> CaptureState {
        if self.started {
            CaptureState::Unavailable
        } else {
            CaptureState::Stopped
        }
    }
    fn backend_name(&self) -> &'static str {
        "none"
    }
}

/// Turns captured PCM into analysis windows: conversion (if needed) → reference fragments →
/// window accumulation (§13.3) → [`SpectrumFeed`]. Owned by the backend's capture thread.
pub struct CaptureSink {
    feed: SpectrumFeed,
    window: WindowAccumulator,
    fragmenter: Fragmenter,
    converter: Option<ReferenceConverter>,
}

impl CaptureSink {
    /// Sink publishing to `feed`.
    pub fn new(feed: SpectrumFeed) -> Self {
        Self {
            feed,
            window: WindowAccumulator::new(),
            fragmenter: Fragmenter::new(),
            converter: None,
        }
    }

    /// One server packet already in the reference format and fragment size (PulseAudio record
    /// stream with the reference buffer attributes).
    pub fn push_reference_packet(&mut self, packet: &[u8]) {
        if let Some(w) = self.window.push(packet) {
            self.feed.publish(w);
        }
    }

    /// Reference-format bytes with arbitrary chunking; regrouped into reference fragments.
    pub fn push_reference_stream(&mut self, bytes: &[u8]) {
        let Self {
            feed,
            window,
            fragmenter,
            ..
        } = self;
        fragmenter.push(bytes, |frag| {
            if let Some(w) = window.push(frag) {
                feed.publish(w);
            }
        });
    }

    /// Native interleaved PCM: converted to the reference format, then fragmented.
    pub fn push_native<S: CaptureSample>(&mut self, interleaved: &[S], rate: u32, channels: usize) {
        let channels = channels.max(1);
        if self
            .converter
            .as_ref()
            .is_none_or(|c| c.in_rate() != rate || c.channels() != channels)
        {
            self.converter = Some(ReferenceConverter::new(rate, channels));
        }
        let Self {
            feed,
            window,
            fragmenter,
            converter,
        } = self;
        if let Some(conv) = converter {
            conv.process(interleaved, &mut |bytes: &[u8]| {
                fragmenter.push(bytes, |frag| {
                    if let Some(w) = window.push(frag) {
                        feed.publish(w);
                    }
                });
            });
        }
    }

    /// Drops partial data (after a stream restart).
    pub fn reset(&mut self) {
        self.window = WindowAccumulator::new();
        self.fragmenter.clear();
        if let Some(c) = &mut self.converter {
            c.reset();
        }
    }
}

/// Picks the best capture backend compiled in and available on this machine.
pub fn default_capture(feed: SpectrumFeed) -> Box<dyn AudioCapture> {
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        if pipewire::server_available() {
            return Box::new(pipewire::PipeWireCapture::new(feed));
        }
    }
    #[cfg(all(target_os = "linux", feature = "pulse"))]
    {
        return Box::new(pulse::PulseCapture::new(feed, None));
    }
    #[cfg(all(windows, feature = "wasapi"))]
    {
        return Box::new(wasapi::WasapiLoopbackCapture::new(feed));
    }
    #[allow(unreachable_code)]
    {
        let _ = feed;
        tracing::info!("audio capture: no backend compiled in, spectrum stays at zero");
        Box::new(NullCapture::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum::WINDOW_SIZE;

    /// Native float data is converted and regrouped into ~100 ms reference fragments: 4800
    /// frames at 48 kHz make one fragment, hence one window.
    #[test]
    fn native_input_yields_one_window_per_reference_fragment() {
        let feed = SpectrumFeed::new();
        let mut sink = CaptureSink::new(feed.clone());
        let mut out = [0u8; WINDOW_SIZE];
        let tone: Vec<f32> = (0..4800 * 2)
            .map(|i| ((i / 2) as f32 * 0.2).sin() * 0.5)
            .collect();
        for chunk in tone.chunks(480 * 2) {
            sink.push_native(chunk, 48_000, 2);
        }
        // The resampler holds back a few frames, so the fragment completes with the next block.
        sink.push_native(&tone[..96], 48_000, 2);
        assert!(feed.take(&mut out), "one window after ~100 ms");
        assert!(out.iter().any(|&b| b != 128));
        sink.push_native(&tone[..960], 48_000, 2);
        assert!(!feed.take(&mut out), "no window before the next fragment");
    }

    #[test]
    fn null_capture_reports_unavailable() {
        let mut c = NullCapture::default();
        assert_eq!(c.state(), CaptureState::Stopped);
        c.start();
        assert_eq!(c.state(), CaptureState::Unavailable);
        c.stop();
        assert_eq!(c.state(), CaptureState::Stopped);
    }
}

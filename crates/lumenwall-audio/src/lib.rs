//! Scene sound playback and audio capture for audio-reactive wallpapers.
//!
//! Built from `specs/07-plateforme-audio-video-web.md` §11–§13 and §22 (see README).
//!
//! - [`spectrum`]: spectrum analysis feeding `g_AudioSpectrum{16,32,64}{Left,Right}`.
//! - [`convert`]: conversion of native capture formats to the reference 8-bit mono 44.1 kHz.
//! - [`capture`]: [`AudioCapture`] trait and the PipeWire / PulseAudio / WASAPI loopback backends.
//! - [`automute`]: "another application is playing" detection, event-driven.
//! - [`playback`] (feature `playback`): scene sound objects, mixer and output stream.
//!
//! Nothing in this crate blocks the render thread: backends connect asynchronously on their
//! own threads, sleep while idle and are closed when the wallpaper is paused.

pub mod automute;
pub mod capture;
pub mod convert;
pub mod resample;
pub mod spectrum;

#[cfg(feature = "playback")]
pub mod playback;
pub mod sound;

#[cfg(all(target_os = "linux", feature = "pulse"))]
mod pulse_util;

pub use automute::{AutomuteDetector, AutomuteSettings, PlaybackWatcher};
pub use capture::{AudioCapture, CaptureSink, CaptureState};
pub use spectrum::{Bands, SpectrumAnalyzer, SpectrumFeed};

/// Default global volume (`--volume`, spec §2.4).
pub const DEFAULT_VOLUME: i32 = 15;
/// Maximum global volume; gain is `volume / 128` (spec §11.1).
pub const MAX_VOLUME: i32 = 128;

/// Clamps a `--volume` value to `[0, 128]` (spec §2.2 post-parse validation).
pub fn clamp_volume(volume: i32) -> u8 {
    volume.clamp(0, MAX_VOLUME) as u8
}

/// Name a capture/playback client registers with the sound server.
#[cfg(all(target_os = "linux", any(feature = "pulse", feature = "pipewire")))]
pub(crate) const CLIENT_NAME: &str = "lumenwall";

#[cfg(test)]
mod tests;

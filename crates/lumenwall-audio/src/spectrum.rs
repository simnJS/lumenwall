//! Audio-reactive spectrum analysis (spec 07 §13.3–§13.6).
//!
//! Input is the reference capture format: unsigned 8-bit mono PCM at 44 100 Hz, analysed in
//! windows of [`WINDOW_SIZE`] samples. Every number in this module is visible on screen through
//! the `g_AudioSpectrum*` uniforms, so the formula is reproduced exactly (`// parity:` marks).
//!
//! Threading: capture backends push windows into a [`SpectrumFeed`] from their own thread; the
//! render thread owns the [`SpectrumAnalyzer`] and calls [`SpectrumAnalyzer::update`] once per
//! frame. Nothing here allocates after construction.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Samples per analysis window (spec §13.3).
pub const WINDOW_SIZE: usize = 1024;
/// Sample rate of the reference capture format (spec §13.2).
pub const REFERENCE_RATE: u32 = 44_100;
/// Size of one capture fragment in the reference format: ~100 ms of 8-bit mono 44.1 kHz (§13.2).
pub const FRAGMENT_BYTES: usize = 4410;
/// Frame rate whose per-frame smoothing step the reference applies when none is configured
/// (default `--fps`, spec §2.4).
pub const DEFAULT_REFERENCE_FPS: f32 = 30.0;
/// Without new capture data for this long, the input is treated as silence (see crate README).
pub const STALE_INPUT_SECONDS: f32 = 0.5;

// parity: per-update slope limit of the displayed values (§13.5).
const SLEW_PER_UPDATE: f32 = 0.3;
// parity: level = 0.35·log10(power) (§13.4 step 3).
const LOG_SCALE: f32 = 0.35;

/// One set of spectrum bands. Left and right are identical (mono capture, §13.6), so a single
/// set is stored and exposed for both channels.
#[derive(Clone, Debug, PartialEq)]
pub struct Bands {
    /// `g_AudioSpectrum16Left` / `g_AudioSpectrum16Right`.
    pub b16: [f32; 16],
    /// `g_AudioSpectrum32Left` / `g_AudioSpectrum32Right`.
    pub b32: [f32; 32],
    /// `g_AudioSpectrum64Left` / `g_AudioSpectrum64Right`.
    pub b64: [f32; 64],
}

impl Bands {
    /// All bands at zero (neutral analyser, §13.1).
    pub const ZERO: Self = Self {
        b16: [0.0; 16],
        b32: [0.0; 32],
        b64: [0.0; 64],
    };

    fn iter_mut(&mut self) -> impl Iterator<Item = &mut f32> {
        self.b16
            .iter_mut()
            .chain(self.b32.iter_mut())
            .chain(self.b64.iter_mut())
    }

    fn iter(&self) -> impl Iterator<Item = &f32> {
        self.b16
            .iter()
            .chain(self.b32.iter())
            .chain(self.b64.iter())
    }
}

impl Default for Bands {
    fn default() -> Self {
        Self::ZERO
    }
}

/// Precomputed band weights `2 − e^(0.5 − b/D)` (§13.4).
struct Weights {
    w64: [f32; 64],
    w32: [f32; 32],
    w16: [f32; 16],
}

impl Weights {
    fn new() -> Self {
        let weight = |b: usize, d: f64| (2.0 - (0.5 - b as f64 / d).exp()) as f32;
        let mut w = Self {
            w64: [0.0; 64],
            w32: [0.0; 32],
            w16: [0.0; 16],
        };
        for (b, slot) in w.w64.iter_mut().enumerate() {
            *slot = weight(b, 63.0);
        }
        // parity: each 32-band slot is written for b = 2j and b = 2j+1 in increasing order; only
        // the last write (b = 2j+1) survives, so only that weight is kept.
        for (j, slot) in w.w32.iter_mut().enumerate() {
            *slot = weight(2 * j + 1, 31.0);
        }
        // parity: each 16-band slot is overwritten for b = 4j..4j+3; b = 4j+3 survives.
        for (j, slot) in w.w16.iter_mut().enumerate() {
            *slot = weight(4 * j + 3, 15.0);
        }
        w
    }
}

/// Converts the power of a bin into the reference level (§13.4 step 3).
#[inline]
fn level(power: f32) -> f32 {
    // parity: no floor; log of a tiny power is strongly negative, exact zero maps to 0.
    if power > 0.0 {
        LOG_SCALE * power.log10()
    } else {
        0.0
    }
}

/// Slope-limited approach of `values` toward `targets` (§13.5).
#[inline]
fn slew(values: &mut Bands, targets: &Bands, max_step: f32) {
    for (v, &t) in values.iter_mut().zip(targets.iter()) {
        let d = t - *v;
        // parity: snap when within one step, else move by exactly one step toward the target.
        if d.abs() <= max_step {
            *v = t;
        } else {
            *v += max_step.copysign(d);
        }
    }
}

/// Spectrum analyser: FFT + bands + smoothing. Owned by the render thread.
pub struct SpectrumAnalyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    window: [u8; WINDOW_SIZE],
    weights: Weights,
    targets: Bands,
    values: Bands,
    reference_fps: f32,
    feed: Option<SpectrumFeed>,
    since_fresh: f32,
    stale: bool,
}

impl SpectrumAnalyzer {
    /// Creates an analyser fed by a capture backend.
    ///
    /// `reference_fps` is the frame rate the reference would iterate at (its `--fps`, default
    /// [`DEFAULT_REFERENCE_FPS`]); smoothing advances `0.3` per reference frame of real time.
    pub fn new(reference_fps: f32) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(WINDOW_SIZE);
        let input = fft.make_input_vec();
        let spectrum = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        Self {
            fft,
            input,
            spectrum,
            scratch,
            window: [128; WINDOW_SIZE],
            weights: Weights::new(),
            targets: Bands::ZERO,
            values: Bands::ZERO,
            reference_fps: if reference_fps.is_finite() && reference_fps > 0.0 {
                reference_fps
            } else {
                DEFAULT_REFERENCE_FPS
            },
            feed: Some(SpectrumFeed::new()),
            since_fresh: 0.0,
            stale: true,
        }
    }

    /// Neutral analyser (§13.1): every band stays at zero and no capture is needed.
    pub fn disabled() -> Self {
        let mut a = Self::new(DEFAULT_REFERENCE_FPS);
        a.feed = None;
        a
    }

    /// Whether this analyser consumes capture data.
    pub fn is_enabled(&self) -> bool {
        self.feed.is_some()
    }

    /// Handle that capture backends publish windows to. `None` for a disabled analyser.
    pub fn feed(&self) -> Option<SpectrumFeed> {
        self.feed.clone()
    }

    /// Changes the reference frame rate used to scale smoothing.
    pub fn set_reference_fps(&mut self, fps: f32) {
        if fps.is_finite() && fps > 0.0 {
            self.reference_fps = fps;
        }
    }

    /// Per-frame update from real elapsed time (PARITY-POLICY rule 6).
    ///
    /// Smoothing advances by `0.3 × elapsed × reference_fps`, which equals the reference's
    /// `0.3` per iteration when Lumenwall runs at the reference frame rate. Then, if the capture
    /// published a new window since the last call, targets are recomputed (§13.4–§13.5 order:
    /// smoothing first, analysis second).
    pub fn update(&mut self, elapsed_seconds: f32) {
        let steps = if elapsed_seconds.is_finite() && elapsed_seconds > 0.0 {
            elapsed_seconds * self.reference_fps
        } else {
            0.0
        };
        self.smooth(SLEW_PER_UPDATE * steps);
        self.poll_feed(elapsed_seconds.max(0.0));
    }

    /// Exactly one reference iteration: smoothing step of `0.3`, then analysis of any new window.
    pub fn update_reference_step(&mut self) {
        self.smooth(SLEW_PER_UPDATE);
        self.poll_feed(1.0 / self.reference_fps);
    }

    fn smooth(&mut self, max_step: f32) {
        if max_step > 0.0 {
            slew(&mut self.values, &self.targets, max_step);
        }
    }

    fn poll_feed(&mut self, elapsed: f32) {
        let Some(feed) = &self.feed else { return };
        if feed.take(&mut self.window) {
            self.since_fresh = 0.0;
            self.stale = false;
            let window = self.window;
            self.analyze(&window);
        } else if !self.stale {
            self.since_fresh += elapsed;
            if self.since_fresh >= STALE_INPUT_SECONDS {
                // Backends that stop delivering when nothing plays (WASAPI loopback, suspended
                // PipeWire sinks) would otherwise freeze the bars; a Pulse monitor delivers
                // silence, which yields exactly zero targets.
                self.stale = true;
                self.targets = Bands::ZERO;
            }
        }
    }

    /// Computes new targets from one reference-format window (§13.4). Public for tests and
    /// benchmarks; normal use goes through [`update`](Self::update).
    pub fn analyze(&mut self, window: &[u8; WINDOW_SIZE]) {
        for (x, &byte) in self.input.iter_mut().zip(window.iter()) {
            // parity: x = (byte − 128)/128, rectangular window (no tapering).
            *x = (f32::from(byte) - 128.0) / 128.0;
        }
        self.analyze_input();
    }

    /// Same as [`analyze`](Self::analyze) for already-normalised samples in `[-1, 1)`.
    pub fn analyze_normalized(&mut self, samples: &[f32; WINDOW_SIZE]) {
        self.input.copy_from_slice(samples);
        self.analyze_input();
    }

    fn analyze_input(&mut self) {
        // parity: real, unnormalised forward FFT of size 1024 (§13.4 step 2).
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            // Buffers are sized by the planner; this cannot fail. Keep the old targets.
            return;
        }
        let mut levels = [0.0f32; 64];
        for (b, l) in levels.iter_mut().enumerate() {
            // parity: only even bins k = 2b (0..126) are used.
            let c = self.spectrum[2 * b];
            *l = level(c.re * c.re + c.im * c.im);
        }
        let w = &self.weights;
        for ((t, &l), &wb) in self.targets.b64.iter_mut().zip(&levels).zip(&w.w64) {
            // parity: upper bound 1, no lower bound (§13.4 step 4).
            *t = (l * wb).min(1.0);
        }
        for j in 0..32 {
            // parity: 32-band slot j keeps the value computed at b = 2j+1.
            self.targets.b32[j] = (levels[2 * j + 1] * w.w32[j]).min(1.0);
        }
        for j in 0..16 {
            // parity: 16-band slot j keeps the value computed at b = 4j+3.
            self.targets.b16[j] = (levels[4 * j + 3] * w.w16[j]).min(1.0);
        }
    }

    /// Smoothed values, as sent to shaders.
    pub fn values(&self) -> &Bands {
        &self.values
    }

    /// Latest unsmoothed targets.
    pub fn targets(&self) -> &Bands {
        &self.targets
    }

    // parity: left = right, both channels expose the same mono values (§13.6).
    /// `g_AudioSpectrum16Left`.
    pub fn left16(&self) -> &[f32; 16] {
        &self.values.b16
    }
    /// `g_AudioSpectrum16Right` (same values as left).
    pub fn right16(&self) -> &[f32; 16] {
        &self.values.b16
    }
    /// `g_AudioSpectrum32Left`.
    pub fn left32(&self) -> &[f32; 32] {
        &self.values.b32
    }
    /// `g_AudioSpectrum32Right` (same values as left).
    pub fn right32(&self) -> &[f32; 32] {
        &self.values.b32
    }
    /// `g_AudioSpectrum64Left`.
    pub fn left64(&self) -> &[f32; 64] {
        &self.values.b64
    }
    /// `g_AudioSpectrum64Right` (same values as left).
    pub fn right64(&self) -> &[f32; 64] {
        &self.values.b64
    }

    /// Resets values and targets to zero.
    pub fn reset(&mut self) {
        self.values = Bands::ZERO;
        self.targets = Bands::ZERO;
        self.stale = true;
        self.since_fresh = 0.0;
    }
}

/// Single-slot mailbox carrying the latest complete window from a capture thread to the
/// analyser. Cloning shares the slot.
#[derive(Clone)]
pub struct SpectrumFeed {
    inner: Arc<FeedInner>,
}

struct FeedInner {
    slot: Mutex<[u8; WINDOW_SIZE]>,
    fresh: AtomicBool,
}

impl SpectrumFeed {
    /// New empty feed.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(FeedInner {
                slot: Mutex::new([128; WINDOW_SIZE]),
                fresh: AtomicBool::new(false),
            }),
        }
    }

    /// Publishes a ready window and raises the "new frame" flag (§13.3).
    pub fn publish(&self, window: &[u8; WINDOW_SIZE]) {
        if let Ok(mut slot) = self.inner.slot.lock() {
            *slot = *window;
        }
        self.inner.fresh.store(true, Ordering::Release);
    }

    /// Copies the latest window into `out` if one arrived since the last call.
    pub fn take(&self, out: &mut [u8; WINDOW_SIZE]) -> bool {
        if !self.inner.fresh.swap(false, Ordering::Acquire) {
            return false;
        }
        match self.inner.slot.lock() {
            Ok(slot) => {
                *out = *slot;
                true
            }
            Err(_) => false,
        }
    }
}

impl Default for SpectrumFeed {
    fn default() -> Self {
        Self::new()
    }
}

/// Window accumulation of §13.3: packets fill the current window; a completing packet either
/// yields its last 1024 bytes (if a full extra window remains) or the completed window, and
/// leftover bytes are discarded.
pub struct WindowAccumulator {
    window: [u8; WINDOW_SIZE],
    fill: usize,
    ready: [u8; WINDOW_SIZE],
}

impl WindowAccumulator {
    /// Empty accumulator.
    pub fn new() -> Self {
        Self {
            window: [128; WINDOW_SIZE],
            fill: 0,
            ready: [128; WINDOW_SIZE],
        }
    }

    /// Feeds one capture packet; returns the ready window when the packet completed one.
    pub fn push(&mut self, packet: &[u8]) -> Option<&[u8; WINDOW_SIZE]> {
        let need = WINDOW_SIZE - self.fill;
        if packet.len() < need {
            self.window[self.fill..self.fill + packet.len()].copy_from_slice(packet);
            self.fill += packet.len();
            return None;
        }
        if packet.len() - need >= WINDOW_SIZE {
            // parity: a full extra window remains → the ready window is the packet's last 1024 bytes.
            self.ready
                .copy_from_slice(&packet[packet.len() - WINDOW_SIZE..]);
        } else {
            // parity: the completed window is ready; the packet's excess bytes are dropped.
            self.window[self.fill..].copy_from_slice(&packet[..need]);
            self.ready = self.window;
        }
        self.fill = 0;
        Some(&self.ready)
    }

    /// Number of bytes accumulated in the current window.
    pub fn fill(&self) -> usize {
        self.fill
    }
}

impl Default for WindowAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Regroups a continuous reference-format sample stream into [`FRAGMENT_BYTES`] packets, which
/// is how the reference's sound server delivers capture data (§13.2). Backends whose packet
/// size is set by the audio graph (PipeWire, WASAPI) go through it so that windows, and hence
/// target updates, occur at the reference cadence (~10 per second).
pub struct Fragmenter {
    buf: Box<[u8; FRAGMENT_BYTES]>,
    len: usize,
}

impl Fragmenter {
    /// Empty fragmenter.
    pub fn new() -> Self {
        Self {
            buf: Box::new([128; FRAGMENT_BYTES]),
            len: 0,
        }
    }

    /// Appends samples; calls `emit` for each completed fragment.
    pub fn push(&mut self, mut data: &[u8], mut emit: impl FnMut(&[u8])) {
        while !data.is_empty() {
            let n = (FRAGMENT_BYTES - self.len).min(data.len());
            self.buf[self.len..self.len + n].copy_from_slice(&data[..n]);
            self.len += n;
            data = &data[n..];
            if self.len == FRAGMENT_BYTES {
                // parity: the reference requests 4410-byte fragments (~100 ms).
                emit(&self.buf[..]);
                self.len = 0;
            }
        }
    }

    /// Drops any partial fragment.
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl Default for Fragmenter {
    fn default() -> Self {
        Self::new()
    }
}

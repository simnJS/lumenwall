//! What the running engine tells the application (the IPC daemon): outputs, wallpapers and
//! their state, user property values, frame counters.
//!
//! The engine thread rewrites the [`EngineReport`] only when something changes (output
//! added, paused, wallpaper loaded, property set), never per frame, and calls the listener
//! when the report actually differs. Other threads read it without waking the engine.
//!
//! Frame counters are recorded only while [`EngineMonitor::set_sampling`] is on (a client
//! looks at the statistics); otherwise a frame costs one relaxed atomic load.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lumenwall_render::ScalingMode;

/// Called on the engine thread after the report changed. Keep it short and non-blocking.
pub type Listener = Arc<dyn Fn(Change) + Send + Sync>;

/// Which part of the report changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Outputs appeared, disappeared, moved or were resized.
    Outputs,
    /// Pause, wallpaper, loading state.
    State,
    /// User property values.
    Properties,
}

/// Snapshot of the engine.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineReport {
    /// Surfaces and GPU device are open (false while idle without any wallpaper).
    pub active: bool,
    /// Paused by the application ([`crate::EngineControl::SetPaused`]).
    pub user_paused: bool,
    /// Outputs seen since start, sorted by name. Outputs left out of the current layout keep
    /// their last known geometry (`shown` false) so clients can put a wallpaper back on them.
    pub outputs: Vec<OutputReport>,
    /// One per target of the plan, in plan order.
    pub targets: Vec<TargetReport>,
}

impl EngineReport {
    /// Target shown on `output`.
    pub fn target_of(&self, output: &str) -> Option<&TargetReport> {
        self.outputs
            .iter()
            .find(|o| o.name == output)
            .and_then(|o| o.target)
            .and_then(|i| self.targets.get(i))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutputReport {
    /// Connector name (`DP-1`), `default` for a window.
    pub name: String,
    /// Make and model when known.
    pub description: Option<String>,
    /// Top-left corner in the desktop space (logical units on Wayland, pixels elsewhere).
    pub position: (i32, i32),
    /// Physical pixels.
    pub size: (u32, u32),
    pub scale: f64,
    pub refresh_mhz: Option<u32>,
    /// The output has a surface in the current layout.
    pub shown: bool,
    /// Index into [`EngineReport::targets`] of the wallpaper it shows.
    pub target: Option<usize>,
    /// Why the platform paused it (fullscreen window, lock, hidden).
    pub paused: Option<PauseCause>,
}

/// Platform pause reason, most significant first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseCause {
    Locked,
    Fullscreen,
    /// Covered, minimised, display off.
    Hidden,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TargetReport {
    /// `default`, an output name or `span:A,B`.
    pub key: String,
    pub outputs: Vec<String>,
    /// Wallpaper folder, as given in the plan.
    pub wallpaper: PathBuf,
    pub title: String,
    pub scaling: ScalingMode,
    pub state: TargetState,
    /// Current user property values (decorative `text` properties excluded).
    pub properties: Vec<(String, PropertyValue)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TargetState {
    /// Read, waiting for its output (renderer not built yet).
    Loading,
    Running,
    /// The renderer could not be built.
    Error(String),
}

/// A user property value in its natural form.
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    Bool(bool),
    Number(f64),
    /// RGB, 0..1.
    Color([f32; 3]),
    Text(String),
}

/// Frame counters since the previous [`EngineMonitor::take_counters`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameCounters {
    /// Time covered by these counters.
    pub elapsed: Duration,
    pub outputs: Vec<OutputCounters>,
    /// GPU time of the measured frames (timestamp queries), all outputs together.
    pub gpu: Duration,
    pub gpu_samples: u32,
    /// GPU memory allocated by Lumenwall (textures, targets, buffers, video frames).
    pub gpu_memory: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputCounters {
    pub name: String,
    pub frames: u32,
    /// CPU time of the render thread for these frames.
    pub cpu: Duration,
}

/// Shared handle between the engine thread and the application.
#[derive(Clone, Default)]
pub struct EngineMonitor {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    report: Mutex<EngineReport>,
    listener: Mutex<Option<Listener>>,
    sampling: AtomicBool,
    counters: Mutex<Counters>,
    /// The engine process whose frames are counted, when the engine runs in one.
    remote: Mutex<Option<Arc<dyn CounterSource>>>,
}

/// Frame counters kept by an engine process (the daemon's monitor forwards to it).
pub(crate) trait CounterSource: Send + Sync {
    fn set_sampling(&self, on: bool);
    /// `None` when the process did not answer in time.
    fn take_counters(&self) -> Option<FrameCounters>;
}

#[derive(Default)]
struct Counters {
    since: Option<Instant>,
    data: FrameCounters,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl EngineMonitor {
    /// Current report (a copy).
    pub fn report(&self) -> EngineReport {
        lock(&self.inner.report).clone()
    }

    /// Reads the report without copying it.
    pub fn with_report<R>(&self, f: impl FnOnce(&EngineReport) -> R) -> R {
        f(&lock(&self.inner.report))
    }

    /// Installs (or removes) the change listener.
    pub fn set_listener(&self, listener: Option<Listener>) {
        *lock(&self.inner.listener) = listener;
    }

    /// Turns the frame counters on or off. Never wakes the engine (an engine process gets a
    /// message, read by its own thread).
    pub fn set_sampling(&self, on: bool) {
        let was = self.inner.sampling.swap(on, Ordering::SeqCst);
        if on && !was {
            let mut c = lock(&self.inner.counters);
            c.since = Some(Instant::now());
            let gpu_memory = c.data.gpu_memory;
            c.data = FrameCounters {
                gpu_memory,
                ..FrameCounters::default()
            };
        }
        if on != was {
            let remote = lock(&self.inner.remote).clone();
            if let Some(r) = remote {
                r.set_sampling(on);
            }
        }
    }

    pub fn sampling(&self) -> bool {
        self.inner.sampling.load(Ordering::Relaxed)
    }

    /// Where the counters are kept while the engine runs in another process (`None`: here).
    pub(crate) fn set_counter_source(&self, source: Option<Arc<dyn CounterSource>>) {
        *lock(&self.inner.remote) = source;
    }

    /// Counters since the previous call (or since sampling started), then resets them.
    pub fn take_counters(&self) -> FrameCounters {
        let remote = lock(&self.inner.remote).clone();
        if let Some(c) = remote.and_then(|r| r.take_counters()) {
            return c;
        }
        let mut c = lock(&self.inner.counters);
        let now = Instant::now();
        let since = c.since.replace(now).unwrap_or(now);
        let gpu_memory = c.data.gpu_memory;
        let mut data = std::mem::take(&mut c.data);
        // Keep the output entries (no allocation on the render thread next time) and the
        // last memory figure (only refreshed when a frame is drawn).
        c.data.outputs = data
            .outputs
            .iter()
            .map(|o| OutputCounters {
                name: o.name.clone(),
                ..OutputCounters::default()
            })
            .collect();
        c.data.gpu_memory = gpu_memory;
        data.elapsed = now - since;
        data
    }

    /// Replaces the report; calls the listener when it changed.
    pub(crate) fn publish(&self, report: EngineReport, change: Change) {
        {
            let mut current = lock(&self.inner.report);
            if *current == report {
                return;
            }
            *current = report;
        }
        let listener = lock(&self.inner.listener).clone();
        if let Some(listener) = listener {
            listener(change);
        }
    }

    /// One presented frame (engine thread, only while sampling).
    pub(crate) fn record_frame(&self, output: &str, cpu: Duration, gpu: Option<Duration>) {
        let mut c = lock(&self.inner.counters);
        let data = &mut c.data;
        match data.outputs.iter_mut().find(|o| o.name == output) {
            Some(o) => {
                o.frames += 1;
                o.cpu += cpu;
            }
            None => data.outputs.push(OutputCounters {
                name: output.to_owned(),
                frames: 1,
                cpu,
            }),
        }
        if let Some(g) = gpu {
            data.gpu += g;
            data.gpu_samples += 1;
        }
    }

    pub(crate) fn set_gpu_memory(&self, bytes: u64) {
        lock(&self.inner.counters).data.gpu_memory = bytes;
    }
}

impl std::fmt::Debug for EngineMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EngineMonitor")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn listener_runs_only_on_real_changes() {
        let m = EngineMonitor::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        m.set_listener(Some(Arc::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        })));
        let mut r = EngineReport {
            active: true,
            ..EngineReport::default()
        };
        m.publish(r.clone(), Change::State);
        m.publish(r.clone(), Change::State);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        r.user_paused = true;
        m.publish(r, Change::State);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(m.report().user_paused);
    }

    #[test]
    fn counters_reset_on_take() {
        let m = EngineMonitor::default();
        m.set_sampling(true);
        m.record_frame(
            "DP-1",
            Duration::from_millis(2),
            Some(Duration::from_millis(1)),
        );
        m.record_frame("DP-1", Duration::from_millis(4), None);
        m.set_gpu_memory(42);
        let c = m.take_counters();
        assert_eq!(c.outputs.len(), 1);
        assert_eq!(c.outputs[0].frames, 2);
        assert_eq!(c.outputs[0].cpu, Duration::from_millis(6));
        assert_eq!((c.gpu_samples, c.gpu_memory), (1, 42));
        let c = m.take_counters();
        assert_eq!(c.outputs[0].frames, 0);
        assert_eq!(c.gpu_memory, 42);
    }
}

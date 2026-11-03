//! Bounded hand-over of decoded frames from the decoding thread to the render thread.
//!
//! The decoder blocks (no timeout, no polling) while the queue is full, so a paused or
//! covered wallpaper costs nothing once the few queued frames are decoded. The render
//! thread never blocks in live playback; it is woken through `notify` when a frame lands in
//! an empty queue (otherwise it already knows the next deadline from the queue head).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Callback run (outside the lock) when a frame arrives in an empty queue.
pub type Notify = std::sync::Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Queue<T> {
    state: Mutex<State<T>>,
    /// Signalled when a frame is popped or the queue stops (decoder side waits).
    space: Condvar,
    /// Signalled when a frame is pushed (offline render waits).
    filled: Condvar,
    capacity: usize,
    notify: Option<Notify>,
    /// Display cap used by the decoder's decimation (0 = keep every frame).
    pub max_fps: AtomicU32,
    pub stopped: AtomicBool,
    /// Counters (debug statistics).
    pub decoded: AtomicU64,
    pub dropped_early: AtomicU64,
    pub loops: AtomicU64,
}

struct State<T> {
    frames: VecDeque<(f64, T)>,
    stopped: bool,
}

impl<T> Queue<T> {
    pub fn new(capacity: usize, max_fps: u32, notify: Option<Notify>) -> Self {
        Self {
            state: Mutex::new(State {
                frames: VecDeque::with_capacity(capacity),
                stopped: false,
            }),
            space: Condvar::new(),
            filled: Condvar::new(),
            capacity: capacity.max(1),
            notify,
            max_fps: AtomicU32::new(max_fps),
            stopped: AtomicBool::new(false),
            decoded: AtomicU64::new(0),
            dropped_early: AtomicU64::new(0),
            loops: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Decoder side: waits for room, then appends. `false` once stopped (the frame is
    /// dropped).
    pub fn push(&self, t: f64, frame: T) -> bool {
        let mut s = self.lock();
        while s.frames.len() >= self.capacity && !s.stopped {
            s = self.space.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
        if s.stopped {
            return false;
        }
        let was_empty = s.frames.is_empty();
        s.frames.push_back((t, frame));
        drop(s);
        self.filled.notify_all();
        if was_empty && let Some(n) = &self.notify {
            n();
        }
        true
    }

    /// Render side: removes every frame due at media time `t` and returns the newest one;
    /// older due frames are passed to `discard`.
    pub fn pop_due(&self, t: f64, mut discard: impl FnMut(T)) -> Option<(f64, T)> {
        let mut s = self.lock();
        let mut newest = None;
        while s.frames.front().is_some_and(|(ft, _)| *ft <= t + 1e-4) {
            if let Some(old) = newest.replace(s.frames.pop_front()?) {
                discard(old.1);
            }
        }
        drop(s);
        if newest.is_some() {
            self.space.notify_one();
        }
        newest
    }

    /// The oldest frame regardless of its time.
    #[cfg(test)]
    pub fn pop_front(&self) -> Option<(f64, T)> {
        let f = self.lock().frames.pop_front();
        if f.is_some() {
            self.space.notify_one();
        }
        f
    }

    /// Media time of the next queued frame.
    pub fn next_time(&self) -> Option<f64> {
        self.lock().frames.front().map(|(t, _)| *t)
    }

    /// Offline render: waits until a frame later than `t` is queued (so the newest frame due
    /// at `t` is known), or `deadline`.
    pub fn wait_beyond(&self, t: f64, deadline: Instant) {
        let mut s = self.lock();
        while !s.stopped && !s.frames.back().is_some_and(|(ft, _)| *ft > t + 1e-4) {
            // Full queue of due frames: the caller must pop first.
            if s.frames.len() >= self.capacity {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            let (g, _) = self
                .filled
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(PoisonError::into_inner);
            s = g;
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.lock().stopped = true;
        self.space.notify_all();
        self.filled.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Frames queued.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().frames.len()
    }

    /// Removes every queued frame.
    pub fn clear(&self) -> Vec<T> {
        let v = self.lock().frames.drain(..).map(|(_, f)| f).collect();
        self.space.notify_all();
        v
    }
}

/// Upper bound of [`Queue::wait_beyond`] in offline renders.
pub(crate) const OFFLINE_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn pop_due_returns_newest_and_discards_older() {
        let q = Queue::new(4, 0, None);
        for i in 0..4 {
            assert!(q.push(f64::from(i) * 0.1, i));
        }
        let mut dropped = Vec::new();
        let (t, f) = q.pop_due(0.25, |f| dropped.push(f)).unwrap();
        assert_eq!((f, dropped), (2, vec![0, 1]));
        assert!((t - 0.2).abs() < 1e-9);
        assert_eq!(q.next_time(), Some(0.30000000000000004));
        assert!(q.pop_due(0.25, |_| {}).is_none());
    }

    #[test]
    fn notify_only_when_the_queue_was_empty() {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let q = Queue::new(
            4,
            0,
            Some(Arc::new(move || {
                n2.fetch_add(1, Ordering::SeqCst);
            })),
        );
        q.push(0.0, ());
        q.push(0.1, ());
        assert_eq!(n.load(Ordering::SeqCst), 1);
        q.pop_due(1.0, |_| {});
        q.push(0.2, ());
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn full_queue_blocks_until_pop_or_stop() {
        let q = Arc::new(Queue::new(1, 0, None));
        q.push(0.0, 0);
        let q2 = q.clone();
        let t = std::thread::spawn(move || q2.push(0.1, 1));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop_front().map(|f| f.1), Some(0));
        assert!(t.join().unwrap());
        let q3 = q.clone();
        let t = std::thread::spawn(move || q3.push(0.2, 2));
        std::thread::sleep(Duration::from_millis(20));
        q.stop();
        assert!(!t.join().unwrap());
    }
}

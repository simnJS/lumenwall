//! Preview images for the UI.
//!
//! Decoding happens on [`ThumbLoader`] threads; results arrive through an iced
//! subscription. Only previews the user can see are requested. Animated previews
//! keep their frames LZ4-compressed; [`Previews::advance`] (called on the animation
//! tick, which only exists while something animates) swaps in a new image handle
//! when a GIF frame's own delay has elapsed, so nothing is redrawn in between.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::futures::channel::mpsc::unbounded;
use iced::futures::{SinkExt, StreamExt};
use iced::widget::image::Handle;

use crate::thumbs::{self, Thumb, ThumbLoader, ThumbResult, Timeline};

/// Fastest preview animation (frames shorter than this are merged).
const MIN_TICK: Duration = Duration::from_millis(33);
/// Compressed animation frames kept in memory; the least recently seen are dropped
/// and reloaded from the disk cache when they come back on screen.
const ANIM_BUDGET: usize = 96 << 20;

/// The tick period for the shortest frame delay among playing previews, from a
/// small fixed set: the subscription keeps the same identity from one tick to the
/// next (re-creating `time::every` on every tick leaks memory in iced 0.14).
fn quantize_tick(shortest_ms: u32) -> Duration {
    const STEPS: [u32; 6] = [33, 40, 50, 60, 100, 150];
    let ms = STEPS
        .iter()
        .rev()
        .find(|&&s| s <= shortest_ms)
        .copied()
        .unwrap_or(STEPS[0]);
    Duration::from_millis(u64::from(ms)).max(MIN_TICK)
}

#[derive(Clone)]
pub enum ThumbEvent {
    Ready(Arc<ThumbLoader>),
    Done(Arc<ThumbResult>),
}

impl std::fmt::Debug for ThumbEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ThumbEvent")
    }
}

/// Runs the decoder pool for as long as the app runs.
pub fn subscription(cache_dir: Option<PathBuf>) -> iced::Subscription<ThumbEvent> {
    iced::Subscription::run_with(cache_dir, |cache_dir| {
        let cache_dir = cache_dir.clone();
        iced::stream::channel(64, async move |mut output| {
            let (tx, mut rx) = unbounded();
            let loader = ThumbLoader::new(cache_dir, 2, move |r| tx.unbounded_send(r).is_ok());
            let _ = output.send(ThumbEvent::Ready(Arc::new(loader))).await;
            while let Some(r) = rx.next().await {
                if output.send(ThumbEvent::Done(Arc::new(r))).await.is_err() {
                    break;
                }
            }
        })
    })
}

#[derive(Default)]
pub struct Previews {
    loader: Option<Arc<ThumbLoader>>,
    entries: HashMap<PathBuf, Entry>,
    scratch: Vec<u8>,
    /// Requested before the loader existed.
    waiting: HashSet<PathBuf>,
}

enum Entry {
    Loading,
    Failed,
    Ready(Ready),
}

struct Ready {
    handle: Handle,
    size: (u32, u32),
    anim: Option<Anim>,
    animated: bool,
    last_seen: Instant,
    reloading: bool,
}

struct Anim {
    thumb: Arc<Thumb>,
    timeline: Timeline,
    start: Instant,
    shown: usize,
}

impl Previews {
    pub fn handle_event(&mut self, event: ThumbEvent) {
        match event {
            ThumbEvent::Ready(loader) => {
                for p in self.waiting.drain() {
                    loader.request(&p);
                }
                self.loader = Some(loader);
            }
            ThumbEvent::Done(result) => match &*result {
                ThumbResult::Ready(path, thumb) => {
                    let now = Instant::now();
                    let animated = thumb.is_animated();
                    let anim = animated.then(|| Anim {
                        timeline: Timeline::new(&thumb.frames),
                        thumb: Arc::clone(thumb),
                        start: now,
                        shown: 0,
                    });
                    match self.entries.get_mut(path) {
                        // Frames reloaded after eviction: keep the image shown.
                        Some(Entry::Ready(r)) => {
                            r.anim = anim;
                            r.reloading = false;
                        }
                        _ => {
                            let handle =
                                Handle::from_rgba(thumb.width, thumb.height, thumb.first.clone());
                            self.entries.insert(
                                path.clone(),
                                Entry::Ready(Ready {
                                    handle,
                                    size: (thumb.width, thumb.height),
                                    anim,
                                    animated,
                                    last_seen: now,
                                    reloading: false,
                                }),
                            );
                        }
                    }
                }
                ThumbResult::Failed(path, e) => {
                    tracing::debug!("preview {}: {e}", path.display());
                    self.entries.insert(path.clone(), Entry::Failed);
                }
            },
        }
    }

    /// Like [`Self::touch`] for remote previews (`(local file, URL)`): downloaded
    /// first, only when on screen.
    pub fn touch_remote<'a>(&mut self, items: impl IntoIterator<Item = (&'a Path, &'a str)>) {
        let now = Instant::now();
        for (path, url) in items {
            match self.entries.get_mut(path) {
                Some(Entry::Ready(r)) => r.last_seen = now,
                Some(_) => {}
                None => {
                    if let Some(l) = &self.loader {
                        self.entries.insert(path.to_owned(), Entry::Loading);
                        l.request_remote(path, url);
                    }
                }
            }
        }
    }

    /// Forgets queued downloads (leaving the Workshop page): nothing more is fetched.
    pub fn cancel_remote(&mut self) {
        if let Some(l) = &self.loader {
            for p in l.cancel_remote() {
                self.entries.remove(&p);
            }
        }
    }

    /// Marks previews as on screen; unknown ones are queued for decoding.
    pub fn touch<'a>(&mut self, paths: impl IntoIterator<Item = &'a Path>) {
        let now = Instant::now();
        for path in paths {
            match self.entries.get_mut(path) {
                Some(Entry::Ready(r)) => r.last_seen = now,
                Some(_) => {}
                None => {
                    self.entries.insert(path.to_owned(), Entry::Loading);
                    match &self.loader {
                        Some(l) => l.request(path),
                        None => {
                            self.waiting.insert(path.to_owned());
                        }
                    }
                }
            }
        }
    }

    pub fn get(&self, path: &Path) -> Option<(&Handle, (u32, u32))> {
        match self.entries.get(path) {
            Some(Entry::Ready(r)) => Some((&r.handle, r.size)),
            _ => None,
        }
    }

    pub fn pending(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e, Entry::Loading))
            .count()
    }

    /// Shows the right frame of each animating preview at `now`, swapping images
    /// only when `may_swap` (the previous frame was drawn). Returns how often the
    /// caller should tick (`None`: nothing animates) and whether an image changed.
    pub fn advance<'a>(
        &mut self,
        animating: impl IntoIterator<Item = &'a Path>,
        now: Instant,
        may_swap: bool,
    ) -> (Option<Duration>, bool) {
        let mut next: Option<u32> = None;
        let mut swapped = false;
        for path in animating {
            let Some(Entry::Ready(r)) = self.entries.get_mut(path) else {
                continue;
            };
            if !r.animated {
                continue;
            }
            r.last_seen = now;
            let Some(anim) = &mut r.anim else {
                if !r.reloading {
                    r.reloading = true;
                    if let Some(l) = &self.loader {
                        l.request(path);
                    }
                }
                continue;
            };
            let elapsed = now.duration_since(anim.start).as_millis() as u64;
            let (idx, until_next) = anim.timeline.at(elapsed);
            if idx != anim.shown && may_swap {
                let (w, h) = (anim.thumb.width, anim.thumb.height);
                if thumbs::decompress_into(&anim.thumb.frames[idx], w, h, &mut self.scratch) {
                    r.handle = Handle::from_rgba(w, h, self.scratch.clone());
                    anim.shown = idx;
                    swapped = true;
                }
            }
            let _ = until_next;
            let frame = anim.thumb.frames[idx].delay_ms;
            next = Some(next.map_or(frame, |n| n.min(frame)));
        }
        (next.map(quantize_tick), swapped)
    }

    /// Drops the frames of animations not seen for a while when over budget.
    pub fn evict(&mut self) {
        let used: usize = self
            .entries
            .values()
            .filter_map(|e| match e {
                Entry::Ready(Ready { anim: Some(a), .. }) => Some(a.thumb.compressed_bytes()),
                _ => None,
            })
            .sum();
        if used <= ANIM_BUDGET {
            return;
        }
        let mut lru: Vec<(Instant, PathBuf, usize)> = self
            .entries
            .iter()
            .filter_map(|(p, e)| match e {
                Entry::Ready(Ready {
                    anim: Some(a),
                    last_seen,
                    ..
                }) => Some((*last_seen, p.clone(), a.thumb.compressed_bytes())),
                _ => None,
            })
            .collect();
        lru.sort_by_key(|e| e.0);
        let mut excess = used - ANIM_BUDGET;
        for (_, path, bytes) in lru {
            if excess == 0 {
                break;
            }
            if let Some(Entry::Ready(r)) = self.entries.get_mut(&path) {
                r.anim = None;
                excess = excess.saturating_sub(bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_quantized() {
        assert_eq!(quantize_tick(10), Duration::from_millis(33));
        assert_eq!(quantize_tick(60), Duration::from_millis(60));
        assert_eq!(quantize_tick(70), Duration::from_millis(60));
        assert_eq!(quantize_tick(100), Duration::from_millis(100));
        assert_eq!(quantize_tick(1000), Duration::from_millis(150));
    }
}

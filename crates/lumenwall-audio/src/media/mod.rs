//! Now playing: what the system's media session plays, for SceneScript media events and the
//! web wallpapers' `wallpaperRegisterMedia*Listener` (spec 07 §16, spec 06 §8.4).
//!
//! - Windows: the Global System Media Transport Controls (`GlobalSystemMediaTransportControlsSessionManager`):
//!   the system's current session, followed through its `CurrentSessionChanged`,
//!   `MediaPropertiesChanged`, `PlaybackInfoChanged` and `TimelinePropertiesChanged` events.
//! - Linux: MPRIS on the session bus (`org.mpris.MediaPlayer2.*`), followed through
//!   `PropertiesChanged`, `Seeked` and `NameOwnerChanged`.
//!
//! A [`MediaHub`] runs the system source only while something subscribes (a scene whose scripts
//! handle media events, a page with a media listener): no thread, no registration and no wake-up
//! otherwise. Each source thread sleeps until the system reports a change; nothing polls (the
//! reference queries the MPRIS position every frame, QO-23, PARITY-POLICY rule 1). Consumers get
//! a [`MediaState`] through [`MediaSubscription::poll`] after their waker ran.
//!
//! Everything stays on the machine: metadata and artwork are read locally and handed to the
//! wallpaper in this process; artwork behind an `http(s)://` URL (some MPRIS players) is not
//! downloaded.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Instant;

#[cfg(feature = "media")]
mod artwork;
#[cfg(all(windows, feature = "media"))]
mod gsmtc;
#[cfg(any(test, all(target_os = "linux", feature = "media")))]
pub(crate) mod mpris;
#[cfg(all(target_os = "linux", feature = "media"))]
mod mpris_bus;

#[cfg(feature = "media")]
pub use artwork::{MAX_SIDE, decode_artwork};

/// Playback state of the current media.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

/// Artwork of the current media: at most [`MAX_SIDE`] pixels on each side.
#[derive(Clone, Debug)]
pub struct Artwork {
    /// PNG encoding of [`Artwork::rgba`] (web wallpapers get it as a `data:image/png` URL).
    pub png: Vec<u8>,
    /// Tightly packed RGBA8 pixels.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Hash of the bytes the artwork was decoded from: equal keys, same artwork.
    pub key: u64,
}

impl PartialEq for Artwork {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.width == other.width && self.height == other.height
    }
}

/// The media being played.
#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub sub_title: String,
    pub album_title: String,
    pub album_artist: String,
    /// Comma-separated.
    pub genres: String,
    /// `music`, `video`, `image`, or empty when the source does not say.
    pub content_type: String,
    pub state: PlaybackState,
    /// Seconds into the media at [`NowPlaying::sampled`].
    pub position: f64,
    /// Seconds; 0 when unknown.
    pub duration: f64,
    pub sampled: Instant,
    pub artwork: Option<Arc<Artwork>>,
}

impl NowPlaying {
    /// An empty, stopped media sampled now.
    pub fn new() -> Self {
        Self {
            title: String::new(),
            artist: String::new(),
            sub_title: String::new(),
            album_title: String::new(),
            album_artist: String::new(),
            genres: String::new(),
            content_type: String::new(),
            state: PlaybackState::Stopped,
            position: 0.0,
            duration: 0.0,
            sampled: Instant::now(),
            artwork: None,
        }
    }

    /// Position at `now`: it advances while playing, within the duration when known.
    pub fn position_at(&self, now: Instant) -> f64 {
        let mut p = self.position;
        if self.state == PlaybackState::Playing {
            p += now.saturating_duration_since(self.sampled).as_secs_f64();
        }
        if self.duration > 0.0 {
            p = p.min(self.duration);
        }
        p.max(0.0)
    }

    /// Same metadata, state and artwork, and a position that only moved with the playback
    /// (a seek, or a correction of more than half a second, is a change).
    pub fn same_as(&self, other: &Self) -> bool {
        let now = Instant::now();
        self.title == other.title
            && self.artist == other.artist
            && self.sub_title == other.sub_title
            && self.album_title == other.album_title
            && self.album_artist == other.album_artist
            && self.genres == other.genres
            && self.content_type == other.content_type
            && self.state == other.state
            && (self.duration - other.duration).abs() < 0.5
            && (self.position_at(now) - other.position_at(now)).abs() < 0.5
            && self.artwork == other.artwork
    }
}

impl Default for NowPlaying {
    fn default() -> Self {
        Self::new()
    }
}

/// What the media source knows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaState {
    /// The system's media source answered (media session manager obtained, session bus
    /// reached): `mediaStatusChanged({enabled})` and the status listener.
    pub available: bool,
    /// The current media; `None` when nothing has a media session.
    pub session: Option<NowPlaying>,
}

impl MediaState {
    fn same_as(&self, other: &Self) -> bool {
        self.available == other.available
            && match (&self.session, &other.session) {
                (None, None) => true,
                (Some(a), Some(b)) => a.same_as(b),
                _ => false,
            }
    }
}

/// Called on the source's thread after each new [`MediaState`]; must be cheap and not block
/// (an engine waker).
pub type MediaWaker = Arc<dyn Fn() + Send + Sync>;

/// Starts a source that reports through the sink; dropping the returned value stops it. Called
/// without any lock held; it must not block (start a thread).
type SourceFactory = dyn Fn(MediaSink) -> Box<dyn Send> + Send + Sync;

/// Shares one media source between every consumer of the process.
pub struct MediaHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    factory: Box<SourceFactory>,
    state: Mutex<HubState>,
}

#[derive(Default)]
struct HubState {
    subscribers: Vec<(u64, MediaWaker)>,
    next_id: u64,
    /// Incremented each time a source starts or stops: a sink of an older one is ignored.
    epoch: u64,
    starting: bool,
    source: Option<Box<dyn Send>>,
    current: Option<Arc<MediaState>>,
    generation: u64,
}

fn lock(m: &Mutex<HubState>) -> MutexGuard<'_, HubState> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl MediaHub {
    /// A hub over the source `factory` builds (tests use fakes).
    pub fn new(factory: impl Fn(MediaSink) -> Box<dyn Send> + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(HubInner {
                factory: Box::new(factory),
                state: Mutex::new(HubState::default()),
            }),
        }
    }

    /// The process-wide hub over the system's media sessions.
    pub fn system() -> &'static MediaHub {
        static HUB: OnceLock<MediaHub> = OnceLock::new();
        HUB.get_or_init(|| MediaHub::new(system_source))
    }

    /// Subscribes; the first subscriber starts the source, the last one to go stops it.
    /// `waker` runs on the source thread after each change.
    pub fn subscribe(&self, waker: MediaWaker) -> MediaSubscription {
        let (id, start) = {
            let mut s = lock(&self.inner.state);
            s.next_id += 1;
            let id = s.next_id;
            s.subscribers.push((id, waker));
            let start = s.source.is_none() && !s.starting;
            if start {
                s.starting = true;
                s.epoch += 1;
            }
            (id, start.then_some(s.epoch))
        };
        if let Some(epoch) = start {
            let source = (self.inner.factory)(MediaSink {
                hub: Arc::downgrade(&self.inner),
                epoch,
            });
            let mut s = lock(&self.inner.state);
            s.starting = false;
            if s.epoch == epoch && !s.subscribers.is_empty() {
                s.source = Some(source);
            } else {
                drop(s);
                drop(source);
            }
        }
        MediaSubscription {
            hub: Arc::clone(&self.inner),
            id,
            seen: 0,
        }
    }

    /// Whether a source runs (tests and diagnostics).
    pub fn is_running(&self) -> bool {
        lock(&self.inner.state).source.is_some()
    }
}

/// A consumer's handle; dropping it unsubscribes.
pub struct MediaSubscription {
    hub: Arc<HubInner>,
    id: u64,
    seen: u64,
}

impl MediaSubscription {
    /// The state, when it changed since the last call (the first call returns the current
    /// state once the source reported one).
    pub fn poll(&mut self) -> Option<Arc<MediaState>> {
        let s = lock(&self.hub.state);
        if s.generation == self.seen {
            return None;
        }
        let current = s.current.clone()?;
        self.seen = s.generation;
        Some(current)
    }

    /// The last state the source reported, without marking it as seen.
    pub fn current(&self) -> Option<Arc<MediaState>> {
        lock(&self.hub.state).current.clone()
    }
}

impl Drop for MediaSubscription {
    fn drop(&mut self) {
        let source = {
            let mut s = lock(&self.hub.state);
            s.subscribers.retain(|(id, _)| *id != self.id);
            if !s.subscribers.is_empty() {
                return;
            }
            s.epoch += 1;
            s.current = None;
            s.generation += 1;
            s.source.take()
        };
        // Outside the lock: a source may publish while it stops.
        drop(source);
    }
}

impl std::fmt::Debug for MediaSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaSubscription")
            .field("id", &self.id)
            .finish()
    }
}

/// Where a source reports.
#[derive(Clone)]
pub struct MediaSink {
    hub: Weak<HubInner>,
    epoch: u64,
}

impl MediaSink {
    /// Reports the current state; wakes the subscribers if it differs from the previous one.
    /// `false` once the source should stop (no subscriber left).
    pub fn publish(&self, state: MediaState) -> bool {
        let Some(hub) = self.hub.upgrade() else {
            return false;
        };
        let wakers: Vec<MediaWaker> = {
            let mut s = lock(&hub.state);
            if s.epoch != self.epoch {
                return false;
            }
            if s.current.as_deref().is_some_and(|c| c.same_as(&state)) {
                return true;
            }
            s.current = Some(Arc::new(state));
            s.generation += 1;
            s.subscribers.iter().map(|(_, w)| Arc::clone(w)).collect()
        };
        for w in wakers {
            w();
        }
        true
    }

    /// Whether the source should stop.
    pub fn is_stopped(&self) -> bool {
        self.hub
            .upgrade()
            .is_none_or(|hub| lock(&hub.state).epoch != self.epoch)
    }
}

#[cfg(all(feature = "media", any(windows, target_os = "linux")))]
fn system_source(sink: MediaSink) -> Box<dyn Send> {
    #[cfg(windows)]
    return gsmtc::start(sink);
    #[cfg(target_os = "linux")]
    return mpris_bus::start(sink);
}

#[cfg(not(all(feature = "media", any(windows, target_os = "linux"))))]
fn system_source(sink: MediaSink) -> Box<dyn Send> {
    tracing::info!("media integration: not available in this build");
    sink.publish(MediaState::default());
    Box::new(())
}

#[cfg(test)]
mod tests;

//! Windows media source: Global System Media Transport Controls.
//!
//! One thread in the multithreaded apartment holds the session manager and the system's current
//! session (the one the volume flyout shows). The system's events (`CurrentSessionChanged`,
//! `MediaPropertiesChanged`, `PlaybackInfoChanged`, `TimelinePropertiesChanged`) only set a
//! flag and wake that thread, which then reads what changed: the event handlers run on system
//! threads and must return at once. Between events the thread waits on a condition variable
//! without a timeout. The artwork stream is read on a media properties change only, and decoded
//! only when its bytes changed.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as Properties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Media::MediaPlaybackType;
use windows::Storage::Streams::{Buffer, DataReader, InputStreamOptions};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::Result as WinResult;

use super::artwork::{MAX_SOURCE_BYTES, artwork_key, decode_artwork};
use super::{MediaSink, MediaState, NowPlaying, PlaybackState};

const SESSION: u32 = 1;
const PROPERTIES: u32 = 2;
const PLAYBACK: u32 = 4;
const TIMELINE: u32 = 8;
const STOP: u32 = 16;

/// Pending event flags, set by the system's threads.
#[derive(Default)]
struct Signal {
    flags: Mutex<u32>,
    cv: Condvar,
}

impl Signal {
    fn raise(&self, bits: u32) {
        *self.flags.lock().unwrap_or_else(PoisonError::into_inner) |= bits;
        self.cv.notify_one();
    }

    fn wait(&self) -> u32 {
        let mut f = self.flags.lock().unwrap_or_else(PoisonError::into_inner);
        while *f == 0 {
            f = self.cv.wait(f).unwrap_or_else(PoisonError::into_inner);
        }
        std::mem::take(&mut *f)
    }
}

/// Stops the thread when dropped (without waiting for it).
struct StopGuard(Arc<Signal>);

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.raise(STOP);
    }
}

pub(super) fn start(sink: MediaSink) -> Box<dyn Send> {
    let signal = Arc::new(Signal::default());
    let thread_signal = Arc::clone(&signal);
    let spawned = thread::Builder::new()
        .name("lumenwall-media".into())
        .spawn(move || {
            // SAFETY: once per thread, balanced by CoUninitialize below.
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            run(&sink, &thread_signal);
            if com {
                // SAFETY: every COM object of this thread was dropped inside `run`.
                unsafe { CoUninitialize() };
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("media integration: cannot start its thread: {e}");
    }
    Box::new(StopGuard(signal))
}

fn handler<T, U>(signal: &Arc<Signal>, bits: u32) -> TypedEventHandler<T, U>
where
    T: windows::core::RuntimeType + 'static,
    U: windows::core::RuntimeType + 'static,
{
    let signal = Arc::clone(signal);
    TypedEventHandler::new(move |_, _| {
        signal.raise(bits);
        Ok(())
    })
}

/// The current session with the three handlers registered on it.
struct Bound {
    session: Session,
    tokens: [i64; 3],
}

impl Bound {
    fn new(session: Session, signal: &Arc<Signal>) -> WinResult<Self> {
        let props = session.MediaPropertiesChanged(&handler(signal, PROPERTIES))?;
        let playback = session.PlaybackInfoChanged(&handler(signal, PLAYBACK))?;
        let timeline = session.TimelinePropertiesChanged(&handler(signal, TIMELINE))?;
        Ok(Self {
            session,
            tokens: [props, playback, timeline],
        })
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        let [props, playback, timeline] = self.tokens;
        let _ = self.session.RemoveMediaPropertiesChanged(props);
        let _ = self.session.RemovePlaybackInfoChanged(playback);
        let _ = self.session.RemoveTimelinePropertiesChanged(timeline);
    }
}

/// What was read from the session so far.
struct Read {
    now: NowPlaying,
    /// Raw timeline: position and its update time (Windows `DateTime` ticks), duration.
    position: f64,
    updated_ticks: i64,
    art_key: Option<u64>,
}

impl Read {
    fn new() -> Self {
        Self {
            now: NowPlaying::new(),
            position: 0.0,
            updated_ticks: 0,
            art_key: None,
        }
    }
}

fn run(sink: &MediaSink, signal: &Arc<Signal>) {
    let manager = match Manager::RequestAsync().and_then(|op| op.join()) {
        Ok(m) => m,
        Err(e) => {
            tracing::info!("media integration: no media session manager ({e})");
            sink.publish(MediaState::default());
            return;
        }
    };
    let token = manager
        .CurrentSessionChanged(&handler(signal, SESSION))
        .inspect_err(|e| tracing::warn!("media integration: CurrentSessionChanged: {e}"))
        .ok();
    tracing::debug!("media integration: following the system's media sessions");
    let started = Instant::now();
    let mut wakes = 0u64;
    let mut bound: Option<Bound> = None;
    let mut read = Read::new();
    let mut pending = SESSION;
    loop {
        if pending & STOP != 0 {
            break;
        }
        tracing::trace!("media integration: woken ({pending:#x})");
        if pending & SESSION != 0 {
            bound = None;
            read = Read::new();
            if let Ok(session) = manager.GetCurrentSession() {
                match Bound::new(session, signal) {
                    Ok(b) => bound = Some(b),
                    Err(e) => tracing::debug!("media integration: session events: {e}"),
                }
            }
            pending |= PROPERTIES | PLAYBACK | TIMELINE;
        }
        let session = bound.as_ref().map(|b| {
            update(&b.session, &mut read, pending);
            read.now.clone()
        });
        if !sink.publish(MediaState {
            available: true,
            session,
        }) {
            break;
        }
        pending = signal.wait();
        if pending & STOP == 0 {
            wakes += 1;
        }
    }
    tracing::debug!(
        "media integration: stopped; woken {wakes} time(s) by the system in {:.0} s",
        started.elapsed().as_secs_f64()
    );
    drop(bound);
    if let Some(t) = token {
        let _ = manager.RemoveCurrentSessionChanged(t);
    }
}

/// Reads the parts of the session that `pending` names.
fn update(session: &Session, read: &mut Read, pending: u32) {
    if pending & PROPERTIES != 0 {
        match session
            .TryGetMediaPropertiesAsync()
            .and_then(|op| op.join())
        {
            Ok(p) => properties(&p, read),
            Err(e) => tracing::debug!("media integration: media properties: {e}"),
        }
    }
    if pending & PLAYBACK != 0
        && let Ok(status) = session.GetPlaybackInfo().and_then(|i| i.PlaybackStatus())
    {
        read.now.state = playback_state(status);
    }
    if pending & TIMELINE != 0
        && let Ok(t) = session.GetTimelineProperties()
    {
        let start = t.StartTime().map(|s| s.Duration).unwrap_or(0);
        let end = t.EndTime().map(|s| s.Duration).unwrap_or(0);
        let position = t.Position().map(|s| s.Duration).unwrap_or(0);
        read.position = ticks_to_secs(position - start);
        read.now.duration = ticks_to_secs((end - start).max(0));
        read.updated_ticks = t.LastUpdatedTime().map(|d| d.UniversalTime).unwrap_or(0);
    }
    // The timeline is the position at its last update: bring it to now while playing.
    let behind = if read.now.state == PlaybackState::Playing {
        elapsed_since(read.updated_ticks)
    } else {
        0.0
    };
    read.now.position = read.position + behind;
    read.now.sampled = Instant::now();
}

fn properties(p: &Properties, read: &mut Read) {
    let text = |r: WinResult<windows::core::HSTRING>| r.map(|s| s.to_string()).unwrap_or_default();
    let now = &mut read.now;
    now.title = text(p.Title());
    now.artist = text(p.Artist());
    now.sub_title = text(p.Subtitle());
    now.album_title = text(p.AlbumTitle());
    now.album_artist = text(p.AlbumArtist());
    now.genres = p
        .Genres()
        .map(|g| {
            let n = g.Size().unwrap_or(0);
            (0..n)
                .filter_map(|i| g.GetAt(i).ok().map(|s| s.to_string()))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    now.content_type = match p.PlaybackType().and_then(|t| t.Value()) {
        Ok(MediaPlaybackType::Music) => "music",
        Ok(MediaPlaybackType::Video) => "video",
        Ok(MediaPlaybackType::Image) => "image",
        _ => "",
    }
    .to_owned();
    match thumbnail_bytes(p) {
        Some(bytes) => {
            let key = artwork_key(&bytes);
            if read.art_key != Some(key) {
                read.art_key = Some(key);
                now.artwork = decode_artwork(&bytes).map(Arc::new);
            }
        }
        None => {
            read.art_key = None;
            now.artwork = None;
        }
    }
}

/// The thumbnail's bytes (JPEG or PNG), when the media has one.
fn thumbnail_bytes(p: &Properties) -> Option<Vec<u8>> {
    let reference = p.Thumbnail().ok()?;
    let stream = reference.OpenReadAsync().and_then(|op| op.join()).ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > MAX_SOURCE_BYTES as u64 {
        return None;
    }
    let size = size as u32;
    let buffer = Buffer::Create(size).ok()?;
    let filled = stream
        .ReadAsync(&buffer, size, InputStreamOptions::None)
        .and_then(|op| op.join())
        .ok()?;
    let len = filled.Length().ok()? as usize;
    let mut bytes = vec![0u8; len];
    DataReader::FromBuffer(&filled)
        .and_then(|r| r.ReadBytes(&mut bytes))
        .ok()?;
    Some(bytes)
}

fn playback_state(s: Status) -> PlaybackState {
    match s {
        Status::Playing => PlaybackState::Playing,
        Status::Paused => PlaybackState::Paused,
        // Closed, Opened, Changing, Stopped.
        _ => PlaybackState::Stopped,
    }
}

/// `TimeSpan` ticks (100 ns) to seconds.
fn ticks_to_secs(ticks: i64) -> f64 {
    ticks as f64 / 1e7
}

/// Windows `DateTime` (100 ns ticks since 1601-01-01 UTC) of 1970-01-01.
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

/// Seconds since the `DateTime` `ticks`; 0 for an unset or future time or one more than a day
/// old (a player that never updates its timeline).
fn elapsed_since(ticks: i64) -> f64 {
    if ticks <= UNIX_EPOCH_TICKS {
        return 0.0;
    }
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0.0;
    };
    let now_ticks = UNIX_EPOCH_TICKS + (now.as_nanos() / 100) as i64;
    let elapsed = ticks_to_secs(now_ticks - ticks);
    if (0.0..86_400.0).contains(&elapsed) {
        elapsed
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_conversions() {
        assert_eq!(ticks_to_secs(25_000_000), 2.5);
        assert_eq!(elapsed_since(0), 0.0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let two_s_ago = UNIX_EPOCH_TICKS + (now.as_nanos() / 100) as i64 - 20_000_000;
        let e = elapsed_since(two_s_ago);
        assert!((1.9..3.0).contains(&e), "{e}");
        assert_eq!(elapsed_since(two_s_ago + 1_000_000_000), 0.0, "future");
        assert_eq!(playback_state(Status::Changing), PlaybackState::Stopped);
        assert_eq!(playback_state(Status::Paused), PlaybackState::Paused);
    }
}

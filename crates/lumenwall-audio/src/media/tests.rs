//! The hub with a fake source: start on the first subscriber, stop on the last, one wake-up per
//! change, nothing for an unchanged state.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

/// A source the test drives: its sink, and how many times it started and stopped.
#[derive(Default)]
struct Fake {
    sink: Mutex<Option<MediaSink>>,
    started: AtomicUsize,
    stopped: Arc<AtomicUsize>,
}

struct Running(Arc<AtomicUsize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn hub() -> (MediaHub, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let f = Arc::clone(&fake);
    let hub = MediaHub::new(move |sink| {
        f.started.fetch_add(1, Ordering::SeqCst);
        *f.sink.lock().unwrap() = Some(sink);
        Box::new(Running(Arc::clone(&f.stopped)))
    });
    (hub, fake)
}

fn counter() -> (MediaWaker, Arc<AtomicUsize>) {
    let n = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&n);
    (
        Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }),
        n,
    )
}

fn playing(title: &str) -> MediaState {
    MediaState {
        available: true,
        session: Some(NowPlaying {
            title: title.into(),
            state: PlaybackState::Playing,
            position: 10.0,
            duration: 200.0,
            ..NowPlaying::new()
        }),
    }
}

fn publish(fake: &Fake, state: MediaState) -> bool {
    fake.sink.lock().unwrap().as_ref().unwrap().publish(state)
}

#[test]
fn the_source_runs_only_while_someone_subscribes() {
    let (hub, fake) = hub();
    assert!(!hub.is_running());
    let (w1, n1) = counter();
    let (w2, n2) = counter();
    let mut a = hub.subscribe(w1);
    assert!(hub.is_running());
    assert_eq!(a.poll(), None, "nothing reported yet");
    let mut b = hub.subscribe(w2);
    assert_eq!(
        fake.started.load(Ordering::SeqCst),
        1,
        "one source for both"
    );

    assert!(publish(&fake, playing("one")));
    assert_eq!(
        (n1.load(Ordering::SeqCst), n2.load(Ordering::SeqCst)),
        (1, 1)
    );
    assert_eq!(a.poll().unwrap().session.as_ref().unwrap().title, "one");
    assert_eq!(a.poll(), None, "seen");
    assert!(b.poll().is_some());

    // Same state again, a moment later (the position advanced with the playback): no wake-up.
    assert!(publish(&fake, playing("one")));
    assert_eq!(n1.load(Ordering::SeqCst), 1);
    assert_eq!(a.poll(), None);

    drop(a);
    assert!(hub.is_running());
    drop(b);
    assert!(!hub.is_running());
    assert_eq!(fake.stopped.load(Ordering::SeqCst), 1);
    // The stopped source's sink is refused.
    assert!(!publish(&fake, playing("late")));

    // A new subscriber starts a new source and sees nothing of the old one.
    let (w3, _) = counter();
    let mut c = hub.subscribe(w3);
    assert_eq!(fake.started.load(Ordering::SeqCst), 2);
    assert_eq!(c.poll(), None);
    assert!(publish(&fake, playing("two")));
    assert_eq!(c.poll().unwrap().session.as_ref().unwrap().title, "two");
}

#[test]
fn a_late_subscriber_gets_the_current_state() {
    let (hub, fake) = hub();
    let (w, _) = counter();
    let _first = hub.subscribe(w.clone());
    publish(&fake, playing("now"));
    let mut late = hub.subscribe(w);
    assert_eq!(late.poll().unwrap().session.as_ref().unwrap().title, "now");
    assert!(late.current().is_some());
}

#[test]
fn changes_that_matter_wake_the_subscribers() {
    let (hub, fake) = hub();
    let (w, n) = counter();
    let mut s = hub.subscribe(w);
    publish(&fake, playing("t"));
    let mut seek = playing("t");
    seek.session.as_mut().unwrap().position = 120.0;
    publish(&fake, seek);
    let mut paused = playing("t");
    paused.session.as_mut().unwrap().state = PlaybackState::Paused;
    publish(&fake, paused);
    let mut art = playing("t");
    art.session.as_mut().unwrap().artwork = Some(Arc::new(Artwork {
        png: vec![1],
        rgba: vec![0; 4],
        width: 1,
        height: 1,
        key: 7,
    }));
    publish(&fake, art);
    publish(
        &fake,
        MediaState {
            available: true,
            session: None,
        },
    );
    assert_eq!(n.load(Ordering::SeqCst), 5);
    assert!(s.poll().unwrap().session.is_none());
}

#[test]
fn the_position_advances_only_while_playing() {
    let t0 = Instant::now();
    let mut now = NowPlaying {
        state: PlaybackState::Playing,
        position: 5.0,
        duration: 6.0,
        sampled: t0,
        ..NowPlaying::new()
    };
    assert!((now.position_at(t0 + Duration::from_millis(500)) - 5.5).abs() < 1e-9);
    assert_eq!(
        now.position_at(t0 + Duration::from_secs(9)),
        6.0,
        "within the duration"
    );
    now.state = PlaybackState::Paused;
    assert_eq!(now.position_at(t0 + Duration::from_secs(9)), 5.0);
}

#[cfg(feature = "media")]
#[test]
fn artwork_is_decoded_reduced_and_reencoded() {
    use image::{ImageBuffer, ImageFormat, Rgba};
    let big: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_fn(1024, 512, |x, _| Rgba([(x % 256) as u8, 30, 200, 255]));
    let mut jpeg = Vec::new();
    image::DynamicImage::ImageRgba8(big)
        .to_rgb8()
        .write_to(&mut std::io::Cursor::new(&mut jpeg), ImageFormat::Jpeg)
        .unwrap();
    let art = decode_artwork(&jpeg).unwrap();
    assert_eq!((art.width, art.height), (MAX_SIDE, MAX_SIDE / 2));
    assert_eq!(art.rgba.len(), (art.width * art.height * 4) as usize);
    assert_eq!(&art.png[1..4], b"PNG");
    let again = decode_artwork(&jpeg).unwrap();
    assert_eq!(art.key, again.key);
    assert!(decode_artwork(b"not an image").is_none());
    assert!(decode_artwork(&[]).is_none());
}

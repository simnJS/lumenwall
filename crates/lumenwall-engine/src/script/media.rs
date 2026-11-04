//! Now playing, from `lumenwall-audio`'s media hub to its two consumers: SceneScript media
//! events (spec 06 §8.4) and the web pages' `wallpaperRegisterMedia*Listener` (spec 07 §15.5).
//!
//! The artwork colours come from `lumenwall_web::media::MediaEvent::thumbnail_from_png`, once per
//! artwork: pages get them as CSS colours, scripts as normalized RGB, so both agree.
//!
//! Pages subscribe themselves ([`crate::web`]: the page's host already wakes the engine loop).
//! Scenes need the loop to call `SceneRuntime::media_changed` / `media_status` and to redraw the
//! outputs showing them: [`MediaFeed`] and [`wants_media`] are that API (wired by `engine.rs`).

use std::sync::Arc;
use std::time::Instant;

use lumenwall_audio::media::{
    Artwork, MediaHub, MediaState, MediaSubscription, MediaWaker, NowPlaying, PlaybackState,
};
use lumenwall_scene::Project;
use lumenwall_script::{
    MediaProperties, MediaSnapshot, MediaThumbnail, PlaybackState as ScriptPlayback,
};
use lumenwall_web::media::{MediaEvent, MediaKind, PlaybackState as WebPlayback};

/// SceneScript media handlers (spec 06 §8.4).
const MEDIA_EVENTS: [&str; 5] = [
    "mediaStatusChanged",
    "mediaPropertiesChanged",
    "mediaThumbnailChanged",
    "mediaPlaybackChanged",
    "mediaTimelineChanged",
];

/// Whether a scene's scripts handle media events: only then does the engine follow the system's
/// media session for it. A plain search of the sources (a handler named in a comment costs a
/// subscription, nothing more).
pub(crate) fn wants_media(project: &Project) -> bool {
    project
        .settings
        .scripts()
        .iter()
        .any(|s| MEDIA_EVENTS.iter().any(|e| s.source.contains(e)))
}

fn script_playback(s: PlaybackState) -> ScriptPlayback {
    match s {
        PlaybackState::Stopped => ScriptPlayback::Stopped,
        PlaybackState::Playing => ScriptPlayback::Playing,
        PlaybackState::Paused => ScriptPlayback::Paused,
    }
}

fn web_playback(s: PlaybackState) -> WebPlayback {
    match s {
        PlaybackState::Stopped => WebPlayback::Stopped,
        PlaybackState::Playing => WebPlayback::Playing,
        PlaybackState::Paused => WebPlayback::Paused,
    }
}

/// The page's thumbnail event of `art` (data URL and colours).
pub(crate) fn thumbnail_event(art: &Artwork) -> MediaEvent {
    MediaEvent::thumbnail_from_png(&art.png, &art.rgba)
}

/// The thumbnail event without artwork: no image, the scripts' default colours.
fn no_thumbnail_event() -> MediaEvent {
    let t = MediaThumbnail::default();
    MediaEvent::Thumbnail {
        thumbnail: String::new(),
        primary_color: css(t.primary_color),
        secondary_color: css(t.secondary_color),
        tertiary_color: css(t.tertiary_color),
        text_color: css(t.text_color),
        high_contrast_color: css(t.high_contrast_color),
    }
}

fn css(c: [f32; 3]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// `#rrggbb` as normalized RGB.
fn rgb(hex: &str) -> Option<[f32; 3]> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    Some([c(0)?, c(2)?, c(4)?].map(|v| f32::from(v) / 255.0))
}

/// The scripts' `MediaThumbnailEvent` for a page thumbnail event.
fn script_thumbnail(event: &MediaEvent) -> MediaThumbnail {
    let MediaEvent::Thumbnail {
        thumbnail,
        primary_color,
        secondary_color,
        tertiary_color,
        text_color,
        high_contrast_color,
    } = event
    else {
        return MediaThumbnail::default();
    };
    let d = MediaThumbnail::default();
    MediaThumbnail {
        has_thumbnail: !thumbnail.is_empty(),
        primary_color: rgb(primary_color).unwrap_or(d.primary_color),
        secondary_color: rgb(secondary_color).unwrap_or(d.secondary_color),
        tertiary_color: rgb(tertiary_color).unwrap_or(d.tertiary_color),
        text_color: rgb(text_color).unwrap_or(d.text_color),
        high_contrast_color: rgb(high_contrast_color).unwrap_or(d.high_contrast_color),
    }
}

/// Builds thumbnail events, decoding the palette once per artwork.
#[derive(Default)]
struct Thumbnails {
    last: Option<(Option<u64>, MediaEvent)>,
}

impl Thumbnails {
    fn event(&mut self, art: Option<&Arc<Artwork>>) -> &MediaEvent {
        let key = art.map(|a| a.key);
        if self.last.as_ref().is_none_or(|(k, _)| *k != key) {
            let event = art.map_or_else(no_thumbnail_event, |a| thumbnail_event(a));
            self.last = Some((key, event));
        }
        &self.last.as_ref().expect("just set").1
    }
}

fn properties_event(n: &NowPlaying) -> MediaEvent {
    MediaEvent::Properties {
        title: n.title.clone(),
        artist: n.artist.clone(),
        sub_title: n.sub_title.clone(),
        album_title: n.album_title.clone(),
        album_artist: n.album_artist.clone(),
        genres: n.genres.clone(),
        content_type: n.content_type.clone(),
    }
}

/// Turns media states into the events a page needs, leaving out those that did not change.
#[derive(Default)]
pub(crate) struct WebMedia {
    last: [Option<MediaEvent>; 5],
    thumbnails: Thumbnails,
}

impl WebMedia {
    /// The events for `state` (status, properties, thumbnail, playback; the timeline with every
    /// change, as a fresh sample of the position).
    pub(crate) fn events(&mut self, state: &MediaState) -> Vec<MediaEvent> {
        let empty = NowPlaying::new();
        let now = state.session.as_ref().unwrap_or(&empty);
        let at = Instant::now();
        let candidates = [
            MediaEvent::Status {
                enabled: state.available,
            },
            properties_event(now),
            self.thumbnails.event(now.artwork.as_ref()).clone(),
            MediaEvent::Playback {
                state: web_playback(now.state),
            },
            MediaEvent::Timeline {
                position: now.position_at(at),
                duration: now.duration,
            },
        ];
        let mut out = Vec::with_capacity(5);
        for event in candidates {
            let slot = &mut self.last[event.kind() as usize];
            if event.kind() == MediaKind::Timeline || slot.as_ref() != Some(&event) {
                *slot = Some(event.clone());
                out.push(event);
            }
        }
        out
    }
}

/// What the scenes that handle media get after a change.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SceneMedia {
    /// `mediaStatusChanged` when availability changed.
    pub status: Option<bool>,
    /// `mediaPropertiesChanged`, `mediaPlaybackChanged`, `mediaTimelineChanged`,
    /// `mediaThumbnailChanged` (spec 06 §8.4: all four on any change).
    pub snapshot: MediaSnapshot,
}

/// The engine's subscription for scenes: taken while a scene wants media, dropped otherwise.
#[derive(Default)]
pub(crate) struct MediaFeed {
    subscription: Option<MediaSubscription>,
    available: Option<bool>,
    thumbnails: Thumbnails,
}

impl MediaFeed {
    /// Follows the system's media sessions while `wanted` (some scene [`wants_media`]); the
    /// engine `waker` runs after each change.
    pub(crate) fn sync(&mut self, wanted: bool, waker: impl FnOnce() -> MediaWaker) {
        self.sync_with(MediaHub::system(), wanted, waker);
    }

    fn sync_with(&mut self, hub: &MediaHub, wanted: bool, waker: impl FnOnce() -> MediaWaker) {
        match (wanted, self.subscription.is_some()) {
            (true, false) => self.subscription = Some(hub.subscribe(waker())),
            (false, true) => *self = Self::default(),
            _ => {}
        }
    }

    /// After a wake-up: the new media state for the scenes, if it changed.
    pub(crate) fn poll(&mut self) -> Option<SceneMedia> {
        let state = self.subscription.as_mut()?.poll()?;
        Some(self.scene_media(&state, true))
    }

    /// The current state for a scene that just started (its scripts know nothing yet).
    pub(crate) fn current(&mut self) -> Option<SceneMedia> {
        let state = self.subscription.as_ref()?.current()?;
        Some(self.scene_media(&state, false))
    }

    fn scene_media(&mut self, state: &MediaState, track: bool) -> SceneMedia {
        let status = if track {
            let changed = self.available != Some(state.available);
            self.available = Some(state.available);
            changed.then_some(state.available)
        } else {
            Some(state.available)
        };
        SceneMedia {
            status,
            snapshot: scene_snapshot(state, &mut self.thumbnails),
        }
    }
}

/// The scripts' view of `state` (nothing playing: empty properties, stopped, no thumbnail).
fn scene_snapshot(state: &MediaState, thumbnails: &mut Thumbnails) -> MediaSnapshot {
    let empty = NowPlaying::new();
    let n = state.session.as_ref().unwrap_or(&empty);
    MediaSnapshot {
        properties: MediaProperties {
            title: n.title.clone(),
            artist: n.artist.clone(),
            sub_title: n.sub_title.clone(),
            album_title: n.album_title.clone(),
            album_artist: n.album_artist.clone(),
            genres: n.genres.clone(),
            content_type: n.content_type.clone(),
        },
        state: script_playback(n.state),
        position: n.position_at(Instant::now()),
        duration: n.duration,
        thumbnail: script_thumbnail(thumbnails.event(n.artwork.as_ref())),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use lumenwall_audio::media::MediaSink;

    use super::*;

    /// Two-colour artwork: 60 % red, 40 % blue.
    pub(crate) fn artwork() -> Arc<Artwork> {
        let mut rgba = Vec::new();
        for i in 0..100 {
            rgba.extend_from_slice(if i < 60 {
                &[200, 30, 30, 255]
            } else {
                &[20, 20, 220, 255]
            });
        }
        Arc::new(Artwork {
            png: b"\x89PNG fake".to_vec(),
            rgba,
            width: 10,
            height: 10,
            key: 42,
        })
    }

    pub(crate) fn playing(title: &str, art: Option<Arc<Artwork>>) -> MediaState {
        MediaState {
            available: true,
            session: Some(NowPlaying {
                title: title.into(),
                artist: "Artist".into(),
                album_title: "Album".into(),
                content_type: "music".into(),
                state: PlaybackState::Playing,
                position: 30.0,
                duration: 180.0,
                artwork: art,
                ..NowPlaying::new()
            }),
        }
    }

    #[test]
    fn colours_come_from_thumbnail_from_png() {
        let art = artwork();
        let event = thumbnail_event(&art);
        assert_eq!(event, MediaEvent::thumbnail_from_png(&art.png, &art.rgba));
        let MediaEvent::Thumbnail {
            thumbnail,
            primary_color,
            ..
        } = &event
        else {
            unreachable!()
        };
        assert!(thumbnail.starts_with("data:image/png;base64,"));
        assert_eq!(primary_color, "#c81e1e");
        let t = script_thumbnail(&event);
        assert!(t.has_thumbnail);
        assert_eq!(t.primary_color, [200.0 / 255.0, 30.0 / 255.0, 30.0 / 255.0]);
        assert_eq!(
            t.secondary_color,
            [20.0 / 255.0, 20.0 / 255.0, 220.0 / 255.0]
        );
        // Without artwork: no image and the scripts' default colours, both ways.
        let none = script_thumbnail(&no_thumbnail_event());
        let d = MediaThumbnail::default();
        assert!(!none.has_thumbnail);
        for (a, b) in [
            (none.primary_color, d.primary_color),
            (none.tertiary_color, d.tertiary_color),
            (none.text_color, d.text_color),
        ] {
            for i in 0..3 {
                assert!((a[i] - b[i]).abs() < 0.003, "{a:?} {b:?}");
            }
        }
        assert_eq!(rgb("#zz0000"), None);
        assert_eq!(rgb("123456"), None);
    }

    #[test]
    fn pages_get_only_what_changed() {
        let mut web = WebMedia::default();
        let first = web.events(&playing("One", Some(artwork())));
        let kinds: Vec<_> = first.iter().map(MediaEvent::kind).collect();
        assert_eq!(kinds, MediaKind::ALL);
        assert_eq!(first[0], MediaEvent::Status { enabled: true });
        let MediaEvent::Properties { title, .. } = &first[1] else {
            unreachable!()
        };
        assert_eq!(title, "One");
        assert_eq!(
            first[3],
            MediaEvent::Playback {
                state: WebPlayback::Playing
            }
        );
        let MediaEvent::Timeline { position, duration } = first[4] else {
            unreachable!()
        };
        assert!((30.0..31.0).contains(&position) && duration == 180.0);

        // Paused: playback and timeline only.
        let mut paused = playing("One", Some(artwork()));
        paused.session.as_mut().unwrap().state = PlaybackState::Paused;
        let kinds: Vec<_> = web.events(&paused).iter().map(MediaEvent::kind).collect();
        assert_eq!(kinds, [MediaKind::Playback, MediaKind::Timeline]);

        // The player went away: empty properties, no thumbnail, stopped.
        let gone = MediaState {
            available: true,
            session: None,
        };
        let events = web.events(&gone);
        assert!(events.contains(&no_thumbnail_event()));
        assert!(events.contains(&MediaEvent::Playback {
            state: WebPlayback::Stopped
        }));
        assert!(!events.iter().any(|e| e.kind() == MediaKind::Status));
    }

    /// A hub whose fake source hands its sink to the test.
    fn fake_hub() -> (MediaHub, Arc<Mutex<Option<MediaSink>>>) {
        let sink = Arc::new(Mutex::new(None));
        let s = Arc::clone(&sink);
        let hub = MediaHub::new(move |new| {
            *s.lock().unwrap() = Some(new);
            Box::new(())
        });
        (hub, sink)
    }

    #[test]
    fn scenes_get_the_four_events_and_the_status() {
        let (hub, sink) = fake_hub();
        let wakes = Arc::new(Mutex::new(0));
        let w = Arc::clone(&wakes);
        let waker = move || -> MediaWaker {
            let w = Arc::clone(&w);
            Arc::new(move || *w.lock().unwrap() += 1)
        };
        let mut feed = MediaFeed::default();
        // Not wanted: the system's hub is not even looked at.
        feed.sync(false, || unreachable!("no subscription without a scene"));
        feed.sync_with(&hub, false, waker.clone());
        assert!(!hub.is_running(), "no scene wants media");
        feed.sync_with(&hub, true, waker.clone());
        assert!(hub.is_running());
        assert_eq!(feed.poll(), None);
        let publish = |s: MediaState| sink.lock().unwrap().as_ref().unwrap().publish(s);
        publish(playing("Soren", Some(artwork())));
        assert_eq!(*wakes.lock().unwrap(), 1);
        let m = feed.poll().unwrap();
        assert_eq!(m.status, Some(true));
        assert_eq!(m.snapshot.properties.title, "Soren");
        assert_eq!(m.snapshot.state, ScriptPlayback::Playing);
        assert!(m.snapshot.thumbnail.has_thumbnail);
        assert_eq!(m.snapshot.duration, 180.0);
        // A new track: the four events again, the status only once.
        publish(playing("Next", None));
        let m = feed.poll().unwrap();
        assert_eq!(m.status, None);
        assert!(!m.snapshot.thumbnail.has_thumbnail);
        // A scene loaded now gets everything.
        let c = feed.current().unwrap();
        assert_eq!(c.status, Some(true));
        assert_eq!(c.snapshot.properties.title, "Next");
        feed.sync_with(&hub, false, waker);
        assert!(!hub.is_running(), "the last scene went: the source stops");
    }

    /// The scripts of 3372348904 ("Soren <3"), reduced: event handlers only, no `update`.
    #[test]
    fn event_only_scripts_like_soren_get_the_title_and_the_artwork_state() {
        use crate::script::tests as st;
        let cover = "export function mediaThumbnailChanged(event) {
                thisObject.visible = event.hasThumbnail;
            }";
        let title = "export function mediaPropertiesChanged(event) {
                thisObject.visible = event.title == 'Soren' && event.artist == 'Artist';
            }";
        let (mut p, b) = st::load(&[
            st::image(
                1,
                "cover",
                &format!(r#", "visible": {}"#, st::scripted("true", cover)),
            ),
            st::image(
                2,
                "title",
                &format!(r#", "visible": {}"#, st::scripted("false", title)),
            ),
        ]);
        assert!(wants_media(&p));
        let mut scripts = st::scripts(&mut p, &b);
        let (hub, sink) = fake_hub();
        let mut feed = MediaFeed::default();
        feed.sync_with(&hub, true, || -> MediaWaker { Arc::new(|| {}) });
        let publish = |s: MediaState| sink.lock().unwrap().as_ref().unwrap().publish(s);
        let mut deliver = |feed: &mut MediaFeed, p: &mut Project| {
            let m = feed.poll().unwrap();
            if let Some(enabled) = m.status {
                scripts.media_status(enabled);
            }
            scripts.media_changed(m.snapshot);
            st::tick(&mut scripts, p, &b, 0.0);
        };
        let visible = |p: &Project, i: usize| p.settings.value(st::img(p, i).visible).boolean();

        publish(playing("Soren", None));
        deliver(&mut feed, &mut p);
        assert!(!visible(&p, 0), "no artwork: the cover hides");
        assert!(visible(&p, 1), "the title script saw the track");

        publish(playing("Soren", Some(artwork())));
        deliver(&mut feed, &mut p);
        assert!(visible(&p, 0), "artwork: the cover shows");

        publish(MediaState {
            available: true,
            session: None,
        });
        deliver(&mut feed, &mut p);
        assert!(!visible(&p, 0) && !visible(&p, 1), "nothing plays");
    }

    #[test]
    fn scenes_without_media_handlers_do_not_subscribe() {
        use crate::script::tests as st;
        let (p, _) = st::load(&[st::image(
            1,
            "a",
            &format!(
                r#", "alpha": {}"#,
                st::scripted("1", "export function update() { return 0.5; }")
            ),
        )]);
        assert!(!wants_media(&p));
    }
}

//! Runs the injected bridge in a real JavaScript engine (QuickJS) and drives it through the
//! host-side session, as the browser host does.

use std::cell::RefCell;
use std::rc::Rc;

use lumenwall_web::BRIDGE_JS;
use lumenwall_web::audio::AUDIO_FRAME_LEN;
use lumenwall_web::media::{MediaEvent, PlaybackState};
use lumenwall_web::properties::{GeneralProperties, PropertySet, PropertyValue, UserProperty};
use lumenwall_web::session::{BridgeSession, SessionEffect};
use rquickjs::{Context, Function, Runtime};
use serde_json::{Value, json};

/// A page: one JS context with the bridge installed, plus the host session.
struct Page {
    runtime: Runtime,
    context: Context,
    posted: Rc<RefCell<Vec<String>>>,
    session: BridgeSession,
    effects: Vec<SessionEffect>,
}

impl Page {
    fn new(properties: Value) -> Self {
        Self::with_prelude(properties, "")
    }

    /// `prelude` runs before the bridge (e.g. a page that already defined its listener).
    fn with_prelude(properties: Value, prelude: &str) -> Self {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        let posted = Rc::new(RefCell::new(Vec::new()));
        let sink = posted.clone();
        context.with(|ctx| {
            ctx.eval::<(), _>("var window = globalThis; var log = [];")
                .unwrap();
            if !prelude.is_empty() {
                ctx.eval::<(), _>(prelude).unwrap();
            }
            let post = Function::new(ctx.clone(), move |message: String| {
                sink.borrow_mut().push(message)
            })
            .unwrap();
            ctx.globals().set("__lumenHostPost", post).unwrap();
            ctx.eval::<(), _>(BRIDGE_JS).unwrap();
        });
        let session = BridgeSession::new(
            PropertySet::from_project_properties(&properties),
            GeneralProperties { fps: 30 },
        );
        let mut page = Self {
            runtime,
            context,
            posted,
            session,
            effects: Vec::new(),
        };
        page.pump();
        page
    }

    /// Evaluates page code.
    fn eval(&mut self, code: &str) {
        self.context.with(|ctx| ctx.eval::<(), _>(code).unwrap());
        self.pump();
    }

    /// Evaluates an expression and returns its JSON value.
    fn json(&self, expr: &str) -> Value {
        let text: String = self
            .context
            .with(|ctx| ctx.eval(format!("JSON.stringify({expr})")).unwrap());
        serde_json::from_str(&text).unwrap()
    }

    fn log(&self) -> Value {
        self.json("log")
    }

    fn clear_log(&mut self) {
        self.eval("log.length = 0;");
    }

    /// Runs microtasks, feeds page messages to the session and session scripts to the page,
    /// until quiet.
    fn pump(&mut self) {
        loop {
            while self.runtime.is_job_pending() {
                self.runtime.execute_pending_job().unwrap();
            }
            let posted: Vec<String> = self.posted.borrow_mut().drain(..).collect();
            for message in &posted {
                self.session.page_message(message, &mut self.effects);
            }
            let scripts: Vec<String> = self
                .effects
                .drain(..)
                .filter_map(|e| match e {
                    SessionEffect::Script(s) => Some(s),
                    _ => None,
                })
                .collect();
            if posted.is_empty() && scripts.is_empty() {
                break;
            }
            for script in scripts {
                self.context.with(|ctx| ctx.eval::<(), _>(script).unwrap());
            }
        }
    }

    fn host(&mut self, f: impl FnOnce(&mut BridgeSession, &mut Vec<SessionEffect>)) {
        f(&mut self.session, &mut self.effects);
        self.pump();
    }
}

fn props() -> Value {
    json!({
        "color": { "type": "color", "value": "1 0.5 0" },
        "speed": { "type": "slider", "value": 2 },
        "on": { "type": "bool", "value": true }
    })
}

const LISTENER: &str = r#"
window.wallpaperPropertyListener = {
    applyUserProperties: function (p) { log.push(['user', p]); },
    applyGeneralProperties: function (p) { log.push(['general', p]); },
    setPaused: function (v) { log.push(['paused', v]); },
    userDirectoryFilesAddedOrChanged: function (n, f) { log.push(['dirChanged', n, f]); },
    userDirectoryFilesRemoved: function (n, f) { log.push(['dirRemoved', n, f]); }
};
"#;

#[test]
fn installs_public_api_and_hides_the_native_channel() {
    let page = Page::new(json!({}));
    for name in [
        "wallpaperRegisterAudioListener",
        "wallpaperRegisterMediaStatusListener",
        "wallpaperRegisterMediaPropertiesListener",
        "wallpaperRegisterMediaThumbnailListener",
        "wallpaperRegisterMediaPlaybackListener",
        "wallpaperRegisterMediaTimelineListener",
        "wallpaperRequestRandomFileForProperty",
    ] {
        assert_eq!(
            page.json(&format!("typeof window.{name}")),
            "function",
            "{name}"
        );
    }
    assert_eq!(page.json("typeof window.__lumenHostPost"), "undefined");
    assert_eq!(page.json("Object.keys(window).indexOf('__lumenWE')"), -1);
    assert_eq!(
        page.json("window.wallpaperMediaIntegration.PLAYBACK_PLAYING"),
        1
    );
    assert_eq!(
        page.json("window.wallpaperMediaIntegration.playback.PAUSED"),
        2
    );
    assert_eq!(
        page.json("window.wallpaperMediaIntegration.PLAYBACK_STOPPED"),
        0
    );
}

#[test]
fn initial_properties_wait_for_a_late_listener() {
    let mut page = Page::new(props());
    // The session sent everything on "ready", before the page defined its listener.
    assert_eq!(page.log(), json!([]));
    page.eval(LISTENER);
    assert_eq!(
        page.log(),
        json!([
            ["user", {
                "color": { "type": "color", "value": "1 0.5 0" },
                "on": { "type": "bool", "value": true },
                "speed": { "type": "slider", "value": 2 }
            }],
            ["general", { "fps": 30 }]
        ])
    );
}

#[test]
fn listener_defined_before_bridge_or_with_var() {
    let page = Page::with_prelude(props(), LISTENER);
    assert_eq!(page.log()[0][0], "user");

    // Classic page script: top-level `var` goes through the bridge's accessor.
    let mut page = Page::new(props());
    page.eval("var wallpaperPropertyListener = { applyUserProperties: function (p) { log.push(Object.keys(p).sort()); } };");
    assert_eq!(page.log(), json!([["color", "on", "speed"]]));
}

#[test]
fn methods_added_after_assignment_still_receive_events() {
    let mut page = Page::new(props());
    page.eval("window.wallpaperPropertyListener = {}; window.wallpaperPropertyListener.applyUserProperties = function (p) { log.push(p.speed.value); };");
    assert_eq!(
        page.log(),
        json!([2]),
        "JS numbers print without a fraction"
    );
}

#[test]
fn changes_are_deltas_and_pause_is_forwarded() {
    let mut page = Page::new(props());
    page.eval(LISTENER);
    page.clear_log();
    page.host(|s, out| {
        s.set_properties(
            vec![
                UserProperty::new("speed", PropertyValue::Slider(5.0)),
                UserProperty::new("on", PropertyValue::Bool(true)),
            ],
            out,
        )
    });
    page.host(|s, out| s.set_fps(60, out));
    page.host(|s, out| s.set_paused(true, out));
    page.host(|s, out| s.set_paused(false, out));
    assert_eq!(
        page.log(),
        json!([
            ["user", { "speed": { "type": "slider", "value": 5 } }],
            ["general", { "fps": 60 }],
            ["paused", true],
            ["paused", false]
        ])
    );
}

#[test]
fn pause_before_listener_is_delivered_once_ready() {
    let mut page = Page::new(json!({}));
    page.host(|s, out| s.set_paused(true, out));
    page.eval(LISTENER);
    let log = page.log();
    assert!(log.as_array().unwrap().contains(&json!(["paused", true])));
}

#[test]
fn audio_listener_receives_128_values() {
    let mut page = Page::new(json!({}));
    let mut frame = [0.0f32; AUDIO_FRAME_LEN];
    frame[0] = 0.75;
    frame[64] = 0.25;
    // No listener yet: nothing to send.
    assert!(page.session.audio(&frame).is_none());

    page.eval("window.wallpaperRegisterAudioListener(function (a) { log.push([a.length, a[0], a[64], Array.isArray(a)]); });");
    assert!(page.session.audio_listener());
    let script = page.session.audio(&frame).unwrap().to_owned();
    page.eval(&script);
    assert_eq!(page.log(), json!([[128, 0.75, 0.25, true]]));

    page.eval("window.wallpaperRegisterAudioListener(null);");
    assert!(!page.session.audio_listener());
}

#[test]
fn media_listeners_and_replay() {
    let mut page = Page::new(json!({}));
    page.host(|s, out| {
        s.media(
            MediaEvent::Playback {
                state: PlaybackState::Playing,
            },
            out,
        )
    });
    page.eval(
        "window.wallpaperRegisterMediaPlaybackListener(function (e) { log.push(e.state === window.wallpaperMediaIntegration.PLAYBACK_PLAYING); });",
    );
    assert_eq!(
        page.log(),
        json!([true]),
        "current state replayed on registration"
    );
    page.eval("window.wallpaperRegisterMediaTimelineListener(function (e) { log.push([e.position, e.duration]); });");
    page.host(|s, out| {
        s.media(
            MediaEvent::Timeline {
                position: 12.5,
                duration: 200.0,
            },
            out,
        )
    });
    page.host(|s, out| s.media(MediaEvent::Status { enabled: true }, out));
    assert_eq!(page.log(), json!([true, [12.5, 200]]));
}

#[test]
fn random_file_requests() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("only.png"), b"x").unwrap();
    let mut page = Page::new(json!({
        "pics": { "type": "directory", "mode": "ondemand", "value": dir.path().to_str().unwrap() }
    }));
    page.eval("window.wallpaperRequestRandomFileForProperty('pics', function (n, f) { log.push([n, f]); });");
    page.eval("window.wallpaperRequestRandomFileForProperty('nothing', function (n, f) { log.push('never'); });");
    let log = page.log();
    assert_eq!(log.as_array().unwrap().len(), 1);
    assert_eq!(log[0][0], "pics");
    let file = log[0][1].as_str().unwrap();
    assert!(
        file.ends_with("only.png") && !file.starts_with('/'),
        "{file}"
    );
}

#[test]
fn fetchall_directories() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.webm"), b"x").unwrap();
    std::fs::write(dir.path().join("b.png"), b"x").unwrap();
    let mut page = Page::with_prelude(
        json!({
            "clips": { "type": "directory", "mode": "fetchall", "filetype": "video", "value": dir.path().to_str().unwrap() }
        }),
        LISTENER,
    );
    let log = page.log();
    let changed = log
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e[0] == "dirChanged")
        .expect("listing")
        .clone();
    assert_eq!(changed[1], "clips");
    assert_eq!(changed[2].as_array().unwrap().len(), 1);
    assert!(changed[2][0].as_str().unwrap().ends_with("a.webm"));
    page.clear_log();
    std::fs::remove_file(dir.path().join("a.webm")).unwrap();
    page.host(|s, out| s.rescan_directories(out));
    assert_eq!(page.log()[0][0], "dirRemoved");
}

#[test]
fn page_exceptions_do_not_break_the_bridge() {
    let mut page = Page::new(props());
    page.eval("window.wallpaperPropertyListener = { applyUserProperties: function () { throw new Error('page bug'); }, setPaused: function (v) { log.push(v); } };");
    page.host(|s, out| s.set_paused(true, out));
    assert_eq!(page.log(), json!([true]));
}

#[test]
fn reload_resends_state() {
    let mut page = Page::new(props());
    page.eval("window.wallpaperRegisterAudioListener(function () {});");
    assert!(page.session.audio_listener());
    // Navigation: the host drops the old context and the bridge of the new document says ready.
    let mut out = Vec::new();
    page.session.context_released(&mut out);
    assert!(out.contains(&SessionEffect::AudioListener(false)));
    let mut fresh = Page::new(props());
    fresh.eval(LISTENER);
    assert_eq!(fresh.log()[0][0], "user");
}

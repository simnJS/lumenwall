//! Wire-format tests: every message survives a JSON round trip, the documented
//! shapes stay stable, and peers tolerate what newer builds add.

use serde_json::json;

use crate::*;

fn roundtrip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = serde_json::to_string(value).unwrap();
    assert!(!text.contains('\n'), "a frame must fit on one line: {text}");
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "round trip changed {text}");
}

fn sample_state() -> State {
    State {
        paused: true,
        pause_reason: Some(PauseReason::Fullscreen),
        muted: false,
        volume: 0.25,
        outputs: vec![OutputState {
            output: "DP-1".into(),
            wallpaper: Some("/w/3018340632".into()),
            scaling: Scaling::Fit,
            status: WallpaperStatus::Running,
            message: None,
        }],
        wallpaper_audio: Some(
            [(
                "3018340632".to_owned(),
                WallpaperAudio {
                    volume: 0.4,
                    muted: true,
                },
            )]
            .into(),
        ),
    }
}

fn sample_props() -> PropertyMap {
    PropertyMap::from([
        ("rain".into(), PropertyValue::Bool(true)),
        ("speed".into(), PropertyValue::Number(1.5)),
        ("schemecolor".into(), PropertyValue::Color([0.1, 0.2, 0.3])),
        ("mode".into(), PropertyValue::Text("night".into())),
    ])
}

fn all_requests() -> Vec<Request> {
    vec![
        Request::Hello {
            protocol: PROTOCOL_VERSION,
            client: "test".into(),
        },
        Request::Bye,
        Request::ListOutputs,
        Request::GetState,
        Request::Apply {
            output: Some("HDMI-A-1".into()),
            wallpaper: "/w/1".into(),
            scaling: Scaling::Stretch,
        },
        Request::Apply {
            output: None,
            wallpaper: "/w/1".into(),
            scaling: Scaling::Fill,
        },
        Request::Clear { output: None },
        Request::GetProperties {
            output: "DP-1".into(),
        },
        Request::SetProperties {
            output: "DP-1".into(),
            values: sample_props(),
            persist: false,
        },
        Request::ResetProperties {
            output: "DP-1".into(),
        },
        Request::SetPaused { paused: true },
        Request::SetMuted { muted: true },
        Request::SetVolume { volume: 0.75 },
        Request::SetWallpaperAudio {
            wallpaper: "/w/3018340632".into(),
            volume: Some(0.5),
            muted: None,
        },
        Request::SetWallpaperAudio {
            wallpaper: "/w/1".into(),
            volume: None,
            muted: Some(true),
        },
        Request::GetSettings,
        Request::SetSettings {
            settings: Settings::default(),
        },
        Request::GetStats,
        Request::Subscribe {
            topics: Topic::ALL.to_vec(),
        },
        Request::Unsubscribe {
            topics: vec![Topic::Stats],
        },
        Request::Shutdown,
    ]
}

#[test]
fn every_request_round_trips() {
    for (id, request) in all_requests().into_iter().enumerate() {
        roundtrip(&ClientFrame {
            id: id as u64,
            request,
        });
    }
}

#[test]
fn every_reply_and_event_round_trips() {
    let responses = vec![
        Response::Welcome(ServerInfo {
            protocol: PROTOCOL_VERSION,
            server: "lumenwall".into(),
            version: "0.1.0".into(),
        }),
        Response::Done,
        Response::Outputs {
            outputs: vec![Output {
                id: "DP-1".into(),
                name: "Dell U2720Q".into(),
                x: -3840,
                y: 0,
                width: 3840,
                height: 2160,
                scale: 1.5,
                refresh_mhz: 59_997,
                primary: true,
            }],
        },
        Response::State(sample_state()),
        Response::Properties {
            output: "DP-1".into(),
            values: sample_props(),
        },
        Response::Settings(Settings::default()),
        Response::Stats(Stats {
            fps: 29.9,
            frame_time_ms: 1.2,
            cpu_percent: 0.8,
            gpu_percent: Some(3.0),
            rss_bytes: 80 << 20,
            vram_bytes: None,
            outputs: vec![OutputStats {
                output: "DP-1".into(),
                fps: 29.9,
                frame_time_ms: 1.2,
                rendering: true,
            }],
        }),
    ];
    for (id, response) in responses.into_iter().enumerate() {
        roundtrip(&ServerFrame::Reply(Reply {
            id: id as u64,
            outcome: Outcome::Ok(response),
        }));
    }
    roundtrip(&ServerFrame::Reply(Reply {
        id: 9,
        outcome: Outcome::Error(RemoteError::new(ErrorCode::UnknownOutput, "no DP-9")),
    }));
    for event in [
        Event::State(sample_state()),
        Event::Outputs { outputs: vec![] },
        Event::Settings(Settings::default()),
        Event::Properties {
            output: "DP-1".into(),
            values: sample_props(),
        },
        Event::Stats(Stats::default()),
        Event::ShuttingDown,
    ] {
        roundtrip(&ServerFrame::Event(event));
    }
}

#[test]
fn documented_shapes_are_stable() {
    // These literal shapes are what scripts and other clients rely on.
    let frame = ClientFrame {
        id: 3,
        request: Request::SetPaused { paused: true },
    };
    assert_eq!(
        serde_json::to_value(&frame).unwrap(),
        json!({"id": 3, "method": "set_paused", "params": {"paused": true}})
    );
    let frame = ClientFrame {
        id: 4,
        request: Request::GetState,
    };
    assert_eq!(
        serde_json::to_value(&frame).unwrap(),
        json!({"id": 4, "method": "get_state"})
    );
    let reply = ServerFrame::Reply(Reply {
        id: 4,
        outcome: Outcome::Ok(Response::Done),
    });
    assert_eq!(
        serde_json::to_value(&reply).unwrap(),
        json!({"reply": {"id": 4, "ok": {"type": "done"}}})
    );
    let err = ServerFrame::Reply(Reply {
        id: 5,
        outcome: Outcome::Error(RemoteError::new(ErrorCode::Unsupported, "x")),
    });
    assert_eq!(
        serde_json::to_value(&err).unwrap(),
        json!({"reply": {"id": 5, "error": {"code": "unsupported", "message": "x"}}})
    );
    let props = serde_json::to_value(sample_props()).unwrap();
    assert_eq!(
        props,
        json!({"mode": "night", "rain": true, "schemecolor": [0.1f32, 0.2f32, 0.3f32], "speed": 1.5})
    );
}

#[test]
fn hand_written_requests_parse() {
    // Scripts may omit optional parameters.
    let frame: ClientFrame =
        serde_json::from_str(r#"{"id":1,"method":"apply","params":{"wallpaper":"/w/x"}}"#).unwrap();
    assert_eq!(
        frame.request,
        Request::Apply {
            output: None,
            wallpaper: "/w/x".into(),
            scaling: Scaling::Fill
        }
    );
    let frame: ClientFrame = serde_json::from_str(r#"{"method":"list_outputs","id":2}"#).unwrap();
    assert_eq!(frame.request, Request::ListOutputs);
    assert!(serde_json::from_str::<ClientFrame>(r#"{"id":1,"method":"fly"}"#).is_err());
}

#[test]
fn newer_peers_are_tolerated() {
    // Unknown fields everywhere, unknown enum values in daemon-to-client data.
    let text = r#"{"event":{"type":"state","paused":true,"pause_reason":"gaming_mode",
        "muted":false,"volume":1.0,"future":42,
        "outputs":[{"output":"DP-1","status":"compiling_shaders","hdr":true}]}}"#;
    let frame: ServerFrame = serde_json::from_str(text).unwrap();
    let ServerFrame::Event(Event::State(state)) = frame else {
        panic!("{frame:?}")
    };
    assert_eq!(state.pause_reason, Some(PauseReason::Unknown));
    assert_eq!(state.outputs[0].status, WallpaperStatus::Unknown);
    assert_eq!(state.outputs[0].scaling, Scaling::Fill);

    // Settings from an older daemon lacking fields get defaults.
    let settings: Settings = serde_json::from_str(r#"{"fps_limit":60}"#).unwrap();
    assert_eq!(
        settings,
        Settings {
            fps_limit: 60,
            ..Settings::default()
        }
    );

    let err: RemoteError = serde_json::from_str(r#"{"code":"quota_exceeded"}"#).unwrap();
    assert_eq!(err.code, ErrorCode::Unknown);

    // A daemon without per-wallpaper levels leaves the field out: clients hide the controls.
    assert_eq!(state.wallpaper_audio, None);
    let text = serde_json::to_string(&State::default()).unwrap();
    assert!(!text.contains("wallpaper_audio"), "{text}");
    // Older clients read a newer state: the extra field is ignored by them, and partial
    // entries get defaults here.
    let state: State = serde_json::from_str(
        r#"{"paused":false,"muted":false,"volume":0.5,
            "wallpaper_audio":{"7":{"muted":true},"/w/x":{"volume":0.25}}}"#,
    )
    .unwrap();
    let levels = state.wallpaper_audio.unwrap();
    assert_eq!(
        levels["7"],
        WallpaperAudio {
            volume: 1.0,
            muted: true
        }
    );
    assert_eq!(levels["/w/x"].gain(), 0.25);
    let req: ClientFrame = serde_json::from_str(
        r#"{"id":4,"method":"set_wallpaper_audio","params":{"wallpaper":"/w/x","volume":0.3}}"#,
    )
    .unwrap();
    assert_eq!(
        req.request,
        Request::SetWallpaperAudio {
            wallpaper: "/w/x".into(),
            volume: Some(0.3),
            muted: None
        }
    );
}

#[test]
fn wallpaper_keys() {
    use std::path::Path;
    let ws = Path::new("/home/me/.local/share/Steam/steamapps/workshop/content/431960");
    assert_eq!(wallpaper_key(&ws.join("3018340632")), "3018340632");
    assert_eq!(
        wallpaper_key(&ws.join("3018340632/project.json")),
        "3018340632"
    );
    // Same id from another Steam library.
    assert_eq!(
        wallpaper_key(Path::new(
            "/mnt/games/SteamLibrary/steamapps/workshop/content/431960/3018340632"
        )),
        "3018340632"
    );
    // Not a Workshop item: the path itself.
    assert_eq!(
        wallpaper_key(Path::new("/w/my wallpaper")),
        "/w/my wallpaper"
    );
    assert_eq!(
        wallpaper_key(Path::new("/other/content/12345/678")),
        "/other/content/12345/678"
    );
    assert_eq!(
        wallpaper_key(&ws.join("draft")),
        ws.join("draft").to_string_lossy()
    );

    let level = WallpaperAudio::default();
    assert!(level.is_default());
    assert_eq!(level.gain(), 1.0);
    assert_eq!(
        WallpaperAudio {
            volume: 0.8,
            muted: true
        }
        .gain(),
        0.0
    );
    assert_eq!(
        WallpaperAudio {
            volume: f32::NAN,
            muted: false
        }
        .gain(),
        0.0
    );
}

#[test]
fn property_values_use_natural_json() {
    let v: PropertyValue = serde_json::from_str("[1, 0.5, 0]").unwrap();
    assert_eq!(v, PropertyValue::Color([1.0, 0.5, 0.0]));
    let v: PropertyValue = serde_json::from_str("3").unwrap();
    assert_eq!(v, PropertyValue::Number(3.0));
    let v: PropertyValue = serde_json::from_str("false").unwrap();
    assert_eq!(v, PropertyValue::Bool(false));
    let v: PropertyValue = serde_json::from_str("\"/home/a.png\"").unwrap();
    assert_eq!(v, PropertyValue::Text("/home/a.png".into()));
}

#[test]
fn event_topics() {
    assert_eq!(Event::Stats(Stats::default()).topic(), Some(Topic::Stats));
    assert_eq!(Event::ShuttingDown.topic(), None);
    let bits: Vec<u8> = Topic::ALL.iter().map(|t| t.bit()).collect();
    assert_eq!(bits, [1, 2, 4, 8, 16]);
}

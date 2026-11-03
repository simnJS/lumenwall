//! Property scripts (M1): loading, update, return values, events, promises
//! (spec 06 §4, §8; WE IComponent).

mod common;

use common::*;
use lumen_script::*;

fn alpha(h: &lumen_script::mock::MockHost, layer: LayerId) -> Option<Value> {
    h.member(ObjectRef::Layer(layer), "alpha").cloned()
}

#[test]
fn return_values_follow_spec_4_2() {
    let cases: [(&str, Option<Value>); 13] = [
        ("return 3;", Some(Value::Int(3))),
        ("return 0.5;", Some(Value::Float(0.5))),
        ("return true;", Some(Value::Bool(true))),
        ("return 'txt';", Some(Value::String("txt".into()))),
        ("return null;", Some(Value::Null)),
        // we-doc: returning nothing leaves the value untouched.
        ("return undefined;", Some(Value::Float(9.0))),
        ("", Some(Value::Float(9.0))),
        ("return { x: 1, y: 2 };", Some(Value::Vec2([1.0, 2.0]))),
        (
            "return { x: 1, y: 2, z: 3 };",
            Some(Value::Vec3([1.0, 2.0, 3.0])),
        ),
        (
            "return { x: 1, y: 2, z: 3, w: 4 };",
            Some(Value::Vec4([1.0, 2.0, 3.0, 4.0])),
        ),
        (
            "return new Vec4(1, 2, 3, 4);",
            Some(Value::Vec4([1.0, 2.0, 3.0, 4.0])),
        ),
        // Réf: an object without numeric x/y is logged and ignored.
        ("return { a: 1 };", Some(Value::Float(9.0))),
        ("return Symbol('s');", Some(Value::Float(9.0))),
    ];
    for (body, expected) in cases {
        let (mut e, mut h) = setup();
        h.set_member(ObjectRef::Layer(1), "alpha", Value::Float(9.0));
        let id = load(
            &mut e,
            &mut h,
            1,
            "alpha",
            &format!("export function update(value) {{ {body} }}"),
        )
        .unwrap();
        // lumen: nothing runs before the first tick.
        assert_eq!(alpha(&h, 1), Some(Value::Float(9.0)), "{body}");
        e.tick(&mut h);
        assert!(e.is_active(id), "{body}");
        assert_eq!(alpha(&h, 1), expected, "{body}");
    }
    let (mut e, mut h) = setup();
    h.set_member(ObjectRef::Layer(1), "alpha", Value::Float(9.0));
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "export function update() { return { a: 1 }; }",
    )
    .unwrap();
    e.tick(&mut h);
    assert_eq!(h.logged("no numeric x/y"), 1);
}

#[test]
fn vector_value_is_a_live_view_and_a_float_converts() {
    let (mut e, mut h) = setup();
    h.set_member(
        ObjectRef::Layer(1),
        "origin",
        Value::Vec3([0.0, 100.0, 0.0]),
    );
    load(
        &mut e,
        &mut h,
        1,
        "origin",
        "export function update(value) { shared.same = shared.prev === value; shared.prev = value; value.y += 10; return value; }",
    )
    .unwrap();
    e.tick(&mut h);
    assert_eq!(
        h.member(ObjectRef::Layer(1), "origin"),
        Some(&Value::Vec3([0.0, 110.0, 0.0]))
    );
    e.tick(&mut h);
    assert_eq!(
        h.member(ObjectRef::Layer(1), "origin"),
        Some(&Value::Vec3([0.0, 120.0, 0.0]))
    );
    // The view object is reused from frame to frame (no allocation per frame).
    assert_eq!(eval(&mut e, &mut h, "shared.same"), Value::Bool(true));
    // Returning a number for a vector property stores a Float (spec 06 §2.2 projections apply).
    let (mut e, mut h) = setup();
    h.set_member(ObjectRef::Layer(1), "scale", Value::Vec3([1.0, 1.0, 1.0]));
    load(
        &mut e,
        &mut h,
        1,
        "scale",
        "export function update(value) { return 2; }",
    )
    .unwrap();
    e.tick(&mut h);
    assert_eq!(
        h.member(ObjectRef::Layer(1), "scale").map(Value::vec3),
        Some([2.0, 2.0, 2.0])
    );
}

#[test]
fn init_at_load_then_update_every_tick() {
    let (mut e, mut h) = setup();
    h.set_member(ObjectRef::Layer(1), "alpha", Value::Int(1));
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.calls = [];
         export function init(v) { shared.calls.push('init:' + v); return v + 1; }
         export function update(v) { shared.calls.push('update:' + v); return v * 2; }",
    )
    .unwrap();
    // lumen: init at load, first update on the first tick.
    assert_eq!(alpha(&h, 1), Some(Value::Int(2)));
    e.tick(&mut h);
    assert_eq!(alpha(&h, 1), Some(Value::Int(4)));
    e.tick(&mut h);
    assert_eq!(alpha(&h, 1), Some(Value::Int(8)));
    assert_eq!(
        string(&eval(&mut e, &mut h, "shared.calls.join()")),
        "init:1,update:2,update:4"
    );
}

#[test]
fn updates_run_in_key_order_with_their_own_this_layer() {
    let (mut e, mut h) = setup();
    for id in [12, 3, 10, 2] {
        h.add_layer(id, &format!("l{id}"));
    }
    let src = |key: &str, layer: i32| {
        format!(
            "export function update() {{ (shared.order = shared.order || []).push('{key}:' + (thisLayer === thisScene.getLayer({layer}))); }}"
        )
    };
    load(&mut e, &mut h, 2, "scale", &src("scale_2", 2)).unwrap();
    load(&mut e, &mut h, 10, "scale", &src("scale_10", 10)).unwrap();
    load(&mut e, &mut h, 12, "alpha", &src("alpha_12", 12)).unwrap();
    load(&mut e, &mut h, 3, "origin", &src("origin_3", 3)).unwrap();
    eval(&mut e, &mut h, "shared.order = []");
    e.tick(&mut h);
    assert_eq!(
        string(&eval(&mut e, &mut h, "shared.order.join()")),
        "alpha_12:true,origin_3:true,scale_10:true,scale_2:true"
    );
    assert_eq!(e.stats().per_frame, 4);
}

#[test]
fn duplicate_keys_and_load_errors() {
    let (mut e, mut h) = setup();
    h.set_member(ObjectRef::Layer(1), "alpha", Value::Float(0.5));
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "export function update() { return 1; }",
    )
    .unwrap();
    assert_eq!(
        load(
            &mut e,
            &mut h,
            1,
            "alpha",
            "export function update() { return 2; }"
        ),
        Err(ScriptError::Duplicate("alpha_1".into()))
    );
    e.tick(&mut h);
    assert_eq!(alpha(&h, 1), Some(Value::Int(1)));

    h.set_member(ObjectRef::Layer(2), "alpha", Value::Float(0.5));
    let err = load(
        &mut e,
        &mut h,
        2,
        "alpha",
        "export function update( { return 1; }",
    )
    .unwrap_err();
    assert!(
        matches!(&err, ScriptError::Failed { key, message } if key == "alpha_2" && message.contains("SyntaxError")),
        "{err:?}"
    );
    let err = load(
        &mut e,
        &mut h,
        2,
        "color",
        "throw new Error('boom'); export function update() { return 1; }",
    )
    .unwrap_err();
    assert!(
        matches!(&err, ScriptError::Failed { message, .. } if message.contains("boom")),
        "{err:?}"
    );
    assert_eq!(alpha(&h, 2), Some(Value::Float(0.5)));
    assert_eq!(h.logged("disabled (load)"), 2);
}

#[test]
fn update_errors_disable_after_three_in_a_row() {
    let (mut e, mut h) = setup();
    for layer in 1..=3 {
        h.set_member(ObjectRef::Layer(layer), "alpha", Value::Float(0.5));
    }
    // Fails from the second call on.
    let bad = load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "let n = 0; export function update(v) { if (++n > 1) { null.boom; } return 0.25; }",
    )
    .unwrap();
    // Fails every other call: never three in a row.
    let flaky = load(
        &mut e,
        &mut h,
        2,
        "alpha",
        "let n = 0; export function update(v) { if (++n % 2 === 0) { throw new Error('flaky'); } return v + 1; }",
    )
    .unwrap();
    let good = load(
        &mut e,
        &mut h,
        3,
        "alpha",
        "export function update(v) { return v + 1; }",
    )
    .unwrap();
    e.tick(&mut h);
    assert_eq!(alpha(&h, 1), Some(Value::Float(0.25)));
    h.set_member(ObjectRef::Layer(1), "alpha", Value::Float(0.5));
    // Errors 1 and 2 are logged, the script keeps running.
    e.tick(&mut h);
    e.tick(&mut h);
    assert!(e.is_active(bad));
    assert_eq!(h.logged("script 'alpha_1': update error (1/3)"), 1);
    assert_eq!(h.logged("script 'alpha_1': update error (2/3)"), 1);
    // The third consecutive error disables it.
    e.tick(&mut h);
    assert!(!e.is_active(bad));
    assert_eq!(h.logged("script 'alpha_1' disabled (update)"), 1);
    assert_eq!(h.logged("TypeError"), 3);
    assert_eq!(
        alpha(&h, 1),
        Some(Value::Float(0.5)),
        "value untouched by failing calls"
    );
    for _ in 0..6 {
        e.tick(&mut h);
    }
    assert!(e.is_active(flaky));
    assert!(e.is_active(good));
    assert_eq!(h.logged("flaky"), 5);
    assert_eq!(alpha(&h, 3), Some(Value::Float(10.5)));
    assert_eq!(e.stats().per_frame, 2);
}

#[test]
fn error_logs_are_capped_per_script() {
    let (mut e, mut h) = setup();
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "let n = 0; export function update() { if (++n % 2) throw new Error('odd'); }",
    )
    .unwrap();
    for _ in 0..40 {
        e.tick(&mut h);
    }
    assert_eq!(h.logged("odd"), 10);
    assert_eq!(h.logged("further update errors are not logged"), 1);
}

#[test]
fn no_scripts_means_no_work() {
    let (mut e, mut h) = setup();
    assert!(!e.has_work());
    e.tick(&mut h);
    assert_eq!(e.stats().per_frame, 0);
    // A script without update does not add per-frame work.
    load(&mut e, &mut h, 1, "alpha", "shared.loaded = true;").unwrap();
    assert!(!e.has_work());
}

#[test]
fn promise_jobs_are_drained_every_frame() {
    let (mut e, mut h) = setup();
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.ticks = 0;
         export function update() { Promise.resolve().then(() => { shared.ticks++; }); }",
    )
    .unwrap();
    assert_eq!(eval(&mut e, &mut h, "shared.ticks"), Value::Int(0));
    e.tick(&mut h);
    assert_eq!(eval(&mut e, &mut h, "shared.ticks"), Value::Int(1));
    e.tick(&mut h);
    assert_eq!(eval(&mut e, &mut h, "shared.ticks"), Value::Int(2));
    // Async continuations and top-level await complete within the call.
    load(&mut e, &mut h, 2, "alpha", "shared.tla = await Promise.resolve(5); export async function init() { await null; shared.after = 1; }").unwrap();
    assert_eq!(
        eval(&mut e, &mut h, "[shared.tla, shared.after]"),
        Value::Array(vec![Value::Int(5), Value::Int(1)])
    );
    let err = load(&mut e, &mut h, 3, "alpha", "await new Promise(() => {});").unwrap_err();
    assert!(
        matches!(&err, ScriptError::Failed { message, .. } if message.contains("top-level await")),
        "{err:?}"
    );
}

#[test]
fn resize_user_properties_and_settings() {
    let config = ScriptConfig {
        user_properties: vec![
            ("speed".into(), Value::Float(1.0)),
            ("on".into(), Value::Bool(true)),
        ],
        general_settings: vec![("language".into(), Value::String("fr-fr".into()))],
        ..ScriptConfig::default()
    };
    let (mut e, mut h) = setup_with(config);
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.props = [];
         export function resizeScreen(size) { shared.size = size; }
         export function applyUserProperties(p) { shared.props.push(Object.keys(p).sort().join('+')); if (p.hasOwnProperty('speed')) shared.speed = p.speed; }
         export function applyGeneralSettings(s) { shared.lang = s.language; }",
    )
    .unwrap();
    assert_eq!(string(&eval(&mut e, &mut h, "shared.lang")), "fr-fr");
    e.resize_screen(&mut h, [2560.0, 1440.0]);
    assert_vec(&eval(&mut e, &mut h, "shared.size"), &[2560.0, 1440.0]);
    e.apply_user_properties(&mut h, &[("speed".into(), Value::Float(3.0))]);
    assert_eq!(
        string(&eval(&mut e, &mut h, "shared.props.join()")),
        "on+speed,speed"
    );
    assert_eq!(num(&eval(&mut e, &mut h, "shared.speed")), 3.0);
    e.apply_general_settings(
        &mut h,
        &[("language".into(), Value::String("en-us".into()))],
    );
    assert_eq!(string(&eval(&mut e, &mut h, "shared.lang")), "en-us");
}

#[test]
fn cursor_events_target_layers() {
    let (mut e, mut h) = setup();
    let src = |n: i32| {
        format!(
            "export function cursorClick(ev) {{ shared.click = [{n}, ev.worldPosition.x, ev.localPosition.y, ev.hitBox]; }}
             export function cursorMove(ev) {{ (shared.moves = shared.moves || []).push({n}); }}
             export function cursorEnter() {{ shared.enter = {n}; }}
             export function cursorLeave() {{ shared.leave = {n}; }}
             export function cursorDown() {{ shared.down = {n}; }}
             export function cursorUp() {{ shared.up = {n}; }}"
        )
    };
    load(&mut e, &mut h, 1, "alpha", &src(1)).unwrap();
    load(&mut e, &mut h, 2, "alpha", &src(2)).unwrap();
    let ev = CursorEvent {
        world_position: [5.0, 6.0, 0.0],
        local_position: [0.5, 0.25, 0.0],
        hit_box: Some("head".into()),
    };
    e.cursor_event(&mut h, CursorEventKind::Click, Some(2), &ev);
    assert_eq!(
        eval(&mut e, &mut h, "shared.click"),
        Value::Array(vec![
            Value::Int(2),
            Value::Int(5),
            Value::Float(0.25),
            Value::String("head".into())
        ])
    );
    e.cursor_event(&mut h, CursorEventKind::Move, None, &CursorEvent::default());
    assert_eq!(string(&eval(&mut e, &mut h, "shared.moves.join()")), "1,2");
    for kind in [
        CursorEventKind::Enter,
        CursorEventKind::Leave,
        CursorEventKind::Down,
        CursorEventKind::Up,
    ] {
        e.cursor_event(&mut h, kind, Some(1), &ev);
    }
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "[shared.enter, shared.leave, shared.down, shared.up]"
        ),
        Value::Array(vec![Value::Int(1); 4])
    );
}

#[test]
fn media_events() {
    let (mut e, mut h) = setup();
    // Keys alpha_1 < alpha_2: script 1 runs first and mutates the shared event objects.
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.seq = [];
         export function mediaPropertiesChanged(ev) { shared.seq.push('p1'); ev.title = 'changed'; }
         export function mediaPlaybackChanged(ev) { shared.seq.push('b1:' + ev.state); }
         export function mediaTimelineChanged(ev) { shared.seq.push('t1:' + ev.position + '/' + ev.duration); }
         export function mediaThumbnailChanged(ev) { shared.seq.push('h1:' + ev.hasThumbnail + ':' + ev.primaryColor.x.toFixed(2)); }
         export function mediaStatusChanged(ev) { shared.status = ev.enabled; }",
    )
    .unwrap();
    load(
        &mut e,
        &mut h,
        2,
        "alpha",
        "export function mediaPropertiesChanged(ev) { shared.seq.push('p2:' + ev.title + ':' + ev.artist + ':' + ev.albumArtist); }
         export function mediaThumbnailChanged(ev) { shared.seq.push('h2:' + (ev.textColor instanceof Vec3)); }",
    )
    .unwrap();
    let media = MediaSnapshot {
        properties: MediaProperties {
            title: "Song".into(),
            artist: "Band".into(),
            album_artist: "Band".into(),
            ..MediaProperties::default()
        },
        state: PlaybackState::Playing,
        position: 12.5,
        duration: 200.0,
        thumbnail: MediaThumbnail::default(),
    };
    e.media_changed(&mut h, &media);
    assert_eq!(
        string(&eval(&mut e, &mut h, "shared.seq.join()")),
        "p1,b1:1,t1:12.5/200,h1:false:0.12,p2:changed:Band:Band,h2:true"
    );
    eval(&mut e, &mut h, "shared.seq = []");
    e.media_playback_changed(&mut h, PlaybackState::Paused);
    e.media_timeline_changed(&mut h, 1.0, 2.0);
    e.media_properties_changed(&mut h, &MediaProperties::default());
    e.media_thumbnail_changed(
        &mut h,
        &MediaThumbnail {
            has_thumbnail: true,
            ..MediaThumbnail::default()
        },
    );
    e.media_status_changed(&mut h, false);
    assert_eq!(
        string(&eval(&mut e, &mut h, "shared.seq.join()")),
        "b1:2,t1:1/2,p1,p2:changed::,h1:true:0.12,h2:true"
    );
    assert_eq!(eval(&mut e, &mut h, "shared.status"), Value::Bool(false));
}

#[test]
fn destroy_on_layer_removal_and_shutdown() {
    let (mut e, mut h) = setup();
    let a = load(&mut e, &mut h, 1, "alpha", "export function update() {} export function destroy() { (shared.d = shared.d || []).push(1); }").unwrap();
    let b = load(
        &mut e,
        &mut h,
        2,
        "alpha",
        "export function destroy() { (shared.d = shared.d || []).push(2); }",
    )
    .unwrap();
    eval(&mut e, &mut h, "shared.l1 = thisScene.getLayer(1)");
    e.remove_layer(&mut h, 1);
    assert!(!e.is_active(a));
    assert!(e.is_active(b));
    assert!(!e.has_work());
    assert_eq!(
        eval(&mut e, &mut h, "shared.l1 === thisScene.getLayer(1)"),
        Value::Bool(false)
    );
    // The key can be reused once the layer is gone.
    load(&mut e, &mut h, 1, "alpha", "shared.reloaded = true;").unwrap();
    e.shutdown(&mut h);
    assert!(!e.is_active(b));
    assert_eq!(string(&eval(&mut e, &mut h, "shared.d.join()")), "1,2");
    assert_eq!(e.stats().active, 0);
}

#[test]
fn host_callbacks() {
    let (mut e, mut h) = setup();
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "const video = thisLayer.getVideoTexture();
         shared.ended = [];
         thisScene.getLayer(1).play();
         engine.registerAsset('sounds/a.mp3', false);
         export function init() { shared.cb = (n) => shared.ended.push(n); thisScene.createLayer({ onEnd: shared.cb }); }",
    )
    .unwrap();
    let (_, _, args) = h
        .calls
        .iter()
        .find(|(_, m, _)| m == "createLayer")
        .unwrap()
        .clone();
    let Value::Record(fields) = &args[0] else {
        panic!("{args:?}")
    };
    let Value::Callback(id) = fields[0].1 else {
        panic!("{fields:?}")
    };
    e.invoke_callback(&mut h, id, &[Value::Int(3)]);
    e.invoke_callback(&mut h, id, &[Value::String("x".into())]);
    assert_eq!(string(&eval(&mut e, &mut h, "shared.ended.join()")), "3,x");
    e.release_callback(id);
    e.invoke_callback(&mut h, id, &[Value::Int(4)]);
    assert_eq!(string(&eval(&mut e, &mut h, "shared.ended.join()")), "3,x");
}

#[test]
fn audio_buffers() {
    let (mut e, mut h) = setup();
    h.audio = Some((vec![0.5, 1.0], vec![1.5, 0.0]));
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.ab = engine.registerAudioBuffers(engine.AUDIO_RESOLUTION_16);
         shared.same = shared.ab === engine.registerAudioBuffers(16);
         try { engine.registerAudioBuffers(10); } catch (e) { shared.err = e.constructor.name; }",
    )
    .unwrap();
    assert!(e.has_work());
    e.tick(&mut h);
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "const a = shared.ab; [a.left.length, a.left[0], a.left[1], a.left[2], a.right[0], a.average[0], a.average[1], a.left instanceof Float32Array]"
        ),
        Value::Array(vec![
            Value::Int(16),
            Value::Float(0.5),
            Value::Float(1.0),
            Value::Float(0.5),
            Value::Float(1.5),
            Value::Float(1.0),
            Value::Float(0.5),
            Value::Bool(true),
        ])
    );
    assert_eq!(
        eval(&mut e, &mut h, "[shared.same, shared.err]"),
        Value::Array(vec![Value::Bool(true), Value::String("TypeError".into())])
    );
    h.audio = None;
    e.tick(&mut h);
    assert_eq!(num(&eval(&mut e, &mut h, "shared.ab.average[0]")), 0.0);
}

#[test]
fn init_value_is_a_copy_not_a_live_view() {
    // Scripts keep the `init` value as their default and derive the result from it (the
    // Porsche wallpapers mirror with `scale.x = -initial.x`); a live view would flip every frame.
    let (mut e, mut h) = setup();
    h.set_member(ObjectRef::Layer(1), "scale", Value::Vec3([1.0, 1.0, 1.0]));
    let id = load(
        &mut e,
        &mut h,
        1,
        "scale",
        "let initial;
         export function init(value) { initial = value; }
         export function update(value) { return new Vec3(-initial.x, initial.y, initial.z); }",
    )
    .unwrap();
    for _ in 0..4 {
        e.tick(&mut h);
        assert!(e.is_active(id));
        assert_eq!(
            h.member(ObjectRef::Layer(1), "scale").cloned(),
            Some(Value::Vec3([-1.0, 1.0, 1.0]))
        );
    }
}

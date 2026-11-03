//! Global objects: engine, input, console, shared, thisScene, layers, localStorage,
//! createScriptProperties, Mat3/Mat4 (spec 06 §3, §6; WE lib.sceneScript.d.ts).

mod common;

use common::*;
use lumenwall_script::mock::MockHost;
use lumenwall_script::*;

fn throws(e: &mut ScriptEngine, h: &mut MockHost, expr: &str) -> String {
    let r = strict(
        e,
        h,
        &format!("try {{ {expr}; return 'none'; }} catch (err) {{ return err.constructor.name; }}"),
    );
    string(&r).to_owned()
}

fn b(v: bool) -> Value {
    Value::Bool(v)
}

#[test]
fn engine_object() {
    let config = ScriptConfig {
        user_properties: vec![
            ("speed".into(), Value::Float(2.5)),
            ("tint".into(), Value::Vec3([1.0, 0.5, 0.0])),
        ],
        ..ScriptConfig::default()
    };
    let (mut e, mut h) = setup_with(config);
    h.clock = Clock {
        runtime: 12.5,
        frametime: 0.016,
        time_of_day: 0.75,
    };
    h.display = Display {
        screen_resolution: [1080.0, 1920.0],
        canvas_size: [1280.0, 720.0],
        screensaver: false,
        editor: true,
    };
    assert_eq!(num(&eval(&mut e, &mut h, "engine.runtime")), 12.5);
    assert_eq!(
        num(&eval(&mut e, &mut h, "engine.frametime")),
        f64::from(0.016f32)
    );
    assert_eq!(num(&eval(&mut e, &mut h, "engine.timeOfDay")), 0.75);
    assert_eq!(throws(&mut e, &mut h, "engine.runtime = 1"), "TypeError");
    assert_eq!(throws(&mut e, &mut h, "engine = 1"), "TypeError");
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "[engine.AUDIO_RESOLUTION_16, engine.AUDIO_RESOLUTION_32, engine.AUDIO_RESOLUTION_64]"
        ),
        Value::Array(vec![Value::Int(16), Value::Int(32), Value::Int(64)])
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "Object.getPrototypeOf(engine) === null && typeof engine.toString"
        ),
        Value::String("undefined".into())
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "const d = Object.getOwnPropertyDescriptor(globalThis, 'engine'); [d.writable, d.enumerable, d.configurable]"
        ),
        Value::Array(vec![b(false), b(true), b(false)])
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "[engine.isPortrait(), engine.isLandscape(), engine.isDesktopDevice(), engine.isMobileDevice(), engine.isWallpaper(), engine.isScreensaver(), engine.isRunningInEditor()]"
        ),
        Value::Array(vec![
            b(true),
            b(false),
            b(true),
            b(false),
            b(true),
            b(false),
            b(true)
        ])
    );
    assert_vec(
        &eval(&mut e, &mut h, "engine.screenResolution"),
        &[1080.0, 1920.0],
    );
    assert_vec(&eval(&mut e, &mut h, "engine.canvasSize"), &[1280.0, 720.0]);
    assert_eq!(
        eval(&mut e, &mut h, "engine.openUserShortcut('site')"),
        b(true)
    );
    assert_eq!(h.shortcuts, vec!["site".to_owned()]);
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "typeof engine.registerAsset('models/a.json', true)"
        ),
        Value::String("object".into())
    );
    assert_eq!(h.assets, vec![("models/a.json".to_owned(), true)]);
    assert_eq!(
        num(&eval(&mut e, &mut h, "engine.userProperties.speed")),
        2.5
    );
    assert_eq!(
        eval(&mut e, &mut h, "engine.userProperties.tint instanceof Vec3"),
        b(true)
    );
    // Live update of engine.userProperties.
    e.apply_user_properties(&mut h, &[("speed".into(), Value::Int(7))]);
    assert_eq!(
        eval(&mut e, &mut h, "engine.userProperties.speed"),
        Value::Int(7)
    );
}

#[test]
fn input_object() {
    let (mut e, mut h) = setup();
    h.cursor = Cursor {
        screen_position: [100.0, 200.0],
        world_position: [1.0, 2.0, 3.0],
        left_down: true,
    };
    assert_vec(
        &eval(&mut e, &mut h, "input.cursorScreenPosition"),
        &[100.0, 200.0],
    );
    assert_vec(
        &eval(&mut e, &mut h, "input.cursorWorldPosition"),
        &[1.0, 2.0, 3.0],
    );
    assert_eq!(eval(&mut e, &mut h, "input.cursorLeftDown"), b(true));
    // Each read is a fresh copy.
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "input.cursorScreenPosition === input.cursorScreenPosition"
        ),
        b(false)
    );
    assert_eq!(
        throws(&mut e, &mut h, "input.cursorLeftDown = false"),
        "TypeError"
    );
}

#[test]
fn console_object() {
    let (mut e, mut h) = setup();
    eval(
        &mut e,
        &mut h,
        "console.log('a', 1, true, new Vec2(1, 2)); console.error('bad'); console.log()",
    );
    assert!(
        h.logs
            .contains(&(LogLevel::Info, "a1true1.000000 2.000000".into()))
    );
    assert!(h.logs.contains(&(LogLevel::Error, "bad".into())));
    assert_eq!(h.logs.len(), 2);
    eval(&mut e, &mut h, "console.log('x', engine)");
    assert!(h.logs.contains(&(LogLevel::Info, "x[object]".into())));
    assert_eq!(
        eval(&mut e, &mut h, "typeof console.warn"),
        Value::String("undefined".into())
    );
}

#[test]
fn shared_and_constants() {
    let (mut e, mut h) = setup();
    load(&mut e, &mut h, 1, "alpha", "shared.factor = 4;").unwrap();
    load(
        &mut e,
        &mut h,
        2,
        "alpha",
        "export function update() { return shared.factor * 2; }",
    )
    .unwrap();
    // lumenwall: the first update runs on the first tick, not at load.
    assert_eq!(h.member(ObjectRef::Layer(2), "alpha"), None);
    e.tick(&mut h);
    assert_eq!(h.member(ObjectRef::Layer(2), "alpha"), Some(&Value::Int(8)));
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "[MediaPlaybackEvent.PLAYBACK_STOPPED, MediaPlaybackEvent.PLAYBACK_PLAYING, MediaPlaybackEvent.PLAYBACK_PAUSED]"
        ),
        Value::Array(vec![Value::Int(0), Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        eval(&mut e, &mut h, "typeof renderContext"),
        Value::String("object".into())
    );
    assert_eq!(throws(&mut e, &mut h, "shared = {}"), "TypeError");
}

#[test]
fn scene_object() {
    let (mut e, mut h) = setup();
    for (k, v) in [
        ("bloom", Value::Bool(true)),
        ("bloomstrength", Value::Float(2.7)),
        ("bloomthreshold", Value::Float(0.9)),
        ("ambientcolor", Value::Vec3([0.1, 0.2, 0.3])),
        ("clearcolor", Value::Vec3([1.0, 0.0, 0.0])),
        ("fov", Value::Float(50.0)),
        ("camerashake", Value::Bool(false)),
        ("cameraparallaxamount", Value::Float(0.5)),
    ] {
        h.set_member(ObjectRef::Scene, k, v);
    }
    // parity: clearenabled reads bloom, skylightcolor reads the ambient color, strengths truncated.
    assert_eq!(eval(&mut e, &mut h, "thisScene.clearenabled"), b(true));
    assert_vec(
        &eval(&mut e, &mut h, "thisScene.skylightcolor"),
        &[0.1, 0.2, 0.3],
    );
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.bloomstrength"),
        Value::Int(2)
    );
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.bloomthreshold"),
        Value::Int(0)
    );
    assert_vec(
        &eval(&mut e, &mut h, "thisScene.clearcolor"),
        &[1.0, 0.0, 0.0],
    );
    assert_eq!(num(&eval(&mut e, &mut h, "thisScene.fov")), 50.0);
    assert_eq!(eval(&mut e, &mut h, "thisScene.camerashake"), b(false));
    assert_eq!(
        num(&eval(&mut e, &mut h, "thisScene.cameraparallaxamount")),
        0.5
    );
    assert_eq!(eval(&mut e, &mut h, "thisScene.nearz"), Value::Undefined);
    // Color views are live and write through.
    eval(&mut e, &mut h, "thisScene.ambientcolor.x = 0.5");
    assert_eq!(
        h.member(ObjectRef::Scene, "ambientcolor"),
        Some(&Value::Vec3([0.5, 0.2, 0.3]))
    );
    // we-doc: members are writable, under their true names.
    eval(
        &mut e,
        &mut h,
        "thisScene.fov = 70; thisScene.clearenabled = false",
    );
    assert_eq!(h.member(ObjectRef::Scene, "fov"), Some(&Value::Int(70)));
    assert_eq!(h.member(ObjectRef::Scene, "clearenabled"), Some(&b(false)));
    assert_eq!(
        eval(&mut e, &mut h, "Object.getPrototypeOf(thisScene) === null"),
        b(true)
    );
}

#[test]
fn scene_layer_lookup() {
    let (mut e, mut h) = setup();
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.getLayer(1) === thisScene.getLayer('first')"
        ),
        b(true)
    );
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.getLayer(99)"),
        Value::Undefined
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.getLayer(2.9) === thisScene.getLayer(2)"
        ),
        b(true)
    );
    // lumenwall: an unknown name gives null instead of throwing.
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.getLayer('nope')"),
        Value::Null
    );
    assert_eq!(
        throws(&mut e, &mut h, "thisScene.getLayer('nope').name"),
        "TypeError"
    );
    assert_eq!(throws(&mut e, &mut h, "thisScene.getLayer()"), "TypeError");
    assert_eq!(
        throws(&mut e, &mut h, "thisScene.getLayer(1, 2)"),
        "TypeError"
    );
    assert_eq!(
        throws(&mut e, &mut h, "thisScene.getLayer(true)"),
        "TypeError"
    );
    // Non-scriptable objects are skipped by name lookup.
    h.layers.insert(
        0,
        lumenwall_script::mock::MockLayer {
            id: 9,
            name: "second".into(),
            scriptable: false,
        },
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.getLayer('second') === thisScene.getLayer(2)"
        ),
        b(true)
    );
    h.layers.remove(0);
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.getLayerByID('2') === thisScene.getLayer(2)"
        ),
        b(true)
    );
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.getLayerByID('x')"),
        Value::Undefined
    );
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.getLayerCount()"),
        Value::Int(3)
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "const l = thisScene.enumerateLayers(); [l.length, l[1] === thisScene.getLayer(2)]"
        ),
        Value::Array(vec![Value::Int(3), b(true)])
    );
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.getLayerIndex('second')"),
        Value::Int(1)
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.getLayerIndex(thisScene.getLayer(3))"
        ),
        Value::Int(2)
    );
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.getLayerIndex('nope')"),
        Value::Int(-1)
    );
}

#[test]
fn scene_host_methods() {
    let (mut e, mut h) = setup();
    h.call_results.insert(
        (ObjectRef::Scene, "createLayer".into()),
        Value::Object(ObjectRef::Layer(2)),
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.createLayer({ name: 'n', origin: new Vec3(1, 2, 3) }) === thisScene.getLayer(2)"
        ),
        b(true)
    );
    let (obj, method, args) = h.calls.last().unwrap().clone();
    assert_eq!((obj, method.as_str()), (ObjectRef::Scene, "createLayer"));
    assert_eq!(
        args,
        vec![Value::Record(vec![
            ("name".into(), Value::String("n".into())),
            ("origin".into(), Value::Vec3([1.0, 2.0, 3.0])),
        ])]
    );
    eval(
        &mut e,
        &mut h,
        "thisScene.sortLayer(thisScene.getLayer(1), 2)",
    );
    assert_eq!(
        h.calls.last().unwrap().2,
        vec![Value::Object(ObjectRef::Layer(1)), Value::Int(2)]
    );
    // Neutral results without host support.
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.destroyLayer('first')"),
        b(false)
    );
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.getCameraTransforms()"),
        Value::Undefined
    );
    h.call_results.insert(
        (ObjectRef::Scene, "getCameraTransforms".into()),
        Value::Record(vec![
            ("eye".into(), Value::Vec3([0.0, 0.0, 5.0])),
            ("zoom".into(), Value::Float(1.0)),
        ]),
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.getCameraTransforms().eye instanceof Vec3"
        ),
        b(true)
    );
}

#[test]
fn layer_objects() {
    let (mut e, mut h) = setup();
    let l = ObjectRef::Layer(1);
    h.set_member(l, "origin", Value::Vec3([1.0, 2.0, 3.0]))
        .set_member(l, "alpha", Value::Float(0.5))
        .set_member(l, "pointsize", Value::Float(12.0));
    load(
        &mut e,
        &mut h,
        1,
        "visible",
        "shared.l = thisLayer; shared.same = thisLayer === thisScene.getLayer(1);",
    )
    .unwrap();
    assert_eq!(eval(&mut e, &mut h, "shared.same"), b(true));
    assert_eq!(num(&eval(&mut e, &mut h, "shared.l.alpha")), 0.5);
    assert_eq!(
        num(&eval(
            &mut e,
            &mut h,
            "shared.l.pointSize + shared.l.pointsize"
        )),
        24.0
    );
    assert_eq!(
        eval(&mut e, &mut h, "shared.l.name"),
        Value::String("first".into())
    );
    assert_eq!(eval(&mut e, &mut h, "shared.l.size"), Value::Undefined);
    eval(&mut e, &mut h, "shared.l.origin.y = 5");
    assert_eq!(h.member(l, "origin"), Some(&Value::Vec3([1.0, 5.0, 3.0])));
    eval(
        &mut e,
        &mut h,
        "shared.l.alpha = 0.25; shared.l.scale = new Vec3(2, 2, 2); shared.l.size = new Vec2(1, 1)",
    );
    assert_eq!(h.member(l, "alpha"), Some(&Value::Float(0.25)));
    assert_eq!(h.member(l, "scale"), Some(&Value::Vec3([2.0, 2.0, 2.0])));
    assert_eq!(h.member(l, "size"), None, "read-only member ignored");
    assert_eq!(
        eval(&mut e, &mut h, "typeof shared.l.toString"),
        Value::String("undefined".into())
    );
    // Unsupported methods give neutral values.
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "[shared.l.getEffectCount(), shared.l.isPlaying(), shared.l.getChildren().length, shared.l.getBoneIndex('x'), shared.l.getEffect('x')]"
        ),
        Value::Array(vec![
            Value::Int(0),
            b(false),
            Value::Int(0),
            Value::Int(-1),
            Value::Undefined
        ])
    );
    eval(
        &mut e,
        &mut h,
        "shared.l.play(); shared.l.setParent(thisScene.getLayer(2), true)",
    );
    assert!(h.calls.contains(&(l, "play".into(), vec![])));
    assert!(h.calls.contains(&(
        l,
        "setParent".into(),
        vec![Value::Object(ObjectRef::Layer(2)), b(true)]
    )));
}

#[test]
fn layer_sub_objects() {
    let (mut e, mut h) = setup();
    let l = ObjectRef::Layer(1);
    let fx = ObjectRef::Effect {
        layer: 1,
        effect: 0,
    };
    h.call_results
        .insert((l, "getEffect".into()), Value::Object(fx));
    h.set_member(fx, "visible", Value::Bool(true));
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "shared.fx = thisScene.getLayer(1).getEffect('blur'); shared.fx.visible"
        ),
        b(true)
    );
    eval(
        &mut e,
        &mut h,
        "shared.fx.visible = false; shared.fx.setMaterialProperty('u', new Vec3(1, 2, 3))",
    );
    assert_eq!(h.member(fx, "visible"), Some(&b(false)));
    assert!(h.calls.contains(&(
        fx,
        "setMaterialProperty".into(),
        vec![Value::String("u".into()), Value::Vec3([1.0, 2.0, 3.0])]
    )));
    assert_eq!(
        eval(&mut e, &mut h, "shared.fx.getMaterialCount()"),
        Value::Int(0)
    );

    let mut m = [0.0f32; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m[12] = 7.0;
    h.call_results
        .insert((l, "getTransformMatrix".into()), Value::Mat4(m));
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "thisScene.getLayer(1).getTransformMatrix().translation()",
        ),
        &[7.0, 0.0, 0.0],
    );
    eval(
        &mut e,
        &mut h,
        "thisScene.getLayer(1).setBoneTransform(0, Mat4.fromTranslation(new Vec3(7, 0, 0)))",
    );
    assert!(h.calls.contains(&(
        l,
        "setBoneTransform".into(),
        vec![Value::Int(0), Value::Mat4(m)]
    )));

    let inst = ObjectRef::ParticleInstance(1);
    h.set_member(inst, "rate", Value::Float(2.0));
    assert_eq!(
        num(&eval(&mut e, &mut h, "thisScene.getLayer(1).instance.rate")),
        2.0
    );
    eval(
        &mut e,
        &mut h,
        "thisScene.getLayer(1).instance.controlpoint3 = new Vec3(1, 2, 3)",
    );
    assert_eq!(
        h.member(inst, "controlpoint3"),
        Some(&Value::Vec3([1.0, 2.0, 3.0]))
    );

    h.call_results.insert(
        (l, "getChildren".into()),
        Value::Array(vec![
            Value::Object(ObjectRef::Layer(2)),
            Value::Object(ObjectRef::Layer(3)),
        ]),
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.getLayer(1).getChildren()[1] === thisScene.getLayer(3)"
        ),
        b(true)
    );
    h.call_results.insert(
        (l, "getVideoTexture".into()),
        Value::Object(ObjectRef::VideoTexture(1)),
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.getLayer(1).getVideoTexture().getCurrentTime()"
        ),
        Value::Int(0)
    );
    h.call_results.insert(
        (ObjectRef::Scene, "createModelData".into()),
        Value::Object(ObjectRef::ModelData(4)),
    );
    assert_eq!(
        eval(&mut e, &mut h, "thisScene.createModelData({}).POSITION"),
        Value::String("position".into())
    );
}

#[test]
fn this_object_follows_the_binding() {
    let (mut e, mut h) = setup();
    let fx = ObjectRef::Effect {
        layer: 1,
        effect: 2,
    };
    h.set_member(fx, "visible", Value::Bool(true));
    e.load_property_script(
        &mut h,
        &PropertyScript {
            layer: 1,
            property: "visible",
            object: Some(fx),
            source: "shared.kind = [thisObject.visible, thisLayer === thisScene.getLayer(1)];\n\
                     export function update(v) { return !v; }",
            binding: 0,
            script_properties: &[],
        },
    )
    .unwrap();
    assert_eq!(
        eval(&mut e, &mut h, "shared.kind"),
        Value::Array(vec![b(true), b(true)])
    );
    e.tick(&mut h);
    assert_eq!(h.member(fx, "visible"), Some(&b(false)));
}

#[test]
fn local_storage() {
    let (mut e, mut h) = setup();
    assert_eq!(
        num(&eval(
            &mut e,
            &mut h,
            "localStorage.set('n', 42); localStorage.get('n')"
        )),
        42.0
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "localStorage.set('o', { a: [1, 'x'], b: null }); JSON.stringify(localStorage.get('o'))"
        ),
        Value::String("{\"a\":[1,\"x\"],\"b\":null}".into())
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "localStorage.set('v', new Vec3(1, 2, 3)); const r = localStorage.get('v'); r instanceof Vec3 && r.toString()"
        ),
        Value::String("1.000000 2.000000 3.000000".into())
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "localStorage.set('nested', { p: new Vec2(1, 2) }); localStorage.get('nested').p instanceof Vec2"
        ),
        b(true)
    );
    assert_eq!(
        eval(&mut e, &mut h, "localStorage.get('missing')"),
        Value::Null
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "[localStorage.delete('n'), localStorage.delete('n')]"
        ),
        Value::Array(vec![b(true), b(false)])
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "localStorage.set('u', 1); localStorage.set('u', undefined); localStorage.get('u')"
        ),
        Value::Null
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "localStorage.set('r', 1); localStorage.remove('r')"
        ),
        b(true)
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "localStorage.set('g', 1, localStorage.LOCATION_GLOBAL); [localStorage.get('g'), localStorage.get('g', 'global')]"
        ),
        Value::Array(vec![Value::Null, Value::Float(1.0)])
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "localStorage.clear('global'); localStorage.get('g', 'global')"
        ),
        Value::Null
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "[localStorage.LOCATION_GLOBAL, localStorage.LOCATION_SCREEN]"
        ),
        Value::Array(vec![
            Value::String("global".into()),
            Value::String("screen".into())
        ])
    );
    // Quota (WE: 100 KB per wallpaper): the write is refused with a warning.
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "localStorage.set('big', 'x'.repeat(200000)); localStorage.get('big')"
        ),
        Value::Null
    );
    assert_eq!(h.logged("quota"), 1);
}

#[test]
fn local_storage_persists_per_file() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("lumenwall-script-ls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let screen = StorageFile::open(dir.join("screen.json"));
    let global = StorageFile::open(dir.join("global.json"));
    {
        let config = ScriptConfig {
            screen_storage: screen.clone(),
            global_storage: global.clone(),
            ..ScriptConfig::default()
        };
        let (mut e, mut h) = setup_with(config);
        eval(
            &mut e,
            &mut h,
            "localStorage.set('best', 1234); localStorage.set('all', 'x', 'global')",
        );
        assert!(screen.is_dirty(), "write is deferred");
        assert!(!dir.join("screen.json").exists());
        e.flush_storage();
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("screen.json")).unwrap(),
        "{\"best\":1234}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("global.json")).unwrap(),
        "{\"all\":\"x\"}"
    );
    drop((screen, global));
    let config = ScriptConfig {
        screen_storage: StorageFile::open(dir.join("screen.json")),
        global_storage: StorageFile::open(dir.join("global.json")),
        ..ScriptConfig::default()
    };
    let (mut e, mut h) = setup_with(config);
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "[localStorage.get('best'), localStorage.get('all', 'global')]"
        ),
        Value::Array(vec![Value::Float(1234.0), Value::String("x".into())])
    );
    // Deferred background write from tick once the delay is over.
    let config = ScriptConfig {
        screen_storage: StorageFile::open(dir.join("screen.json")),
        limits: ScriptLimits {
            storage_flush_delay: std::time::Duration::ZERO,
            ..ScriptLimits::default()
        },
        ..ScriptConfig::default()
    };
    let (mut e2, mut h2) = setup_with(config);
    eval(&mut e2, &mut h2, "localStorage.set('best', 99)");
    e2.tick(&mut h2);
    drop(e2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn script_properties() {
    let (mut e, mut h) = setup();
    h.script_properties
        .insert((7, "speed".into()), Value::Float(2.0));
    h.script_properties
        .insert((7, "tint".into()), Value::Vec3([1.0, 0.0, 0.0]));
    let id = e
        .load_property_script(
            &mut h,
            &PropertyScript {
                layer: 1,
                property: "alpha",
                object: None,
                source: r#"
                    export var scriptProperties = createScriptProperties()
                        .addSlider({ name: 'speed', label: 'Speed', value: 1, min: 0, max: 10 })
                        .addColor({ name: 'tint', value: '0 0 0' })
                        .addCheckbox({ name: 'flag', value: true })
                        .finish();
                    shared.sp = scriptProperties;
                    try { scriptProperties.speed = 1; shared.err1 = 'none'; } catch (e) { shared.err1 = e.constructor.name; }
                    try { scriptProperties.flag = 1; shared.err2 = 'none'; } catch (e) { shared.err2 = e.constructor.name; }
                    export function update() { return scriptProperties.speed * 10; }
                "#,
                binding: 7,
                script_properties: &["speed", "tint"],
            },
        )
        .unwrap();
    assert!(e.is_active(id));
    e.tick(&mut h);
    assert_eq!(num(h.member(ObjectRef::Layer(1), "alpha").unwrap()), 20.0);
    // Live: follows the host value.
    h.script_properties
        .insert((7, "speed".into()), Value::Float(3.0));
    e.tick(&mut h);
    assert_eq!(num(h.member(ObjectRef::Layer(1), "alpha").unwrap()), 30.0);
    assert_eq!(eval(&mut e, &mut h, "shared.sp.flag"), b(true));
    assert_eq!(
        eval(&mut e, &mut h, "[shared.sp.unknown, shared.sp.toString]"),
        Value::Array(vec![Value::Undefined, Value::Undefined])
    );
    assert_eq!(
        eval(&mut e, &mut h, "[shared.err1, shared.err2]"),
        Value::Array(vec![
            Value::String("TypeError".into()),
            Value::String("TypeError".into())
        ])
    );
    eval(&mut e, &mut h, "shared.sp.tint.y = 0.5");
    assert_eq!(
        h.script_properties.get(&(7, "tint".into())),
        Some(&Value::Vec3([1.0, 0.5, 0.0]))
    );
    // Outside of any script, finish() has nothing to bind to.
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "createScriptProperties().addSlider({ name: 'a', value: 1 }).finish()"
        ),
        Value::Undefined
    );
}

#[test]
fn matrices_load_lazily_and_can_be_shadowed() {
    let (mut e, mut h) = setup();
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "const d = Object.getOwnPropertyDescriptor(globalThis, 'Mat4'); [typeof d.get, d.enumerable]"
        ),
        Value::Array(vec![Value::String("function".into()), b(false)])
    );
    assert_eq!(
        eval(&mut e, &mut h, "typeof Mat4.identity"),
        Value::String("function".into())
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "const d2 = Object.getOwnPropertyDescriptor(globalThis, 'Mat3'); [typeof d2.value, d2.writable]"
        ),
        Value::Array(vec![Value::String("function".into()), b(true)])
    );
    let (mut e, mut h) = setup();
    assert_eq!(eval(&mut e, &mut h, "Mat3 = 5; Mat3"), Value::Int(5));
}

#[test]
fn mat4() {
    let (mut e, mut h) = setup();
    assert_eq!(
        eval(&mut e, &mut h, "new Mat4().m.join(',')"),
        Value::String("1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1".into())
    );
    assert_eq!(
        eval(&mut e, &mut h, "Mat4.identity().equals(new Mat4())"),
        b(true)
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.fromTranslation(new Vec3(1, 2, 3)).transformPoint(new Vec3(1, 1, 1))",
        ),
        &[2.0, 3.0, 4.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.fromTranslation(new Vec3(1, 2, 3)).transformDirection(new Vec3(1, 1, 1))",
        ),
        &[1.0, 1.0, 1.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.fromRotation(90, new Vec3(0, 0, 1)).transformDirection(new Vec3(1, 0, 0))",
        ),
        &[0.0, 1.0, 0.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.fromScale(2).transformPoint(new Vec3(1, 2, 3))",
        ),
        &[2.0, 4.0, 6.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.fromScale(new Vec3(1, 2, 3)).transformPoint(new Vec3(1, 1, 1))",
        ),
        &[1.0, 2.0, 3.0],
    );
    assert_eq!(
        num(&eval(
            &mut e,
            &mut h,
            "Mat4.fromScale(new Vec3(1, 2, 3)).determinant()"
        )),
        6.0
    );
    eval(
        &mut e,
        &mut h,
        "shared.m = Mat4.compose(new Vec3(1, 2, 3), new Vec3(10, 20, 30), new Vec3(2, 3, 4))",
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "shared.m.multiply(shared.m.inverse()).equals(new Mat4())"
        ),
        b(true)
    );
    assert_vec(
        &eval(&mut e, &mut h, "shared.m.decompose().translation"),
        &[1.0, 2.0, 3.0],
    );
    assert_vec(
        &eval(&mut e, &mut h, "shared.m.decompose().rotation"),
        &[10.0, 20.0, 30.0],
    );
    assert_vec(
        &eval(&mut e, &mut h, "shared.m.decompose().scale"),
        &[2.0, 3.0, 4.0],
    );
    assert_vec(
        &eval(&mut e, &mut h, "Mat4.fromEuler(10, 20, 30).extractEuler()"),
        &[10.0, 20.0, 30.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.fromEuler(new Vec3(0, 0, 90)).transformDirection(new Vec3(1, 0, 0))",
        ),
        &[0.0, 1.0, 0.0],
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "shared.m.transpose().transpose().equals(shared.m)"
        ),
        b(true)
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "const t = new Mat4(); t.translation(new Vec3(5, 6, 7)); t.translation()",
        ),
        &[5.0, 6.0, 7.0],
    );
    eval(
        &mut e,
        &mut h,
        "shared.r = Mat4.fromRotation(90, new Vec3(0, 0, 1))",
    );
    assert_vec(&eval(&mut e, &mut h, "shared.r.right()"), &[0.0, 1.0, 0.0]);
    assert_vec(&eval(&mut e, &mut h, "shared.r.up()"), &[-1.0, 0.0, 0.0]);
    assert_vec(
        &eval(&mut e, &mut h, "shared.r.forward()"),
        &[0.0, 0.0, 1.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.fromTranslation(new Vec3(1, 2, 3)).multiply(new Vec4(1, 0, 0, 1))",
        ),
        &[2.0, 2.0, 3.0, 1.0],
    );
    assert_eq!(
        num(&eval(&mut e, &mut h, "new Mat4().multiply(2).m[0]")),
        2.0
    );
    assert_eq!(
        num(&eval(
            &mut e,
            &mut h,
            "new Mat4().add(new Mat4()).subtract(new Mat4()).m[5]"
        )),
        1.0
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.fromTranslation(new Vec3(1, 0, 0)).translate(new Vec3(0, 2, 0)).translation()",
        ),
        &[1.0, 2.0, 0.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.lookAt(new Vec3(0, 0, 5), new Vec3(0, 0, 0), new Vec3(0, 1, 0)).transformPoint(new Vec3(0, 0, 0))",
        ),
        &[0.0, 0.0, -5.0],
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "Mat4.fromScale(new Vec3(2, 4, 8)).normalMatrix().m.join(',')"
        ),
        Value::String("0.5,0,0,0,0.25,0,0,0,0.125".into())
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat4.fromBasis(new Vec3(0, 1, 0), new Vec3(-1, 0, 0), new Vec3(0, 0, 1)).transformDirection(new Vec3(1, 0, 0))",
        ),
        &[0.0, 1.0, 0.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "new Mat4().rotate(90, new Vec3(0, 0, 1)).scale(2).transformPoint(new Vec3(1, 0, 0))",
        ),
        &[0.0, 2.0, 0.0],
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "Mat4.fromTranslation(new Vec3(1, 2, 3)).toString().split(' ').slice(12).join(' ')"
        ),
        Value::String("1.000000 2.000000 3.000000 1.000000".into())
    );
    assert_eq!(
        eval(&mut e, &mut h, "new Mat4().copy() instanceof Mat4"),
        b(true)
    );
}

#[test]
fn mat3() {
    let (mut e, mut h) = setup();
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat3.fromRotation(90).transformDirection(new Vec2(1, 0))",
        ),
        &[0.0, 1.0],
    );
    assert_close(
        num(&eval(&mut e, &mut h, "Mat3.fromRotation(30).angle()")),
        30.0,
    );
    eval(
        &mut e,
        &mut h,
        "shared.m = Mat3.compose(new Vec2(5, 6), 45, new Vec2(2, 3))",
    );
    assert_vec(
        &eval(&mut e, &mut h, "shared.m.decompose().translation"),
        &[5.0, 6.0],
    );
    assert_close(
        num(&eval(&mut e, &mut h, "shared.m.decompose().rotation")),
        45.0,
    );
    assert_vec(
        &eval(&mut e, &mut h, "shared.m.decompose().scale"),
        &[2.0, 3.0],
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "shared.m.multiply(shared.m.inverse()).equals(new Mat3())"
        ),
        b(true)
    );
    assert_eq!(
        num(&eval(
            &mut e,
            &mut h,
            "Mat3.fromScale(new Vec2(2, 3)).determinant()"
        )),
        6.0
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "Mat3.fromMat4(Mat4.fromScale(2)).m.join(',')"
        ),
        Value::String("2,0,0,0,2,0,0,0,2".into())
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat3.fromTranslation(new Vec2(1, 2)).transformPoint(new Vec2(1, 1))",
        ),
        &[2.0, 3.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat3.fromTranslation(new Vec2(1, 2)).translation()",
        ),
        &[1.0, 2.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat3.fromScale(2).multiply(new Vec3(1, 2, 1))",
        ),
        &[2.0, 4.0, 1.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "Mat3.fromBasis(new Vec2(0, 1), new Vec2(-1, 0)).transformDirection(new Vec2(1, 0))",
        ),
        &[0.0, 1.0],
    );
    assert_vec(
        &eval(
            &mut e,
            &mut h,
            "new Mat3().translate(new Vec2(1, 0)).rotate(90).scale(2).transformPoint(new Vec2(1, 0))",
        ),
        &[1.0, 2.0],
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "new Mat3().transpose().add(new Mat3()).subtract(new Mat3()).equals(new Mat3())"
        ),
        b(true)
    );
    assert_eq!(
        eval(&mut e, &mut h, "new Mat3().toString()"),
        Value::String(
            "1.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 0.000000 1.000000"
                .into()
        )
    );
}

#[test]
fn every_host_object_kind_routes_to_the_host() {
    let (mut e, mut h) = setup();
    let anim = ObjectRef::Animation(4);
    let alayer = ObjectRef::AnimationLayer { layer: 1, index: 0 };
    let tex = ObjectRef::TextureAnimation(1);
    let mat = ObjectRef::Material {
        layer: 1,
        effect: 0,
        material: 2,
    };
    let fx = ObjectRef::Effect {
        layer: 1,
        effect: 0,
    };
    h.call_results.insert(
        (ObjectRef::Scene, "getAnimation".into()),
        Value::Object(anim),
    );
    h.call_results.insert(
        (ObjectRef::Layer(1), "getAnimationLayer".into()),
        Value::Object(alayer),
    );
    h.call_results.insert(
        (ObjectRef::Layer(1), "getTextureAnimation".into()),
        Value::Object(tex),
    );
    h.call_results
        .insert((ObjectRef::Layer(1), "getEffect".into()), Value::Object(fx));
    h.call_results
        .insert((fx, "getMaterial".into()), Value::Object(mat));
    h.call_results.insert(
        (ObjectRef::Scene, "getInitialLayerConfig".into()),
        Value::Record(vec![("name".into(), Value::String("first".into()))]),
    );
    h.set_member(anim, "fps", Value::Int(30)).set_member(
        anim,
        "name",
        Value::String("walk".into()),
    );
    h.set_member(tex, "frameCount", Value::Int(12));
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "const a = thisScene.getAnimation('walk'); a.rate = 2; a.play(); a.fps = 99; [a.fps, a.name, a.isPlaying(), a.getFrame()]"
        ),
        Value::Array(vec![
            Value::Int(30),
            Value::String("walk".into()),
            b(false),
            Value::Int(0)
        ])
    );
    assert_eq!(h.member(anim, "rate"), Some(&Value::Int(2)));
    assert!(h.calls.contains(&(anim, "play".into(), vec![])));
    eval(
        &mut e,
        &mut h,
        "const l = thisScene.getLayer(1).getAnimationLayer(0); l.blend = 0.5; l.addEndedCallback(() => 1)",
    );
    assert_eq!(h.member(alayer, "blend"), Some(&Value::Float(0.5)));
    assert!(
        matches!(h.calls.last(), Some((o, m, args)) if *o == alayer && m == "addEndedCallback" && matches!(args[..], [Value::Callback(_)]))
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "const t = thisScene.getLayer(1).getTextureAnimation(); t.join(); t.frameCount"
        ),
        Value::Int(12)
    );
    eval(
        &mut e,
        &mut h,
        "thisScene.getLayer(1).getEffect(0).getMaterial(2).getAnimation('x')",
    );
    assert!(
        h.calls
            .contains(&(mat, "getAnimation".into(), vec![Value::String("x".into())]))
    );
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "thisScene.getInitialLayerConfig('first').name"
        ),
        Value::String("first".into())
    );
    eval(
        &mut e,
        &mut h,
        "thisScene.setCameraTransforms({ eye: new Vec3(0, 0, 1), zoom: 2 }); thisScene.destroyModelData(undefined)",
    );
    assert!(h.calls.contains(&(
        ObjectRef::Scene,
        "setCameraTransforms".into(),
        vec![Value::Record(vec![
            ("eye".into(), Value::Vec3([0.0, 0.0, 1.0])),
            ("zoom".into(), Value::Int(2))
        ])]
    )));
    assert_eq!(
        eval(
            &mut e,
            &mut h,
            "Object.getPrototypeOf(Object.getPrototypeOf(thisScene.getLayer(1))) === null"
        ),
        b(true)
    );
    assert_eq!(
        throws(
            &mut e,
            &mut h,
            "Object.getPrototypeOf(thisScene.getLayer(1)).getEffectCount.call({})"
        ),
        "TypeError"
    );
}

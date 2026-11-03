//! Dynamic text layers (M2, spec 06 §9).

mod common;

use common::*;
use lumenwall_script::mock::MockHost;
use lumenwall_script::*;

fn create(
    e: &mut ScriptEngine,
    h: &mut MockHost,
    layer: LayerId,
    source: &str,
    initial: &str,
) -> Result<ScriptId, ScriptError> {
    e.create_text_script(
        h,
        &TextScript {
            layer,
            source,
            initial_text: initial,
            binding: 5,
            script_properties: &[],
        },
    )
}

fn text(e: &ScriptEngine, id: ScriptId) -> String {
    e.text(id).expect("text layer").to_owned()
}

#[test]
fn update_return_and_mutation() {
    let (mut e, mut h) = setup();
    let a = create(
        &mut e,
        &mut h,
        1,
        "'use strict';\nexport function update(value) { return value + '!'; }",
        "a",
    )
    .unwrap();
    assert_eq!(text(&e, a), "a");
    assert!(e.tick_text(&mut h, a));
    assert_eq!(text(&e, a), "a!");
    assert!(e.tick_text(&mut h, a));
    assert_eq!(text(&e, a), "a!!");

    let b = create(
        &mut e,
        &mut h,
        2,
        "var n = 0; function update() { thisLayer.text = 'n=' + (++n); }",
        "",
    )
    .unwrap();
    e.tick_text(&mut h, b);
    e.tick_text(&mut h, b);
    assert_eq!(text(&e, b), "n=2");

    // Unchanged text reports no change.
    let c = create(
        &mut e,
        &mut h,
        3,
        "function update() { return 'same'; }",
        "",
    )
    .unwrap();
    assert!(e.tick_text(&mut h, c));
    assert!(!e.tick_text(&mut h, c));
}

#[test]
fn init_runs_once_on_first_tick() {
    let (mut e, mut h) = setup();
    let id = create(
        &mut e,
        &mut h,
        1,
        "var inits = 0; function init() { inits++; } function update() { return 'i' + inits; }",
        "",
    )
    .unwrap();
    assert_eq!(text(&e, id), " ");
    e.tick_text(&mut h, id);
    e.tick_text(&mut h, id);
    assert_eq!(text(&e, id), "i1");
}

#[test]
fn text_normalisation() {
    let (mut e, mut h) = setup();
    for (src, expected) in [
        ("function update() { return ''; }", " "),
        ("function update() { thisLayer.text = undefined; }", " "),
        ("function update() { thisLayer.text = null; }", " "),
        ("function update() { thisLayer.text = 5; }", "5"),
        ("function update() { return 5; }", "start"),
        (
            "function update() { thisLayer.text = new Vec2(1, 2); }",
            "1.000000 2.000000",
        ),
    ] {
        let id = create(&mut e, &mut h, 1, src, "start").unwrap();
        e.tick_text(&mut h, id);
        assert_eq!(text(&e, id), expected, "{src}");
        e.destroy_script(&mut h, id);
    }
}

#[test]
fn sloppy_mode_and_scopes() {
    let (mut e, mut h) = setup();
    let a = create(
        &mut e,
        &mut h,
        1,
        "'use strict';\nvar x = 'A'; function update() { leaked = 7; return x; }",
        "",
    )
    .unwrap();
    let b = create(
        &mut e,
        &mut h,
        2,
        "\"use strict\";\nvar x = 'B'; function update() { return x; }",
        "",
    )
    .unwrap();
    e.tick_text(&mut h, a);
    e.tick_text(&mut h, b);
    assert_eq!((text(&e, a), text(&e, b)), ("A".into(), "B".into()));
    // Non-strict: an undeclared assignment creates a global (spec 06 §9.8).
    assert_eq!(eval(&mut e, &mut h, "leaked"), Value::Int(7));
}

#[test]
fn creation_failures_keep_the_initial_text() {
    let (mut e, mut h) = setup();
    for src in [
        "import { mix } from 'WEMath'; function update() { return 'x'; }",
        "export default function () {}",
        "let props = 1;",
        "throw new Error('top');",
    ] {
        let err = create(&mut e, &mut h, 1, src, "initial").unwrap_err();
        assert!(matches!(err, ScriptError::Failed { .. }), "{src}");
    }
    assert_eq!(h.logged("disabled (text layer creation)"), 4);
    assert_eq!(h.logged("SyntaxError"), 3);
    let id = create(&mut e, &mut h, 1, "throw 1", "kept").unwrap_err();
    assert!(matches!(id, ScriptError::Failed { .. }));
}

#[test]
fn time_context_and_inherited_globals() {
    let (mut e, mut h) = setup();
    h.clock = Clock {
        runtime: 2.0,
        frametime: 0.5,
        time_of_day: 0.0,
    };
    h.set_member(ObjectRef::Layer(1), "alpha", Value::Float(0.25));
    let id = create(
        &mut e,
        &mut h,
        1,
        "var before = [engine.frametime, thisScene.time, thisScene.fps].join();
         function update() {
             return [before, engine.frametime, engine.time, thisScene.time, thisScene.currentTime, thisScene.dt, thisScene.fps,
                     typeof engine.runtime, typeof thisScene.getLayer, thisLayer.alpha].join(' ');
         }",
        "",
    )
    .unwrap();
    e.tick_text(&mut h, id);
    assert_eq!(text(&e, id), "0,0,60 0.5 2 2 2 0.5 2 number function 0.25");
}

#[test]
fn props_are_frozen_except_live_vectors() {
    let (mut e, mut h) = setup();
    h.script_properties
        .insert((5, "size".into()), Value::Float(3.0));
    h.script_properties
        .insert((5, "col".into()), Value::Vec3([1.0, 0.0, 0.0]));
    let id = e
        .create_text_script(
            &mut h,
            &TextScript {
                layer: 1,
                source: "function update() {
                    var sp = createScriptProperties().addSlider({ name: 'size', value: 9 }).addSlider({ name: 'extra', value: 4 }).finish();
                    return props.size + ':' + props.col.x + ':' + sp.extra + ':' + sp.size;
                }",
                initial_text: "",
                binding: 5,
                script_properties: &["size", "col"],
            },
        )
        .unwrap();
    e.tick_text(&mut h, id);
    assert_eq!(text(&e, id), "3:1:4:3");
    h.script_properties
        .insert((5, "size".into()), Value::Float(10.0));
    h.script_properties
        .insert((5, "col".into()), Value::Vec3([0.5, 0.0, 0.0]));
    e.tick_text(&mut h, id);
    assert_eq!(text(&e, id), "3:0.5:4:3");
}

#[test]
fn errors_and_destroy() {
    let (mut e, mut h) = setup();
    let id = create(&mut e, &mut h, 1, "var n = 0; function update() { if (++n > 1) throw new Error('text boom'); return 'ok' + n; }", "").unwrap();
    // Two errors are logged and the layer keeps running; the third disables it.
    for _ in 0..3 {
        e.tick_text(&mut h, id);
    }
    assert!(e.is_active(id));
    e.tick_text(&mut h, id);
    assert!(!e.is_active(id));
    e.tick_text(&mut h, id);
    assert_eq!(text(&e, id), "ok1");
    assert_eq!(h.logged("text boom"), 3);
    assert_eq!(h.logged("disabled (update)"), 1);

    // An init error disables at once.
    let id = create(
        &mut e,
        &mut h,
        3,
        "function init() { throw new Error('init boom'); } function update() { return 'x'; }",
        "",
    )
    .unwrap();
    e.tick_text(&mut h, id);
    assert!(!e.is_active(id));
    assert_eq!(h.logged("disabled (text init)"), 1);

    let id = create(&mut e, &mut h, 2, "function destroy() { shared.destroyed = thisLayer.text; } function update() { return 'live'; }", "").unwrap();
    e.tick_text(&mut h, id);
    e.destroy_script(&mut h, id);
    assert_eq!(
        eval(&mut e, &mut h, "shared.destroyed"),
        Value::String("live".into())
    );
    assert!(!e.tick_text(&mut h, id));
}

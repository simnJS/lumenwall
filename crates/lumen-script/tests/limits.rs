//! Execution budget, memory limit, stack limit: a faulty script is disabled with a log,
//! the engine keeps running.

mod common;

use std::time::{Duration, Instant};

use common::*;
use lumen_script::*;

fn limits() -> ScriptConfig {
    ScriptConfig {
        limits: ScriptLimits {
            tick_budget: Duration::from_millis(40),
            call_budget: Duration::from_millis(40),
            load_budget: Duration::from_millis(40),
            memory_limit: 16 * 1024 * 1024,
            max_jobs_per_tick: 1000,
            ..ScriptLimits::default()
        },
        ..ScriptConfig::default()
    }
}

#[test]
fn infinite_loop_in_update_is_interrupted() {
    let (mut e, mut h) = setup_with(limits());
    h.set_member(ObjectRef::Layer(2), "alpha", Value::Int(0));
    let bad = load(
        &mut e,
        &mut h,
        1,
        "alpha",
        // A JS try/catch cannot swallow the interruption.
        "let n = 0; export function update() { if (++n > 1) { for (;;) { try { for (;;) {} } catch (e) {} } } }",
    )
    .unwrap();
    let good = load(
        &mut e,
        &mut h,
        2,
        "alpha",
        "export function update(v) { return v + 1; }",
    )
    .unwrap();
    let start = Instant::now();
    e.tick(&mut h);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "tick took {:?}",
        start.elapsed()
    );
    assert!(!e.is_active(bad));
    assert_eq!(h.logged("execution budget exceeded"), 1);
    // The script after the culprit is skipped this frame only.
    e.tick(&mut h);
    assert!(e.is_active(good));
    assert_eq!(h.member(ObjectRef::Layer(2), "alpha"), Some(&Value::Int(2)));
}

#[test]
fn infinite_loops_at_load_in_events_and_timers() {
    let (mut e, mut h) = setup_with(limits());
    let err = load(&mut e, &mut h, 1, "alpha", "while (true) {}").unwrap_err();
    assert!(
        matches!(&err, ScriptError::Failed { message, .. } if message.contains("budget")),
        "{err:?}"
    );

    let id = load(
        &mut e,
        &mut h,
        2,
        "alpha",
        "export function resizeScreen() { for (;;) {} }",
    )
    .unwrap();
    e.resize_screen(&mut h, [1.0, 1.0]);
    assert!(!e.is_active(id));

    let id = load(
        &mut e,
        &mut h,
        3,
        "alpha",
        "engine.setTimeout(() => { for (;;) {} }, 0);",
    )
    .unwrap();
    e.tick(&mut h);
    assert!(!e.is_active(id));
    assert_eq!(h.logged("execution budget exceeded"), 3);
    assert_eq!(eval(&mut e, &mut h, "1 + 1"), Value::Int(2));
}

#[test]
fn text_layer_loop_is_interrupted() {
    let (mut e, mut h) = setup_with(limits());
    let id = e
        .create_text_script(
            &mut h,
            &TextScript {
                layer: 1,
                source: "function update() { while (true) {} }",
                initial_text: "start",
                binding: 0,
                script_properties: &[],
            },
        )
        .unwrap();
    assert!(!e.tick_text(&mut h, id));
    assert_eq!(e.text(id).as_deref(), Some("start"));
    assert_eq!(h.logged("execution budget exceeded"), 1);
}

#[test]
fn memory_limit() {
    let (mut e, mut h) = setup_with(limits());
    let id = load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "const keep = []; export function update() { if (shared.go) { for (;;) keep.push(new Array(100000).fill(1)); } }",
    )
    .unwrap();
    eval(&mut e, &mut h, "shared.go = true");
    e.tick(&mut h);
    assert!(!e.is_active(id));
    assert_eq!(h.logged("out of memory"), 1);
    // The runtime is still usable once the script's memory is released.
    assert_eq!(eval(&mut e, &mut h, "[1, 2, 3].length"), Value::Int(3));
    assert!(e.stats().memory_used < 16 * 1024 * 1024);
}

#[test]
fn stack_overflow() {
    let (mut e, mut h) = setup_with(limits());
    let id = load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "function f(n) { return f(n + 1) + 1; } export function update() { f(0); }",
    )
    .unwrap_err();
    assert!(
        matches!(&id, ScriptError::Failed { message, .. } if message.contains("RangeError")),
        "{id:?}"
    );
}

#[test]
fn runaway_promise_chain_is_capped() {
    let (mut e, mut h) = setup_with(limits());
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.n = 0; function spin() { shared.n++; Promise.resolve().then(spin); } export function init() { spin(); }",
    )
    .unwrap();
    assert!(h.logged("promise job limit") >= 1);
    e.tick(&mut h);
    let n = num(&eval(&mut e, &mut h, "shared.n"));
    assert!(n > 1000.0 && n < 5000.0, "{n}");
}

#[test]
fn unhandled_rejections_are_logged() {
    let (mut e, mut h) = setup();
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "export function update() { Promise.reject(new Error('nobody listens')); }",
    )
    .unwrap();
    assert!(h.logged("unhandled promise rejection: Error: nobody listens") >= 1);
}

#[test]
fn engine_drop_releases_everything() {
    // Every kind of retained JS handle exists when the engine is dropped.
    let (mut e, mut h) = setup();
    h.audio = Some((vec![1.0], vec![1.0]));
    h.set_member(ObjectRef::Layer(1), "origin", Value::Vec3([0.0; 3]));
    load(
        &mut e,
        &mut h,
        1,
        "origin",
        "engine.setInterval(() => {}, 10); engine.registerAudioBuffers(32);
         thisScene.createLayer({ cb: () => 1 });
         export function update(v) { return v; }",
    )
    .unwrap();
    e.create_text_script(
        &mut h,
        &TextScript {
            layer: 2,
            source: "function update() { return 'x'; }",
            initial_text: "",
            binding: 0,
            script_properties: &[],
        },
    )
    .unwrap();
    e.tick(&mut h);
    drop(e);
}

//! engine.setTimeout / engine.setInterval (spec 06 §10).

mod common;

use common::*;
use lumen_script::*;

fn fired(e: &mut ScriptEngine, h: &mut lumen_script::mock::MockHost) -> String {
    string(&eval(e, h, "shared.log.join()")).to_owned()
}

#[test]
fn timeout_fires_once_after_its_delay() {
    let (mut e, mut h) = setup();
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.log = []; engine.setTimeout(() => shared.log.push('t'), 100);",
    )
    .unwrap();
    assert!(e.has_work());
    h.advance(0.05);
    e.tick(&mut h);
    assert_eq!(fired(&mut e, &mut h), "");
    h.advance(0.06);
    e.tick(&mut h);
    h.advance(0.5);
    e.tick(&mut h);
    assert_eq!(fired(&mut e, &mut h), "t");
    assert_eq!(e.stats().timers, 0);
    assert!(!e.has_work());
}

#[test]
fn interval_fires_at_most_once_per_tick() {
    let (mut e, mut h) = setup();
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.log = []; engine.setInterval(() => shared.log.push('i'), 100);",
    )
    .unwrap();
    for _ in 0..3 {
        h.advance(0.1);
        e.tick(&mut h);
    }
    assert_eq!(fired(&mut e, &mut h), "i,i,i");
    // No catch-up after a long frame.
    h.advance(1.0);
    e.tick(&mut h);
    assert_eq!(fired(&mut e, &mut h), "i,i,i,i");
}

#[test]
fn cancel_functions() {
    let (mut e, mut h) = setup();
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.log = [];
         const cancel = engine.setTimeout(() => shared.log.push('never'), 10);
         shared.len = cancel.length;
         shared.r = cancel(123);
         cancel();
         let n = 0;
         const stop = engine.setInterval(() => { shared.log.push('i' + (++n)); if (n === 2) stop(); }, 10);
         engine.setInterval(function () { 'use strict'; shared.self = this; }, 10);",
    )
    .unwrap();
    for _ in 0..5 {
        h.advance(0.02);
        e.tick(&mut h);
    }
    assert_eq!(fired(&mut e, &mut h), "i1,i2");
    assert_eq!(
        eval(&mut e, &mut h, "[shared.len, shared.r, shared.self]"),
        Value::Array(vec![Value::Int(2), Value::Undefined, Value::Null])
    );
}

#[test]
fn zero_and_negative_delays_wait_for_the_next_tick() {
    let (mut e, mut h) = setup();
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.log = [];
         export function update() {
             if (!shared.armed) { shared.armed = true; engine.setTimeout(() => shared.log.push('zero'), 0); engine.setTimeout(() => shared.log.push('neg'), -50); }
         }",
    )
    .unwrap();
    // Created during the first tick's update (after timers ran): nothing fires yet.
    e.tick(&mut h);
    assert_eq!(fired(&mut e, &mut h), "");
    e.tick(&mut h);
    assert_eq!(fired(&mut e, &mut h), "zero,neg");
}

#[test]
fn intervals_run_before_timeouts_and_new_timers_wait() {
    let (mut e, mut h) = setup();
    load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.log = [];
         engine.setTimeout(() => { shared.log.push('timeout'); engine.setTimeout(() => shared.log.push('nested'), 0); }, 5);
         engine.setInterval(() => shared.log.push('interval'), 5);",
    )
    .unwrap();
    h.advance(0.01);
    e.tick(&mut h);
    assert_eq!(fired(&mut e, &mut h), "interval,timeout");
    e.tick(&mut h);
    assert_eq!(fired(&mut e, &mut h), "interval,timeout,nested");
}

#[test]
fn argument_validation() {
    let (mut e, mut h) = setup();
    assert_eq!(
        strict(
            &mut e,
            &mut h,
            "try { engine.setTimeout(); } catch (e) { return e.constructor.name; }"
        ),
        Value::String("TypeError".into())
    );
    assert_eq!(
        strict(
            &mut e,
            &mut h,
            "try { engine.setInterval('code', 10); } catch (e) { return e.constructor.name; }"
        ),
        Value::String("TypeError".into())
    );
}

#[test]
fn callback_error_disables_its_script_and_timers() {
    let (mut e, mut h) = setup();
    let id = load(
        &mut e,
        &mut h,
        1,
        "alpha",
        "shared.log = [];
         engine.setInterval(() => shared.log.push('alive'), 10);
         engine.setTimeout(() => { throw new Error('timer boom'); }, 15);",
    )
    .unwrap();
    for _ in 0..4 {
        h.advance(0.01);
        e.tick(&mut h);
    }
    assert!(!e.is_active(id));
    assert_eq!(e.stats().timers, 0);
    assert_eq!(fired(&mut e, &mut h), "alive,alive");
    assert_eq!(h.logged("timer boom"), 1);
}

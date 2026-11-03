#![allow(dead_code)]

use lumenwall_script::mock::MockHost;
use lumenwall_script::*;

pub fn setup() -> (ScriptEngine, MockHost) {
    setup_with(ScriptConfig::default())
}

pub fn setup_with(config: ScriptConfig) -> (ScriptEngine, MockHost) {
    let mut host = MockHost::new();
    host.add_layer(1, "first")
        .add_layer(2, "second")
        .add_layer(3, "third");
    (ScriptEngine::new(config).expect("engine"), host)
}

/// Loads a property script bound to `property` of layer `layer`.
pub fn load(
    e: &mut ScriptEngine,
    host: &mut MockHost,
    layer: LayerId,
    property: &str,
    source: &str,
) -> Result<ScriptId, ScriptError> {
    e.load_property_script(
        host,
        &PropertyScript {
            layer,
            property,
            object: None,
            source,
            binding: layer as u64,
            script_properties: &[],
        },
    )
}

/// Evaluates a global script; panics with the JS error.
pub fn eval(e: &mut ScriptEngine, host: &mut MockHost, source: &str) -> Value {
    e.eval(host, source)
        .unwrap_or_else(|err| panic!("{source}: {err}"))
}

/// Evaluates an expression inside a strict function.
pub fn strict(e: &mut ScriptEngine, host: &mut MockHost, body: &str) -> Value {
    eval(
        e,
        host,
        &format!("(function () {{ 'use strict'; {body} }})()"),
    )
}

pub fn num(v: &Value) -> f64 {
    match v {
        Value::Int(i) => f64::from(*i),
        Value::Float(f) => f64::from(*f),
        other => panic!("not a number: {other:?}"),
    }
}

pub fn components(v: &Value) -> Vec<f32> {
    match v {
        Value::Vec2(c) => c.to_vec(),
        Value::Vec3(c) => c.to_vec(),
        Value::Vec4(c) => c.to_vec(),
        other => panic!("not a vector: {other:?}"),
    }
}

#[track_caller]
pub fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-5 * expected.abs().max(1.0),
        "expected {expected}, got {actual}"
    );
}

#[track_caller]
pub fn assert_vec(v: &Value, expected: &[f32]) {
    let c = components(v);
    assert_eq!(c.len(), expected.len(), "dimension of {v:?}");
    for (a, b) in c.iter().zip(expected) {
        assert!(
            a == b || (a - b).abs() <= 1e-5 * b.abs().max(1.0) || (a.is_nan() && b.is_nan()),
            "expected {expected:?}, got {c:?}"
        );
    }
}

pub fn string(v: &Value) -> &str {
    match v {
        Value::String(s) => s,
        other => panic!("not a string: {other:?}"),
    }
}

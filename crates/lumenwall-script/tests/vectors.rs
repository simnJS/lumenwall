//! Vec2 / Vec3 / Vec4 (spec 06 §5, WE lib.sceneScript.d.ts).

mod common;

use common::*;
use lumenwall_script::mock::MockHost;
use lumenwall_script::*;

fn v(e: &mut ScriptEngine, h: &mut MockHost, expr: &str) -> Value {
    eval(e, h, expr)
}

/// Name of the error thrown by `expr` (strict mode), or "none".
fn throws(e: &mut ScriptEngine, h: &mut MockHost, expr: &str) -> String {
    let r = strict(
        e,
        h,
        &format!("try {{ {expr}; return 'none'; }} catch (err) {{ return err.constructor.name; }}"),
    );
    string(&r).to_owned()
}

#[test]
fn constructors() {
    let (mut e, mut h) = setup();
    assert_vec(&v(&mut e, &mut h, "new Vec2()"), &[0.0, 0.0]);
    assert_vec(&v(&mut e, &mut h, "new Vec2(5)"), &[5.0, 5.0]);
    assert_vec(&v(&mut e, &mut h, "new Vec3(1, 2, 3)"), &[1.0, 2.0, 3.0]);
    assert_vec(
        &v(&mut e, &mut h, "new Vec4(1, 2, 3, 4)"),
        &[1.0, 2.0, 3.0, 4.0],
    );
    assert_vec(&v(&mut e, &mut h, "new Vec3(1, 2)"), &[1.0, 2.0, 0.0]);
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(1, '2', true)"),
        &[1.0, 2.0, 1.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3('1 2.5 -3')"),
        &[1.0, 2.5, -3.0],
    );
    assert_vec(&v(&mut e, &mut h, "new Vec2('7')"), &[7.0, 0.0]);
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(new Vec3(1, 2, 3))"),
        &[1.0, 2.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec4(new Vec2(1, 2))"),
        &[1.0, 2.0, 0.0, 0.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3({ x: 1, y: 2, z: 3 })"),
        &[1.0, 2.0, 3.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec4(9, 8, 7, 6, 5)"),
        &[9.0, 8.0, 7.0, 6.0],
    );
    assert_eq!(throws(&mut e, &mut h, "new Vec3(true)"), "TypeError");
    assert_eq!(throws(&mut e, &mut h, "new Vec3(null)"), "TypeError");
    assert_eq!(throws(&mut e, &mut h, "new Vec3({ x: 1 })"), "TypeError");
    assert_eq!(
        v(
            &mut e,
            &mut h,
            "[Vec2.name, Vec3.name, Vec4.name, Vec3.length, new Vec3() instanceof Vec3, new Vec3() instanceof Vec2]"
        ),
        Value::Array(vec![
            Value::String("Vec2".into()),
            Value::String("Vec3".into()),
            Value::String("Vec4".into()),
            Value::Int(3),
            Value::Bool(true),
            Value::Bool(false),
        ])
    );
    // Components are stored as f32.
    assert_eq!(
        num(&v(&mut e, &mut h, "new Vec2(0.1, 0).x")),
        f64::from(0.1f32)
    );
}

#[test]
fn components_and_enumeration() {
    let (mut e, mut h) = setup();
    assert_eq!(
        v(
            &mut e,
            &mut h,
            "const a = new Vec4(1, 2, 3, 4); [a.x, a.y, a.z, a.w]"
        ),
        Value::Array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ])
    );
    assert_vec(
        &v(&mut e, &mut h, "const b = new Vec3(1, 2, 3); b.y = 7.5; b"),
        &[1.0, 7.5, 3.0],
    );
    assert_eq!(throws(&mut e, &mut h, "new Vec3().x = '5'"), "TypeError");
    assert_eq!(throws(&mut e, &mut h, "new Vec3().w = 1"), "TypeError");
    assert_eq!(throws(&mut e, &mut h, "new Vec3().foo = 1"), "TypeError");
    assert_eq!(v(&mut e, &mut h, "new Vec3().w"), Value::Undefined);
    assert_eq!(v(&mut e, &mut h, "new Vec2().z"), Value::Undefined);
    assert_eq!(
        v(&mut e, &mut h, "Object.keys(new Vec3(1, 2, 3)).length"),
        Value::Int(0)
    );
    assert_eq!(
        v(&mut e, &mut h, "JSON.stringify(new Vec3(1, 2, 3))"),
        Value::String("{}".into())
    );
}

#[test]
fn this_and_argument_validation() {
    let (mut e, mut h) = setup();
    assert_eq!(
        throws(&mut e, &mut h, "Vec3.prototype.add.call({}, 1)"),
        "TypeError"
    );
    assert_eq!(
        throws(
            &mut e,
            &mut h,
            "Vec2.prototype.add.call(new Vec3(1, 1, 1), 1)"
        ),
        "TypeError"
    );
    // Spec 06 §5.3: a Vec2 is accepted by Vec3 operations, not the other way round.
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(1, 2, 3).add(new Vec2(1, 1))"),
        &[2.0, 3.0, 3.0],
    );
    assert_eq!(
        throws(&mut e, &mut h, "new Vec2(1, 2).add(new Vec3(1, 1, 1))"),
        "TypeError"
    );
    assert_eq!(
        throws(
            &mut e,
            &mut h,
            "new Vec3(1, 2, 3).add(new Vec4(1, 1, 1, 1))"
        ),
        "TypeError"
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(1, 2, 3).add({ x: 1, y: 1 })"),
        &[2.0, 3.0, 3.0],
    );
    assert_eq!(
        throws(&mut e, &mut h, "new Vec3(1, 2, 3).add({ x: 1 })"),
        "TypeError"
    );
    assert_eq!(
        throws(&mut e, &mut h, "new Vec3(1, 2, 3).add('1')"),
        "TypeError"
    );
    assert_eq!(
        throws(&mut e, &mut h, "new Vec2(1, 2).add({ x: 1, y: 2, z: 3 })"),
        "TypeError"
    );
    assert_eq!(
        throws(
            &mut e,
            &mut h,
            "new Vec3(1, 2, 3).add({ x: 1, y: 2, z: 3, w: 4 })"
        ),
        "TypeError"
    );
    // Spec 06 §5.5 / Q-VEC-2: wrong arity gives undefined.
    assert_eq!(
        v(&mut e, &mut h, "new Vec3(1, 2, 3).add()"),
        Value::Undefined
    );
    assert_eq!(
        v(&mut e, &mut h, "new Vec3(1, 2, 3).subtract(1, 2)"),
        Value::Undefined
    );
    assert_eq!(
        v(&mut e, &mut h, "new Vec3(1, 2, 3).dot()"),
        Value::Undefined
    );
    assert_eq!(v(&mut e, &mut h, "new Vec2(1, 2).min()"), Value::Undefined);
}

#[test]
fn arithmetic() {
    let (mut e, mut h) = setup();
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(1, 2, 3).add(new Vec3(1, 1, 1))"),
        &[2.0, 3.0, 4.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(1, 2, 3).add(2)"),
        &[3.0, 4.0, 5.0],
    );
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "new Vec3(5, 5, 5).subtract(new Vec3(1, 2, 3))",
        ),
        &[4.0, 3.0, 2.0],
    );
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "new Vec3(1, 2, 3).multiply(new Vec3(2, 3, 4))",
        ),
        &[2.0, 6.0, 12.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec4(1, 2, 3, 4).multiply(2)"),
        &[2.0, 4.0, 6.0, 8.0],
    );
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "new Vec3(2, 6, 12).divide(new Vec3(2, 3, 4))",
        ),
        &[1.0, 2.0, 3.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(1, -1).divide(0)"),
        &[f32::INFINITY, f32::NEG_INFINITY],
    );
    assert_eq!(
        num(&v(
            &mut e,
            &mut h,
            "new Vec3(1, 2, 3).dot(new Vec3(4, 5, 6))"
        )),
        32.0
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(1, 0, 0).cross(new Vec3(0, 1, 0))"),
        &[0.0, 0.0, 1.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(0, 1, 0).cross(new Vec3(1, 0, 0))"),
        &[0.0, 0.0, -1.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(1, -2, 3).negate()"),
        &[-1.0, 2.0, -3.0],
    );
    // Results are copies.
    assert_eq!(
        num(&v(
            &mut e,
            &mut h,
            "const a = new Vec3(1, 2, 3); const b = a.copy(); b.x = 9; a.x"
        )),
        1.0
    );
}

#[test]
fn measures() {
    let (mut e, mut h) = setup();
    assert_eq!(num(&v(&mut e, &mut h, "new Vec3(3, 4, 0).length()")), 5.0);
    assert_eq!(
        num(&v(&mut e, &mut h, "new Vec3(3, 4, 0).lengthSqr()")),
        25.0
    );
    assert_eq!(
        num(&v(
            &mut e,
            &mut h,
            "new Vec2(0, 0).distance(new Vec2(3, 4))"
        )),
        5.0
    );
    assert_eq!(
        num(&v(
            &mut e,
            &mut h,
            "new Vec2(0, 0).distanceSqr(new Vec2(3, 4))"
        )),
        25.0
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(3, 4).normalize()"),
        &[0.6, 0.8],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(0, 0).normalize()"),
        &[f32::NAN, f32::NAN],
    );
    assert_eq!(
        v(&mut e, &mut h, "new Vec2(1, 1 / 0).isFinite()"),
        Value::Bool(false)
    );
    assert_eq!(
        v(&mut e, &mut h, "new Vec4(1, 2, 3, 4).isFinite()"),
        Value::Bool(true)
    );
    // we-doc: equals uses an epsilon.
    assert_eq!(
        v(
            &mut e,
            &mut h,
            "new Vec3(1, 2, 3).equals(new Vec3(1, 2, 3.000001))"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        v(
            &mut e,
            &mut h,
            "new Vec3(1, 2, 3).equals(new Vec3(1, 2, 3.1))"
        ),
        Value::Bool(false)
    );
    assert_eq!(
        v(&mut e, &mut h, "new Vec3(1, 2, 3).equals()"),
        Value::Bool(false)
    );
    assert_eq!(
        throws(&mut e, &mut h, "new Vec3(1, 2, 3).equals(new Vec2(1, 2))"),
        "TypeError"
    );
    assert_eq!(
        throws(&mut e, &mut h, "new Vec3(1, 2, 3).equals(5)"),
        "TypeError"
    );
}

#[test]
fn geometry() {
    let (mut e, mut h) = setup();
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(1, -1).reflect(new Vec2(0, 1))"),
        &[1.0, 1.0],
    );
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "new Vec3(0, -1, 0).refract(new Vec3(0, 1, 0), 1)",
        ),
        &[0.0, -1.0, 0.0],
    );
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "new Vec3(0.8, -0.6, 0).refract(new Vec3(0, 1, 0), 0.5)",
        ),
        &[0.4, -0.916_515, 0.0],
    );
    // Total internal reflection gives the zero vector.
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "new Vec3(0.8, -0.6, 0).refract(new Vec3(0, 1, 0), 1.5)",
        ),
        &[0.0, 0.0, 0.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(2, 3).project(new Vec2(1, 0))"),
        &[2.0, 0.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(1, 2).perpendicular()"),
        &[-2.0, 1.0],
    );
    assert_close(num(&v(&mut e, &mut h, "new Vec2(0, 1).angle()")), 90.0);
    assert_close(num(&v(&mut e, &mut h, "new Vec2(-1, 0).angle()")), 180.0);
    assert_close(
        num(&v(
            &mut e,
            &mut h,
            "new Vec2(1, 0).angleBetween(new Vec2(0, 1))",
        )),
        90.0,
    );
    assert_close(
        num(&v(
            &mut e,
            &mut h,
            "new Vec2(0, 1).angleBetween(new Vec2(1, 0))",
        )),
        -90.0,
    );
    assert_close(
        num(&v(
            &mut e,
            &mut h,
            "new Vec3(1, 0, 0).angleBetween(new Vec3(0, 0, -1))",
        )),
        90.0,
    );
    assert_close(
        num(&v(
            &mut e,
            &mut h,
            "new Vec3(1, 0, 0).angleBetween(new Vec3(-2, 0, 0))",
        )),
        180.0,
    );
    assert_vec(&v(&mut e, &mut h, "new Vec2(1, 0).rotate(90)"), &[0.0, 1.0]);
    assert_eq!(
        throws(&mut e, &mut h, "new Vec2(1, 0).rotate('a')"),
        "TypeError"
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(0, 2, 0).toSpherical()"),
        &[2.0, 0.0, 0.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(1, 0, 0).toSpherical()"),
        &[1.0, 90.0, 90.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(0, 0, 1).toSpherical()"),
        &[1.0, 90.0, 0.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "Vec3.fromSpherical(1, 90, 90)"),
        &[1.0, 0.0, 0.0],
    );
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "const s = new Vec3(1, 2, 3).toSpherical(); Vec3.fromSpherical(s.x, s.y, s.z)",
        ),
        &[1.0, 2.0, 3.0],
    );
    assert_eq!(
        v(&mut e, &mut h, "typeof new Vec2().cross"),
        Value::String("undefined".into())
    );
    assert_eq!(
        v(&mut e, &mut h, "typeof new Vec3().rotate"),
        Value::String("undefined".into())
    );
}

#[test]
fn interpolation_and_component_math() {
    let (mut e, mut h) = setup();
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "new Vec3(0, 0, 0).mix(new Vec3(10, 20, 30), 0.5)",
        ),
        &[5.0, 10.0, 15.0],
    );
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "new Vec3(0, 0, 0).mix(new Vec3(10, 20, 30), new Vec3(0, 0.5, 1))",
        ),
        &[0.0, 10.0, 30.0],
    );
    assert_eq!(
        throws(&mut e, &mut h, "new Vec3().mix(new Vec3())"),
        "TypeError"
    );
    assert_eq!(
        throws(&mut e, &mut h, "new Vec3().mix(new Vec3(), 'x')"),
        "TypeError"
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(1, 5).min(new Vec2(3, 2))"),
        &[1.0, 2.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(1, 5).max(new Vec2(3, 2))"),
        &[3.0, 5.0],
    );
    assert_vec(&v(&mut e, &mut h, "new Vec2(1, 5).min(4)"), &[1.0, 4.0]);
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(-1, 0.5, 2).clamp(0, 1)"),
        &[0.0, 0.5, 1.0],
    );
    assert_vec(
        &v(
            &mut e,
            &mut h,
            "new Vec3(-1, 0.5, 2).clamp(new Vec3(0, 0, 0), new Vec3(1, 0.2, 1))",
        ),
        &[0.0, 0.2, 1.0],
    );
    assert_vec(&v(&mut e, &mut h, "new Vec2(-1, 2).abs()"), &[1.0, 2.0]);
    assert_vec(
        &v(&mut e, &mut h, "new Vec4(-3, 0, 5, NaN).sign()"),
        &[-1.0, 0.0, 1.0, 0.0],
    );
    // Half away from zero.
    assert_vec(
        &v(&mut e, &mut h, "new Vec4(2.5, -2.5, 1.4, -1.6).round()"),
        &[3.0, -3.0, 1.0, -2.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(1.5, -1.5).floor()"),
        &[1.0, -2.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(1.5, -1.5).ceil()"),
        &[2.0, -1.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec2(1.25, -1.25).fract()"),
        &[0.25, 0.75],
    );
    assert_vec(&v(&mut e, &mut h, "new Vec2(5, -5).mod(3)"), &[2.0, 1.0]);
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(0.2, 0.5, 0.8).step(0.5)"),
        &[0.0, 1.0, 1.0],
    );
    assert_vec(
        &v(&mut e, &mut h, "new Vec3(0.5, 2, 0.25).smoothStep(0, 1)"),
        &[0.5, 1.0, 0.156_25],
    );
}

#[test]
fn text_form() {
    let (mut e, mut h) = setup();
    assert_eq!(
        v(&mut e, &mut h, "new Vec3(1.5, -0, 2).toString()"),
        Value::String("1.500000 -0.000000 2.000000".into())
    );
    assert_eq!(
        v(&mut e, &mut h, "String(new Vec2(NaN, Infinity))"),
        Value::String("nan inf".into())
    );
    assert_eq!(
        v(&mut e, &mut h, "`${new Vec2(-1 / 0, 0.1)}`"),
        Value::String("-inf 0.100000".into())
    );
    assert_eq!(
        v(&mut e, &mut h, "new Vec4(1, 2, 3, 4) + ''"),
        Value::String("1.000000 2.000000 3.000000 4.000000".into())
    );
}

#[test]
fn live_views_track_the_host_value() {
    let (mut e, mut h) = setup();
    h.set_member(ObjectRef::Scene, "clearcolor", Value::Vec3([1.0, 2.0, 3.0]));
    load(
        &mut e,
        &mut h,
        2,
        "alpha",
        "shared.o = thisScene.clearcolor;",
    )
    .unwrap();
    // Reads follow the host.
    h.set_member(ObjectRef::Scene, "clearcolor", Value::Vec3([4.0, 5.0, 6.0]));
    assert_vec(&v(&mut e, &mut h, "shared.o.copy()"), &[4.0, 5.0, 6.0]);
    // Component writes go to the host with the vector type.
    v(&mut e, &mut h, "shared.o.z = 9");
    assert_eq!(
        h.member(ObjectRef::Scene, "clearcolor"),
        Some(&Value::Vec3([4.0, 5.0, 9.0]))
    );
    // The view keeps its dimension; a Float value reads as a splat projection.
    h.set_member(ObjectRef::Scene, "clearcolor", Value::Float(2.0));
    assert_vec(&v(&mut e, &mut h, "shared.o.copy()"), &[2.0, 2.0, 2.0]);
    assert_eq!(
        v(&mut e, &mut h, "shared.o.toString()"),
        Value::String("2.000000".into())
    );
    // A vanished target throws instead of crashing.
    h.members
        .remove(&(ObjectRef::Scene, "clearcolor".to_owned()));
    assert_eq!(throws(&mut e, &mut h, "shared.o.x"), "TypeError");
}

#[test]
fn layer_members_read_as_copies() {
    // Wallpaper Engine's Dino Run keeps `initialPosition = thisLayer.origin` while it moves
    // the layer through another copy, then writes it back.
    let (mut e, mut h) = setup();
    h.set_member(ObjectRef::Layer(1), "origin", Value::Vec3([1.0, 2.0, 3.0]));
    load(
        &mut e,
        &mut h,
        2,
        "alpha",
        "shared.initial = thisScene.getLayer(1).origin;
         shared.moving = thisScene.getLayer(1).origin;
         shared.moving.y += 10;",
    )
    .unwrap();
    assert_eq!(
        h.member(ObjectRef::Layer(1), "origin"),
        Some(&Value::Vec3([1.0, 2.0, 3.0]))
    );
    v(
        &mut e,
        &mut h,
        "thisScene.getLayer(1).origin = shared.moving",
    );
    assert_eq!(
        h.member(ObjectRef::Layer(1), "origin"),
        Some(&Value::Vec3([1.0, 12.0, 3.0]))
    );
    assert_vec(
        &v(&mut e, &mut h, "shared.initial.copy()"),
        &[1.0, 2.0, 3.0],
    );
}

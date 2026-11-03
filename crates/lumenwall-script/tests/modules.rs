//! Native modules WEMath, WEColor, WEVector (spec 06 §7).

mod common;

use common::*;
use lumenwall_script::*;

/// Loads a module script that stores `expr` (evaluated with the imports) into `shared.r`.
fn run(
    e: &mut ScriptEngine,
    h: &mut lumenwall_script::mock::MockHost,
    layer: LayerId,
    imports: &str,
    expr: &str,
) -> Value {
    let src = format!(
        "{imports}\ntry {{ shared.r = {expr}; }} catch (err) {{ shared.r = 'throws ' + err.constructor.name; }}"
    );
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    load(e, h, layer, &format!("p{n}"), &src).expect("module loads");
    eval(e, h, "shared.r")
}

#[test]
fn we_math() {
    let (mut e, mut h) = setup();
    let imp = "import { smoothStep, mix, deg2rad, rad2deg } from 'WEMath';";
    assert_eq!(
        num(&run(&mut e, &mut h, 1, imp, "smoothStep(0, 1, 0.5)")),
        0.5
    );
    assert_eq!(
        num(&run(&mut e, &mut h, 1, imp, "smoothStep(0, 1, 0.25)")),
        0.156_25
    );
    assert_eq!(
        num(&run(&mut e, &mut h, 1, imp, "smoothStep(0, 1, 2)")),
        1.0
    );
    assert!(num(&run(&mut e, &mut h, 1, imp, "smoothStep(1, 1, 1)")).is_nan());
    assert_eq!(
        string(&run(&mut e, &mut h, 1, imp, "smoothStep(0, 1)")),
        "throws TypeError"
    );
    assert_eq!(
        string(&run(&mut e, &mut h, 1, imp, "smoothStep(0, 1, '0.5')")),
        "throws TypeError"
    );
    assert_eq!(num(&run(&mut e, &mut h, 1, imp, "mix(10, 20, 0.25)")), 12.5);
    assert_eq!(num(&run(&mut e, &mut h, 1, imp, "mix.length")), 1.0);
    assert_eq!(
        string(&run(&mut e, &mut h, 1, imp, "mix(1, 2)")),
        "throws TypeError"
    );
    assert_eq!(
        run(
            &mut e,
            &mut h,
            1,
            imp,
            "deg2rad === Math.PI / 180 && rad2deg === 180 / Math.PI"
        ),
        Value::Bool(true)
    );
    // Namespace import.
    assert_eq!(
        num(&run(
            &mut e,
            &mut h,
            2,
            "import * as M from 'WEMath';",
            "M.mix(0, 4, 0.5)"
        )),
        2.0
    );
}

#[test]
fn we_color() {
    let (mut e, mut h) = setup();
    let imp = "import { rgb2hsv, hsv2rgb, normalizeColor, expandColor } from 'WEColor';";
    // we-doc: normalized HSV (hue in [0, 1)).
    assert_vec(
        &run(&mut e, &mut h, 1, imp, "rgb2hsv(new Vec3(1, 0, 0))"),
        &[0.0, 1.0, 1.0],
    );
    assert_vec(
        &run(&mut e, &mut h, 1, imp, "rgb2hsv(new Vec3(0, 1, 0))"),
        &[1.0 / 3.0, 1.0, 1.0],
    );
    assert_vec(
        &run(&mut e, &mut h, 1, imp, "rgb2hsv(new Vec3(1, 0, 1))"),
        &[300.0 / 360.0, 1.0, 1.0],
    );
    assert_vec(
        &run(
            &mut e,
            &mut h,
            1,
            imp,
            "rgb2hsv({ x: 0.5, y: 0.5, z: 0.5 })",
        ),
        &[0.0, 0.0, 0.5],
    );
    assert_vec(
        &run(&mut e, &mut h, 1, imp, "hsv2rgb(new Vec3(2 / 3, 1, 1))"),
        &[0.0, 0.0, 1.0],
    );
    assert_vec(
        &run(
            &mut e,
            &mut h,
            1,
            imp,
            "hsv2rgb(rgb2hsv(new Vec3(0.2, 0.4, 0.6)))",
        ),
        &[0.2, 0.4, 0.6],
    );
    assert_vec(
        &run(
            &mut e,
            &mut h,
            1,
            imp,
            "normalizeColor(new Vec3(255, 51, 0))",
        ),
        &[1.0, 0.2, 0.0],
    );
    assert_vec(
        &run(&mut e, &mut h, 1, imp, "expandColor(new Vec3(1, 0.2, 0))"),
        &[255.0, 51.0, 0.0],
    );
    assert_eq!(
        string(&run(
            &mut e,
            &mut h,
            1,
            imp,
            "normalizeColor({ x: '1', y: 0, z: 0 })"
        )),
        "throws TypeError"
    );
    assert_eq!(
        string(&run(&mut e, &mut h, 1, imp, "rgb2hsv(1)")),
        "throws TypeError"
    );
    assert_eq!(
        string(&run(&mut e, &mut h, 1, imp, "rgb2hsv()")),
        "throws TypeError"
    );
    assert_eq!(
        run(
            &mut e,
            &mut h,
            1,
            imp,
            "rgb2hsv(new Vec3()) instanceof Vec3"
        ),
        Value::Bool(true)
    );
}

#[test]
fn we_vector() {
    let (mut e, mut h) = setup();
    let imp = "import { angleVector2, vectorAngle2 } from 'WEVector';";
    assert_vec(
        &run(&mut e, &mut h, 1, imp, "angleVector2(90)"),
        &[0.0, 1.0],
    );
    assert_vec(
        &run(&mut e, &mut h, 1, imp, "angleVector2(180)"),
        &[-1.0, 0.0],
    );
    assert_close(
        num(&run(&mut e, &mut h, 1, imp, "vectorAngle2(new Vec2(0, 1))")),
        90.0,
    );
    assert_close(
        num(&run(
            &mut e,
            &mut h,
            1,
            imp,
            "vectorAngle2(new Vec2(-1, -1))",
        )),
        -135.0,
    );
}

#[test]
fn unknown_modules_fail_the_import() {
    let (mut e, mut h) = setup();
    h.set_member(ObjectRef::Layer(1), "alpha", Value::Float(0.5));
    for src in [
        "import { x } from 'wemath'; export function update() { return 1; }",
        "import { x } from './helper.js'; export function update() { return 1; }",
        "import { nope } from 'WEMath'; export function update() { return 1; }",
    ] {
        let err = load(&mut e, &mut h, 1, "alpha", src).unwrap_err();
        assert!(matches!(err, ScriptError::Failed { .. }), "{err:?}");
        e.remove_layer(&mut h, 1);
    }
    // The value stays untouched (spec 06 §8.1 step 3).
    assert_eq!(
        h.member(ObjectRef::Layer(1), "alpha"),
        Some(&Value::Float(0.5))
    );
    assert_eq!(h.logged("disabled (load)"), 3);
}

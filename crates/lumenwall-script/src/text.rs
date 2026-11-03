//! M2: dynamic text layer scripts (spec 06 §9).

/// Source pre-processing of spec 06 §9.2, applied in order, each removal repeated until
/// no occurrence is left.
pub(crate) fn preprocess(source: &str) -> String {
    let mut s = source.to_owned();
    // parity: removes the strict directive and every "export " sequence anywhere.
    for pattern in ["'use strict';", "\"use strict\";", "export "] {
        while s.contains(pattern) {
            s = s.replace(pattern, "");
        }
    }
    s
}

/// Wraps a pre-processed text script in a non-strict function giving it its own scope
/// (spec 06 §9.3). The function returns the layer record (`this` of `init`/`update`/
/// `destroy`).
///
/// we-doc: the local `engine` and `thisScene` inherit from the real globals, so the
/// documented members (`engine.runtime`, `thisScene.getLayer`…) stay reachable; the
/// reference-only members (`engine.time`, `thisScene.time/currentTime/dt/fps`) shadow them.
/// The local `thisLayer` is the text layer's own object with a `text` data property.
pub(crate) fn wrapper_source(body: &str) -> String {
    const PROLOGUE: &str = r#"(function (__lumenwall_props, __lumenwall_layer, __lumenwall_scene, __lumenwall_engine, __lumenwall_time) {
var props = Object.assign({}, __lumenwall_props);
var thisLayer = __lumenwall_layer;
var thisScene = Object.create(__lumenwall_scene, {
  time: { get: function () { return __lumenwall_time.time; } },
  currentTime: { get: function () { return __lumenwall_time.time; } },
  dt: { get: function () { return __lumenwall_time.dt; } },
  fps: { get: function () { return __lumenwall_time.fps; } }
});
var engine = Object.create(__lumenwall_engine, {
  frametime: { get: function () { return __lumenwall_time.dt; } },
  time: { get: function () { return __lumenwall_time.time; } }
});
var createScriptProperties = function () {
  var builder = {};
  var add = function (options) {
    if (options && options.name !== undefined && !Object.prototype.hasOwnProperty.call(props, options.name)) {
      props[options.name] = options.value;
    }
    return builder;
  };
  builder.addSlider = add; builder.addCheckbox = add; builder.addCombo = add;
  builder.addText = add; builder.addColor = add; builder.addFile = add;
  builder.addDirectory = add; builder.addTextInput = add;
  builder.finish = function () { return props; };
  return builder;
};
"#;
    const EPILOGUE: &str = r#"
;return {
  thisLayer: thisLayer,
  thisScene: thisScene,
  init: typeof init === 'function' ? init : undefined,
  update: typeof update === 'function' ? update : undefined,
  destroy: typeof destroy === 'function' ? destroy : undefined
};
})"#;
    let mut out = String::with_capacity(PROLOGUE.len() + body.len() + EPILOGUE.len());
    out.push_str(PROLOGUE);
    out.push_str(body);
    out.push_str(EPILOGUE);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocessing_follows_spec() {
        assert_eq!(preprocess("'use strict';\nlet a = 1;"), "\nlet a = 1;");
        assert_eq!(preprocess("\"use strict\";x"), "x");
        // Without the semicolon the directive stays.
        assert_eq!(preprocess("'use strict'\nx"), "'use strict'\nx");
        assert_eq!(
            preprocess("export function update() {}"),
            "function update() {}"
        );
        assert_eq!(preprocess("export default 1"), "default 1");
        // Removal is repeated until nothing is left.
        assert_eq!(preprocess("exexport port x"), "x");
        // Anywhere, including strings and identifier tails.
        assert_eq!(
            preprocess("let s = 'export me'; myexport y"),
            "let s = 'me'; myy"
        );
        // A tab is not a space.
        assert_eq!(
            preprocess("export\tfunction f() {}"),
            "export\tfunction f() {}"
        );
    }
}

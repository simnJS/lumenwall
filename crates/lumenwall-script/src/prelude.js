// Lumenwall SceneScript prelude: createScriptProperties (spec 06 §6.9).
// Evaluated once per runtime as a function receiving native helpers.
(function (natives) {
  'use strict';
  const { finishScriptProperties } = natives;

  // Spec 06 §6.9: only `name` and `value` of each control are kept as declared defaults.
  function createScriptProperties() {
    const defaults = Object.create(null);
    const builder = {};
    const add = function (options) {
      if (options !== null && typeof options === 'object' && options.name !== undefined) {
        defaults[String(options.name)] = options.value;
      }
      return builder;
    };
    for (const name of ['addSlider', 'addCheckbox', 'addCombo', 'addText', 'addColor', 'addFile', 'addDirectory', 'addTextInput']) {
      builder[name] = add;
    }
    builder.finish = function () {
      const props = finishScriptProperties(defaults);
      return props === undefined ? undefined : Object.freeze(props);
    };
    return builder;
  }

  return { createScriptProperties };
})

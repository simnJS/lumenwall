# lumenwall-script

SceneScript runtime: property scripts (ES modules exporting `update(value)` and the other
`IComponent` events) and dynamic text layers, on QuickJS-ng through `rquickjs` 0.14.

## Sources (clean room)

- `specs/06-scripting.md` (all sections), applied through `docs/PARITY-POLICY.md`.
- `docs/PERFORMANCE.md`, `docs/research/platform.md` (scripting row).
- Public Wallpaper Engine documentation: SceneScript reference page, `lib.sceneScript.d.ts`
  (v2.8), class pages `IEngine`, `ILocalStorage`, `Vec2`, `IThisPropertyObject`,
  `AudioBuffers`, event pages `init` and `applyUserProperties`.
- `rquickjs` 0.14 / QuickJS-ng API documentation and crate sources.

## Use

```rust
let mut scripts = ScriptEngine::new(ScriptConfig {
    screen_storage: StorageFile::open(dir.join("screen0.json")),
    global_storage: wallpaper_global_store.clone(), // one per wallpaper, shared by screens
    ..ScriptConfig::default()
})?;
scripts.load_property_script(&mut host, &PropertyScript { layer: 42, property: "origin", object: None,
    source, binding: 7, script_properties: &["speed"] })?;   // at object creation
scripts.tick(&mut host);                       // once per frame, after cursor/parallax, before drawing
if scripts.tick_text(&mut host, text_id) { /* re-render scripts.text(text_id) */ }
```

The engine implements `SceneHost` (`get`/`set`/`call` on `ObjectRef` handles, layer lookup,
clock, cursor, display, audio spectrum, script properties, log). `mock::MockHost` is an
in-memory implementation for tests.

Host duties: create the engine only for scenes that have scripts; send `text` of text objects to
`create_text_script` only (spec F2); call `tick_text` only for visible text layers; pass media
times in seconds; call `remove_layer` / `shutdown` so `destroy()` runs.

## Performance

- One runtime and context per wallpaper; nothing runs between calls (no threads, no timers;
  `localStorage` writes use a short-lived thread at most every `storage_flush_delay`).
- `tick` with nothing to do (no `update`, timer, audio buffer or job) calls no JavaScript.
- Modules are compiled once; `update` and event functions are kept as `Persistent` handles; the
  live `value` vector passed to `update` is cached per script; no JSON and no Rust allocation per frame
  for numeric and vector values. `Vec2/3/4` are native classes; `Mat3/Mat4` (JS) compile on first use.
- Bench (`cargo bench -p lumenwall-script`, release, this machine):

| Case | Time |
|---|---|
| 100 trivial `update(value) { return value; }` | 11 µs / frame (≈ 110 ns per script) |
| 100 `update` writing `value.y = Math.sin(engine.runtime) * 10` | 45 µs / frame |
| `tick` without scripts | 60 ns |
| `ScriptEngine::new` | 210 µs, 180 KB heap (+1.5 KB per small script, +70 KB once matrices are used) |

## Spec 06 §14.1 decisions (all [Réf])

F1 exports read, `update` called · F2 text → M2 only (host) · F3 load failure logged, value intact ·
F4 no `update` → value intact · F5 `z`/`w` read · F6 non-vector object logged, ignored · F7/F8
working layer objects, `thisLayer` = the running script's layer · F9 `createScriptProperties` with
declared defaults, bound to the running script · F10 timers with cancel function · F11/F12 vector
methods and global constructors · F13 working `WEMath`/`WEColor` · F14 `[object]` fallback ·
F15 negative delay = 0.

Reproduced reference behaviour (`// parity:`): `clearenabled` reads bloom, `skylightcolor` reads the
ambient color, bloom strength/threshold truncated; `console.log` without separator, only
`log`/`error`; binary vector ops with wrong arity return `undefined`; `%f` number format;
`WEMath.mix.length === 1`; `rgb2hsv` f32 comparison and `hsv2rgb` truncation quirks; M2 source
pre-processing; `pointSize` alias; update order by key bytes; assigning a non-component to a
vector throws (strict code).

Orchestrator decisions (`// lumenwall:`):

- `thisScene.getLayer('unknown')` returns `null` (the reference throws), so scripts probing for
  an optional layer keep running.
- The first `update` runs on the first tick after loading, not at load (spec §8.1 step 5,
  Q-M1-2); `init` still runs at load.
- An `update` error (property or text script) is logged and the script keeps running; three
  consecutive errors disable it. At most 10 such errors are logged per script. A budget overrun
  or out-of-memory in `update` disables at once (it would stall every frame).
- `WEColor` uses a normalized hue in [0, 1) (`lib.sceneScript.d.ts`: "normalized HSV").

## lib.sceneScript.d.ts coverage

✅ implemented here · 🔌 forwarded to `SceneHost` (neutral result when the host returns `None`)

| API | Status |
|---|---|
| Events `init`, `update`, `destroy`, `resizeScreen`, `applyUserProperties`, `applyGeneralSettings`, `cursorEnter/Leave/Move/Down/Up/Click`, `mediaStatus/Playback/Properties/Thumbnail/TimelineChanged` | ✅ (host triggers them) |
| `Vec2`, `Vec3`, `Vec4`: constructors (number, components, string, vector), all methods, `Vec3.fromSpherical` | ✅ native |
| `Mat3`, `Mat4`: all statics and methods, `m` array | ✅ JS, lazy |
| `WEMath`, `WEColor`, `WEVector` | ✅ |
| `IEngine` (all members), `AudioBuffers` | ✅ (clock, display, spectrum from host) |
| `IInput`, `IConsole`, `ILocalStorage`, `shared`, `renderContext`, `thisObject`, `thisLayer` | ✅ |
| `createScriptProperties` (`addSlider/Checkbox/Combo/Text/Color` + `addFile/Directory/TextInput`) | ✅ |
| `IScene` properties; `getLayer`, `getLayerByID`, `getLayerCount`, `enumerateLayers`, `getLayerIndex` | ✅ |
| `IScene.createLayer`, `destroyLayer`, `sortLayer`, `getInitialLayerConfig`, `get/setCameraTransforms`, `getAnimation`, `createModelData`, `destroyModelData` | 🔌 |
| `ILayer` properties (all interfaces it extends, `instance`) | 🔌 live vectors |
| `ILayer` methods: transforms, `lookAt*`, parenting, attachments, bones, blend shapes, animation layers, effects, sound and particle playback | 🔌 |
| `IEffect`, `IMaterial`, `IAnimation`, `IAnimationLayer`, `ITextureAnimation`, `IVideoTexture`, `IParticleSystemInstance`, `IModelData`, `IAssetHandle` | 🔌 handle objects |
| `CursorEvent`, `Media*Event` | ✅ built from host data |
| `CameraTransforms`, `AnimationEvent` | plain objects supplied by the host |
| Function arguments (`addEndedCallback`…) | ✅ `Value::Callback`, fired by `invoke_callback` |

Subsystems Lumenwall lacks today (puppet bones, animation layers, model data, effect materials)
answer with neutral values (`undefined`, `0`, `-1`, `false`, `[]`) until the host implements them.

## Deviations

From the spec's [Réf], following `docs/PARITY-POLICY.md` rule 4/5 (`// we-doc:`) or the product
brief:

- Errors at load, in `init`, events, timers, callbacks or text `init`, budget overruns and
  out-of-memory disable the script at once with one error log; `update` errors follow the
  three-in-a-row rule above. A disabled script's timers and callbacks are released (spec §11:
  silent and continues).
- Limits (spec: none, Q-RT-2): `tick_budget` 100 ms, `call_budget` 100 ms, `load_budget` 1 s,
  64 MiB heap, 512 KiB stack, 10 000 promise jobs per tick. Jobs run after every entry point
  (Q-RT-1).
- `init`/`update` returning `undefined` keep the value (WE `init` page; Réf resets to Null). `null`
  still resets.
- `init(value)` runs at load, before the first `update`; `applyUserProperties` (all properties at load, then
  changes only), `applyGeneralSettings`, `resizeScreen`, cursor events, `mediaStatusChanged`,
  `destroy` are dispatched (spec §8.5: never).
- Media timeline in seconds; thumbnail colors and all `MediaPropertiesEvent` fields from the host
  (defaults = spec constants).
- `input.cursorScreenPosition` in pixels; `cursorWorldPosition`, `cursorLeftDown` from the host.
- Vectors: `toString` separates with a space; `equals` uses an epsilon (1e-5); `new VecN()` is the
  zero vector; `new Vec2(vec3)` truncates; `mix` accepts a vector amount.
- `WEColor.rgb2hsv`/`hsv2rgb` use a normalized hue (see above); the spec uses degrees (Q-COL-1).
- `thisScene` members and layer properties are writable through the host (Réf: read-only / ignored).
  Writes use the true member name. Unknown names on layer objects become ordinary properties.
- `engine.openUserShortcut` returns a boolean; `localStorage` follows `ILocalStorage` (any JSON
  value, `delete`, locations, persistence, 100 KB quota) and keeps `remove` and `null` for missing
  keys.
- Text layers: the local `engine` and `thisScene` inherit from the real globals and the local
  `thisLayer` is the layer object with its own `text` property, so documented members
  (`engine.runtime`…) work (Q-M2-1). An empty initial text reads as `" "` (§9.1).
- In non-strict text scripts, assigning a non-component to a vector is ignored instead of throwing.

## To calibrate on real Wallpaper Engine

Unverified conventions, kept as is until compared with WE:

- `Vec*.equals` / `Mat*.equals` tolerance: 1e-5 absolute per component.
- Matrices column-major; Euler angles applied X then Y then Z (`R = Rz·Ry·Rx`).
- `toSpherical` / `fromSpherical`: theta from +Y, azimuth phi measured from +Z toward +X.
- `Vec2.perpendicular()` = (−y, x).

## Tests

`tests/vectors.rs`, `globals.rs`, `modules.rs`, `lifecycle.rs`, `timers.rs`, `limits.rs`, `text.rs`,
unit tests in `value`, `util`, `storage`, `modules`, `text`; bench `benches/update.rs`.

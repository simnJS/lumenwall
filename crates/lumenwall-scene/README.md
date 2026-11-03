# lumenwall-scene

Data model for `project.json`, user properties, `scene.json`, objects, models, materials and effects.

## Inputs (clean room)

- `specs/02-modele-scene.md` (only behavioural source)
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/research/render.md`, `docs/TESTING.md`
- Public Rust / serde documentation. No upstream code, fork or `/tmp/claude-1000` content was read.

## Design

- **One typed serde pass per file** (`raw.rs`): lenient structures that borrow strings from the input and record the JSON type of each field instead of failing. The conversion that follows reports wrong types as `LoadError::Data { file, path: $.objects[3].effects[0].file, error }`. serde itself can only fail on syntax. No `serde_json::Value` is kept, except `ParticleObject::instance_override` (as requested) and transient property `value`s.
- **Index-based model**: settings are stored contiguously in `Settings` (`SettingId`), strings are interned (`Sym`), models/materials/effects live in arenas of the `Scene` (`ModelId`, `MaterialId`, `EffectId`).
- **Bindings resolved once**: a setting bound to a property stores `PropId` and an interned condition; each property keeps its subscriber list. `Project::set_property_text` touches only those settings: no string lookup per frame or per update.
- **Value** (`value.rs`): every view is computed on write. A single `Vec4` holds the float/vec2/vec3/vec4 views (they are always prefixes), so a read is a field load.
- Scripts are stored as data (`Settings::scripts`, `Settings::scripted`) for `lumenwall-script`.
- Particle content is not modelled: `ParticleSource::File(path)` or `Inline(RawValue)` plus `instanceoverride`. `Project::add_setting` / `add_setting_json` / the public `raw::RawSetting` let `lumenwall-particles` register bindable settings.
- `AssetSource` (read a path → `Cow<[u8]>`, so an mmap source copies nothing) is implemented for `HashMap<String, Vec<u8>>`.

## Coverage

§0 conversions and vector/colour strings, §1.1 sequence, §2 project, §3 properties (10 kinds, updates from text, `--set-property`, listing §15.1), §4 values, §5 settings/conditions/scripts, §6 scene/camera/general, §7.1–7.8 and 7.10 objects, §8–§11, §12 pass matching, 6-level textures, combos, constants, compatibility passes (the blend swap is the renderer's, see below), §13 creation/render order, transforms, visibility, §14 bloom, §15.2 dump.

## Blending contract

`ImagePasses` / `PassDesc::blending` carry each pass's **own** blending: the material pass's `blending` (copy passes `normal`). The last bullet of §12.6 (spec 04 §9.8: with more than one pass, the last pass takes the first pass's blending and the first pass becomes `normal`) is **not** applied here. `lumenwall-render`'s chain planner applies it exactly once, to the chain it actually draws (after the integration's debug filters, together with the puppet rule). Applying it in both crates made every multi-pass image opaque. `crates/lumenwall-engine/tests/blend_contract.rs` fails if the transfer happens zero or two times.

Not here: §7.9 particle files (`lumenwall-particles`), Annexes A/B (`lumenwall-formats` / `lumenwall-assets`; the tests contain a minimal test-only PKG reader).

Tests: 17 unit, 42 on the spec examples and open-question edge cases, plus 2 `#[ignore]` tests on real content: wallpaper 3018340632 and every scene wallpaper of `$LUMENWALL_WE_WORKSHOP`. All 7 load and build with no warning (`cargo test -p lumenwall-scene -- --ignored`).

## Open questions: decisions

Rule numbers refer to `docs/PARITY-POLICY.md`.

| Q | Decision | Rule |
|---|---|---|
| Q1 | Integer colours stay 0–255 (`"1 1 1"` ≈ black), `// parity:` | 3, 5 (WE behaviour unknown) |
| Q2 | `#RRGGBB` byte shift reproduced; 8-digit colours ≥ 0x80 no longer fail (read as u32) | 5 for the shift, 2 for the error |
| Q3 | Commas → spaces without merging, reproduced | 3 |
| Q4 | Component counting reproduced exactly | 3 |
| Q5 | `nearz`/`farz`/`fov` read from `camera`, else from `general` (where WE writes them), else the defaults | 5 (WE format known) |
| Q6 | Missing or null `orthogonalprojection` = perspective scene (`None`), no error | 2, 4 |
| Q7 | Numeric `workshopid` keeps all 64 bits; the synthetic counter still moves for every project | 4/5; counter harmless |
| Q8 | Combo-bound numeric fields read 0; condition ignored for non-text properties, `// parity:` | 3 |
| Q9 | `textinput` string stored raw, like a later update; other JSON stored as compact JSON | 5 (WE shows raw text) |
| Q10 | Listing prints the stored text for scenetexture/file/textinput (diagnostic only) | not viewer-visible |
| Q11 | Binding to a `text` property yields Null, reproduced | 3 |
| Q12 | E2 (process abort) → typed `LoadError` with JSON path | 2 |
| Q13 | Invalid slider/colour/combo override → warning, property unchanged | 2 |
| Q14 | Instance textures only fill empty slots, `// parity:` | 5 (WE behaviour unknown) |
| Q15 | Non-`copy` commands classified `Command::Swap`; the builder skips them with a warning | 5 |
| Q16 | Parent visibility not inherited (reference). Effect visibility is **live**: `Project::image_passes` re-reads it (WE documents `IEffect.visible` and property-bound effects) | 3 / 4 |
| Q17 | Duplicate ids: first wins (reproduced). Cycles cut with a warning (iterative walk, no recursion). Missing parent → root + warning. Bloom object kept out of the id space | 2 |
| Q18 | Overrides applied before the scene is parsed, so snapshots (`size`, text `size`/`padding`) see them | 5 (WE applies user values at load) |
| Q19 | Text `size`/`padding` also accept `{"value": …}` (strict read of `value`); wrong component count still an error. Camera vectors strict. Emitters: n/a | 2 |
| Q20 | Light/shape/unknown objects become generic nodes with a `GenericKind` hint and a warning | 3 (rendering is out of scope) |
| Q21 | Bloom defaults 0, values frozen at build (refreshed by a new `build`), 4th pass without override, reproduced | 3 |
| Q22 | n/a (`lumenwall-formats`) | — |
| Q23 | Becoming Null keeps the text view, `// parity:` | 3 |
| Q24 | Float boolean view by truncation (0.5 → false), `// parity:` | 3 |
| Q25 | n/a (`lumenwall-particles`) | — |
| Q26 | Model `width`/`height` stored, unused | 3 |
| Q27 | Image whose material has no pass: not set up, warning (no global failure) | 2 |
| Q28 | Magenta-tint compatibility pass reproduced, `// parity:` | 3 |
| Q29 | Combo values written as integer strings (`"1"`) accepted; other strings stay a typed error | 2 |
| Q30 | Non-string/object `user` stays a typed error; numeric `condition` read like a combo value; missing `condition` = none | 2 |
| Q31 | Missing or unreadable model/material file → warning, image not set up; missing effect file or effect material → effect dropped; bloom material missing → no bloom. Malformed JSON stays fatal | 2 |

## Deviations

- Q2 (8-digit hex), Q5, Q6, Q7, Q9, Q10, Q12, Q13, Q16 (effects), Q17, Q18, Q19, Q27, Q29, Q30, Q31: see the table.
- A UTF-8 byte order mark at the start of a JSON file is skipped.
- A quoted `bool` (`"true"`, `"1"` → true, other text → false) or quoted number on a `slider`
  property value is read like override text instead of failing the load: Wallpaper Engine
  ships such a `project.json` (the default web wallpaper corsair_o_tron).
- Object `dependencies` also accept objects `{ "id", "index", "type" }` (written by Wallpaper Engine for particle links, spec 05 A7.8): their `id` is used as the dependency (strict integer); `index` and `type` are not kept. Spec 02 §7.2 only describes integers.
- Spec ambiguity: §13.3 says "group" scale/angles for types other than image and text, while §7.2 says particles use their re-read settings. Particles use their own settings (§7.2).
- A passthrough image with at least one effect is created even when all its effects are hidden at setup. `ImagePasses::draws` is then false (nothing drawn), so it can appear when an effect becomes visible.
- Failed object setups are memoised instead of being retried on each reference. The predicate is deterministic, so the result is the same.
- No per-load cache of referenced files: each reference re-reads and re-parses its file (independent copies, §1.1-4). Parsing is microseconds, and an mmap `AssetSource` returns borrowed bytes.
- Structure dump (§15.2): the indentation and the `(not loaded)` marker are Lumenwall's choice.
- Text objects (§7.7) also read `effects`, `colorBlendMode`, `parallaxDepth` and `brightness`,
  and carry `TextObject::layer`: the image layer the renderer draws them as (Wallpaper Engine
  behaviour, parity rule 5). Its synthetic model and material use the WE `font` shader with the
  glyph texture `<lumenwall>/text/<n>` in slot 0; its colour, alpha and brightness are neutral
  constants. `Object::as_layer` returns it (and an image's own data), and the pass builder
  (`image_passes`, `pass_*`, `main_texture`, compatibility materials) works on layers.
  `Object::as_image` is unchanged.
- The `text` content of a text object stays text: §5.2 would turn `"26 NOV 2024"` into a vec3 and
  `"2024"` into a float, whose text view is empty, so the text would disappear (WE displays it).
  Text bound to a property keeps the property's value.

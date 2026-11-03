# lumen-shader

Wallpaper Engine shader text (+ combos, constants, textures) → one validated naga module per pass, with binding reflection and an on-disk cache. All work happens at load time; nothing runs per frame.

## Specs used (clean room)

- `specs/03-shaders.md` (whole document), `specs/02-modele-scene.md` §9–§12 (material passes, overrides, combos, constants, texture maps).
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/research/render.md`.
- Binding contract of `crates/lumen-render/README.md` ("Integration contract" > "Shaders").
- Public documentation only: Khronos GLSL 4.50 / Vulkan GLSL, naga and wgpu API docs. No upstream code was read.

## Approach

```
.vert/.frag ─▶ we::preprocess_unit (§4: includes, #require, combos, annotations, gl_FragColor)
            ─▶ we::assemble (§9: dialect macros, #defines §5.5, §9.1/§9.2 adaptations)
            ─▶ glsl:: C preprocessor ─▶ parser ─▶ typer (R1–R7) ─▶ linker (R8) ─▶ strict Vulkan GLSL 4.50
            ─▶ naga glsl-in (per stage) ─▶ merge (vs_main + fs_main) ─▶ naga validation ─▶ reflection ─▶ cache
```

**Why a Rust front-end instead of patched glslang.** naga's GLSL front-end rejects the dialect (implicit conversions, loose overloads, integer conditions, float `%`, float indices, wrapped swizzles, free uniforms, combined samplers). Instead of maintaining glslang patches in C++ (`docs/research/render.md`), the crate has its own front-end (~6 k lines in `src/glsl`: preprocessor with Prosser hide sets, recursive-descent parser, typer with a builtin table, linker, emitter). It makes every conversion explicit, so the emitted GLSL 4.50 is strictly typed and naga accepts it unchanged. It is 100 % Rust (builds for Windows with no C toolchain), roughly 0.5 ms per effect pass single-threaded, and every rule is one place in `glsl/typer.rs`. glsl-lang was not used: its AST has no place for types or resolved symbols, and the emitter had to work around naga's limits anyway (no `mat2(vec4)`, no std140 two-row matrices, `gl_VertexIndex` is `uint`, no implicit-LOD lookups in vertex shaders, the DX12 back-end has no `inverse`/`outerProduct`).

### Output (lumen-render contract)

- One `naga::Module` with entry points `vs_main` and `fs_main` (`CompiledProgram::module`, for `lumen_render::ShaderDesc { upstream_clip_space: true }`). The position stays in the GLSL convention.
- `@group(0) @binding(0)`: pass block (std140) whose members are the WE uniforms that are actually used (unreachable code is pruned), ordered to reduce padding. `bool` is stored as `uint`, `matCx2` as `vec4[C]` (naga rejects two-row matrices in std140), `float[N]`/`int[N]` read only through indices are packed as `vec4[N/4]` (array stride 4 in the reflection).
- `@group(0) @binding(1)`: the renderer's frame block (`dynbuf::FRAME_GLOBALS_WGSL` layout) for `g_Time`, `g_Daytime`, pointer, texel sizes and audio spectra, when `CompileOptions::frame_globals` (default) and the declared type matches. Updated once per frame for all passes.
- `@group(1) @binding(2N)` / `2N + 1`: `texture2D` / filtering `sampler` of slot N. `g_Texture0`…`g_Texture7` read slot N; any other sampler name (including `g_Texture8/9`) reads slot 0 (spec 03 §8.1). Every `sampler2D` is split, also when passed to user functions (texture + sampler parameters).
- Vertex inputs keep their names (`a_Position`, `a_TexCoord`…); one fragment output at location 0.
- `PassShader::key` (blake3 of final sources + options + versions) is the program identity for the renderer's `ShaderId`.

### Reflection

`ProgramReflection`: pass block members (name, type, offset, array length/stride, stages), O(1) engine uniforms (`engine(EngineUniform::ModelViewProjectionMatrix)`, mapped only when the declared type is the one the engine writes, §12.4), frame-block uniforms, texture slots, attributes. `PassShader` adds the annotations (`constant(key)` = §7.3 binding with its offset, `initial_uniforms()` = block with the registered defaults, §12.1), combos, and `texture_stacks()` (§8.2). `ConstantValue::to_uniform` implements the §7.4 table.

### Cache

`ShaderCompiler::new(options, Some(dir))`: key = blake3(final vertex + fragment sources, which contain every include and combo `#define`, options, crate version, cache format, naga major). Entry = postcard(module + reflection) behind a blake3 checksum; atomic write (temp + rename); a corrupt entry is a miss. A cached pass loads in ~30 µs. Identical programs are also shared in memory (`Arc`), compiled once even when requested concurrently; `load_passes` compiles in parallel with rayon. `clear_memory_cache()` drops the in-memory table after loading.

## Coverage of the relaxations (spec 03 §10.2)

| Rule | Implementation | Tests (`tests/dialect.rs`) |
|---|---|---|
| R1 | bool→float everywhere; float→int/uint and uint→int for `=`, initialisers, `+ - * / %`, `*=` (`i *= 0.5` multiplies by 0) | `r1_*` |
| R2 | broadcast, vector→scalar, vector narrowing in assignments, arguments, returns, struct constructors, `!` | `r2_*` |
| R3 | strict phase, then loose phase counting adaptations; ambiguity only from 3 tied candidates | `r3_*` |
| R4 | `if`/loops: `!= 0`; ternary: `> 0` (`// parity:`) | `r4_integer_conditions` |
| R5 | float `%` and `%=` → `mod` (floor semantics) | `r5_float_modulo` |
| R6 | float indices truncated with `int()` | `r6_float_indices` |
| R7 | out-of-range swizzle components wrap modulo the size (warning); scalar swizzles | `r7_swizzle_wraps` |
| R8 | varyings and uniforms of different sizes widened; the narrower stage reads a truncated copy, the vertex pads with 0 and w = 1 | `r8_*`, `section_9_*` |

Dialect macros (§3.3) are textual macros with C semantics in the assembled header: `mul`, swapped `max`, `lerp`, `frac`, `saturate`, `CAST2/3/4/3X3`, `float2…4`, `int2…4`, `texSample2D(Lod)`, `log10`, `atan2`, `fmod`, `ddx`, `ddy`, `GLSL` (`compat_macros`). Includes, `#require LightingV1`, combos, annotations, workshop/zcompat substitution and the virtual `commands/copy` shader have unit tests.

## Decisions on the open questions (spec 03 §17)

| Q | Decision | Policy |
|---|---|---|
| 1 | `require`/`requireany` reproduced as observed (`// parity:`). | 3/5 |
| 2 | Texture combo required with a texture-name default: V = 1 instead of abandoning the object. | 2 |
| 3 | Float default given as a string keeps its integer part (`// parity:`). | 3/5 |
| 4 | Annotation without `material`: default never applied (`initial_uniforms` leaves 0). | 3 |
| 7 | Integer conditions of statements use `!= 0` (HLSL semantics). | 5 |
| 8 | Type + shape change in one step accepted (`vec3 v = 1;`). | 4 |
| 9 | R8 applied to uniforms; padded varying components are 0, w = 1 (like §9.1). | 4 |
| 10 | `sampler2DComparison` is a plain 2D sampler unless the shader defines it as a macro. | 2 |
| 11 | Includes neither deduplicated nor conditional (reference); identical redeclarations of uniforms, structs and functions coming from a double include are tolerated (first kept, warning). | 3/2 |
| 12 | An `#if` on the first line moves the include block to the start of the text. | 2 |
| 15 | `ddy` = `dFdy` (wgpu framebuffer y points down like D3D); `CompileOptions::negate_ddy` restores the OpenGL reference form. To validate visually. | 5 |
| 18 | Uniform, attribute and varying names are kept; GLSL 4.50 keywords free in 3.30 (`sample`, `filter`, `texture2D`…) get an `_l` suffix in the module only. | 2 |
| 5, 6, 13, 14, 16, 17 | Renderer / scene / audio crates. | — |

## Deviations

- Objects the reference abandons are kept when the cause is data, not code (policy 2): `[COMBO]` without `combo` (ignored) or with a non-integer default (truncated), malformed vector defaults (missing components 0, a number is broadcast), missing defaults (0), non-numeric `float`/`int` string defaults (0). Each case is a warning in `PassShader::warnings`.
- Supersets of the reference language (never change a shader the reference accepts): `int → uint` in assignments, GLSL 4.00 tie-break when two strict candidates exist, `texture2D`/`texture2DLod`, integer `in` redeclarations of `gl_InstanceID`/`gl_VertexID`, global initialisers that are not constant (run at the start of `main`).
- `mul`, `log10` and `fmod` expansions are fully parenthesised (function-call semantics).
- Uniforms with initialisers: the initialiser is ignored (warning).
- Implicit-LOD lookups in vertex shaders use level 0 (as GL does).

## Real content (`docs/TESTING.md`, nothing copied into the repository)

`LUMEN_WE_ASSETS=… cargo test -p lumen-shader -- --ignored --nocapture` (add `--features render-contract` to also run lumen-render's own reflection, clip fix, validation and wgpu module creation on a GPU). Each pass also goes through naga's SPIR-V and HLSL back-ends.

- Reference wallpaper 3018340632: **23/23** passes (image, 11 effects with the scene overrides, 4 particle systems).
- WE `assets`: **93/95** material passes, **53/56** GLSL shader pairs with default combos, **74/95** single-combo variations. Failure classes, all outside spec 03: macros/types not in §3.3 (`CASTF`, `DECLARE_SAMPLER2D_PARAMETER`, `sampler2DBackBuffer`), engine-provided defines (`BLENDROWCOUNT` for puppets, `BONECOUNT` for skinning, which is 0 by default). The `HLSL/` folder (D3D sources) is skipped.

Timings (release, 32 threads): the 23 reference passes in 1.9 ms (12 ms on one thread), 95 asset materials in 2.7 ms.

## Limits

- Not supported: interface blocks, arrays of arrays, initializer lists, `double`, struct-typed uniforms, sampler arrays, non-2D samplers (the contract binds 2D textures only), more than one fragment output, arrays inside a struct constructor argument (naga GLSL front-end).
- Spans are not stored in the cache (errors from wgpu on a cached module have no source location).

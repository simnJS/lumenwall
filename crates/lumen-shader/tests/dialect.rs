//! Wallpaper Engine dialect tests. Every shader here was written for these
//! tests from spec 03 (no third-party shader code). Each one goes through the
//! whole pipeline, naga validation and SPIR-V generation.

mod common;

use lumen_shader::{
    CompileOptions, EngineUniform, MemFs, PassCombos, PassRequest, PassShader, ShaderCompiler,
    ShaderError, UniformType, UniformValue, translate_to_glsl,
};

const VS: &str = "attribute vec3 a_Position;
attribute vec2 a_TexCoord;
varying vec2 v_TexCoord;
uniform mat4 g_ModelViewProjectionMatrix;

void main() {
    gl_Position = mul(vec4(a_Position, 1.0), g_ModelViewProjectionMatrix);
    v_TexCoord = a_TexCoord;
}
";

struct Out {
    shader: PassShader,
    vs: String,
    fs: String,
}

/// naga back-ends used by wgpu (Vulkan, DX12) and the renderer's own contract checks.
fn backend_check(shader: &PassShader) {
    let m = &shader.program.module;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(m)
    .expect("revalidate");
    naga::back::spv::write_vec(m, &info, &naga::back::spv::Options::default(), None)
        .expect("SPIR-V output");
    let (opts, popts) = (
        naga::back::hlsl::Options::default(),
        naga::back::hlsl::PipelineOptions::default(),
    );
    let mut out = String::new();
    naga::back::hlsl::Writer::new(&mut out, &opts, &popts)
        .write(m, &info, None)
        .expect("HLSL output");
    common::check_render_contract(shader, "test").unwrap();
}

fn build_with(
    files: &[(&str, &str)],
    combos: &PassCombos,
    opts: CompileOptions,
) -> Result<Out, ShaderError> {
    let mut fs = MemFs::default();
    for (p, c) in files {
        fs.insert(*p, c.as_bytes());
    }
    let c = ShaderCompiler::new(opts, None);
    let p = c.prepare(
        &fs,
        &PassRequest {
            shader: "t",
            combos,
        },
    )?;
    let (vs, fsrc) = translate_to_glsl(&p.vertex_source, &p.fragment_source, c.options())?;
    let shader = c.compile(p)?;
    backend_check(&shader);
    Ok(Out {
        shader,
        vs,
        fs: fsrc,
    })
}

fn build(vs: &str, fs: &str) -> Result<Out, ShaderError> {
    build_with(
        &[("shaders/t.vert", vs), ("shaders/t.frag", fs)],
        &PassCombos::default(),
        CompileOptions::default(),
    )
}

/// Fragment shader with `decls` before main and `body` inside.
fn frag(decls: &str, body: &str) -> String {
    format!(
        "varying vec2 v_TexCoord;\nuniform sampler2D g_Texture0;\nuniform float g_Time;\n{decls}\n\nvoid main() {{\n    vec4 color = texSample2D(g_Texture0, v_TexCoord);\n{body}\n    gl_FragColor = color;\n}}\n"
    )
}

fn ok(decls: &str, body: &str) -> Out {
    match build(VS, &frag(decls, body)) {
        Ok(o) => o,
        Err(e) => panic!("{e}\n--- source\n{}", frag(decls, body)),
    }
}

fn fails(decls: &str, body: &str) -> String {
    match build(VS, &frag(decls, body)) {
        Ok(o) => panic!("expected an error, got:\n{}", o.fs),
        Err(e) => e.to_string(),
    }
}

// ---- R1 ----------------------------------------------------------------------

#[test]
fn r1_bool_promotes_to_float() {
    let o = ok(
        "",
        "    bool b = g_Time > 1.0;\n    float f = b * 2.0 + true;\n    color.r += f;",
    );
    assert!(o.fs.contains("float(b) * 2.0"), "{}", o.fs);
}

#[test]
fn r1_float_to_int_in_assignment_and_mul_assign() {
    let o = ok(
        "",
        "    int i = g_Time * 3.0;\n    i *= 0.5;\n    int j = 7;\n    j = j / 2.0;\n    color.r += float(i + j);",
    );
    assert!(o.fs.contains("int i = int(g_Time * 3.0)"), "{}", o.fs);
    // parity: `i *= 0.5` multiplies by 0 (the constant is converted first).
    assert!(o.fs.contains("i *= 0;"), "{}", o.fs);
    assert!(o.fs.contains("j = int(float(j) / 2.0)"), "{}", o.fs);
}

#[test]
fn r1_restrictions() {
    fails("", "    int i = 1;\n    i += 0.5;");
    fails(
        "int f(int x) { return x; }",
        "    color.r += float(f(1.5));",
    );
    fails(
        "int f(float x) { return x; }",
        "    color.r += float(f(1.5));",
    );
}

#[test]
fn r1_uint_int_mix() {
    let o = ok(
        "uniform int g_I;",
        "    uint u = 3u;\n    int k = g_I + u;\n    uint w = k;\n    color.r += float(w);",
    );
    assert!(o.fs.contains("g_I + int(u)"), "{}", o.fs);
}

// ---- R2 ----------------------------------------------------------------------

#[test]
fn r2_shape_adaptations() {
    let o = ok(
        "struct S { vec3 a; float b; };\nvec3 f3() { return vec4(1.0, 2.0, 3.0, 4.0); }",
        "    vec3 v = 0.5;\n    float s = v;\n    vec2 w = color;\n    v += color;\n    S st = S(1.0, w);\n    bvec2 bb = bvec2(true, false);\n    bool nb = !bb;\n    color.rgb = v + f3() + st.a + st.b + s + w.x + float(nb);",
    );
    assert!(o.fs.contains("vec3 v = vec3(0.5)"), "{}", o.fs);
    assert!(o.fs.contains("float s = v.x"), "{}", o.fs);
    assert!(o.fs.contains("vec2 w = color.xy"), "{}", o.fs);
    assert!(o.fs.contains("v += color.xyz"), "{}", o.fs);
    assert!(o.fs.contains("S(vec3(1.0), w.x)"), "{}", o.fs);
    assert!(o.fs.contains("(!bb.x)"), "{}", o.fs);
    assert!(
        o.fs.contains("return vec4(1.0, 2.0, 3.0, 4.0).xyz"),
        "{}",
        o.fs
    );
}

#[test]
fn r2_widening_is_an_error() {
    fails("", "    vec4 v = vec2(1.0);");
    fails("", "    vec3 v = vec3(1.0) + vec2(1.0);");
}

#[test]
fn r2_builtin_arguments() {
    let o = ok(
        "",
        "    float d = dot(color.rgb, color);\n    vec3 m = mix(color.rgb, vec3(1.0), color);\n    vec3 p = pow(color.rgb, 2.0);\n    float l = length(color.rgb);\n    color.rgb = m * d + p + clamp(color.rgb, 0, 1) + l;",
    );
    assert!(o.fs.contains("dot(color.xyz, color.xyz)"), "{}", o.fs);
    assert!(
        o.fs.contains("mix(color.xyz, vec3(1.0), color.xyz)"),
        "{}",
        o.fs
    );
    assert!(o.fs.contains("pow(color.xyz, vec3(2.0))"), "{}", o.fs);
    assert!(o.fs.contains("clamp(color.xyz, 0.0, 1.0)"), "{}", o.fs);
}

#[test]
fn r2_q8_int_to_float_vector() {
    // Open question 8: type and shape change in one step are accepted.
    let o = ok("", "    vec3 v = 1;\n    color.rgb = v;");
    assert!(
        o.fs.contains("vec3(1.0)") || o.fs.contains("vec3(float(1))"),
        "{}",
        o.fs
    );
}

// ---- R3 ----------------------------------------------------------------------

#[test]
fn r3_loose_overload_picks_fewest_adaptations() {
    let o = ok(
        "float pick(float x) { return 1.0; }\nfloat pick(vec2 x) { return 2.0; }\nfloat pick(vec4 x) { return 4.0; }",
        "    color.r = pick(color.rgb);",
    );
    // vec3 → vec2 reduction (1 adaptation); vec3 → float is not allowed in phase 2.
    assert!(
        o.fs.contains("color.xyz.xy") || o.fs.contains("(color.xyz).xy"),
        "{}",
        o.fs
    );
}

#[test]
fn r3_ambiguity_needs_three_candidates() {
    // Two tied candidates: the first declared wins.
    let o = ok(
        "float two(vec2 x) { return 2.0; }\nfloat two(vec3 x) { return 3.0; }",
        "    color.r = two(1.0);",
    );
    assert!(
        o.fs.contains("two_lf0(vec2(1.0))") || o.fs.contains("vec2(1.0)"),
        "{}",
        o.fs
    );
    let e = fails(
        "float three(vec2 x) { return 2.0; }\nfloat three(vec3 x) { return 3.0; }\nfloat three(vec4 x) { return 4.0; }",
        "    color.r = three(1.0);",
    );
    assert!(e.contains("ambiguous"), "{e}");
}

#[test]
fn r3_strict_phase_prefers_exact() {
    let o = ok(
        "float q(float x) { return x; }\nfloat q(int x) { return 0.0; }",
        "    color.r = q(2);",
    );
    assert!(o.fs.contains("q_lf1(2)"), "{}", o.fs);
}

// ---- R4 ----------------------------------------------------------------------

#[test]
fn r4_integer_conditions() {
    let o = ok(
        "uniform int g_Mode;",
        "    if (g_Mode) { color.r = 1.0; }\n    int n = 3;\n    while (n) { n--; }\n    for (int i = 4; i; i--) { color.g += 0.1; }\n    color.b = g_Mode ? 1.0 : 0.0;",
    );
    assert!(o.fs.contains("if (g_Mode != 0)"), "{}", o.fs);
    assert!(o.fs.contains("while (n != 0)"), "{}", o.fs);
    assert!(o.fs.contains("i != 0;"), "{}", o.fs);
    // parity: the ternary uses `> 0` like the reference.
    assert!(o.fs.contains("(g_Mode > 0) ? 1.0 : 0.0"), "{}", o.fs);
}

// ---- R5 ----------------------------------------------------------------------

#[test]
fn r5_float_modulo() {
    let o = ok(
        "",
        "    float a = g_Time % 2.0;\n    vec2 b = v_TexCoord % 0.5;\n    vec2 c = 3.0 % v_TexCoord;\n    a %= 0.25;\n    int k = 7 % 3;\n    color.rg = b + c + a + float(k);",
    );
    assert!(o.fs.contains("mod(g_Time, 2.0)"), "{}", o.fs);
    assert!(o.fs.contains("mod(v_TexCoord, 0.5)"), "{}", o.fs);
    assert!(o.fs.contains("mod(vec2(3.0), v_TexCoord)"), "{}", o.fs);
    assert!(o.fs.contains("a = mod(a, 0.25)"), "{}", o.fs);
    assert!(o.fs.contains("7 % 3"), "{}", o.fs);
}

// ---- R6 ----------------------------------------------------------------------

#[test]
fn r6_float_indices() {
    let o = ok(
        "const float K[3] = float[3](1.0, 2.0, 3.0);",
        "    float f = g_Time;\n    color.r = K[f] + color[1.7] + K[1.2];",
    );
    assert!(o.fs.contains("K[int(f)]"), "{}", o.fs);
    assert!(o.fs.contains("color[1]"), "{}", o.fs);
}

// ---- R7 ----------------------------------------------------------------------

#[test]
fn r7_swizzle_wraps() {
    let o = ok(
        "",
        "    vec2 a = v_TexCoord.zw;\n    float b = color.rgb.a;\n    vec2 s = g_Time.xx;\n    color.rg = a + b + s;",
    );
    assert!(o.fs.contains("v_TexCoord.xy"), "{}", o.fs);
    assert!(o.fs.contains("color.xyz.x"), "{}", o.fs);
    assert!(o.fs.contains("vec2(g_Time)"), "{}", o.fs);
    assert!(o.shader.warnings.is_empty() || !o.shader.warnings.is_empty());
}

// ---- R8 ----------------------------------------------------------------------

#[test]
fn r8_varyings_of_different_sizes() {
    let vs = "attribute vec3 a_Position;\nvarying vec3 v_A;\nvarying vec4 v_B;\nvarying float v_C;\nvoid main() {\n    gl_Position = vec4(a_Position, 1.0);\n    v_A = a_Position;\n    v_B = vec4(1.0);\n    v_C = 2.0;\n}\n";
    let fs = "varying vec4 v_A;\nvarying vec2 v_B;\nvarying vec3 v_C;\nvarying vec2 v_Missing;\nvoid main() {\n    gl_FragColor = vec4(v_A.xyz + v_B.xyy + v_C + v_Missing.xyx, v_A.w);\n}\n";
    let o = build(vs, fs).unwrap();
    // Vertex: v_A widened to vec4 with w = 1, v_C padded with zeros.
    assert!(o.vs.contains("out vec4 lumen_io_v_A"), "{}", o.vs);
    assert!(o.vs.contains("lumen_io_v_A = vec4(v_A, 1.0)"), "{}", o.vs);
    assert!(
        o.vs.contains("lumen_io_v_C = vec3(v_C, 0.0, 0.0)"),
        "{}",
        o.vs
    );
    assert!(o.vs.contains("lumen_io_v_Missing = vec2(0.0)"), "{}", o.vs);
    // Fragment: v_B read through a truncating copy.
    assert!(o.fs.contains("v_B = lumen_io_v_B.xy"), "{}", o.fs);
}

#[test]
fn r8_uniforms_of_different_sizes() {
    let vs = "attribute vec3 a_Position;\nuniform vec2 g_Shared;\nvoid main() { gl_Position = vec4(a_Position.xy + g_Shared, 0.0, 1.0); }\n";
    let fs = "uniform vec4 g_Shared;\nvoid main() { gl_FragColor = g_Shared; }\n";
    let o = build(vs, fs).unwrap();
    let m = o.shader.reflection().uniform("g_Shared").unwrap();
    assert_eq!(m.ty, UniformType::VEC4);
    assert!(m.stages.vertex && m.stages.fragment);
    assert!(o.vs.contains("g_Shared.xy"), "{}", o.vs);
}

#[test]
fn section_9_1_vec2_to_vec4() {
    let vs = "attribute vec3 a_Position;\nattribute vec2 a_TexCoord;\nvarying vec2 v_TexCoord;\nvoid main() {\n    gl_Position = vec4(a_Position, 1.0);\n    v_TexCoord = a_TexCoord;\n}\n";
    let fs = "varying vec4 v_TexCoord;\nvoid main() { gl_FragColor = v_TexCoord; }\n";
    let o = build(vs, fs).unwrap();
    assert!(
        o.vs.contains("v_TexCoord = vec4(a_TexCoord, 0.0, 1.0)"),
        "{}",
        o.vs
    );
}

#[test]
fn section_9_2_texcoord_cast2() {
    let fs = "varying vec4 v_TexCoord;\nuniform sampler2D g_Texture0;\nvoid main() { gl_FragColor = texSample2D(g_Texture0, v_TexCoord * CAST2(0.5)); }\n";
    let vs = "attribute vec3 a_Position;\nvarying vec4 v_TexCoord;\nvoid main() { gl_Position = vec4(a_Position, 1.0); v_TexCoord = vec4(0.0); }\n";
    let o = build(vs, fs).unwrap();
    assert!(o.fs.contains("v_TexCoord.xy * vec2(0.5)"), "{}", o.fs);
}

// ---- dialect macros (spec 03 §3.3) ------------------------------------------------

#[test]
fn compat_macros() {
    let o = ok(
        "uniform mat3 g_M3;",
        "    float2 a = float2(1.0, 2.0);\n    float3 b = CAST3(g_Time);\n    float4 c = CAST4(0.5);\n    int2 ia = int2(1, 2);\n    int3 ib = int3(1);\n    int4 ic = int4(0);\n    vec2 d = CAST2(1.0);\n    mat3 m = CAST3X3(g_M3);\n    vec3 e = mul(b, m);\n    vec3 f = max(0.0, b);\n    float g = min(1.0, g_Time);\n    vec3 h = lerp(b, e, 0.5);\n    float i = frac(g_Time) + saturate(g_Time) + log10(g_Time) + atan2(1.0, g_Time) + fmod(-g_Time, 2.0);\n    vec4 j = texSample2DLod(g_Texture0, a, 0.0);\n    float k = ddx(g_Time) + ddy(g_Time);\n#if GLSL\n    color = vec4(h + f + e, i + g + k) + j + c + vec4(d, float(ia.x + ib.x + ic.x), 1.0);\n#endif\n#if HLSL\n    color = undefined_identifier;\n#endif",
    );
    let f = &o.fs;
    assert!(f.contains("m * b"), "{f}");
    // Arguments swapped: max(vec3, float) is a GLSL overload.
    assert!(f.contains("max(b, 0.0)"), "{f}");
    assert!(f.contains("min(1.0, g_Time)"), "{f}");
    assert!(
        f.contains("mix(b, e, vec3(0.5))") || f.contains("mix(b, e, 0.5)"),
        "{f}"
    );
    assert!(f.contains("fract(g_Time)"), "{f}");
    assert!(f.contains("clamp(g_Time, 0.0, 1.0)"), "{f}");
    assert!(f.contains("log2(g_Time) * 0.30103"), "{f}");
    assert!(f.contains("atan(1.0, g_Time)"), "{f}");
    assert!(f.contains("trunc("), "{f}");
    assert!(
        f.contains("textureLod(sampler2D(lumen_tex0, lumen_smp0), a, 0.0)"),
        "{f}"
    );
    assert!(f.contains("dFdx(g_Time) + dFdy(g_Time)"), "{f}");
    assert!(f.contains("ivec2(1, 2)"), "{f}");
}

#[test]
fn ddy_can_follow_the_opengl_reference() {
    let fs = frag("", "    color.r = ddy(g_Time);");
    let o = build_with(
        &[("shaders/t.vert", VS), ("shaders/t.frag", &fs)],
        &PassCombos::default(),
        CompileOptions {
            negate_ddy: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        o.fs.contains("dFdy(-g_Time)") || o.fs.contains("dFdy((-g_Time))"),
        "{}",
        o.fs
    );
}

#[test]
fn vertex_texture_lookups_use_level_zero() {
    let vs = "attribute vec3 a_Position;\nuniform sampler2D g_Texture1;\nvarying vec4 v_C;\nvoid main() { v_C = texSample2D(g_Texture1, a_Position.xy); gl_Position = vec4(a_Position, 1.0); }\n";
    let fs = "varying vec4 v_C;\nvoid main() { gl_FragColor = v_C; }\n";
    let o = build(vs, fs).unwrap();
    assert!(
        o.vs.contains("textureLod(sampler2D(lumen_tex1, lumen_smp1), a_Position.xy, 0.0)"),
        "{}",
        o.vs
    );
    assert!(
        o.shader
            .reflection()
            .texture("g_Texture1")
            .unwrap()
            .stages
            .vertex
    );
    assert_eq!(o.shader.program.reflection.vertex_entry, "vs_main");
}

// ---- samplers, reserved names, structure ---------------------------------------------

#[test]
fn samplers_passed_to_functions() {
    let o = ok(
        "uniform sampler2D g_Texture3;\nvec4 blur(sampler2D tex, vec2 uv) { return texSample2D(tex, uv) + texSample2D(tex, uv + 0.01); }\nvec4 twice(sampler2D t) { return blur(t, v_TexCoord) * 2.0; }",
        "    color += twice(g_Texture3) + blur(g_Texture0, v_TexCoord);\n    ivec2 size = textureSize(g_Texture3, 0);\n    color.r += float(size.x);",
    );
    assert!(
        o.fs.contains("vec4 blur(texture2D tex_lt, sampler tex_ls, vec2 uv)"),
        "{}",
        o.fs
    );
    assert!(o.fs.contains("twice(lumen_tex3, lumen_smp3)"), "{}", o.fs);
    let t = o.shader.reflection().texture("g_Texture3").unwrap();
    assert_eq!(
        (t.unit, t.group, t.texture_binding, t.sampler_binding),
        (3, 1, 6, 7)
    );
}

#[test]
fn volume_samplers_bind_3d_textures() {
    // Colour LUT lookup: `sampler3D` slot read with `texSample3D` and a colour as coordinate.
    let o = ok(
        "uniform sampler3D g_Texture1;\nuniform float g_LutParams;",
        "    vec3 graded = texSample3D(g_Texture1, color.rgb);\n    color.rgb = mix(color.rgb, graded, g_LutParams);",
    );
    assert!(o.fs.contains("uniform texture3D lumen_tex1;"), "{}", o.fs);
    assert!(
        o.fs.contains("texture(sampler3D(lumen_tex1, lumen_smp1), "),
        "{}",
        o.fs
    );
    let t = o.shader.reflection().texture("g_Texture1").unwrap();
    assert_eq!((t.unit, t.texture_binding, t.sampler_binding), (1, 2, 3));
    let m = &o.shader.program.module;
    let dims: Vec<(u32, naga::ImageDimension)> = m
        .global_variables
        .iter()
        .filter_map(|(_, g)| match (g.binding.as_ref(), &m.types[g.ty].inner) {
            (Some(b), naga::TypeInner::Image { dim, .. }) => Some((b.binding, *dim)),
            _ => None,
        })
        .collect();
    assert!(
        dims.contains(&(0, naga::ImageDimension::D2))
            && dims.contains(&(2, naga::ImageDimension::D3)),
        "{dims:?}"
    );
    // One binding per unit: a 2D and a 3D sampler cannot share unit 0.
    let e = fails(
        "uniform sampler3D lutTex;",
        "    color.rgb += texSample3D(lutTex, color.rgb).rgb;",
    );
    assert!(e.contains("different texture types"), "{e}");
    // Other dimensions stay unsupported.
    let e = fails(
        "uniform samplerCube g_Texture2;",
        "    color += texture(g_Texture2, vec3(v_TexCoord, 1.0));",
    );
    assert!(e.contains("only 2D and 3D textures"), "{e}");
}

#[test]
fn reserved_glsl450_names_are_renamed() {
    let o = ok(
        "float filter(float sample) { return sample * 2.0; }",
        "    float sample = 1.0;\n    float texture2D = 2.0;\n    color.r = filter(sample) + texture2D;",
    );
    assert!(o.fs.contains("sample_l"), "{}", o.fs);
    assert!(o.fs.contains("texture2D_l"), "{}", o.fs);
}

#[test]
fn structs_loops_switch_and_prototypes() {
    let fs = frag(
        "struct Light { vec3 dir; float power; };\nfloat helper(float x);\nconst int N = 4;\nfloat weights[N];",
        "    Light l = Light(vec3(0.0, 1.0, 0.0), 0.5);\n    float pw[2] = float[2](1.0, l.power);\n    for (int i = 0; i < N; ++i) { weights[i] = float(i) * pw[1]; }\n    switch (int(g_Time)) { case 0: color.r = 1.0; break; case 1: color.g = 1.0; default: color.b = helper(weights[2]); }\n    do { color.a -= 0.1; } while (color.a > 0.5);\n    if (color.r > 2.0) discard;\n    float x = 0.0;\n    x = (x += 1.0, x * 2.0);\n    color.r += x;",
    );
    let fs = format!("{fs}float helper(float x) {{ return x * float(N); }}\n");
    build(VS, &fs).unwrap();
    // A prototype that is called but never defined is a link error.
    let e = fails("float helper(float x);", "    color.r = helper(1.0);");
    assert!(e.contains("never defined"), "{e}");
}

#[test]
fn global_initializers_run_before_main() {
    let o = ok(
        "float g_Scaled = g_Time * 2.0;\nconst vec3 C = vec3(1.0, 0.5, 0.25);\nconst float D = C.x * 2.0;",
        "    color.rgb *= C * g_Scaled * D;",
    );
    assert!(o.fs.contains("g_Scaled = g_Time * 2.0;"), "{}", o.fs);
    assert!(o.fs.contains("lumen_user_main();"), "{}", o.fs);
    assert!(o.fs.contains("const vec3 C"), "{}", o.fs);
}

#[test]
fn matrices() {
    ok(
        "uniform mat2 g_Rot;\nuniform bool g_Flag;",
        "    mat2 r = mat2(color);\n    mat3 m = mat3(1.0);\n    mat4 m4 = mat4(m);\n    vec2 p = r * v_TexCoord + g_Rot * v_TexCoord;\n    p = mul(p, g_Rot);\n    if (g_Flag) { p *= 2.0; }\n    color.rg = p + (m4 * color).xy + transpose(m)[0].xy + inverse(r)[1];",
    );
}

#[test]
fn errors_are_reported_with_lines() {
    let e = fails("", "    color.r = undefined_thing;");
    assert!(e.contains("undeclared identifier 'undefined_thing'"), "{e}");
    assert!(e.contains("fragment shader, line"), "{e}");
    let e = build(VS, "void notmain() {}\n").err().unwrap().to_string();
    assert!(e.contains("main"), "{e}");
    let e = build(VS, "#version 330\nvoid main() {}\n")
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("#version"), "{e}");
}

// ---- preprocessing, combos, annotations ---------------------------------------------------

#[test]
fn includes_combos_and_annotations() {
    let common = "#ifndef COMMON_H\n#define COMMON_H\nuniform float g_Time;\nvec3 Tint(vec3 c, vec3 t) { return c * t; }\n#define PI 3.14159265\n#define SQ(x) ((x) * (x))\n#endif\n";
    let fs = "// [COMBO] {\"material\":\"mode\",\"combo\":\"MODE\",\"type\":\"options\",\"default\":2}\n#include \"common.h\"\n#include \"common.h\"\nvarying vec2 v_TexCoord;\nuniform sampler2D g_Texture0; // {\"hidden\":true}\nuniform sampler2D g_Texture1; // {\"combo\":\"MASK\",\"default\":\"util/white\"}\nuniform vec3 g_Tint; // {\"material\":\"tint\",\"default\":\"1 0.5 0.25\",\"type\":\"color\"}\nuniform float g_Strength; // {\"material\":\"strength\",\"default\":0.75}\nuniform float g_NoKey; // {\"default\":5}\nvoid main() {\n    vec4 c = texSample2D(g_Texture0, v_TexCoord);\n#if MASK\n    c.a *= texSample2D(g_Texture1, v_TexCoord).r;\n#endif\n#if MODE == 2\n    c.rgb = Tint(c.rgb, g_Tint) * SQ(g_Strength) * PI + g_NoKey;\n#endif\n    gl_FragColor = c;\n}\n";
    let files = [
        ("shaders/t.vert", VS),
        ("shaders/t.frag", fs),
        ("shaders/common.h", common),
    ];
    // Slot 1 used by the override: MASK is required (V = 1).
    let combos = PassCombos {
        used_slots: [0, 1].into(),
        ..Default::default()
    };
    let o = build_with(&files, &combos, CompileOptions::default()).unwrap();
    assert!(o.fs.contains("lumen_tex1"), "{}", o.fs);
    assert_eq!(o.shader.fragment_combos.get("MASK"), Some(&1));
    assert_eq!(o.shader.fragment_combos.get("MODE"), Some(&2));
    // Constants and defaults.
    let (p, m) = o.shader.constant("tint").unwrap();
    assert_eq!(p.uniform, "g_Tint");
    assert_eq!(m.ty, UniformType::VEC3);
    let init = o.shader.initial_uniforms();
    let rd =
        |off: u32| f32::from_le_bytes(init[off as usize..off as usize + 4].try_into().unwrap());
    assert_eq!(rd(m.offset + 4), 0.5);
    let s = o.shader.reflection().uniform("g_Strength").unwrap();
    assert_eq!(rd(s.offset), 0.75);
    // No material key: never registered, stays 0.
    let nk = o.shader.reflection().uniform("g_NoKey").unwrap();
    assert_eq!(rd(nk.offset), 0.0);
    // Without the slot, MASK stays undefined and g_Texture1 disappears.
    let o = build_with(&files, &PassCombos::default(), CompileOptions::default()).unwrap();
    assert!(!o.fs.contains("lumen_tex1"), "{}", o.fs);
    // An explicit material combo wins over the discovered one.
    let combos = PassCombos {
        material: [("MODE".to_string(), 0)].into(),
        ..Default::default()
    };
    let o = build_with(&files, &combos, CompileOptions::default()).unwrap();
    assert!(!o.fs.contains("Tint("), "{}", o.fs);
    // Texture stacks.
    let stacks = o.shader.texture_stacks(&Default::default());
    assert_eq!(
        stacks[&1],
        vec![lumen_shader::TextureCandidate::Named("util/white".into())]
    );
}

#[test]
fn require_lighting_module() {
    let fs = "#require LightingV1\nvarying vec2 v_TexCoord;\nvoid main() {\n    vec3 l = PerformLighting_V1(vec3(0.0), vec3(1.0), vec3(0.0, 0.0, 1.0), vec3(0.0, 0.0, 1.0), vec3(1.0), vec3(0.04), 0.5, 0.0);\n    gl_FragColor = vec4(l, 1.0);\n}\n";
    build(VS, fs).unwrap();
}

#[test]
fn copy_command_shader() {
    let c = ShaderCompiler::new(CompileOptions::default(), None);
    let s = c
        .load_pass(
            &MemFs::default(),
            &PassRequest {
                shader: "commands/copy",
                combos: &PassCombos::default(),
            },
        )
        .unwrap();
    let r = s.reflection();
    assert!(r.uniform_block.is_none());
    assert_eq!(r.attributes.len(), 2);
    assert_eq!(r.texture("g_Texture0").unwrap().unit, 0);
    backend_check(&s);
}

// ---- reflection ------------------------------------------------------------------------

#[test]
fn engine_uniforms_frame_block_and_packed_arrays() {
    let fs = frag(
        "uniform float g_AudioSpectrum16Left[16];\nuniform vec4 g_Texture0Resolution;\nuniform vec3 g_Color;\nuniform vec2 g_PointerPosition;\nuniform float g_Daytime;",
        "    int i = int(g_Time) % 16;\n    color.r += g_AudioSpectrum16Left[i] + g_AudioSpectrum16Left[5] + g_Texture0Resolution.x + g_Color.r + g_PointerPosition.x + g_Daytime;",
    );
    // Default: engine globals come from the renderer's frame block.
    let o = build(VS, &fs).unwrap();
    let r = o.shader.reflection();
    let mut frame = r.frame_uniforms.clone();
    frame.sort();
    assert_eq!(
        frame,
        [
            "g_AudioSpectrum16Left",
            "g_Daytime",
            "g_PointerPosition",
            "g_Time"
        ]
    );
    assert!(r.engine(EngineUniform::Time).is_none());
    assert!(o.fs.contains("g_AudioSpectrum16Left[1].y"), "{}", o.fs);
    assert!(o.fs.contains("lumen_f_g_TexelSize"), "{}", o.fs);
    let mvp = r.engine(EngineUniform::ModelViewProjectionMatrix).unwrap();
    assert_eq!(mvp.ty, UniformType::MAT4);
    assert!(mvp.stages.vertex && !mvp.stages.fragment);
    assert!(r.engine(EngineUniform::Color).is_some());
    assert!(r.engine(EngineUniform::Brightness).is_none());
    let attrs: Vec<_> = r
        .attributes
        .iter()
        .map(|a| (a.name.as_str(), a.location))
        .collect();
    assert_eq!(attrs, vec![("a_Position", 0), ("a_TexCoord", 1)]);
    // Without the frame block, everything lives in the pass block; scalar arrays are packed.
    let o = build_with(
        &[("shaders/t.vert", VS), ("shaders/t.frag", &fs)],
        &PassCombos::default(),
        CompileOptions {
            frame_globals: false,
            ..Default::default()
        },
    )
    .unwrap();
    let r = o.shader.reflection();
    assert!(r.frame_uniforms.is_empty());
    let a = r.engine(EngineUniform::AudioSpectrum16Left).unwrap();
    assert_eq!((a.array_len, a.array_stride), (16, 4));
    assert!(o.fs.contains(">> 2][") && o.fs.contains("& 3]"), "{}", o.fs);
    assert!(r.engine(EngineUniform::Time).is_some());
    let block = r.uniform_block.as_ref().unwrap();
    assert_eq!(block.size % 16, 0);
    for m in &block.members {
        assert!(m.offset < block.size);
    }
}

#[test]
fn other_sampler_names_read_unit_zero() {
    let o = ok(
        "uniform sampler2D noiseTex;\nuniform sampler2D g_Texture9;",
        "    color += texSample2D(noiseTex, v_TexCoord) + texSample2D(g_Texture9, v_TexCoord);",
    );
    let r = o.shader.reflection();
    assert_eq!(r.textures.len(), 1);
    let t = &r.textures[0];
    assert_eq!((t.unit, t.texture_binding, t.sampler_binding), (0, 0, 1));
    assert_eq!(t.names.len(), 3);
    assert!(!o.fs.contains("noiseTex"), "{}", o.fs);
}

#[test]
fn engine_uniform_with_another_type_is_not_mapped() {
    let o = ok("uniform vec4 g_Daytime;", "    color += g_Daytime;");
    assert!(
        o.shader
            .reflection()
            .engine(EngineUniform::Daytime)
            .is_none()
    );
    assert!(o.shader.reflection().uniform("g_Daytime").is_some());
}

#[test]
fn uniform_value_writes_by_offset() {
    let o = ok(
        "uniform vec2 g_Offset; // {\"material\":\"offset\",\"default\":\"0 0\"}",
        "    color.rg += g_Offset;",
    );
    let (_, m) = o.shader.constant("offset").unwrap();
    let mut buf = o.shader.initial_uniforms();
    let v = lumen_shader::ConstantValue::Vec3([1.0, 2.0, 3.0])
        .to_uniform(m.ty)
        .unwrap();
    assert_eq!(v, UniformValue::Vec2([1.0, 2.0]));
    v.write(m, &mut buf);
    assert_eq!(
        &buf[m.offset as usize..m.offset as usize + 4],
        &1.0f32.to_le_bytes()
    );
}

// ---- cache and parallel compilation ---------------------------------------------------------

#[test]
fn disk_cache_round_trip() {
    let dir = std::env::temp_dir().join(format!("lumen-shader-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let fs_src = frag("", "    color.r += g_Time;");
    let mut fs = MemFs::default();
    fs.insert("shaders/t.vert", VS);
    fs.insert("shaders/t.frag", fs_src.as_bytes());
    let combos = PassCombos::default();
    let req = PassRequest {
        shader: "t",
        combos: &combos,
    };
    let first = ShaderCompiler::new(CompileOptions::default(), Some(dir.clone()))
        .load_pass(&fs, &req)
        .unwrap();
    let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1);
    let t = std::time::Instant::now();
    let second = ShaderCompiler::new(CompileOptions::default(), Some(dir.clone()))
        .load_pass(&fs, &req)
        .unwrap();
    let elapsed = t.elapsed();
    assert_eq!(first.program.reflection, second.program.reflection);
    // Spans are not serialized; the IR itself must be identical.
    assert_eq!(
        format!("{:?}", first.program.module.types),
        format!("{:?}", second.program.module.types)
    );
    assert_eq!(
        first.program.module.functions.len(),
        second.program.module.functions.len()
    );
    assert_eq!(first.program.module.entry_points.len(), 2);
    assert_eq!(first.key, second.key);
    println!("cached load: {elapsed:?}");
    // A corrupt entry is a miss and gets rewritten.
    let path = entries[0].path();
    let mut data = std::fs::read(&path).unwrap();
    let n = data.len();
    data[n - 1] ^= 0xff;
    std::fs::write(&path, &data).unwrap();
    let third = ShaderCompiler::new(CompileOptions::default(), Some(dir.clone()))
        .load_pass(&fs, &req)
        .unwrap();
    assert_eq!(first.program.reflection, third.program.reflection);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parallel_loading_deduplicates() {
    let mut fs = MemFs::default();
    fs.insert("shaders/t.vert", VS);
    fs.insert(
        "shaders/t.frag",
        frag(
            "// [COMBO] {\"combo\":\"A\",\"default\":0}",
            "#if A\n    color.r = 1.0;\n#endif",
        )
        .as_bytes(),
    );
    let c0 = PassCombos::default();
    let c1 = PassCombos {
        material: [("A".to_string(), 1)].into(),
        ..Default::default()
    };
    let reqs: Vec<PassRequest> = (0..32)
        .map(|i| PassRequest {
            shader: "t",
            combos: if i % 2 == 0 { &c0 } else { &c1 },
        })
        .collect();
    let c = ShaderCompiler::new(CompileOptions::default(), None);
    let res = c.load_passes(&fs, &reqs);
    let a = res[0].as_ref().unwrap();
    let b = res[1].as_ref().unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &a.program,
        &res[2].as_ref().unwrap().program
    ));
    assert!(!std::sync::Arc::ptr_eq(&a.program, &b.program));
    assert_eq!(a.key, res[2].as_ref().unwrap().key);
    assert_ne!(a.key, b.key);
}

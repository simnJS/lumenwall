// Internal pass shaders following the pass binding contract (see shader.rs).
// Vertex positions are in the upstream clip convention: lumen-render appends
// the wgpu correction like for every WE shader.
//
// The bloom fragments are stand-ins for the WE asset materials
// (downsample_quarter_bloom, downsample_eighth_blur_v, blur_h_bloom, combine):
// same passes, inputs and sizes, approximate filters. Parity comes from the
// real WE shaders translated by lumen-shader.

struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_Texture0Resolution: vec4<f32>,
    g_BloomStrength: f32,
    g_BloomThreshold: f32,
}

@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
@group(1) @binding(2) var g_Texture1: texture_2d<f32>;
@group(1) @binding(3) var g_Texture1_s: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// genericimage-like: MVP · position.
@vertex
fn vs_generic(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    return o;
}

// `commands/copy` (spec 03 §2.4): position emitted as-is, no matrix.
@vertex
fn vs_copy(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    return o;
}

@fragment
fn fs_copy(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv);
}

fn texel() -> vec2<f32> {
    return 1.0 / max(u.g_Texture0Resolution.xy, vec2<f32>(1.0));
}

fn tap(uv: vec2<f32>) -> vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, uv);
}

// Quarter-size downsample (4 bilinear taps = 4x4 texels) with threshold.
@fragment
fn fs_bloom_quarter(i: VOut) -> @location(0) vec4<f32> {
    let t = texel();
    let c = 0.25 * (tap(i.uv + t * vec2<f32>(-1.0, -1.0)) + tap(i.uv + t * vec2<f32>(1.0, -1.0))
        + tap(i.uv + t * vec2<f32>(-1.0, 1.0)) + tap(i.uv + t * vec2<f32>(1.0, 1.0)));
    let bright = max(c.rgb - vec3<f32>(u.g_BloomThreshold), vec3<f32>(0.0));
    return vec4<f32>(bright * u.g_BloomStrength, 1.0);
}

fn gauss(k: i32) -> f32 {
    var w = array<f32, 5>(0.2270, 0.1945, 0.1216, 0.0540, 0.0162);
    return w[abs(k)];
}

// Half-size downsample of the quarter buffer with a vertical 9-tap blur.
@fragment
fn fs_bloom_eighth_v(i: VOut) -> @location(0) vec4<f32> {
    let t = texel();
    var c = vec3<f32>(0.0);
    for (var k = -4; k <= 4; k++) {
        c += gauss(k) * tap(i.uv + vec2<f32>(0.0, 2.0 * f32(k) * t.y)).rgb;
    }
    return vec4<f32>(c, 1.0);
}

// Horizontal 9-tap blur.
@fragment
fn fs_bloom_h(i: VOut) -> @location(0) vec4<f32> {
    let t = texel();
    var c = vec3<f32>(0.0);
    for (var k = -4; k <= 4; k++) {
        c += gauss(k) * tap(i.uv + vec2<f32>(f32(k) * t.x, 0.0)).rgb;
    }
    return vec4<f32>(c, 1.0);
}

// Scene copy (slot 0) + blurred highlights (slot 1).
@fragment
fn fs_bloom_combine(i: VOut) -> @location(0) vec4<f32> {
    let a = textureSample(g_Texture0, g_Texture0_s, i.uv);
    let b = textureSample(g_Texture1, g_Texture1_s, i.uv);
    return vec4<f32>(a.rgb + b.rgb, a.a);
}

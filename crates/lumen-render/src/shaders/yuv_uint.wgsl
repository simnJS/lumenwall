// YUV → RGB conversion of a decoded video frame into an RGBA8 target (spec 07 §14).
// 9-16-bit planes stored in R16Uint / Rg16Uint (no 16-bit normalized formats needed),
// read with textureLoad and filtered by hand. Same parameters as yuv_float.wgsl.

struct Params {
    r: vec4<f32>,
    g: vec4<f32>,
    b: vec4<f32>,
    chroma: vec4<f32>,
    dst: vec4<f32>,
    crop: vec4<f32>,
}

@group(0) @binding(0) var plane0: texture_2d<u32>;
@group(0) @binding(1) var plane1: texture_2d<u32>;
@group(0) @binding(2) var plane2: texture_2d<u32>;
@group(0) @binding(4) var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

// Bilinear filter of the first two channels at `uv` (normalized), clamp to edge.
fn bilinear(t: texture_2d<u32>, uv: vec2<f32>) -> vec2<f32> {
    let size = vec2<i32>(textureDimensions(t));
    let c = uv * vec2<f32>(size) - 0.5;
    let f = fract(c);
    let i0 = vec2<i32>(floor(c));
    let lo = clamp(i0, vec2<i32>(0), size - 1);
    let hi = clamp(i0 + 1, vec2<i32>(0), size - 1);
    let t00 = vec2<f32>(textureLoad(t, vec2<i32>(lo.x, lo.y), 0).rg);
    let t10 = vec2<f32>(textureLoad(t, vec2<i32>(hi.x, lo.y), 0).rg);
    let t01 = vec2<f32>(textureLoad(t, vec2<i32>(lo.x, hi.y), 0).rg);
    let t11 = vec2<f32>(textureLoad(t, vec2<i32>(hi.x, hi.y), 0).rg);
    return mix(mix(t00, t10, f.x), mix(t01, t11, f.x), f.y);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy * params.dst.zw * params.crop.xy;
    let y = bilinear(plane0, uv).x;
    let csize = vec2<f32>(textureDimensions(plane1));
    let cuv = uv + params.chroma.xy / csize;
    var c: vec2<f32>;
    if (params.chroma.z > 0.5) {
        c = vec2<f32>(bilinear(plane1, cuv).x, bilinear(plane2, cuv).x);
    } else {
        c = bilinear(plane1, cuv);
    }
    if (params.chroma.w > 0.5) {
        c = c.yx;
    }
    let v = vec4<f32>(y, c, 1.0);
    let rgb = vec3<f32>(dot(params.r, v), dot(params.g, v), dot(params.b, v));
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

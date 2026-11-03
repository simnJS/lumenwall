// YUV → RGB conversion of a decoded video frame into an RGBA8 target (spec 07 §14).
// 8-bit planes (R8Unorm / Rg8Unorm), sampled with a linear clamp-to-edge sampler.
// Written for wgpu directly: no clip fix.

struct Params {
    // rgb = (dot(r, v), dot(g, v), dot(b, v)) with v = (Y, Cb, Cr, 1) in sample units.
    r: vec4<f32>,
    g: vec4<f32>,
    b: vec4<f32>,
    // xy: chroma siting offset in chroma texels; z: 1 = three planes (Y, U, V);
    // w: 1 = interleaved plane stores V before U.
    chroma: vec4<f32>,
    // Target size in pixels: (w, h, 1/w, 1/h).
    dst: vec4<f32>,
    // xy: visible part of the planes (decoders pad the picture to their alignment).
    crop: vec4<f32>,
}

@group(0) @binding(0) var plane0: texture_2d<f32>;
@group(0) @binding(1) var plane1: texture_2d<f32>;
@group(0) @binding(2) var plane2: texture_2d<f32>;
@group(0) @binding(3) var smp: sampler;
@group(0) @binding(4) var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // Row 0 of the frame is the top row of the target.
    let uv = pos.xy * params.dst.zw * params.crop.xy;
    let y = textureSampleLevel(plane0, smp, uv, 0.0).r;
    let csize = vec2<f32>(textureDimensions(plane1));
    let cuv = uv + params.chroma.xy / csize;
    var c: vec2<f32>;
    if (params.chroma.z > 0.5) {
        c = vec2<f32>(
            textureSampleLevel(plane1, smp, cuv, 0.0).r,
            textureSampleLevel(plane2, smp, cuv, 0.0).r,
        );
    } else {
        c = textureSampleLevel(plane1, smp, cuv, 0.0).rg;
    }
    if (params.chroma.w > 0.5) {
        c = c.yx;
    }
    let v = vec4<f32>(y, c, 1.0);
    let rgb = vec3<f32>(dot(params.r, v), dot(params.g, v), dot(params.b, v));
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

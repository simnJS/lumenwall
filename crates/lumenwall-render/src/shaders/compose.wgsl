// Final composition (spec 04 §21.1). Written for wgpu directly: no clip fix.
// One oversized triangle covers the viewport set to the output region.

struct Params {
    // u0, u1, v0, v1
    uv: vec4<f32>,
    // x: 1 = transparent border outside [0, 1] (clamp mode `border`)
    mode: vec4<f32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // (s, t): s = 0 left, t = 0 top of the output region
    @location(0) st: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var o: VsOut;
    o.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.st = vec2<f32>(x, y);
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = vec2<f32>(
        mix(params.uv.x, params.uv.y, in.st.x),
        mix(params.uv.z, params.uv.w, in.st.y),
    );
    var c = textureSampleLevel(src, src_sampler, uv, 0.0);
    if (params.mode.x > 0.5 && (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)))) {
        c = vec4<f32>(0.0);
    }
    // The output is always presented opaque (spec 04 §11.6, Q11).
    return vec4<f32>(c.rgb, 1.0);
}

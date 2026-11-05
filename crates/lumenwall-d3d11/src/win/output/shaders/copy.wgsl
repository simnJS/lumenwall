// Copy of an external frame (shared BGRA or RGBA texture) into a target, by draw. The view hides
// the channel order; colour and alpha are copied as they are. Same size: texel centres map to
// texel centres, so the copy is exact; otherwise the sampler filters.

struct Params {
    // Target size in pixels: (w, h, 1/w, 1/h).
    dst: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(1) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, smp, pos.xy * params.dst.zw, 0.0);
}

// Lumenwall stand-in shaders, used when a Wallpaper Engine shader cannot be translated (or
// lumenwall-shader is not built in). They follow the lumenwall-render pass binding contract:
// pass block at @group(0) @binding(0), slot N at @group(1) @binding(2N / 2N+1), vertex
// position in the upstream clip convention (lumenwall-render appends the wgpu correction).
//
// They are not Wallpaper Engine shaders: the image shows without its effects, particles are
// plain camera-facing sprites. `TEXFMT` is replaced before parsing: 0 RGBA, 1 RG8, 2 R8.

struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_Color: vec3<f32>,
    g_Alpha: f32,
    g_Brightness: f32,
    g_RenderVar0: vec4<f32>,
    g_RenderVar1: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;

const TEXFMT: i32 = __TEXFMT__;
const TRAIL: bool = __TRAIL__;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

fn texel(uv: vec2<f32>) -> vec4<f32> {
    let t = textureSample(g_Texture0, g_Texture0_s, uv);
    if (TEXFMT == 1) {
        return vec4<f32>(t.rrr, t.g);
    }
    if (TEXFMT == 2) {
        return vec4<f32>(1.0, 1.0, 1.0, t.r);
    }
    return t;
}

// Material and effect passes: MVP · position, texture coordinates as is.
@vertex
fn vs_image(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    o.color = vec4<f32>(1.0);
    return o;
}

// `copy` command: position already in clip space.
@vertex
fn vs_copy(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    o.color = vec4<f32>(1.0);
    return o;
}

// Base pass of an image: texture tinted by the image colour, alpha and brightness.
@fragment
fn fs_image(i: VOut) -> @location(0) vec4<f32> {
    let c = texel(i.uv);
    return vec4<f32>(c.rgb * u.g_Color * u.g_Brightness, c.a * u.g_Alpha);
}

// Effect passes and copies: the input unchanged.
@fragment
fn fs_passthrough(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv);
}

// Sprite particles (spec 05 §12.3 vertex format): one quad per particle, expanded here
// around the particle centre in the system's local XY plane, rotated by rotation.z; with
// TRAIL the quad is stretched backwards along the velocity (g_RenderVar0 = length, max, min).
@vertex
fn vs_sprite(
    @location(0) a_Position: vec3<f32>,
    @location(1) a_TexCoordVec4: vec4<f32>,
    @location(2) a_Color: vec4<f32>,
    @location(3) a_TexCoordVec4C1: vec4<f32>,
) -> VOut {
    let corner = a_TexCoordVec4.xy;
    let size = a_TexCoordVec4.w;
    var local = vec2<f32>(corner.x - 0.5, 0.5 - corner.y) * size;
    var p = a_Position;
    if (TRAIL) {
        let v = a_TexCoordVec4C1.xy;
        let speed = length(v);
        let dir = select(vec2<f32>(0.0, 1.0), v / max(speed, 1e-6), speed > 1e-6);
        let side = vec2<f32>(-dir.y, dir.x);
        let len = clamp(speed * u.g_RenderVar0.x, u.g_RenderVar0.z, max(u.g_RenderVar0.y, u.g_RenderVar0.z));
        // Head at the particle, tail behind it.
        let along = (0.5 - corner.y) * (len + size) - 0.5 * len;
        p = vec3<f32>(p.xy + side * local.x + dir * along, p.z);
    } else {
        let a = a_TexCoordVec4.z;
        let c = cos(a);
        let s = sin(a);
        local = vec2<f32>(local.x * c - local.y * s, local.x * s + local.y * c);
        p = vec3<f32>(p.xy + local, p.z);
    }
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(p, 1.0);
    var uv = corner;
    let sheet = u.g_RenderVar1;
    if (sheet.z > 0.0) {
        let frames = sheet.z;
        let frame = floor(fract(a_TexCoordVec4C1.w) * frames);
        let cols = max(1.0, round(1.0 / max(sheet.x, 1e-6)));
        let col = frame - cols * floor(frame / cols);
        let row = floor(frame / cols);
        uv = vec2<f32>((col + corner.x) * sheet.x, (row + corner.y) * sheet.y);
    }
    o.uv = uv;
    o.color = a_Color;
    return o;
}

// Rope particles (spec 05 §12.4 vertex format): a flat ribbon between start and end.
@vertex
fn vs_rope(
    @location(0) a_PositionVec4: vec4<f32>,
    @location(1) a_TexCoordVec4: vec4<f32>,
    @location(2) a_TexCoordVec4C1: vec4<f32>,
    @location(3) a_TexCoordVec4C2: vec4<f32>,
    @location(4) a_TexCoordVec4C3: vec4<f32>,
    @location(5) a_TexCoordC4: vec2<f32>,
    @location(6) a_Color: vec4<f32>,
) -> VOut {
    let start = a_PositionVec4.xyz;
    let end = a_TexCoordVec4.xyz;
    let uv = a_TexCoordC4;
    let d = end.xy - start.xy;
    let n = normalize(select(vec2<f32>(0.0, 1.0), vec2<f32>(-d.y, d.x), length(d) > 1e-6));
    let at_end = uv.x > 0.5;
    let base = select(start, end, at_end);
    let width = select(a_PositionVec4.w, a_TexCoordVec4C2.w, at_end);
    let p = vec3<f32>(base.xy + n * (uv.y - 0.5) * width, base.z);
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(p, 1.0);
    let trail = a_TexCoordVec4C1.w + select(0.0, 1.0, at_end);
    o.uv = vec2<f32>(uv.y, trail / max(a_TexCoordVec4.w - 1.0, 1.0));
    o.color = select(a_Color, a_TexCoordVec4C3, at_end);
    return o;
}

@fragment
fn fs_particle(i: VOut) -> @location(0) vec4<f32> {
    let t = texel(i.uv);
    return vec4<f32>(t.rgb * i.color.rgb * u.g_Color * u.g_Brightness, t.a * i.color.a * u.g_Alpha);
}

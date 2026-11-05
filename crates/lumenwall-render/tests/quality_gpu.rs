//! GPU tests of the quality settings: multisampled scene target, 16-bit float targets and the
//! texture resolution. They need a Vulkan or DX12 adapter (`LUMENWALL_TEST_SOFTWARE=1` forces a
//! CPU one); without any adapter each test prints "skipped" and returns.

use std::sync::OnceLock;

use lumenwall_render::blend::BlendMode;
use lumenwall_render::graph::{Geometry, Op, PassDesc, SceneBuilder, SlotBinding};
use lumenwall_render::headless::read_texture_rgba8;
use lumenwall_render::shader::ShaderDesc;
use lumenwall_render::texture::{MipLevel, SamplerKey, TexFlags, TexelFormat, TextureUpload};
use lumenwall_render::{
    ContextOptions, GpuContext, Msaa, QualitySettings, RenderScene, TextureResolution, Vertex,
    WgpuReason,
};

const SRC: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    return o;
}
@fragment fn fs_white(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
@fragment fn fs_four(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(4.0, 4.0, 4.0, 1.0);
}
@fragment fn fs_quarter(i: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(g_Texture0, g_Texture0_s, i.uv);
    return vec4<f32>(c.rgb * 0.25, 1.0);
}
@fragment fn fs_copy(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv);
}
";

const SIDE: u32 = 32;

fn shared() -> Option<&'static GpuContext> {
    static CTX: OnceLock<Option<GpuContext>> = OnceLock::new();
    CTX.get_or_init(|| {
        GpuContext::new_headless(&ContextOptions {
            prefer_software: std::env::var_os("LUMENWALL_TEST_SOFTWARE").is_some(),
            ..ContextOptions::default()
        })
        .ok()
    })
    .as_ref()
}

macro_rules! gpu {
    () => {
        match shared() {
            Some(c) => c,
            None => {
                eprintln!("skipped: no GPU adapter");
                return;
            }
        }
    };
}

fn shader(b: &mut SceneBuilder<'_>, fs: &str) -> lumenwall_render::ShaderId {
    b.add_shader(ShaderDesc::from_wgsl(fs, SRC, "vs", fs).unwrap())
        .unwrap()
}

/// A triangle in clip space whose slanted edge crosses pixels.
fn triangle() -> [Vertex; 3] {
    let v = |x: f32, y: f32| Vertex {
        position: [x, y, 0.0],
        uv: [0.0, 0.0],
    };
    [v(-0.9, -0.8), v(0.85, -0.3), v(-0.2, 0.9)]
}

/// Renders a white triangle on black, with `msaa` on the scene target. Returns the scene, the
/// level in force and the pixels.
fn draw_triangle(ctx: &GpuContext, msaa: Msaa) -> (RenderScene, Msaa, Vec<u8>) {
    let mut b = SceneBuilder::new(ctx, (SIDE as f32, SIDE as f32));
    let st = b
        .targets
        .create_scene_targets(ctx, (SIDE, SIDE), SamplerKey::CLAMP_LINEAR);
    let level = if msaa == Msaa::Off {
        Msaa::Off
    } else {
        b.enable_msaa(st.full, msaa)
    };
    let white = shader(&mut b, "fs_white");
    let v = b.add_vertices(&triangle());
    let mut desc = PassDesc::new("tri", white, st.full, Geometry::Vertices(v));
    desc.blend = BlendMode::Normal;
    let pass = b.add_pass(desc).unwrap();
    b.push(Op::Clear {
        target: st.full,
        color: [0.0, 0.0, 0.0, 1.0],
    });
    b.push(Op::Draw(pass));
    let mut scene = b.build().unwrap();
    scene.render(ctx);
    let px = read_texture_rgba8(ctx, scene.targets().get(st.full).texture()).unwrap();
    (scene, level, px)
}

fn partial_pixels(px: &[u8]) -> usize {
    px.chunks_exact(4)
        .filter(|p| p[0] > 8 && p[0] < 247)
        .count()
}

#[test]
fn multisampling_antialiases_the_edges_and_keeps_the_interior() {
    let ctx = gpu!();
    let (flat, _, flat_px) = draw_triangle(ctx, Msaa::Off);
    let (ms, level, ms_px) = draw_triangle(ctx, Msaa::X4);
    if level == Msaa::Off {
        eprintln!("skipped: adapter has no 4x multisampling for Rgba8");
        return;
    }
    // Default: nothing changes the recording path; multisampling goes through wgpu.
    assert_ne!(flat.wgpu_reason(), Some(WgpuReason::Quality));
    assert_eq!(ms.wgpu_reason(), Some(WgpuReason::Quality));
    assert!(!ms.records_direct());
    // A hard edge has no intermediate value; a multisampled one has many.
    assert_eq!(partial_pixels(&flat_px), 0, "single-sampled edge is hard");
    assert!(
        partial_pixels(&ms_px) >= 10,
        "multisampled edge has coverage values, got {}",
        partial_pixels(&ms_px)
    );
    // Centre of the triangle and a far corner are the same either way.
    let at = |px: &[u8], x: u32, y: u32| px[((y * SIDE + x) * 4) as usize];
    assert_eq!(at(&flat_px, 14, 17), 255);
    assert_eq!(at(&ms_px, 14, 17), 255);
    assert_eq!(at(&flat_px, 31, 0), 0);
    assert_eq!(at(&ms_px, 31, 0), 0);
}

#[test]
fn unsupported_multisampling_falls_back_to_a_supported_level() {
    let ctx = gpu!();
    let mut b = SceneBuilder::new(ctx, (SIDE as f32, SIDE as f32));
    let st = b
        .targets
        .create_scene_targets(ctx, (SIDE, SIDE), SamplerKey::CLAMP_LINEAR);
    let level = b.enable_msaa(st.full, Msaa::X8);
    assert!(level.samples() <= 8);
    let flags = ctx
        .adapter()
        .get_texture_format_features(wgpu::TextureFormat::Rgba8Unorm)
        .flags;
    assert!(level == Msaa::Off || flags.sample_count_supported(level.samples()));
}

/// Texel `x = 0, y = 0` of an Rgba16Float texture as `f32` red.
fn read_red_f16(ctx: &GpuContext, texture: &wgpu::Texture) -> f32 {
    let row = SIDE * 8;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = ctx.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("test.readback"),
        size: u64::from(padded) * u64::from(SIDE),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(SIDE),
            },
        },
        wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue().submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    ctx.wait_idle();
    rx.recv().unwrap().unwrap();
    let view = buffer.slice(..).get_mapped_range().unwrap();
    let bits = u16::from_le_bytes([view[0], view[1]]);
    half_to_f32(bits)
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f32::from(h & 0x3ff);
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        31 => sign * f32::INFINITY,
        e => sign * (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
    }
}

/// A pass writes 4.0, a second one (reading the scene target through its hazard copy) scales it
/// by a quarter: 8-bit targets clamp the first write to 1.0 and end at 0.25, float targets end
/// at 1.0.
fn scale_down_scene(ctx: &GpuContext, hdr: bool) -> (RenderScene, lumenwall_render::RtId) {
    let mut b = SceneBuilder::new(ctx, (SIDE as f32, SIDE as f32));
    if hdr {
        b.use_hdr_targets();
    }
    let st = b
        .targets
        .create_scene_targets(ctx, (SIDE, SIDE), SamplerKey::CLAMP_LINEAR);
    let four = shader(&mut b, "fs_four");
    let quarter = shader(&mut b, "fs_quarter");
    let quad = b.add_vertices(&lumenwall_render::math::unit_quad());
    let first = b
        .add_pass(PassDesc::new(
            "four",
            four,
            st.full,
            Geometry::Vertices(quad),
        ))
        .unwrap();
    let second = b
        .add_pass(
            PassDesc::new("quarter", quarter, st.full, Geometry::Vertices(quad))
                .with_slot(0, SlotBinding::target(st.full)),
        )
        .unwrap();
    b.push(Op::Draw(first));
    b.push(Op::Draw(second));
    let mut scene = b.build().unwrap();
    scene.render(ctx);
    (scene, st.full)
}

#[test]
fn hdr_targets_keep_values_above_one() {
    let ctx = gpu!();
    let (eight, rt) = scale_down_scene(ctx, false);
    let px = read_texture_rgba8(ctx, eight.targets().get(rt).texture()).unwrap();
    assert!(
        (i32::from(px[0]) - 64).abs() <= 1,
        "8-bit clamps: {}",
        px[0]
    );
    let (float, rt) = scale_down_scene(ctx, true);
    assert_eq!(
        float.targets().get(rt).texture().format(),
        wgpu::TextureFormat::Rgba16Float
    );
    assert_eq!(float.wgpu_reason(), Some(WgpuReason::Quality));
    let red = read_red_f16(ctx, float.targets().get(rt).texture());
    assert!((red - 1.0).abs() < 1e-3, "float keeps 4.0: got {red}");
}

#[test]
fn texture_resolution_drops_the_top_mip_levels() {
    let ctx = gpu!();
    let upload = |b: &mut SceneBuilder<'_>| {
        let l0 = vec![200u8; 16 * 16 * 4];
        let l1 = vec![150u8; 8 * 8 * 4];
        let l2 = vec![100u8; 4 * 4 * 4];
        b.add_texture(&TextureUpload {
            label: "mips",
            format: TexelFormat::Rgba8,
            levels: &[
                MipLevel {
                    width: 16,
                    height: 16,
                    data: &l0,
                },
                MipLevel {
                    width: 8,
                    height: 8,
                    data: &l1,
                },
                MipLevel {
                    width: 4,
                    height: 4,
                    data: &l2,
                },
            ],
            content_size: None,
            flags: TexFlags(TexFlags::CLAMP_UVS),
        })
        .unwrap()
    };
    let probe = |res: TextureResolution| {
        let mut b = SceneBuilder::new(ctx, (SIDE as f32, SIDE as f32));
        b.set_quality(QualitySettings {
            texture_resolution: res,
            ..Default::default()
        });
        let id = upload(&mut b);
        let t = b.texture(id).unwrap();
        (t.texture().width(), t.content_size())
    };
    assert_eq!(probe(TextureResolution::Full), ((16), (16, 16)));
    // Smaller in memory, same declared size: texture coordinates do not change.
    assert_eq!(probe(TextureResolution::Half), (8, (16, 16)));
    assert_eq!(probe(TextureResolution::Quarter), (4, (16, 16)));
}

#[test]
fn default_quality_costs_nothing_more() {
    let ctx = gpu!();
    let mut b = SceneBuilder::new(ctx, (SIDE as f32, SIDE as f32));
    assert!(b.quality().is_default());
    let st = b
        .targets
        .create_scene_targets(ctx, (SIDE, SIDE), SamplerKey::CLAMP_LINEAR);
    let copy = shader(&mut b, "fs_copy");
    let quad = b.add_vertices(&lumenwall_render::math::unit_quad());
    let p = b
        .add_pass(PassDesc::new("p", copy, st.full, Geometry::Vertices(quad)))
        .unwrap();
    b.push(Op::Draw(p));
    let scene = b.build().unwrap();
    let t = scene.targets().get(st.full);
    assert_eq!(t.samples(), 1);
    assert!(t.msaa_view().is_none());
    assert_eq!(t.texture().format(), wgpu::TextureFormat::Rgba8Unorm);
    assert_ne!(scene.wgpu_reason(), Some(WgpuReason::Quality));
}

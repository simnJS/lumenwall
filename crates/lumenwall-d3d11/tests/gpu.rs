//! Building blocks on a real Direct3D 11 device. WARP (the software rasteriser) by default, so
//! the workspace check never touches the GPU; `LUMENWALL_D3D11_TEST_ADAPTER=hardware` runs the
//! same tests on the default hardware adapter (under `%LOCALAPPDATA%\lumenwall-dev\gpu.lock`).

#![cfg(windows)]

use std::time::{Duration, Instant};

use lumenwall_d3d11::layout::{CONSTANT_WINDOW_ALIGN, Dimension, MipLevel};
use lumenwall_d3d11::windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use lumenwall_d3d11::windows::Win32::Graphics::Direct3D11::ID3D11Resource;
use lumenwall_d3d11::windows::core::Interface;
use lumenwall_d3d11::*;
use lumenwall_formats::tex::GpuFormat;

fn device() -> Device {
    let hardware = std::env::var("LUMENWALL_D3D11_TEST_ADAPTER").is_ok_and(|v| v == "hardware");
    Device::new(&DeviceOptions {
        adapter: if hardware {
            AdapterChoice::Default
        } else {
            AdapterChoice::Warp
        },
        ..DeviceOptions::default()
    })
    .expect("Direct3D 11 device")
}

fn program(
    device: &Device,
    wgsl: &str,
    cache: Option<&BytecodeCache>,
) -> (ShaderProgram, CacheOutcome) {
    let module = naga::front::wgsl::parse_str(wgsl).expect("test WGSL");
    let hlsl = hlsl::translate(&module, "vs_main", "fs_main").expect("HLSL");
    compile_program(device, &hlsl, cache, None, CompileOptions::default()).expect("program")
}

/// 51, 102, 153, 204 and 255 are exact in UNORM8.
fn unorm(v: u8) -> f32 {
    f32::from(v) / 255.0
}

#[test]
fn device_on_warp_or_hardware() {
    let d = device();
    let caps = d.caps();
    assert!(
        caps.feature_level >= 0xb000,
        "feature level {:#x}",
        caps.feature_level
    );
    assert!(caps.constant_buffer_offsetting);
    assert!(!d.info().name.is_empty());
    d.check_lost().unwrap();
    assert!(d.wait_idle(Duration::from_secs(10)).unwrap());
    d.trim().unwrap();
    let m = d.memory().unwrap();
    assert!(m.local.budget > 0 || m.non_local.budget > 0, "{m:?}");
}

#[test]
fn clear_present_readback_and_resize() {
    let d = device();
    let window = OffscreenWindow::new(64, 48).unwrap();
    let mut swap = SwapChain::for_hwnd(&d, window.hwnd(), (64, 48)).unwrap();
    for (size, rgba) in [
        ((64, 48), [51u8, 102, 204, 255]),
        ((80, 40), [204, 153, 51, 255]),
    ] {
        if size != swap.size() {
            window.resize(size.0, size.1).unwrap();
            swap.resize(&d, size).unwrap();
        }
        let ctx = d.context();
        // SAFETY: views of this device.
        unsafe {
            ctx.OMSetRenderTargets(Some(&[Some(swap.rtv().clone())]), None);
            ctx.ClearRenderTargetView(swap.rtv(), &rgba.map(unorm));
        }
        let (w, h, pixels) = read_texture_rgba8(&d, swap.back_buffer()).unwrap();
        assert_eq!((w, h), size);
        assert_eq!(pixels.len(), (w * h * 4) as usize);
        assert!(
            pixels.as_chunks::<4>().0.iter().all(|p| *p == rgba),
            "first pixel {:?}, expected {rgba:?}",
            &pixels[..4]
        );
        let status = swap.present(&d).unwrap();
        assert!(matches!(
            status,
            PresentStatus::Presented | PresentStatus::Occluded
        ));
        window.pump();
    }
    d.trim().unwrap();
}

/// Bytes `0, 1, 2…` (wrapping), seeded so levels differ.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

#[test]
fn every_texel_format_round_trips_with_mips_and_volumes() {
    let d = device();
    let formats = [
        GpuFormat::R8Unorm,
        GpuFormat::Rg8Unorm,
        GpuFormat::Rgba8Unorm,
        GpuFormat::Bc1RgbaUnorm,
        GpuFormat::Bc2RgbaUnorm,
        GpuFormat::Bc3RgbaUnorm,
        GpuFormat::Bc7RgbaUnorm,
        GpuFormat::R16Float,
        GpuFormat::Rg16Float,
        GpuFormat::Rgba16Float,
        GpuFormat::Rgb10a2Unorm,
    ];
    for format in formats {
        // 16×8 with every level down to 1×1 (BC levels below 4×4 are still one block).
        let sizes: Vec<(u32, u32)> = (0..5)
            .map(|k| ((16u32 >> k).max(1), (8u32 >> k).max(1)))
            .collect();
        let data: Vec<Vec<u8>> = sizes
            .iter()
            .enumerate()
            .map(|(k, &(w, h))| pattern(format.level_size(w, h) as usize, k as u8))
            .collect();
        let levels: Vec<MipLevel<'_>> = sizes
            .iter()
            .zip(&data)
            .map(|(&(w, h), d)| MipLevel {
                width: w,
                height: h,
                depth: 1,
                data: d,
            })
            .collect();
        let t = Texture::new(&d, format, Dimension::D2, &levels).unwrap();
        assert_eq!(t.mip_levels(), 5);
        for (k, expected) in data.iter().enumerate() {
            let back = read_subresource(&d, t.resource(), k as u32).unwrap();
            assert_eq!(&back.data, expected, "{format:?} level {k}");
            assert_eq!(back.format, dxgi_format(format));
        }
    }
    // A colour LUT-like volume: 4×4×4 RGBA8, three levels.
    let data: Vec<Vec<u8>> = (0..3)
        .map(|k| {
            let e = 4u32 >> k;
            pattern((e * e * e * 4) as usize, 40 + k as u8)
        })
        .collect();
    let levels: Vec<MipLevel<'_>> = data
        .iter()
        .enumerate()
        .map(|(k, d)| {
            let e = 4u32 >> k;
            MipLevel {
                width: e,
                height: e,
                depth: e,
                data: d,
            }
        })
        .collect();
    let v = Texture::new(&d, GpuFormat::Rgba8Unorm, Dimension::D3, &levels).unwrap();
    assert_eq!(v.size(), (4, 4, 4));
    for (k, expected) in data.iter().enumerate() {
        let back = read_subresource(&d, v.resource(), k as u32).unwrap();
        assert_eq!(&back.data, expected, "volume level {k}");
        let e = 4u32 >> k;
        assert_eq!(back.size, (e, e, e));
    }
    // Invalid uploads are errors, not driver crashes.
    let bad = [MipLevel {
        width: 4,
        height: 4,
        depth: 1,
        data: &[0u8; 10],
    }];
    assert!(matches!(
        Texture::new(&d, GpuFormat::Rgba8Unorm, Dimension::D2, &bad),
        Err(Error::InvalidData(_))
    ));
}

#[test]
fn writable_texture_updates_level0() {
    let d = device();
    let t = Texture::new_writable(&d, GpuFormat::Rgba8Unorm, (2, 2), None).unwrap();
    let zeros = read_subresource(&d, t.resource(), 0).unwrap();
    assert!(zeros.data.iter().all(|&b| b == 0));
    let px: Vec<u8> = (0..16).collect();
    t.write_level0(&d, &px).unwrap();
    assert_eq!(read_subresource(&d, t.resource(), 0).unwrap().data, px);
    assert!(t.write_level0(&d, &px[..8]).is_err());
}

const PIPELINE_WGSL: &str = r"
struct Pass { color: vec4<f32>, offset: vec2<f32>, scale: f32, probe: vec3<f32>, after: f32, }
struct Frame { g_Time: f32, }
@group(0) @binding(0) var<uniform> pass_block: Pass;
@group(0) @binding(1) var<uniform> frame: Frame;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0Sampler: sampler;
struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) extra: vec4<f32>, }
@vertex fn vs_main(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>, @location(5) a_Unfed: vec4<f32>) -> VsOut {
    var o: VsOut;
    o.pos = vec4<f32>(a_Position.xy * pass_block.scale + pass_block.offset, a_Position.z, 1.0);
    o.uv = a_TexCoord;
    o.extra = a_Unfed;
    return o;
}
@fragment fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
    let t = textureSample(g_Texture0, g_Texture0Sampler, i.uv);
    return vec4<f32>(t.r, pass_block.after, frame.g_Time, i.extra.w * t.a + i.extra.x);
}
";

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// A full-target quad: (x, y, z, u, v) × 6, clip y up, texture v down.
fn quad(z: f32) -> Vec<f32> {
    let (l, r, t, b) = (-1.0, 1.0, 1.0, -1.0);
    vec![
        l, t, z, 0.0, 0.0, r, t, z, 1.0, 0.0, l, b, z, 0.0, 1.0, //
        r, t, z, 1.0, 0.0, r, b, z, 1.0, 1.0, l, b, z, 0.0, 1.0,
    ]
}

#[test]
fn pipeline_binds_arena_windows_textures_samplers_and_defaults() {
    let d = device();
    let (prog, _) = program(&d, PIPELINE_WGSL, None);
    let res = prog.resources();
    assert_eq!(res.textures, 1);
    assert_eq!(res.samplers, 1);

    // Constant arena: a dummy window first, so the pass block sits at a non-zero offset.
    let mut layout = ConstantLayout::new();
    let _dummy = layout.alloc(64).unwrap();
    let pass = layout.alloc(res.uniform_block.unwrap()).unwrap();
    let frame = layout.alloc(res.frame_block.unwrap()).unwrap();
    assert_eq!(pass.offset, CONSTANT_WINDOW_ALIGN);
    let mut arena = ConstantArena::new(&d, &layout).unwrap();
    // color (0..16), offset (16..24), scale (24), probe (32..44), after (44).
    arena.write(pass, 0, &f32s(&[9.0, 9.0, 9.0, 9.0])).unwrap();
    arena.write(pass, 16, &f32s(&[0.0, 0.0, 1.0])).unwrap();
    arena
        .write(pass, 32, &f32s(&[7.0, 7.0, 7.0, unorm(102)]))
        .unwrap();
    arena.write(frame, 0, &f32s(&[unorm(153)])).unwrap();
    assert!(
        arena.write(pass, 250, &[0; 8]).is_err(),
        "writes stay in their window"
    );
    assert!(arena.upload(&d).unwrap());
    assert!(
        !arena.upload(&d).unwrap(),
        "nothing changed, nothing mapped"
    );

    // Vertices in a shared arena at a non-zero offset; unfed input from the defaults buffer.
    let mut vertices = LinearArena::new();
    vertices.push(&[0xAB; 20], 16);
    let quad_offset = vertices.push(&f32s(&quad(0.5)), 16);
    let vb = StaticBuffer::new(&d, StaticBufferKind::Vertex, vertices.bytes()).unwrap();
    let defaults = StaticBuffer::new(&d, StaticBufferKind::Vertex, &DEFAULT_ATTRIBUTES).unwrap();
    let fed = [
        VertexElement {
            location: 0,
            format: VertexFormat::Float32x3,
            offset: 0,
            slot: 0,
        },
        VertexElement {
            location: 1,
            format: VertexFormat::Float32x2,
            offset: 12,
            slot: 0,
        },
    ];
    let elements = prog.complete_elements(&fed, 1);
    assert_eq!(elements.len(), 3);
    let layout_obj = create_input_layout(&d, &prog, &elements).unwrap();
    assert!(
        create_input_layout(&d, &prog, &fed).is_err(),
        "a missing input is refused"
    );

    // 2×2 texture, red channel 200/100/50/25 from the top-left, alpha 255; nearest, clamp.
    let texels: Vec<u8> = [200u8, 100, 50, 25]
        .iter()
        .flat_map(|&r| [r, 0, 0, 255])
        .collect();
    let tex = Texture::new(
        &d,
        GpuFormat::Rgba8Unorm,
        Dimension::D2,
        &[MipLevel {
            width: 2,
            height: 2,
            depth: 1,
            data: &texels,
        }],
    )
    .unwrap();
    let mut states = StateCache::new();
    let sampler = states
        .sampler(&d, SamplerKey::new(AddressMode::Clamp, true, false, false))
        .unwrap();
    let blend = states.blend(&d, BlendKey::OPAQUE).unwrap();
    let raster = states.raster(&d, RasterKey::default()).unwrap();

    let target = RenderTarget::new(&d, TargetDesc::new((8, 8))).unwrap();
    let ctx = d.context();
    target.bind(&d);
    target.clear(&d, [0.0; 4]);
    prog.bind(&d);
    arena.bind(&d, 0, pass);
    arena.bind(&d, 1, frame);
    vb.bind_vertices(&d, 0, 20, quad_offset);
    defaults.bind_vertices(&d, 1, 0, 0);
    // SAFETY: objects of this device; six vertices in the bound buffer.
    unsafe {
        ctx.IASetInputLayout(layout_obj.as_ref());
        ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ctx.PSSetShaderResources(0, Some(&[Some(tex.srv().clone())]));
        ctx.PSSetSamplers(0, Some(&[Some(sampler)]));
        ctx.OMSetBlendState(&blend, None, 0xFFFF_FFFF);
        ctx.RSSetState(&raster);
        ctx.Draw(6, 0);
    }
    let (w, h, px) = read_texture_rgba8(&d, target.texture()).unwrap();
    assert_eq!((w, h), (8, 8));
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        px[i..i + 4].try_into().unwrap()
    };
    // Quadrants follow the texture (uv (0, 0) at the top left), green = `after` (offset 44 of
    // the block), blue = the frame block, alpha = the unfed input's w (1) × texel alpha + x (0).
    assert_eq!(at(1, 1), [200, 102, 153, 255]);
    assert_eq!(at(6, 1), [100, 102, 153, 255]);
    assert_eq!(at(1, 6), [50, 102, 153, 255]);
    assert_eq!(at(6, 6), [25, 102, 153, 255]);
}

const SOLID_WGSL: &str = r"
struct Pass { color: vec4<f32>, z: f32, }
@group(0) @binding(0) var<uniform> pass_block: Pass;
@vertex fn vs_main(@location(0) a_Position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(a_Position.xy, pass_block.z, 1.0);
}
@fragment fn fs_main() -> @location(0) vec4<f32> { return pass_block.color; }
";

struct Solid {
    prog: ShaderProgram,
    layout: Option<lumenwall_d3d11::windows::Win32::Graphics::Direct3D11::ID3D11InputLayout>,
    vb: StaticBuffer,
}

fn solid(d: &Device, positions: &[f32]) -> Solid {
    let (prog, _) = program(d, SOLID_WGSL, None);
    let layout = create_input_layout(
        d,
        &prog,
        &[VertexElement {
            location: 0,
            format: VertexFormat::Float32x3,
            offset: 0,
            slot: 0,
        }],
    )
    .unwrap();
    let vb = StaticBuffer::new(d, StaticBufferKind::Vertex, &f32s(positions)).unwrap();
    Solid { prog, layout, vb }
}

impl Solid {
    fn draw(&self, d: &Device, arena: &ConstantArena, window: ConstantRegion, vertices: u32) {
        self.prog.bind(d);
        arena.bind(d, 0, window);
        self.vb.bind_vertices(d, 0, 12, 0);
        // SAFETY: objects of this device.
        unsafe {
            d.context().IASetInputLayout(self.layout.as_ref());
            d.context()
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            d.context().Draw(vertices, 0);
        }
    }
}

#[test]
fn msaa_resolve_gives_partial_coverage() {
    let d = device();
    // One triangle: the lower-right half of the target, edge along the diagonal.
    let tri = solid(&d, &[-1.0, -1.0, 0.5, 1.0, 1.0, 0.5, 1.0, -1.0, 0.5]);
    let mut layout = ConstantLayout::new();
    let w = layout.alloc(32).unwrap();
    let mut arena = ConstantArena::new(&d, &layout).unwrap();
    arena
        .write(w, 0, &f32s(&[1.0, 1.0, 1.0, 1.0, 0.5]))
        .unwrap();
    arena.upload(&d).unwrap();
    let mut states = StateCache::new();
    let mut levels = Vec::new();
    for samples in [1, 4] {
        let target = RenderTarget::new(
            &d,
            TargetDesc {
                samples,
                ..TargetDesc::new((16, 16))
            },
        )
        .unwrap();
        let raster = states
            .raster(
                &d,
                RasterKey {
                    multisample: samples > 1,
                    ..RasterKey::default()
                },
            )
            .unwrap();
        target.bind(&d);
        target.clear(&d, [0.0, 0.0, 0.0, 1.0]);
        // SAFETY: state of this device.
        unsafe { d.context().RSSetState(&raster) };
        tri.draw(&d, &arena, w, 3);
        target.resolve(&d);
        let (_, _, px) = read_texture_rgba8(&d, target.texture()).unwrap();
        let mut reds: Vec<u8> = px.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        reds.sort_unstable();
        reds.dedup();
        levels.push(reds);
    }
    assert_eq!(levels[0], vec![0, 255], "no MSAA: covered or not");
    assert!(
        levels[1].iter().any(|&r| r > 0 && r < 255),
        "4× MSAA: edge pixels partly covered, got {:?}",
        levels[1]
    );
    assert!(matches!(
        RenderTarget::new(
            &d,
            TargetDesc {
                samples: 3,
                ..TargetDesc::new((4, 4))
            }
        ),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn depth_test_keeps_the_nearest_whatever_the_order() {
    let d = device();
    let full = solid(
        &d,
        &[
            -1.0, -1.0, 0.0, -1.0, 1.0, 0.0, 1.0, 1.0, 0.0, -1.0, -1.0, 0.0, 1.0, 1.0, 0.0, 1.0,
            -1.0, 0.0,
        ],
    );
    let mut layout = ConstantLayout::new();
    let near = layout.alloc(32).unwrap();
    let far = layout.alloc(32).unwrap();
    let mut arena = ConstantArena::new(&d, &layout).unwrap();
    arena
        .write(near, 0, &f32s(&[1.0, 0.0, 0.0, 1.0, 0.2]))
        .unwrap();
    arena
        .write(far, 0, &f32s(&[0.0, 1.0, 0.0, 1.0, 0.6]))
        .unwrap();
    arena.upload(&d).unwrap();
    let mut states = StateCache::new();
    let depth = states
        .depth(
            &d,
            DepthKey {
                test: true,
                write: true,
            },
        )
        .unwrap();
    let raster = states
        .raster(
            &d,
            RasterKey {
                depth_clip: true,
                ..RasterKey::default()
            },
        )
        .unwrap();
    let target = RenderTarget::new(
        &d,
        TargetDesc {
            depth: true,
            ..TargetDesc::new((4, 4))
        },
    )
    .unwrap();
    assert!(target.depth().is_some());
    for order in [[near, far], [far, near]] {
        target.bind(&d);
        target.clear(&d, [0.0; 4]);
        // SAFETY: states of this device.
        unsafe {
            d.context().OMSetDepthStencilState(&depth, 0);
            d.context().RSSetState(&raster);
        }
        for w in order {
            full.draw(&d, &arena, w, 6);
        }
        let (_, _, px) = read_texture_rgba8(&d, target.texture()).unwrap();
        assert!(
            px.as_chunks::<4>().0.iter().all(|p| *p == [255, 0, 0, 255]),
            "{:?}",
            &px[..4]
        );
    }
}

#[test]
fn state_cache_returns_one_object_per_key() {
    let d = device();
    let mut s = StateCache::new();
    let a = s.blend(&d, BlendKey::translucent()).unwrap();
    let b = s.blend(&d, BlendKey::translucent()).unwrap();
    let c = s.blend(&d, BlendKey::additive()).unwrap();
    assert_eq!(a.as_raw(), b.as_raw());
    assert_ne!(a.as_raw(), c.as_raw());
    let k = SamplerKey::new(AddressMode::Border, false, true, false);
    assert_eq!(
        s.sampler(&d, k).unwrap().as_raw(),
        s.sampler(&d, k).unwrap().as_raw()
    );
    s.raster(
        &d,
        RasterKey {
            cull: Cull::Back,
            ..RasterKey::default()
        },
    )
    .unwrap();
    s.depth(&d, DepthKey::default()).unwrap();
    assert_eq!(s.counts(), (1, 2, 1, 1));
}

#[test]
fn gpu_timer_measures_without_waiting() {
    let d = device();
    let mut timer = GpuTimer::new(&d).unwrap();
    let target = RenderTarget::new(&d, TargetDesc::new((256, 256))).unwrap();
    for _ in 0..3 {
        timer.begin(&d).unwrap();
        target.bind(&d);
        target.clear(&d, [0.5; 4]);
        timer.end(&d);
        d.flush();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = None;
    while got.is_none() && Instant::now() < deadline {
        got = timer.poll(&d).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    let t = got.expect("a GPU time within 5 s");
    assert!(t < Duration::from_secs(1), "{t:?}");
}

#[test]
fn bytecode_cache_skips_the_compiler_the_second_time() {
    let d = device();
    let dir = tempfile::tempdir().unwrap();
    let cache = BytecodeCache::new(dir.path().join("dxbc")).unwrap();
    let (_, first) = program(&d, SOLID_WGSL, Some(&cache));
    let (p, second) = program(&d, SOLID_WGSL, Some(&cache));
    assert_eq!((first, second), (CacheOutcome::Compiled, CacheOutcome::Hit));
    assert_eq!(p.inputs().len(), 1);
    let (_, none) = program(&d, SOLID_WGSL, None);
    assert_eq!(none, CacheOutcome::Uncached);
}

#[test]
fn dynamic_buffer_rewrites_and_bounds() {
    let d = device();
    let b = DynamicBuffer::new(&d, 100, false).unwrap();
    assert_eq!(b.capacity(), 112);
    b.write(&d, &[1; 112]).unwrap();
    assert!(b.write(&d, &[1; 113]).is_err());
    let idx = DynamicBuffer::new(&d, 64, true).unwrap();
    idx.write(&d, &[0; 64]).unwrap();
    // The buffers are live objects of the device.
    let _: ID3D11Resource = b.buffer().cast().unwrap();
}

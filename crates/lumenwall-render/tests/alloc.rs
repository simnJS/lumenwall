//! The per-frame CPU update path (frame globals, uniform copies, image
//! geometry and matrices) performs no heap allocation. Single test per
//! binary so the counting allocator only sees this thread's work.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use glam::{Vec2, Vec3};
use lumenwall_render::dynbuf::FrameInputs;
use lumenwall_render::graph::{Op, SceneBuilder, TextureRef};
use lumenwall_render::image::{ImageFrame, ImagePass, ImageSpec, add_image};
use lumenwall_render::math::{Alignment, NodeTransform, OrthoCamera};
use lumenwall_render::shader::ShaderDesc;
use lumenwall_render::texture::{MipLevel, SamplerKey, TexFlags, TexelFormat, TextureUpload};
use lumenwall_render::{ContextOptions, GpuContext};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator, only counting calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const SRC: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_ModelViewProjectionMatrixInverse: mat4x4<f32>,
    g_Time: f32,
    g_PointerPosition: vec2<f32>,
    g_AudioSpectrum16Left: array<vec4<f32>, 4>,
}
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord + u.g_PointerPosition * 0.0;
    return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv) * (1.0 + 0.0 * (u.g_Time + u.g_AudioSpectrum16Left[0].x));
}
";

#[test]
fn per_frame_update_does_not_allocate() {
    let ctx = match GpuContext::new_headless(&ContextOptions::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let mut b = SceneBuilder::new(&ctx, (256.0, 256.0));
    let st = b
        .targets
        .create_scene_targets(&ctx, (256, 256), SamplerKey::CLAMP_LINEAR);
    b.push(Op::Clear {
        target: st.full,
        color: [0.0, 0.0, 0.0, 1.0],
    });
    let data = vec![255u8; 16 * 16 * 4];
    let tex = b
        .add_texture(&TextureUpload {
            label: "t",
            format: TexelFormat::Rgba8,
            levels: &[MipLevel {
                width: 16,
                height: 16,
                data: &data,
            }],
            content_size: None,
            flags: TexFlags::default(),
        })
        .unwrap();
    let sh = b
        .add_shader(ShaderDesc::from_wgsl("s", SRC, "vs", "fs").unwrap())
        .unwrap();
    let composites = b
        .targets
        .create_composites(&ctx, 1, (128, 128), SamplerKey::CLAMP_LINEAR);
    let spec = ImageSpec {
        label: "img".into(),
        size: Vec2::new(128.0, 128.0),
        alignment: Alignment::default(),
        main: TextureRef::Texture(tex),
        main_sizes: ((16, 16), (16, 16)),
        animated: false,
        passthrough: false,
        fullscreen: false,
        composites,
        scene_target: st.full,
        final_to_scene: true,
    };
    let passes: Vec<ImagePass> = (0..6)
        .map(|i| ImagePass::new(format!("p{i}"), sh))
        .collect();
    let img = add_image(&mut b, &spec, &passes).unwrap();
    let mut scene = b.build().unwrap();
    let vp = OrthoCamera::canonical(256.0, 256.0).view_projection();
    let audio16 = [0.1; 16];
    let audio32 = [0.2; 32];
    let audio64 = [0.3; 64];
    scene.render(&ctx);
    ctx.wait_idle();

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for i in 0..200 {
        let t = i as f32 * 0.033;
        scene.update_frame(&FrameInputs {
            time: t,
            daytime: 0.5,
            pointer: [t.sin(), t.cos()],
            pointer_last: [0.0, 0.0],
            audio16: &audio16,
            audio32: &audio32,
            audio64: &audio64,
        });
        img.update(
            &mut scene,
            &ImageFrame {
                view_projection: vp,
                transform: NodeTransform {
                    origin: Vec3::new(128.0 + t, 128.0, 0.0),
                    scale: Vec3::ONE,
                    angle: t,
                },
                parallax: Vec3::new(t, 0.0, 0.0),
                scene_size: Vec2::new(256.0, 256.0),
            },
        );
        let _ = scene.needs_redraw();
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "per-frame CPU update allocated");

    // Informative: allocations inside wgpu while recording one frame.
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let mut enc = ctx.device().create_command_encoder(&Default::default());
    scene.encode(&ctx, &mut enc);
    let cb = enc.finish();
    let recorded = ALLOCATIONS.load(Ordering::Relaxed) - before;
    ctx.queue().submit([cb]);
    eprintln!("allocations inside wgpu to create an encoder and record one frame: {recorded}");
}

//! The per-frame CPU update path (frame globals, uniform copies, image
//! geometry and matrices) and the front's frame (`RenderScene::render_frame`: one call into the
//! backend) perform no heap allocation. Single test per binary so the counting allocator only
//! sees this thread's work.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use glam::{Vec2, Vec3};
use lumenwall_render::backend::{
    BackendCaps, Frame, Gpu, GraphCounts, SceneBackend, SceneDesc, SegmentTime,
};
use lumenwall_render::dynbuf::FrameInputs;
use lumenwall_render::graph::{MeshLayout, Op, SceneBuilder, StreamLayout, TextureRef};
use lumenwall_render::image::{ImageFrame, ImagePass, ImageSpec, add_image};
use lumenwall_render::math::{Alignment, NodeTransform, OrthoCamera};
use lumenwall_render::shader::{PreparedShader, ShaderDesc};
use lumenwall_render::targets::RtId;
use lumenwall_render::texture::{
    MipLevel, SamplerKey, TexFlags, TexelFormat, TextureInfo, TextureUpload, VolumeUpload,
};
use lumenwall_render::{ContextOptions, GpuContext, Result};

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
    front_frame_does_not_allocate();
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

/// A backend that keeps nothing: what is left of a frame is the front's own work.
struct Quiet;

impl SceneBackend for Quiet {
    fn uniform_alignment(&self, _: Gpu<'_>) -> u32 {
        256
    }
    fn caps(&self, _: Gpu<'_>) -> BackendCaps {
        BackendCaps {
            bc: true,
            max_texture_2d: 16384,
        }
    }

    fn supports_samples(&self, _: Gpu<'_>, _: wgpu::TextureFormat, _: u32) -> bool {
        false
    }
    fn add_texture(&mut self, _: Gpu<'_>, upload: &TextureUpload<'_>) -> Result<TextureInfo> {
        Ok(TextureInfo::for_upload(upload))
    }
    fn add_volume_texture(&mut self, _: Gpu<'_>, upload: &VolumeUpload<'_>) -> Result<TextureInfo> {
        Ok(TextureInfo::for_volume(upload))
    }
    fn add_shader(&mut self, _: Gpu<'_>, _: PreparedShader) -> Result<()> {
        Ok(())
    }
    fn add_mesh(&mut self, _: Gpu<'_>, _: &str, _: &MeshLayout, _: &[u8], _: &[u16]) -> Result<()> {
        Ok(())
    }
    fn add_stream(
        &mut self,
        _: Gpu<'_>,
        _: &str,
        _: &StreamLayout,
        _: u32,
        _: &[u32],
    ) -> Result<()> {
        Ok(())
    }
    fn build(&mut self, _: Gpu<'_>, _: &SceneDesc<'_>) -> Result<()> {
        Ok(())
    }
    fn extend(&mut self, _: Gpu<'_>, _: &SceneDesc<'_>, _: usize) -> Result<()> {
        Ok(())
    }
    fn rebind(&mut self, _: Gpu<'_>, _: &SceneDesc<'_>) -> Result<()> {
        Ok(())
    }
    fn replan(&mut self, _: Gpu<'_>, _: &SceneDesc<'_>) {}
    fn render(&mut self, _: &mut Frame<'_>, scene: &SceneDesc<'_>) -> bool {
        // Reads what a backend reads every frame.
        std::hint::black_box((
            scene.dynamic.shadow().dirty_range(),
            scene.passes.iter().map(|p| p.variant).sum::<usize>(),
        ));
        false
    }
    fn read_target(&mut self, _: Gpu<'_>, _: &SceneDesc<'_>, _: RtId) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
    fn profile(&mut self, _: Gpu<'_>, _: &SceneDesc<'_>) -> Option<Vec<SegmentTime>> {
        None
    }
    fn counts(&self) -> GraphCounts {
        GraphCounts::default()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The front's frame on a backend that records nothing (no GPU needed).
fn front_frame_does_not_allocate() {
    let mut b = SceneBuilder::with_backend(Gpu::Other, Box::new(Quiet), (256.0, 256.0));
    let st = b
        .targets
        .create_scene_targets(Gpu::Other, (256, 256), SamplerKey::CLAMP_LINEAR);
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
    let composites =
        b.targets
            .create_composites(Gpu::Other, 1, (128, 128), SamplerKey::CLAMP_LINEAR);
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
    let (audio16, audio32, audio64) = ([0.1; 16], [0.2; 32], [0.3; 64]);
    scene.render_frame(&mut Frame::Other(&mut ()));

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
        if scene.needs_redraw() {
            scene.render_frame(&mut Frame::Other(&mut ()));
        }
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "the front's frame allocated");
}

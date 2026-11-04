//! GPU tests of the direct Vulkan path: same pixels as the wgpu path, automatic fallback per
//! scene, lifecycle of the wgpu-hal objects. They need a Vulkan adapter (the fallback test
//! also a Direct3D 12 one) and print "skipped" otherwise.

use std::sync::{Mutex, MutexGuard};

use glam::{Vec2, Vec3};

use super::{Recording, WgpuReason, faults, live_objects};
use crate::context::{ContextOptions, GpuContext};
use crate::graph::{Geometry, Op, PassDesc, RenderScene, SceneBuilder, SlotBinding, TextureRef};
use crate::headless::read_texture_rgba8;
use crate::image::{ImageFrame, ImageHandle, ImagePass, ImageSpec, add_image};
use crate::math::{Alignment, NodeTransform, OrthoCamera, unit_quad};
use crate::shader::ShaderDesc;
use crate::targets::{PingPong, RtId, SceneTargets};
use crate::texture::{MipLevel, SamplerKey, TexFlags, TexelFormat, TextureUpload};

const SRC: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_Time: f32,
}
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
@group(1) @binding(2) var g_Texture1: texture_2d<f32>;
@group(1) @binding(3) var g_Texture1_s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    return o;
}
@fragment fn fs_copy(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv);
}
@fragment fn fs_wave(i: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(g_Texture0, g_Texture0_s, i.uv + vec2<f32>(0.03 * sin(u.g_Time), 0.0));
    return vec4<f32>(1.0 - c.rgb, c.a);
}
@fragment fn fs_add(i: VOut) -> @location(0) vec4<f32> {
    return 0.5 * (textureSample(g_Texture0, g_Texture0_s, i.uv) + textureSample(g_Texture1, g_Texture1_s, i.uv));
}
";

/// The tests count wgpu-hal objects process-wide: one at a time.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn context(backends: Option<wgpu::Backends>) -> Option<GpuContext> {
    GpuContext::new_headless(&ContextOptions {
        backends,
        ..ContextOptions::default()
    })
    .ok()
}

/// A Vulkan context, or `None` (test skipped).
fn vulkan() -> Option<GpuContext> {
    let ctx = context(Some(wgpu::Backends::VULKAN));
    if ctx.is_none() {
        eprintln!("skipped: no Vulkan adapter");
    }
    ctx
}

struct Scene {
    scene: RenderScene,
    st: SceneTargets,
    image: ImageHandle,
    texture: crate::graph::TextureId,
    composites: PingPong,
    size: Vec2,
}

/// A scene with a three-pass image chain (ping-pong composites, a pass sampling two inputs),
/// a pass reading the scene target it draws into (hazard copy) and a clear.
fn build(ctx: &GpuContext, recording: Recording) -> Scene {
    let (w, h) = (64u32, 48u32);
    let mut b = SceneBuilder::new(ctx, (w as f32, h as f32));
    b.set_recording(recording);
    let st = b
        .targets
        .create_scene_targets(ctx, (w, h), SamplerKey::CLAMP_LINEAR);
    b.push(Op::Clear {
        target: st.full,
        color: [0.1, 0.2, 0.3, 1.0],
    });
    let mut data = Vec::new();
    for y in 0..16u32 {
        for x in 0..16u32 {
            data.extend_from_slice(&[(x * 16) as u8, (y * 16) as u8, ((x ^ y) * 16) as u8, 255]);
        }
    }
    let texture = b
        .add_texture(&TextureUpload {
            label: "test",
            format: TexelFormat::Rgba8,
            levels: &[MipLevel {
                width: 16,
                height: 16,
                data: &data,
            }],
            content_size: None,
            flags: TexFlags(TexFlags::CLAMP_UVS),
        })
        .unwrap();
    let shader = |b: &mut SceneBuilder<'_>, fs: &str| {
        b.add_shader(ShaderDesc::from_wgsl(fs, SRC, "vs", fs).unwrap())
            .unwrap()
    };
    let copy = shader(&mut b, "fs_copy");
    let wave = shader(&mut b, "fs_wave");
    let add = shader(&mut b, "fs_add");
    let composites = b
        .targets
        .create_composites(ctx, 1, (40, 30), SamplerKey::CLAMP_LINEAR);
    let spec = ImageSpec {
        label: "image".into(),
        size: Vec2::new(40.0, 30.0),
        alignment: Alignment::default(),
        main: TextureRef::Texture(texture),
        main_sizes: ((16, 16), (16, 16)),
        animated: false,
        passthrough: false,
        fullscreen: false,
        composites,
        scene_target: st.full,
        final_to_scene: true,
    };
    let mut second = ImagePass::new("add", add);
    second.slots[1] = Some(crate::image::SlotSource::Texture(TextureRef::Texture(
        texture,
    )));
    let image = add_image(
        &mut b,
        &spec,
        &[
            ImagePass::new("base", copy),
            ImagePass::new("wave", wave),
            second,
        ],
    )
    .unwrap();
    // A full-screen pass sampling the scene target it draws into: a hazard copy first.
    let v = b.add_vertices(&unit_quad());
    let mut desc = PassDesc::new("feedback", wave, st.full, Geometry::Vertices(v));
    desc.blend = crate::blend::BlendMode::Translucent;
    let desc = desc.with_slot(0, SlotBinding::target(st.full));
    let pass = b.add_pass(desc).unwrap();
    b.push(Op::Draw(pass));
    let scene = b.build().unwrap();
    Scene {
        scene,
        st,
        image,
        texture,
        composites,
        size: Vec2::new(w as f32, h as f32),
    }
}

fn frame(size: Vec2, t: f32) -> ImageFrame {
    ImageFrame {
        view_projection: OrthoCamera::canonical(size.x, size.y).view_projection(),
        transform: NodeTransform {
            origin: Vec3::new(32.0 + 4.0 * t, 24.0, 0.0),
            scale: Vec3::ONE,
            angle: 0.1 * t,
        },
        parallax: Vec3::ZERO,
        scene_size: size,
    }
}

/// Renders frame `i` and reads the scene target back.
fn render(ctx: &GpuContext, s: &mut Scene, i: u32) -> Vec<u8> {
    let t = i as f32 * 0.25;
    let time = s.scene.uniform(s.image.passes[1], "g_Time");
    if let Some(slot) = time {
        s.scene.shadow_mut().write_floats(&slot, &[t]);
    }
    s.image.update(&mut s.scene, &frame(s.size, t));
    s.scene.render(ctx);
    read_texture_rgba8(ctx, s.scene.targets().get(s.st.full).texture()).unwrap()
}

fn target_pixels(ctx: &GpuContext, s: &Scene, id: RtId) -> Vec<u8> {
    read_texture_rgba8(ctx, s.scene.targets().get(id).texture()).unwrap()
}

#[test]
fn both_paths_draw_the_same_pixels() {
    let _g = serial();
    let Some(ctx) = vulkan() else { return };
    let mut direct = build(&ctx, Recording::Auto);
    let mut wgpu = build(&ctx, Recording::Wgpu);
    assert!(
        direct.scene.records_direct(),
        "Vulkan scenes record directly"
    );
    assert_eq!(wgpu.scene.wgpu_reason(), Some(WgpuReason::Requested));
    assert!(
        direct.scene.counts().copies >= 1,
        "the hazard copy is part of the frame"
    );
    for i in 0..4 {
        let (a, b) = (render(&ctx, &mut direct, i), render(&ctx, &mut wgpu, i));
        assert!(a == b, "frame {i}: the direct path differs from wgpu");
        // The composites too (ping-pong targets written and sampled during the frame).
        for id in [direct.composites.a, direct.composites.b] {
            assert!(
                target_pixels(&ctx, &direct, id) == target_pixels(&ctx, &wgpu, id),
                "frame {i}: composite {id:?}"
            );
        }
    }
    let a = target_pixels(&ctx, &direct, direct.st.full);
    assert!(
        a.chunks(4).any(|p| p != [26, 51, 77, 255]),
        "the image is drawn"
    );
}

/// Direct3D 12 (Windows) records through wgpu, with the same pixels.
#[test]
fn other_backends_record_through_wgpu() {
    let _g = serial();
    let Some(dx12) = context(Some(wgpu::Backends::DX12)) else {
        eprintln!("skipped: no Direct3D 12 adapter");
        return;
    };
    let mut s = build(&dx12, Recording::Auto);
    assert!(!s.scene.records_direct());
    assert_eq!(s.scene.wgpu_reason(), Some(WgpuReason::NotVulkan));
    let Some(vk) = vulkan() else { return };
    let mut v = build(&vk, Recording::Auto);
    for i in 0..2 {
        let (a, b) = (render(&dx12, &mut s, i), render(&vk, &mut v, i));
        // Two drivers: rasterisation may differ in the last bit, not more.
        let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max();
        assert!(
            worst <= Some(2),
            "frame {i}: Direct3D 12 and Vulkan differ by {worst:?}"
        );
    }
}

/// A wgpu-hal object that cannot be created at build sends the scene to wgpu: same pixels,
/// nothing left behind.
#[test]
fn a_refused_pipeline_falls_back_at_build() {
    let _g = serial();
    let Some(ctx) = vulkan() else { return };
    let before = live_objects();
    let mut reference = build(&ctx, Recording::Wgpu);
    faults::fail_pipelines(true);
    let mut s = build(&ctx, Recording::Auto);
    faults::fail_pipelines(false);
    assert!(!s.scene.records_direct());
    assert_eq!(s.scene.wgpu_reason(), Some(WgpuReason::Unsupported));
    assert_eq!(
        live_objects(),
        before,
        "the abandoned direct graph is destroyed"
    );
    for i in 0..3 {
        assert!(render(&ctx, &mut s, i) == render(&ctx, &mut reference, i));
    }
}

/// A frame that cannot be planned after load (here: an injected failure when the visibility
/// changes) sends the running scene to wgpu without a glitch.
#[test]
fn a_refused_frame_falls_back_while_running() {
    let _g = serial();
    let Some(ctx) = vulkan() else { return };
    let before = live_objects();
    let mut reference = build(&ctx, Recording::Wgpu);
    let mut s = build(&ctx, Recording::Auto);
    assert!(s.scene.records_direct());
    for i in 0..2 {
        assert!(render(&ctx, &mut s, i) == render(&ctx, &mut reference, i));
    }
    let passes = s.image.passes.clone();
    faults::fail_programs(true);
    s.scene.set_enabled(&ctx, &passes[1..2], false);
    faults::fail_programs(false);
    reference.scene.set_enabled(&ctx, &passes[1..2], false);
    assert!(!s.scene.records_direct(), "back to wgpu");
    assert_eq!(live_objects(), before);
    for i in 2..5 {
        assert!(
            render(&ctx, &mut s, i) == render(&ctx, &mut reference, i),
            "frame {i} after the fallback"
        );
    }
}

/// Objects of the direct path: created at build, none per frame, a bounded number across
/// rebuilds (resizes, visibility), all destroyed with the scene. Textures written through the
/// queue and readbacks between frames keep working.
#[test]
fn objects_live_and_die_with_the_scene() {
    let _g = serial();
    let Some(ctx) = vulkan() else { return };
    let before = live_objects();
    let mut s = build(&ctx, Recording::Auto);
    let mut reference = build(&ctx, Recording::Wgpu);
    let built = s.scene.direct_stats().unwrap().objects;
    assert!(built > 0);
    assert_eq!(live_objects() - before, built);
    for i in 0..3 {
        render(&ctx, &mut s, i);
    }
    assert_eq!(
        s.scene.direct_stats().unwrap().objects,
        built,
        "nothing per frame"
    );

    // Visibility changes rebuild the frame from cached objects.
    let passes = s.image.passes.clone();
    for on in [false, true, false, true] {
        s.scene.set_enabled(&ctx, &passes[2..], on);
        reference.scene.set_enabled(&ctx, &passes[2..], on);
        assert!(render(&ctx, &mut s, 3) == render(&ctx, &mut reference, 3));
    }
    let after_visibility = s.scene.direct_stats().unwrap().objects;
    assert!(
        after_visibility <= built + 4,
        "visibility: {built} → {after_visibility}"
    );

    // Resizes reallocate targets: objects of the old ones go after a GPU wait.
    for (i, size) in [(32, 24), (24, 20), (40, 30)].into_iter().enumerate() {
        for sc in [&mut s, &mut reference] {
            let id: RtId = sc.composites.a;
            sc.scene.targets_mut().resize(&ctx, id, size);
            sc.scene.rebuild_bindings(&ctx).unwrap();
        }
        assert!(s.scene.records_direct());
        let (a, b) = (
            render(&ctx, &mut s, 4 + i as u32),
            render(&ctx, &mut reference, 4 + i as u32),
        );
        assert!(a == b, "after resize {i}");
        let now = s.scene.direct_stats().unwrap().objects;
        assert!(
            now <= after_visibility + 8,
            "resize {i}: {after_visibility} → {now}"
        );
    }

    // A texture written through the queue (text, video) after `mark_dirty`.
    let white = vec![255u8; 16 * 16 * 4];
    for sc in [&mut s, &mut reference] {
        sc.scene
            .texture(sc.texture)
            .unwrap()
            .write_level0(&ctx, &white);
        sc.scene.mark_dirty();
    }
    let (a, b) = (render(&ctx, &mut s, 8), render(&ctx, &mut reference, 8));
    assert!(a == b, "after a texture write");
    // A readback between two frames, then another frame.
    let _ = target_pixels(&ctx, &s, s.st.full);
    assert!(render(&ctx, &mut s, 9) == render(&ctx, &mut reference, 9));

    drop(s);
    assert_eq!(
        live_objects(),
        before,
        "every wgpu-hal object freed with the scene"
    );
}

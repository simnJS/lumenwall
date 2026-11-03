//! Render one textured image with a simple time-driven effect, off screen,
//! and write the composed output to a PNG.
//!
//! ```text
//! cargo run -p lumenwall-render --example headless -- [out.png] [--software]
//! ```

use glam::{Vec2, Vec3};
use lumenwall_render::compose::{ComposeSettings, Compositor, ScalingMode};
use lumenwall_render::dynbuf::FrameInputs;
use lumenwall_render::graph::{Op, SceneBuilder, TextureRef};
use lumenwall_render::headless::HeadlessOutput;
use lumenwall_render::image::{ImageFrame, ImagePass, ImageSpec, add_image};
use lumenwall_render::math::{Alignment, NodeTransform, OrthoCamera, scene_logical_size};
use lumenwall_render::shader::ShaderDesc;
use lumenwall_render::texture::{MipLevel, SamplerKey, TexFlags, TexelFormat, TextureUpload};
use lumenwall_render::{BlendMode, ContextOptions, GpuContext};

/// Shaders in the WE convention (upstream clip space), as lumenwall-shader will
/// produce them: pass block at @group(0) @binding(0), slot N at @group(1).
const SHADERS: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_Time: f32,
}
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;

struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    return o;
}

// Base material pass (genericimage-like copy).
@fragment
fn fs_image(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv);
}

// Effect pass: horizontal wave + warm tint.
@fragment
fn fs_wave(i: VOut) -> @location(0) vec4<f32> {
    let uv = i.uv + vec2<f32>(sin(i.uv.y * 25.0 + u.g_Time * 3.0) * 0.01, 0.0);
    let c = textureSample(g_Texture0, g_Texture0_s, uv);
    return vec4<f32>(c.rgb * vec3<f32>(1.0, 0.85, 0.7), c.a);
}
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut out_path = std::path::PathBuf::from("lumenwall-headless.png");
    let mut software = false;
    for a in std::env::args().skip(1) {
        if a == "--software" {
            software = true;
        } else {
            out_path = a.into();
        }
    }

    let ctx = GpuContext::new_headless(&ContextOptions {
        prefer_software: software,
        ..ContextOptions::default()
    })?;
    let info = ctx.adapter().get_info();
    println!(
        "adapter: {} ({:?}), BC: {}",
        info.name,
        info.backend,
        ctx.supports_bc()
    );

    // Scene: 640x360 projection, white clear colour (WE default).
    let scene_size = Vec2::new(640.0, 360.0);
    let logical = scene_logical_size(scene_size.x, scene_size.y);
    let mut b = SceneBuilder::new(&ctx, (scene_size.x, scene_size.y));
    let st = b
        .targets
        .create_scene_targets(&ctx, logical, SamplerKey::CLAMP_LINEAR);
    b.push(Op::Clear {
        target: st.full,
        color: [0.08, 0.09, 0.12, 1.0],
    });

    // Texture: 200x120 content padded to 256x128 (exercises uMax/vMax).
    let (sw, sh, cw, ch) = (256u32, 128u32, 200u32, 120u32);
    let mut pixels = vec![0u8; (sw * sh * 4) as usize];
    for y in 0..ch {
        for x in 0..cw {
            let i = ((y * sw + x) * 4) as usize;
            let checker = ((x / 20) + (y / 20)) % 2 == 0;
            let r = (x * 255 / cw) as u8;
            let g = (y * 255 / ch) as u8;
            let bl = if checker { 220 } else { 90 };
            pixels[i..i + 4].copy_from_slice(&[r, g, bl, 255]);
        }
    }
    let tex = b.add_texture(&TextureUpload {
        label: "procedural",
        format: TexelFormat::Rgba8,
        levels: &[MipLevel {
            width: sw,
            height: sh,
            data: &pixels,
        }],
        content_size: Some((cw, ch)),
        flags: TexFlags(TexFlags::CLAMP_UVS),
    })?;
    drop(pixels); // CPU copy freed right after upload.

    let image_shader = b.add_shader(ShaderDesc::from_wgsl("image", SHADERS, "vs", "fs_image")?)?;
    let wave_shader = b.add_shader(ShaderDesc::from_wgsl("wave", SHADERS, "vs", "fs_wave")?)?;

    let size = Vec2::new(cw as f32, ch as f32);
    let composites = b.targets.create_composites(
        &ctx,
        1,
        (cw, ch),
        SamplerKey::for_render_target(TexFlags(TexFlags::CLAMP_UVS)),
    );
    let spec = ImageSpec {
        label: "image".into(),
        size,
        alignment: Alignment::default(),
        main: TextureRef::Texture(tex),
        main_sizes: ((sw, sh), (cw, ch)),
        animated: false,
        passthrough: false,
        fullscreen: false,
        composites,
        scene_target: st.full,
        final_to_scene: true,
    };
    let mut base = ImagePass::new("base", image_shader);
    base.blend = BlendMode::Translucent;
    let image = add_image(&mut b, &spec, &[base, ImagePass::new("wave", wave_shader)])?;
    let mut scene = b.build()?;
    println!("graph: {:?}", scene.counts());

    // One frame at t = 1.25 s.
    let (a16, a32, a64) = ([0.0; 16], [0.0; 32], [0.0; 64]);
    scene.update_frame(&FrameInputs {
        time: 1.25,
        daytime: 0.5,
        pointer: [0.5, 0.5],
        pointer_last: [0.5, 0.5],
        audio16: &a16,
        audio32: &a32,
        audio64: &a64,
    });
    image.update(
        &mut scene,
        &ImageFrame {
            view_projection: OrthoCamera::canonical(scene_size.x, scene_size.y).view_projection(),
            transform: NodeTransform {
                origin: Vec3::new(320.0, 180.0, 0.0),
                scale: Vec3::new(2.0, 2.0, 1.0),
                angle: 0.15,
            },
            parallax: Vec3::ZERO,
            scene_size,
        },
    );

    // Compose into an 800x450 "screen" with the default scaling mode.
    let out = HeadlessOutput::new(&ctx, 800, 450);
    let mut compositor = Compositor::new(&ctx);
    let mut composer = compositor.create_output(
        &ctx,
        HeadlessOutput::FORMAT,
        scene.targets().get(st.full),
        ComposeSettings {
            scaling: ScalingMode::Default,
            ..ComposeSettings::default()
        },
    );
    composer.update(&ctx, out.size(), logical);
    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    scene.encode(&ctx, &mut encoder);
    compositor.encode(
        &mut encoder,
        &composer,
        out.view(),
        [0.0, 0.0, 800.0, 450.0],
        Some(wgpu::Color::BLACK),
    );
    ctx.queue().submit(std::iter::once(encoder.finish()));
    out.write_png(&ctx, &out_path)?;
    println!(
        "wrote {} (encode {:?}, GPU memory tracked {} KiB)",
        out_path.display(),
        scene.last_encode_time(),
        ctx.memory().bytes() / 1024
    );
    Ok(())
}

//! GPU cost of a chain shaped like the reference wallpaper (docs/TESTING.md,
//! 3018340632): a 3440x1440 scene, one full-size image with 11 effects =
//! 19 passes (8 single-pass effects, `shine` = 5 passes at 1/2 size,
//! `localcontrast` = 4 passes at 1/4 size, chromatic aberration), then bloom
//! (5 passes), then composition to a 3440x1440 output.
//!
//! The shaders are synthetic stand-ins with a similar number of texture taps;
//! no WE file is read. Prints the average GPU and CPU time per frame.
//!
//! ```text
//! cargo run --release -p lumenwall-render --example chain_bench -- [--software|--low-power] [frames]
//! ```

use std::time::{Duration, Instant};

use glam::{Vec2, Vec3};
use lumenwall_render::bloom::{BloomParams, BloomShaders, add_bloom};
use lumenwall_render::compose::{ComposeSettings, Compositor};
use lumenwall_render::dynbuf::FrameInputs;
use lumenwall_render::graph::{Op, SceneBuilder, TextureRef};
use lumenwall_render::headless::HeadlessOutput;
use lumenwall_render::image::{ImageFrame, ImagePass, ImageSpec, SlotSource, add_image};
use lumenwall_render::math::{Alignment, NodeTransform, OrthoCamera};
use lumenwall_render::shader::ShaderDesc;
use lumenwall_render::stats::GpuTimer;
use lumenwall_render::targets::scaled_size;
use lumenwall_render::texture::{MipLevel, SamplerKey, TexFlags, TexelFormat, TextureUpload};
use lumenwall_render::{ContextOptions, GpuContext, RtId, ShaderId};

const SHADERS: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_Texture0Resolution: vec4<f32>,
    g_Time: f32,
}
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
@group(1) @binding(2) var g_Texture1: texture_2d<f32>;
@group(1) @binding(3) var g_Texture1_s: sampler;
@group(1) @binding(4) var g_Texture2: texture_2d<f32>;
@group(1) @binding(5) var g_Texture2_s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    return o;
}
fn t0(uv: vec2<f32>) -> vec4<f32> { return textureSample(g_Texture0, g_Texture0_s, uv); }
@fragment fn fs_image(i: VOut) -> @location(0) vec4<f32> { return t0(i.uv); }
// Typical single-pass WE effect: a few dependent taps (noise/mask + distorted read).
@fragment fn fs_fx(i: VOut) -> @location(0) vec4<f32> {
    let n = textureSample(g_Texture1, g_Texture1_s, i.uv * 3.0 + vec2<f32>(u.g_Time * 0.01, 0.0)).r;
    let d = vec2<f32>(sin(i.uv.y * 30.0 + u.g_Time), cos(i.uv.x * 30.0 + u.g_Time)) * 0.002 * n;
    let c = t0(i.uv + d);
    let m = textureSample(g_Texture2, g_Texture2_s, i.uv).g;
    return mix(t0(i.uv), c, m);
}
@fragment fn fs_down(i: VOut) -> @location(0) vec4<f32> {
    let t = 1.0 / u.g_Texture0Resolution.xy;
    return 0.25 * (t0(i.uv + t * vec2<f32>(-1.0, -1.0)) + t0(i.uv + t * vec2<f32>(1.0, -1.0))
        + t0(i.uv + t * vec2<f32>(-1.0, 1.0)) + t0(i.uv + t * vec2<f32>(1.0, 1.0)));
}
// Shine cast: radial blur, 24 taps toward a light point.
@fragment fn fs_cast(i: VOut) -> @location(0) vec4<f32> {
    var c = vec4<f32>(0.0);
    let dir = (vec2<f32>(0.5, 0.2) - i.uv) / 24.0;
    for (var k = 0; k < 24; k++) {
        c += t0(i.uv + dir * f32(k)) * (1.0 - f32(k) / 24.0);
    }
    return c / 12.0;
}
fn w(k: i32) -> f32 {
    var g = array<f32, 5>(0.2270, 0.1945, 0.1216, 0.0540, 0.0162);
    return g[abs(k)];
}
@fragment fn fs_gauss_x(i: VOut) -> @location(0) vec4<f32> {
    let t = 1.0 / u.g_Texture0Resolution.xy;
    var c = vec4<f32>(0.0);
    for (var k = -4; k <= 4; k++) { c += w(k) * t0(i.uv + vec2<f32>(f32(k) * t.x, 0.0)); }
    return c;
}
@fragment fn fs_gauss_y(i: VOut) -> @location(0) vec4<f32> {
    let t = 1.0 / u.g_Texture0Resolution.xy;
    var c = vec4<f32>(0.0);
    for (var k = -4; k <= 4; k++) { c += w(k) * t0(i.uv + vec2<f32>(0.0, f32(k) * t.y)); }
    return c;
}
// Combine: slot 0 = blurred buffer, slot 1 or 2 = previous.
@fragment fn fs_combine(i: VOut) -> @location(0) vec4<f32> {
    let a = t0(i.uv);
    let b = textureSample(g_Texture1, g_Texture1_s, i.uv) + textureSample(g_Texture2, g_Texture2_s, i.uv);
    return vec4<f32>(b.rgb + a.rgb * 0.3, b.a);
}
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut opts = ContextOptions {
        profiling: true,
        ..ContextOptions::default()
    };
    let mut frames = 300u32;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--software" => opts.prefer_software = true,
            "--low-power" => opts.power_preference = wgpu::PowerPreference::LowPower,
            n => frames = n.parse().unwrap_or(frames),
        }
    }
    let ctx = GpuContext::new_headless(&opts)?;
    let info = ctx.adapter().get_info();
    println!(
        "adapter: {} ({:?}, {:?})",
        info.name, info.backend, info.device_type
    );

    let (w, h) = (3440u32, 1440u32);
    let size = Vec2::new(w as f32, h as f32);
    let mut b = SceneBuilder::new(&ctx, (size.x, size.y));
    let st = b
        .targets
        .create_scene_targets(&ctx, (w, h), SamplerKey::CLAMP_LINEAR);
    b.push(Op::Clear {
        target: st.full,
        color: [1.0, 1.0, 1.0, 1.0],
    });

    // 4K-class main texture (RGBA8; BC would be lighter) and two small helpers.
    let tex_data: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % 251) as u8, (i % 241) as u8, (i % 239) as u8, 255])
        .collect();
    let main = b.add_texture(&TextureUpload {
        label: "main",
        format: TexelFormat::Rgba8,
        levels: &[MipLevel {
            width: w,
            height: h,
            data: &tex_data,
        }],
        content_size: None,
        flags: TexFlags(TexFlags::CLAMP_UVS),
    })?;
    drop(tex_data);
    let small: Vec<u8> = (0..256 * 256)
        .flat_map(|i| [(i * 7 % 256) as u8; 4])
        .collect();
    let noise = b.add_texture(&TextureUpload {
        label: "noise",
        format: TexelFormat::Rgba8,
        levels: &[MipLevel {
            width: 256,
            height: 256,
            data: &small,
        }],
        content_size: None,
        flags: TexFlags::default(),
    })?;

    let mut sh = |fs: &str| -> Result<ShaderId, lumenwall_render::RenderError> {
        b.add_shader(ShaderDesc::from_wgsl(fs, SHADERS, "vs", fs)?)
    };
    let (s_image, s_fx, s_down, s_cast, s_gx, s_gy, s_comb) = (
        sh("fs_image")?,
        sh("fs_fx")?,
        sh("fs_down")?,
        sh("fs_cast")?,
        sh("fs_gauss_x")?,
        sh("fs_gauss_y")?,
        sh("fs_combine")?,
    );

    let flags = SamplerKey::for_render_target(TexFlags(TexFlags::CLAMP_UVS));
    let composites = b.targets.create_composites(&ctx, 1, (w, h), flags);
    let image_scope = b.targets.new_scope(b.targets.scene_scope());
    let fx_target = |b: &mut SceneBuilder<'_>, name: &str, scale: f32| -> RtId {
        let scope = b.targets.new_scope(image_scope);
        b.targets.create(
            &ctx,
            scope,
            name,
            scaled_size((size.x, size.y), scale),
            flags,
        )
    };
    let h1 = fx_target(&mut b, "_rt_HalfCompoBuffer1", 2.0);
    let h2 = fx_target(&mut b, "_rt_HalfCompoBuffer2", 2.0);
    let q1 = fx_target(&mut b, "_rt_QuarterCompoBuffer1", 4.0);
    let q2 = fx_target(&mut b, "_rt_QuarterCompoBuffer2", 4.0);

    let fx = |label: &str| {
        let mut p = ImagePass::new(label, s_fx);
        p.slots[1] = Some(SlotSource::Texture(TextureRef::Texture(noise)));
        p.slots[2] = Some(SlotSource::Texture(TextureRef::Texture(noise)));
        p
    };
    let with =
        |label: &str, shader: ShaderId, target: Option<RtId>, binds: &[(usize, SlotSource)]| {
            let mut p = ImagePass::new(label, shader);
            p.target = target;
            for (slot, src) in binds {
                p.slots[*slot] = Some(*src);
            }
            p
        };
    let t = |r: RtId| SlotSource::Texture(TextureRef::Target(r));
    let mut passes = vec![ImagePass::new("base", s_image)];
    for name in [
        "depthparallax",
        "iris",
        "shake",
        "foliagesway",
        "shake",
        "shake",
        "foliagesway",
        "pulse",
    ] {
        passes.push(fx(name));
    }
    passes.push(with(
        "shine.down",
        s_down,
        Some(h1),
        &[(0, SlotSource::Previous)],
    ));
    passes.push(with("shine.cast", s_cast, Some(h2), &[(0, t(h1))]));
    passes.push(with("shine.gx", s_gx, Some(h1), &[(0, t(h2))]));
    passes.push(with("shine.gy", s_gy, Some(h2), &[(0, t(h1))]));
    passes.push(with(
        "shine.combine",
        s_comb,
        None,
        &[(0, t(h2)), (1, SlotSource::Previous)],
    ));
    passes.push(with(
        "lc.down",
        s_down,
        Some(q1),
        &[(0, SlotSource::Previous)],
    ));
    passes.push(with("lc.gx", s_gx, Some(q2), &[(0, t(q1))]));
    passes.push(with("lc.gy", s_gy, Some(q1), &[(0, t(q2))]));
    passes.push(with(
        "lc.combine",
        s_comb,
        None,
        &[(0, t(q1)), (2, SlotSource::Previous)],
    ));
    passes.push(fx("chromaticaberration"));
    passes[0].blend = lumenwall_render::BlendMode::Translucent;

    let spec = ImageSpec {
        label: "edit".into(),
        size,
        alignment: Alignment::default(),
        main: TextureRef::Texture(main),
        main_sizes: ((w, h), (w, h)),
        animated: false,
        passthrough: false,
        fullscreen: false,
        composites,
        scene_target: st.full,
        final_to_scene: true,
    };
    let image = add_image(&mut b, &spec, &passes)?;

    let bt = b.targets.create_bloom_targets(&ctx, (w, h));
    let bloom_ab = b.targets.create_bloom_composites(&ctx, (w, h));
    let bloom_shaders = BloomShaders::internal(&mut b)?;
    add_bloom(
        &mut b,
        &bloom_shaders,
        &st,
        &bt,
        bloom_ab,
        BloomParams {
            strength: 0.5,
            threshold: 0.7,
        },
    )?;
    let t_build = Instant::now();
    let mut scene = b.build()?;
    println!(
        "build: {:?}, graph: {:?}",
        t_build.elapsed(),
        scene.counts()
    );
    println!(
        "GPU memory tracked: {:.1} MiB",
        ctx.memory().bytes() as f64 / (1024.0 * 1024.0)
    );

    let out = HeadlessOutput::new(&ctx, w, h);
    let mut compositor = Compositor::new(&ctx);
    let mut composer = compositor.create_output(
        &ctx,
        HeadlessOutput::FORMAT,
        scene.targets().get(st.full),
        ComposeSettings::default(),
    );
    composer.update(&ctx, (w, h), (w, h));

    let mut timer = GpuTimer::new(&ctx);
    if timer.is_none() {
        println!("timestamps unsupported: GPU time unavailable");
    }
    let (a16, a32, a64) = ([0.1; 16], [0.1; 32], [0.1; 64]);
    let vp = OrthoCamera::canonical(size.x, size.y).view_projection();
    let mut gpu_total = Duration::ZERO;
    let mut gpu_samples = 0u32;
    let mut cpu_total = Duration::ZERO;
    let wall = Instant::now();
    for f in 0..frames {
        let time = f as f32 / 30.0;
        let c0 = Instant::now();
        scene.update_frame(&FrameInputs {
            time,
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
                view_projection: vp,
                transform: NodeTransform {
                    origin: Vec3::new(size.x / 2.0, size.y / 2.0, 0.0),
                    ..NodeTransform::default()
                },
                parallax: Vec3::ZERO,
                scene_size: size,
            },
        );
        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        if let Some(t) = timer.as_mut() {
            t.begin(&mut enc);
        }
        scene.encode(&ctx, &mut enc);
        compositor.encode(
            &mut enc,
            &composer,
            out.view(),
            [0.0, 0.0, w as f32, h as f32],
            Some(wgpu::Color::BLACK),
        );
        if let Some(t) = timer.as_mut() {
            t.end(&mut enc);
        }
        ctx.queue().submit(std::iter::once(enc.finish()));
        if let Some(t) = timer.as_mut() {
            t.after_submit();
        }
        cpu_total += c0.elapsed();
        // Pace like a 30 fps loop would: wait for the frame, then read the timer.
        ctx.wait_idle();
        if let Some(t) = timer.as_mut()
            && f >= 10
            && let Some(d) = t.poll(&ctx)
        {
            gpu_total += d;
            gpu_samples += 1;
        }
    }
    println!(
        "{frames} frames in {:?}: CPU update+encode {:.3} ms/frame, GPU {} ({} samples)",
        wall.elapsed(),
        cpu_total.as_secs_f64() * 1000.0 / f64::from(frames),
        if gpu_samples > 0 {
            format!(
                "{:.3} ms/frame",
                gpu_total.as_secs_f64() * 1000.0 / f64::from(gpu_samples)
            )
        } else {
            "n/a".into()
        },
        gpu_samples
    );
    Ok(())
}

//! Headless GPU tests. They need a Vulkan (or DX12) adapter; lavapipe works
//! (`LUMEN_TEST_SOFTWARE=1` forces a CPU adapter). Without any adapter each
//! test prints "skipped" and returns.

use std::sync::OnceLock;

use glam::{Vec2, Vec3};
use lumen_render::blend::{BlendMode, CullMode};
use lumen_render::bloom::{BloomParams, BloomShaders, add_bloom};
use lumen_render::compose::{ClampMode, ComposeSettings, Compositor, ScalingMode};
use lumen_render::dynbuf::FrameInputs;
use lumen_render::graph::{Geometry, Op, PassDesc, SceneBuilder, SlotBinding, TextureRef};
use lumen_render::headless::{HeadlessOutput, read_texture_rgba8};
use lumen_render::image::{ImageFrame, ImagePass, ImageSpec, SlotSource, add_image};
use lumen_render::math::{Alignment, NodeTransform, OrthoCamera, unit_quad};
use lumen_render::shader::ShaderDesc;
use lumen_render::stats::GpuTimer;
use lumen_render::targets::SceneTargets;
use lumen_render::texture::{MipLevel, SamplerKey, TexFlags, TexelFormat, TextureUpload};
use lumen_render::{ContextOptions, GpuContext, RenderScene, TextureId};

const SRC: &str = r"
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
@fragment fn fs_invert(i: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(g_Texture0, g_Texture0_s, i.uv);
    return vec4<f32>(1.0 - c.rgb, c.a);
}
@fragment fn fs_add(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv) + textureSample(g_Texture1, g_Texture1_s, i.uv);
}
@fragment fn fs_time(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(fract(u.g_Time), 0.0, 0.0, 1.0);
}
@fragment fn fs_resolution(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(u.g_Texture0Resolution.x / 255.0, 0.0, 0.0, 1.0);
}
";

fn shared() -> Option<&'static GpuContext> {
    static CTX: OnceLock<Option<GpuContext>> = OnceLock::new();
    CTX.get_or_init(new_ctx).as_ref()
}

fn new_ctx() -> Option<GpuContext> {
    let opts = ContextOptions {
        prefer_software: std::env::var_os("LUMEN_TEST_SOFTWARE").is_some(),
        profiling: true,
        ..ContextOptions::default()
    };
    match GpuContext::new_headless(&opts) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("no GPU adapter: {e}");
            None
        }
    }
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

fn texture(
    b: &mut SceneBuilder<'_>,
    w: u32,
    h: u32,
    content: Option<(u32, u32)>,
    f: impl Fn(u32, u32) -> [u8; 4],
) -> TextureId {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&f(x, y));
        }
    }
    let levels = [MipLevel {
        width: w,
        height: h,
        data: &data,
    }];
    b.add_texture(&TextureUpload {
        label: "test",
        format: TexelFormat::Rgba8,
        levels: &levels,
        content_size: content,
        flags: TexFlags(TexFlags::CLAMP_UVS),
    })
    .unwrap()
}

fn shader(b: &mut SceneBuilder<'_>, fs: &str) -> lumen_render::ShaderId {
    b.add_shader(ShaderDesc::from_wgsl(fs, SRC, "vs", fs).unwrap())
        .unwrap()
}

struct Setup<'c> {
    b: SceneBuilder<'c>,
    st: SceneTargets,
    size: Vec2,
}

fn setup(ctx: &GpuContext, w: u32, h: u32, clear: [f64; 4]) -> Setup<'_> {
    let mut b = SceneBuilder::new(ctx, (w as f32, h as f32));
    let st = b
        .targets
        .create_scene_targets(ctx, (w, h), SamplerKey::CLAMP_LINEAR);
    b.push(Op::Clear {
        target: st.full,
        color: clear,
    });
    Setup {
        b,
        st,
        size: Vec2::new(w as f32, h as f32),
    }
}

fn image_spec(
    s: &mut Setup<'_>,
    ctx: &GpuContext,
    id: i64,
    size: Vec2,
    main: TextureId,
) -> ImageSpec {
    let t = s.b.texture(main).unwrap();
    let sizes = (t.storage_size(), t.content_size());
    let composites = s.b.targets.create_composites(
        ctx,
        id,
        (size.x as u32, size.y as u32),
        SamplerKey::CLAMP_LINEAR,
    );
    ImageSpec {
        label: format!("image {id}"),
        size,
        alignment: Alignment::default(),
        main: TextureRef::Texture(main),
        main_sizes: sizes,
        animated: false,
        passthrough: false,
        fullscreen: false,
        composites,
        scene_target: s.st.full,
        final_to_scene: true,
    }
}

fn frame(size: Vec2, origin: Vec3, scale: Vec3) -> ImageFrame {
    ImageFrame {
        view_projection: OrthoCamera::canonical(size.x, size.y).view_projection(),
        transform: NodeTransform {
            origin,
            scale,
            angle: 0.0,
        },
        parallax: Vec3::ZERO,
        scene_size: size,
    }
}

fn read(ctx: &GpuContext, scene: &RenderScene, s: &SceneTargets) -> Vec<u8> {
    read_texture_rgba8(ctx, scene.targets().get(s.full).texture()).unwrap()
}

fn px(data: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [data[i], data[i + 1], data[i + 2], data[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| x.abs_diff(*y) <= tol)
}

/// Texel row 0 must land on scene row 0, and the composed output must show it
/// at the top (spec 04 §0.2, §10.2, §21.4).
#[test]
fn orientation_is_preserved_end_to_end() {
    let ctx = gpu!();
    let mut s = setup(ctx, 64, 64, [0.0, 1.0, 0.0, 1.0]);
    let tex = texture(&mut s.b, 8, 8, None, |_, y| {
        if y < 4 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        }
    });
    let sh = shader(&mut s.b, "fs_copy");
    let spec = image_spec(&mut s, ctx, 1, Vec2::new(64.0, 64.0), tex);
    let img = add_image(&mut s.b, &spec, &[ImagePass::new("base", sh)]).unwrap();
    let mut compositor = Compositor::new(ctx);
    let mut scene = s.b.build().unwrap();
    img.update(
        &mut scene,
        &frame(s.size, Vec3::new(32.0, 32.0, 0.0), Vec3::ONE),
    );
    scene.render(ctx);
    let d = read(ctx, &scene, &s.st);
    assert!(
        near(px(&d, 64, 5, 2), [255, 0, 0, 255], 2),
        "top is red: {:?}",
        px(&d, 64, 5, 2)
    );
    assert!(
        near(px(&d, 64, 5, 60), [0, 0, 255, 255], 2),
        "bottom is blue"
    );

    let out = HeadlessOutput::new(ctx, 32, 32);
    let mut oc = compositor.create_output(
        ctx,
        HeadlessOutput::FORMAT,
        scene.targets().get(s.st.full),
        ComposeSettings::default(),
    );
    oc.update(ctx, (32, 32), (64, 64));
    let mut enc = ctx.device().create_command_encoder(&Default::default());
    compositor.encode(
        &mut enc,
        &oc,
        out.view(),
        [0.0, 0.0, 32.0, 32.0],
        Some(wgpu::Color::BLACK),
    );
    ctx.queue().submit([enc.finish()]);
    let o = out.read_rgba8(ctx).unwrap();
    assert!(near(px(&o, 32, 16, 1), [255, 0, 0, 255], 2));
    assert!(near(px(&o, 32, 16, 30), [0, 0, 255, 255], 2));
}

/// Power-of-two padding: only the content (top-left) is shown (spec 04 §8.7).
#[test]
fn padded_texture_uses_content_window() {
    let ctx = gpu!();
    let mut s = setup(ctx, 32, 32, [0.0, 0.0, 0.0, 1.0]);
    let tex = texture(&mut s.b, 8, 8, Some((4, 8)), |x, _| {
        if x < 4 {
            [255, 0, 0, 255]
        } else {
            [255, 255, 255, 255]
        }
    });
    let sh = shader(&mut s.b, "fs_copy");
    let spec = image_spec(&mut s, ctx, 1, Vec2::new(32.0, 32.0), tex);
    let img = add_image(&mut s.b, &spec, &[ImagePass::new("base", sh)]).unwrap();
    let mut scene = s.b.build().unwrap();
    img.update(
        &mut scene,
        &frame(s.size, Vec3::new(16.0, 16.0, 0.0), Vec3::ONE),
    );
    scene.render(ctx);
    let d = read(ctx, &scene, &s.st);
    // Bilinear filtering bleeds into the padding only at the last half texel
    // (as upstream); everything left of it is pure content.
    for x in [1, 16, 24] {
        assert!(
            near(px(&d, 32, x, 16), [255, 0, 0, 255], 3),
            "x={x}: {:?}",
            px(&d, 32, x, 16)
        );
    }
}

/// `cullmode normal` removes a mirrored image, `nocull` keeps it (spec 04 §11.5, §24.3).
#[test]
fn culling_and_front_face() {
    let ctx = gpu!();
    for (scale_x, cull, drawn) in [
        (1.0, CullMode::Back, true),
        (-1.0, CullMode::Back, false),
        (-1.0, CullMode::None, true),
    ] {
        let mut s = setup(ctx, 32, 32, [0.0, 1.0, 0.0, 1.0]);
        let tex = texture(&mut s.b, 4, 4, None, |_, _| [255, 0, 0, 255]);
        let sh = shader(&mut s.b, "fs_copy");
        let spec = image_spec(&mut s, ctx, 1, Vec2::new(16.0, 16.0), tex);
        let mut p = ImagePass::new("base", sh);
        p.cull = cull;
        let img = add_image(&mut s.b, &spec, &[p]).unwrap();
        let mut scene = s.b.build().unwrap();
        img.update(
            &mut scene,
            &frame(
                s.size,
                Vec3::new(16.0, 16.0, 0.0),
                Vec3::new(scale_x, 1.0, 1.0),
            ),
        );
        scene.render(ctx);
        let d = read(ctx, &scene, &s.st);
        let c = px(&d, 32, 16, 16);
        assert_eq!(
            c == [255, 0, 0, 255],
            drawn,
            "scale {scale_x}, {cull:?}: {c:?}"
        );
    }
}

/// Translucent blending on the scene and RGB-only writes keep alpha = 1 (spec 04 §11.6, §14).
#[test]
fn translucent_blend_keeps_scene_alpha() {
    let ctx = gpu!();
    let mut s = setup(ctx, 16, 16, [0.0, 0.0, 1.0, 1.0]);
    let tex = texture(&mut s.b, 4, 4, None, |_, _| [255, 0, 0, 128]);
    let sh = shader(&mut s.b, "fs_copy");
    let spec = image_spec(&mut s, ctx, 1, Vec2::new(16.0, 16.0), tex);
    let mut p = ImagePass::new("base", sh);
    p.blend = BlendMode::Translucent;
    let img = add_image(&mut s.b, &spec, &[p]).unwrap();
    let mut scene = s.b.build().unwrap();
    img.update(
        &mut scene,
        &frame(s.size, Vec3::new(8.0, 8.0, 0.0), Vec3::ONE),
    );
    scene.render(ctx);
    let d = read(ctx, &scene, &s.st);
    assert!(
        near(px(&d, 16, 8, 8), [128, 0, 127, 255], 2),
        "{:?}",
        px(&d, 16, 8, 8)
    );
}

/// Base pass + two effects through A/B: invert twice = identity, and the final
/// pass carries the model's blend (spec 04 §9.8, §10).
#[test]
fn ping_pong_chain() {
    let ctx = gpu!();
    let mut s = setup(ctx, 32, 32, [0.0, 0.0, 0.0, 1.0]);
    let tex = texture(&mut s.b, 4, 4, None, |x, _| [40 * x as u8, 200, 10, 255]);
    let copy = shader(&mut s.b, "fs_copy");
    let inv = shader(&mut s.b, "fs_invert");
    let spec = image_spec(&mut s, ctx, 7, Vec2::new(32.0, 32.0), tex);
    let img = add_image(
        &mut s.b,
        &spec,
        &[
            ImagePass::new("base", copy),
            ImagePass::new("inv1", inv),
            ImagePass::new("inv2", inv),
        ],
    )
    .unwrap();
    let mut scene = s.b.build().unwrap();
    assert_eq!(scene.counts().passes, 3);
    img.update(
        &mut scene,
        &frame(s.size, Vec3::new(16.0, 16.0, 0.0), Vec3::ONE),
    );
    scene.render(ctx);
    let d = read(ctx, &scene, &s.st);
    // Column 0 of the texture (x = 0 → red 0).
    assert!(
        near(px(&d, 32, 1, 16), [0, 200, 10, 255], 2),
        "{:?}",
        px(&d, 32, 1, 16)
    );
    let b = read_texture_rgba8(ctx, scene.targets().get(spec.composites.b).texture()).unwrap();
    assert!(
        near(px(&b, 32, 1, 16), [255, 55, 245, 255], 2),
        "B holds the first inversion"
    );
}

/// A pass that samples its own destination reads a copy (spec 04 §24.6).
#[test]
fn read_write_hazard_uses_a_copy() {
    let ctx = gpu!();
    let mut s = setup(ctx, 16, 16, [0.25, 0.0, 0.0, 1.0]);
    let tex = texture(&mut s.b, 4, 4, None, |_, _| [0, 64, 0, 255]);
    let add = shader(&mut s.b, "fs_add");
    let spec = image_spec(&mut s, ctx, 1, Vec2::new(16.0, 16.0), tex);
    let mut p = ImagePass::new("add", add);
    p.slots[1] = Some(SlotSource::Texture(TextureRef::Target(s.st.full)));
    let img = add_image(&mut s.b, &spec, &[p]).unwrap();
    let mut scene = s.b.build().unwrap();
    assert_eq!(scene.counts().copies, 1);
    img.update(
        &mut scene,
        &frame(s.size, Vec3::new(8.0, 8.0, 0.0), Vec3::ONE),
    );
    scene.render(ctx);
    let d = read(ctx, &scene, &s.st);
    assert!(
        near(px(&d, 16, 8, 8), [64, 64, 0, 255], 2),
        "{:?}",
        px(&d, 16, 8, 8)
    );
}

/// Attributes the engine does not feed read (0, 0, 0, 1) (spec 04 §11.5).
#[test]
fn unfed_attributes_default() {
    let ctx = gpu!();
    let src = r"
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(2) a_Color: vec4<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(a_Position, 1.0);
    o.c = a_Color + vec4<f32>(0.25, 0.0, 0.0, 0.0);
    return o;
}
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32> }
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> { return i.c; }
";
    let mut s = setup(ctx, 8, 8, [1.0, 1.0, 1.0, 1.0]);
    let sh =
        s.b.add_shader(ShaderDesc::from_wgsl("unfed", src, "vs", "fs").unwrap())
            .unwrap();
    let v = s.b.add_vertices(&unit_quad());
    let id =
        s.b.add_pass(PassDesc::new("unfed", sh, s.st.full, Geometry::Vertices(v)))
            .unwrap();
    s.b.push(Op::Draw(id));
    let mut scene = s.b.build().unwrap();
    scene.render(ctx);
    let d = read(ctx, &scene, &s.st);
    assert!(
        near(px(&d, 8, 4, 4), [64, 0, 0, 255], 1),
        "{:?}",
        px(&d, 8, 4, 4)
    );
}

/// Animated textures switch bind groups and update g_Texture0Resolution.
#[test]
fn slot0_variants() {
    let ctx = gpu!();
    let mut s = setup(ctx, 8, 8, [0.0, 0.0, 0.0, 1.0]);
    let a = texture(&mut s.b, 4, 4, None, |_, _| [255, 0, 0, 255]);
    let b = texture(&mut s.b, 8, 8, None, |_, _| [0, 0, 255, 255]);
    let copy = shader(&mut s.b, "fs_copy");
    let res = shader(&mut s.b, "fs_resolution");
    let v = s.b.add_vertices(&unit_quad());
    let mut d1 = PassDesc::new("anim", copy, s.st.full, Geometry::Vertices(v));
    d1.slot0_variants = vec![TextureRef::Texture(a), TextureRef::Texture(b)];
    let p1 = s.b.add_pass(d1).unwrap();
    s.b.push(Op::Draw(p1));
    // Second pass: not drawn, only used to observe its uniform block.
    let mut d2 = PassDesc::new("res", res, s.st.full, Geometry::Vertices(v));
    d2.slot0_variants = vec![TextureRef::Texture(a), TextureRef::Texture(b)];
    let p2 = s.b.add_pass(d2).unwrap();
    let mut scene = s.b.build().unwrap();
    assert_eq!(scene.counts().bundles, 0, "variant passes are not frozen");
    scene.render(ctx);
    let d = read(ctx, &scene, &s.st);
    assert!(near(px(&d, 8, 4, 4), [255, 0, 0, 255], 1));
    scene.set_variant(p1, 1);
    assert!(scene.needs_redraw());
    scene.render(ctx);
    let d = read(ctx, &scene, &s.st);
    assert!(near(px(&d, 8, 4, 4), [0, 0, 255, 255], 1));
    // The resolution of slot 0 follows the variant.
    let slot = scene.uniform(p2, "g_Texture0Resolution").unwrap();
    let off = slot.offset as usize;
    scene.set_variant(p2, 1);
    let bytes = &scene.shadow_mut().bytes()[off..off + 4];
    assert_eq!(f32::from_le_bytes(bytes.try_into().unwrap()), 8.0);
}

/// The five bloom passes run and spread light around a bright image (spec 04 §17).
#[test]
fn bloom_chain() {
    let ctx = gpu!();
    let mut s = setup(ctx, 64, 64, [0.0, 0.0, 0.0, 1.0]);
    let tex = texture(&mut s.b, 4, 4, None, |_, _| [255, 255, 255, 255]);
    let sh = shader(&mut s.b, "fs_copy");
    let spec = image_spec(&mut s, ctx, 1, Vec2::new(16.0, 16.0), tex);
    let img = add_image(&mut s.b, &spec, &[ImagePass::new("base", sh)]).unwrap();
    let bt = s.b.targets.create_bloom_targets(ctx, (64, 64));
    let comps = s.b.targets.create_bloom_composites(ctx, (64, 64));
    let shaders = BloomShaders::internal(&mut s.b).unwrap();
    add_bloom(
        &mut s.b,
        &shaders,
        &s.st,
        &bt,
        comps,
        BloomParams {
            strength: 2.0,
            threshold: 0.5,
        },
    )
    .unwrap();
    let mut scene = s.b.build().unwrap();
    assert_eq!(scene.counts().passes, 6);
    img.update(
        &mut scene,
        &frame(s.size, Vec3::new(32.0, 32.0, 0.0), Vec3::ONE),
    );
    scene.render(ctx);
    let d = read(ctx, &scene, &s.st);
    let glow = px(&d, 64, 20, 32);
    assert!(glow[0] > 10, "bloom spreads outside the image: {glow:?}");
    assert_eq!(glow[3], 255, "combine writes RGB only");
    assert!(near(px(&d, 64, 32, 32), [255, 255, 255, 255], 0));
}

/// Fit + border gives black bands; clamp repeats the edge (spec 04 §21).
#[test]
fn composition_modes() {
    let ctx = gpu!();
    let s = setup(ctx, 100, 100, [1.0, 1.0, 1.0, 1.0]);
    let full = s.st.full;
    let mut scene = s.b.build().unwrap();
    scene.render(ctx);
    let mut compositor = Compositor::new(ctx);
    let out = HeadlessOutput::new(ctx, 200, 100);
    for (clamp, band) in [
        (ClampMode::Border, [0, 0, 0, 255]),
        (ClampMode::Clamp, [255, 255, 255, 255]),
    ] {
        let settings = ComposeSettings {
            scaling: ScalingMode::Fit,
            clamp,
            flip_vertical: false,
        };
        let mut oc = compositor.create_output(
            ctx,
            HeadlessOutput::FORMAT,
            scene.targets().get(full),
            settings,
        );
        let r = oc.update(ctx, (200, 100), (100, 100));
        assert_eq!((r.u0, r.u1), (-0.5, 1.5));
        let mut enc = ctx.device().create_command_encoder(&Default::default());
        compositor.encode(
            &mut enc,
            &oc,
            out.view(),
            [0.0, 0.0, 200.0, 100.0],
            Some(wgpu::Color::RED),
        );
        ctx.queue().submit([enc.finish()]);
        let o = out.read_rgba8(ctx).unwrap();
        assert_eq!(px(&o, 200, 10, 50), band, "{clamp:?}");
        assert_eq!(px(&o, 200, 100, 50), [255, 255, 255, 255]);
    }
}

/// BC1 data is uploaded as-is when the GPU supports it.
#[test]
fn bc1_upload() {
    let ctx = gpu!();
    if !ctx.supports_bc() {
        eprintln!("skipped: no BC support");
        return;
    }
    let mut s = setup(ctx, 8, 8, [0.0, 0.0, 0.0, 1.0]);
    // One 4x4 block: color0 = pure red (RGB565 0xF800), color1 = black, all indices 0.
    let block = [0x00, 0xF8, 0x00, 0x00, 0, 0, 0, 0];
    let levels = [MipLevel {
        width: 4,
        height: 4,
        data: &block,
    }];
    let tex =
        s.b.add_texture(&TextureUpload {
            label: "bc1",
            format: TexelFormat::Bc1,
            levels: &levels,
            content_size: None,
            flags: TexFlags::default(),
        })
        .unwrap();
    let sh = shader(&mut s.b, "fs_copy");
    let v = s.b.add_vertices(&unit_quad());
    let mut desc = PassDesc::new("bc1", sh, s.st.full, Geometry::Vertices(v))
        .with_slot(0, SlotBinding::texture(tex));
    desc.blend = BlendMode::Normal;
    let id = s.b.add_pass(desc).unwrap();
    s.b.push(Op::Draw(id));
    let mut scene = s.b.build().unwrap();
    scene.render(ctx);
    assert!(near(
        px(&read(ctx, &scene, &s.st), 8, 4, 4),
        [255, 0, 0, 255],
        1
    ));
}

/// Image visibility toggles its passes (spec 04 Q8: an image made visible
/// later composes into the scene).
#[test]
fn visibility_toggle() {
    let ctx = gpu!();
    let mut s = setup(ctx, 16, 16, [0.0, 1.0, 0.0, 1.0]);
    let tex = texture(&mut s.b, 4, 4, None, |_, _| [255, 0, 0, 255]);
    let sh = shader(&mut s.b, "fs_copy");
    let spec = image_spec(&mut s, ctx, 1, Vec2::new(16.0, 16.0), tex);
    let img = add_image(
        &mut s.b,
        &spec,
        &[ImagePass::new("base", sh), ImagePass::new("fx", sh)],
    )
    .unwrap();
    let mut scene = s.b.build().unwrap();
    img.update(
        &mut scene,
        &frame(s.size, Vec3::new(8.0, 8.0, 0.0), Vec3::ONE),
    );
    scene.set_enabled(ctx, &img.passes, false);
    scene.render(ctx);
    assert_eq!(px(&read(ctx, &scene, &s.st), 16, 8, 8), [0, 255, 0, 255]);
    scene.set_enabled(ctx, &img.passes, true);
    assert!(scene.needs_redraw());
    scene.render(ctx);
    assert_eq!(px(&read(ctx, &scene, &s.st), 16, 8, 8), [255, 0, 0, 255]);
}

/// The wgpu pipeline cache is written to disk (Vulkan only).
#[test]
fn pipeline_cache_is_saved() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("lumen-pipeline-cache");
    let _ = std::fs::remove_dir_all(&dir);
    let opts = ContextOptions {
        pipeline_cache_dir: Some(dir.clone()),
        prefer_software: std::env::var_os("LUMEN_TEST_SOFTWARE").is_some(),
        ..ContextOptions::default()
    };
    let Ok(ctx) = GpuContext::new_headless(&opts) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    if !ctx.caps().pipeline_cache {
        eprintln!("skipped: no pipeline cache on this backend");
        return;
    }
    let mut s = setup(&ctx, 8, 8, [0.0; 4]);
    let sh = shader(&mut s.b, "fs_copy");
    let v = s.b.add_vertices(&unit_quad());
    let id =
        s.b.add_pass(PassDesc::new("p", sh, s.st.full, Geometry::Vertices(v)))
            .unwrap();
    s.b.push(Op::Draw(id));
    let _scene = s.b.build().unwrap();
    ctx.save_pipeline_cache().unwrap();
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    assert!(files[0].metadata().unwrap().len() > 0);
    // A second context loads it without error.
    let ctx2 = GpuContext::new_headless(&opts).unwrap();
    assert!(ctx2.pipeline_cache().is_some());
}

/// Steady state: no GPU object created per frame, dirty tracking, one
/// uniform upload per frame, timestamps.
#[test]
fn steady_state_frames() {
    // Own device: the resource counter must not see other tests.
    let Some(ctx) = new_ctx() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let ctx = &ctx;
    let mut s = setup(ctx, 32, 32, [0.0, 0.0, 0.0, 1.0]);
    let tex = texture(&mut s.b, 4, 4, None, |_, _| [255, 255, 255, 255]);
    let copy = shader(&mut s.b, "fs_copy");
    let time = shader(&mut s.b, "fs_time");
    let spec = image_spec(&mut s, ctx, 1, Vec2::new(16.0, 16.0), tex);
    let img = add_image(
        &mut s.b,
        &spec,
        &[ImagePass::new("base", copy), ImagePass::new("timed", time)],
    )
    .unwrap();
    let mut scene = s.b.build().unwrap();
    assert!(
        scene.counts().bundles >= 1,
        "frozen passes are pre-recorded"
    );
    let mut timer = GpuTimer::new(ctx);
    let audio16 = [0.0; 16];
    let audio32 = [0.0; 32];
    let audio64 = [0.0; 64];
    let f = frame(s.size, Vec3::new(16.0, 16.0, 0.0), Vec3::ONE);
    let render = |scene: &mut RenderScene, timer: &mut Option<GpuTimer>, t: f32| {
        scene.update_frame(&FrameInputs {
            time: t,
            daytime: 0.5,
            pointer: [0.5, 0.5],
            pointer_last: [0.5, 0.5],
            audio16: &audio16,
            audio32: &audio32,
            audio64: &audio64,
        });
        img.update(scene, &f);
        let mut enc = ctx.device().create_command_encoder(&Default::default());
        if let Some(t) = timer.as_mut() {
            t.begin(&mut enc);
        }
        scene.encode(ctx, &mut enc);
        if let Some(t) = timer.as_mut() {
            t.end(&mut enc);
        }
        ctx.queue().submit([enc.finish()]);
        if let Some(t) = timer.as_mut() {
            t.after_submit();
        }
    };
    render(&mut scene, &mut timer, 0.25);
    let resources = ctx.resources_created();
    let memory = ctx.memory().bytes();
    for i in 0..10 {
        render(&mut scene, &mut timer, 0.25 + i as f32 * 0.01);
        ctx.wait_idle();
        if let Some(t) = timer.as_mut() {
            t.poll(ctx);
        }
    }
    assert_eq!(
        ctx.resources_created(),
        resources,
        "no GPU object created per frame"
    );
    assert_eq!(ctx.memory().bytes(), memory);
    let d = read(ctx, &scene, &s.st);
    assert!(
        near(px(&d, 32, 16, 16), [87, 0, 0, 255], 2),
        "g_Time reaches the pass block: {:?}",
        px(&d, 32, 16, 16)
    );
    // Same inputs again: nothing to redraw (no accumulation in this graph).
    render(&mut scene, &mut timer, 0.34);
    assert!(!scene.accumulates());
    scene.update_frame(&FrameInputs {
        time: 0.34,
        daytime: 0.5,
        pointer: [0.5, 0.5],
        pointer_last: [0.5, 0.5],
        audio16: &audio16,
        audio32: &audio32,
        audio64: &audio64,
    });
    img.update(&mut scene, &f);
    assert!(!scene.needs_redraw(), "static frame can be skipped");
    scene.update_frame(&FrameInputs {
        time: 0.5,
        daytime: 0.5,
        pointer: [0.5, 0.5],
        pointer_last: [0.5, 0.5],
        audio16: &audio16,
        audio32: &audio32,
        audio64: &audio64,
    });
    assert!(
        scene.needs_redraw(),
        "time change dirties a time-dependent scene"
    );
    if let Some(t) = timer.as_ref() {
        assert!(t.last().is_some(), "GPU timer produced a measurement");
    }
}

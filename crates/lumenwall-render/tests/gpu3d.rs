//! Headless GPU tests of the 3D additions (spec 09): static meshes, depth buffers, the Wallpaper
//! Engine clip convention (F-3D-O), back-face culling with `FrontFace::Ccw`, clip-space images.
//! Without an adapter each test prints "skipped".

use std::sync::OnceLock;

use glam::{Mat4, Vec3};
use lumenwall_render::blend::{BlendMode, CullMode};
use lumenwall_render::graph::{
    DepthState, Geometry, MeshAttribute, MeshLayout, Op, PassDesc, SceneBuilder,
};
use lumenwall_render::headless::read_texture_rgba8;
use lumenwall_render::math::{look_at, perspective_we};
use lumenwall_render::shader::ShaderDesc;
use lumenwall_render::texture::SamplerKey;
use lumenwall_render::{ContextOptions, GpuContext};

const SRC: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_Color: vec3<f32>,
}
@group(0) @binding(0) var<uniform> u: Pass;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs_clip(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(a_TexCoord * 2.0 - 1.0, 0.5, 1.0);
    o.uv = a_TexCoord;
    return o;
}
@vertex fn vs_mvp(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    return o;
}
@fragment fn fs_v(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(i.uv.y, 0.0, 0.0, 1.0);
}
@fragment fn fs_uv(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(i.uv.x, i.uv.y, 0.0, 1.0);
}
@fragment fn fs_color(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(u.g_Color, 1.0);
}
";

fn shared() -> Option<&'static GpuContext> {
    static CTX: OnceLock<Option<GpuContext>> = OnceLock::new();
    CTX.get_or_init(|| {
        let opts = ContextOptions {
            prefer_software: std::env::var_os("LUMENWALL_TEST_SOFTWARE").is_some(),
            ..ContextOptions::default()
        };
        GpuContext::new_headless(&opts)
            .map_err(|e| eprintln!("no GPU adapter: {e}"))
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

/// 3D shader: WE clip convention, no correction (spec 09 §0.3).
fn shader3d(b: &mut SceneBuilder<'_>, vs: &str, fs: &str) -> lumenwall_render::ShaderId {
    let mut d = ShaderDesc::from_wgsl(fs, SRC, vs, fs).unwrap();
    d.upstream_clip_space = false;
    b.add_shader(d).unwrap()
}

/// F-3D1 quad (spec 09 §4.1) as a position + UV mesh, optionally moved along z and coloured.
fn quad_mesh(b: &mut SceneBuilder<'_>, z: f32, clockwise: bool) -> lumenwall_render::MeshId {
    let verts: [([f32; 3], [f32; 2]); 4] = [
        ([-1.0, -1.0, z], [0.0, 1.0]),
        ([1.0, -1.0, z], [1.0, 1.0]),
        ([1.0, 1.0, z], [1.0, 0.0]),
        ([-1.0, 1.0, z], [0.0, 0.0]),
    ];
    let mut bytes = Vec::new();
    for (p, uv) in verts {
        for f in p.iter().chain(&uv) {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    let indices: &[u16] = if clockwise {
        &[0, 2, 1, 0, 3, 2]
    } else {
        &[0, 1, 2, 0, 2, 3]
    };
    let layout = MeshLayout {
        stride: 20,
        attributes: vec![
            MeshAttribute {
                name: "a_Position".into(),
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
            },
            MeshAttribute {
                name: "a_TexCoord".into(),
                format: wgpu::VertexFormat::Float32x2,
                offset: 12,
            },
        ],
    };
    b.add_mesh("quad", layout, &bytes, indices)
}

fn read(
    ctx: &GpuContext,
    scene: &lumenwall_render::RenderScene,
    rt: lumenwall_render::RtId,
) -> (u32, Vec<u8>) {
    let t = scene.targets().get(rt);
    let (w, _) = t.size();
    (w, read_texture_rgba8(ctx, t.texture()).unwrap())
}

#[test]
fn f_3d_o_orientation() {
    let ctx = gpu!();
    let (w, h) = (16u32, 16u32);
    let mut b = SceneBuilder::new(ctx, (w as f32, h as f32));
    let st = b
        .targets
        .create_scene_targets(ctx, (w, h), SamplerKey::CLAMP_LINEAR);
    b.add_depth(st.full);
    b.push(Op::Clear {
        target: st.full,
        color: [0.0, 0.0, 0.0, 1.0],
    });
    let mesh = quad_mesh(&mut b, 0.0, false);
    let sh = shader3d(&mut b, "vs_clip", "fs_v");
    let mut desc = PassDesc::new("f3do", sh, st.full, Geometry::Mesh(mesh));
    desc.front_face = wgpu::FrontFace::Ccw;
    let p = b.add_pass(desc).unwrap();
    b.push(Op::Draw(p));
    let mut scene = b.build().unwrap();
    scene.render(ctx);
    let (_, px) = read(ctx, &scene, st.full);
    let red = |x: u32, y: u32| px[((y * w + x) * 4) as usize];
    // As in Wallpaper Engine: v = 1 on the first row.
    assert!(red(8, 0) > 230, "first row {}", red(8, 0));
    assert!(red(8, h - 1) < 25, "last row {}", red(8, h - 1));
}

#[test]
fn f_3d_o_with_matrices_and_depth() {
    let ctx = gpu!();
    let (w, h) = (64u32, 36u32);
    let mut b = SceneBuilder::new(ctx, (w as f32, h as f32));
    let st = b
        .targets
        .create_scene_targets(ctx, (w, h), SamplerKey::CLAMP_LINEAR);
    b.add_depth(st.full);
    b.push(Op::Clear {
        target: st.full,
        color: [0.0, 0.0, 0.0, 1.0],
    });
    let vp = perspective_we(50.0, w as f32 / h as f32, 0.01, 10000.0)
        * look_at(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), Vec3::Y);
    // Quad at z = −3 with UV colours: (−1, 1) has UV (0, 0) and must land top-left.
    let near = quad_mesh(&mut b, -3.0, false);
    let sh_uv = shader3d(&mut b, "vs_mvp", "fs_uv");
    let sh_col = shader3d(&mut b, "vs_mvp", "fs_color");
    let mut desc = PassDesc::new("near", sh_uv, st.full, Geometry::Mesh(near));
    desc.front_face = wgpu::FrontFace::Ccw;
    desc.cull = CullMode::Back;
    desc.depth = DepthState {
        test: true,
        write: true,
    };
    let p_near = b.add_pass(desc).unwrap();
    // A larger quad behind, drawn after: hidden by the depth test where they overlap.
    let far = quad_mesh(&mut b, -6.0, false);
    let mut desc = PassDesc::new("far", sh_col, st.full, Geometry::Mesh(far));
    desc.front_face = wgpu::FrontFace::Ccw;
    desc.cull = CullMode::Back;
    desc.depth = DepthState {
        test: true,
        write: true,
    };
    let p_far = b.add_pass(desc).unwrap();
    // A clockwise (back-facing) quad in front of everything: culled.
    let back = quad_mesh(&mut b, -1.5, true);
    let mut desc = PassDesc::new("back", sh_col, st.full, Geometry::Mesh(back));
    desc.front_face = wgpu::FrontFace::Ccw;
    desc.cull = CullMode::Back;
    desc.blend = BlendMode::Normal;
    let p_back = b.add_pass(desc).unwrap();
    for (p, m, colour) in [
        (p_near, vp, [0.0, 0.0, 0.0]),
        (
            p_far,
            vp * Mat4::from_scale(Vec3::new(4.0, 4.0, 1.0)),
            [0.0, 0.0, 1.0],
        ),
        (p_back, vp, [1.0, 1.0, 1.0]),
    ] {
        if let Some(s) = b.uniform(p, "g_ModelViewProjectionMatrix") {
            b.shadow_mut().write_mat4(&s, &m);
        }
        if let Some(s) = b.uniform(p, "g_Color") {
            b.shadow_mut().write_floats(&s, &colour);
        }
        b.push(Op::Draw(p));
    }
    let mut scene = b.build().unwrap();
    scene.render(ctx);
    // Rendered twice: the depth buffer is cleared every frame.
    scene.mark_dirty();
    scene.render(ctx);
    let (_, px) = read(ctx, &scene, st.full);
    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    };
    // Near quad: columns 19..45, rows 5..31 (fov 50, distance 3, 64 × 36).
    let tl = at(21, 7);
    let br = at(43, 29);
    assert!(
        tl[0] < 60 && tl[1] < 60 && tl[2] == 0,
        "top-left uv ≈ (0, 0): {tl:?}"
    );
    assert!(
        br[0] > 190 && br[1] > 190 && br[2] == 0,
        "bottom-right uv ≈ (1, 1): {br:?}"
    );
    // Outside the near quad: the far quad (blue).
    assert_eq!(at(10, 2), [0, 0, 255]);
    // Beyond its edge (columns 6..58): the clear colour.
    assert_eq!(at(2, 2), [0, 0, 0]);
}

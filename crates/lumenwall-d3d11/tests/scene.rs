//! The scene executor on a real Direct3D 11 device: `lumenwall-render`'s GPU scenarios
//! (`tests/gpu.rs`, `gpu3d.rs`, `quality_gpu.rs`) rebuilt as plans, with the same assertions, plus
//! the executor's own rules (one constant `Map` per frame at most, no object created by a frame,
//! aliasing, streams, profiling, errors as values) and the shared GPU memory of a synthetic
//! 100-pass scene.
//!
//! WARP (the software rasteriser) by default, so the workspace check never touches the GPU;
//! `LUMENWALL_D3D11_TEST_ADAPTER=hardware` runs the same tests on the default hardware adapter
//! (under `%LOCALAPPDATA%\lumenwall-dev\gpu.lock`).
//!
//! The plans are built by [`Front`], a small stand-in for `lumenwall-render`'s neutral front (the
//! bridge of task 5 produces them from `SceneDesc`): uniform windows of the shadow with the front's
//! defaults (identity matrices, ones, texture resolutions), the image chain of spec 04 (copy into
//! composite A, effects through A/B, the last pass into the scene with the image's blend and RGB
//! writes), hazard scratch targets, and the clip-space correction `(x, −y, (z + w)/2, w)` written
//! in the WGSL of 2D passes (the front applies it to the naga module).
//!
//! Not here: the composition of `orientation_is_preserved_end_to_end` (task 4's compositor) and the
//! texture-resolution test (mips dropped by the front before the upload).

#![cfg(windows)]

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use lumenwall_d3d11::layout::{
    CONSTANT_WINDOW_ALIGN, ConstantRegion, Dimension, MipLevel, TargetFormat, VertexFormat,
};
use lumenwall_d3d11::plan::{
    AttributePlan, FrameInput, GeometryPlan, MeshLayoutPlan, OpPlan, PassPlan, ScenePlan, SlotPlan,
    StreamFrame, StreamPlan, TargetPlan, TextureSource,
};
use lumenwall_d3d11::scene::{FrameStats, Scene, SceneBuilder, ScenePlanExtension};
use lumenwall_d3d11::state::{
    AddressMode, BlendKey, Cull, DepthKey, RasterKey, SamplerKey, write_mask,
};
use lumenwall_d3d11::windows::Win32::Graphics::Direct3D11::ID3D11Resource;
use lumenwall_d3d11::windows::core::Interface;
use lumenwall_d3d11::{
    AdapterChoice, Device, DeviceOptions, Error, GpuTimer, Resources, hlsl, read_subresource,
    read_texture_rgba8,
};
use lumenwall_formats::tex::GpuFormat;

fn hardware() -> bool {
    std::env::var("LUMENWALL_D3D11_TEST_ADAPTER").is_ok_and(|v| v == "hardware")
}

fn device() -> Device {
    Device::new(&DeviceOptions {
        adapter: if hardware() {
            AdapterChoice::Default
        } else {
            AdapterChoice::Warp
        },
        ..DeviceOptions::default()
    })
    .expect("Direct3D 11 device")
}

// ---------------------------------------------------------------------------------------------
// Matrices (column-major, glam's conventions) and image geometry (lumenwall-render's `math`).

type Mat4 = [f32; 16];

const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut m = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            m[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    m
}

fn ortho(l: f32, r: f32, b: f32, t: f32, n: f32, f: f32) -> Mat4 {
    [
        2.0 / (r - l),
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / (t - b),
        0.0,
        0.0,
        0.0,
        0.0,
        -2.0 / (f - n),
        0.0,
        -(r + l) / (r - l),
        -(t + b) / (t - b),
        -(f + n) / (f - n),
        1.0,
    ]
}

/// `OrthoCamera::canonical(w, h).view_projection()`: `ortho(−W/2, W/2, −H/2, H/2, 0, 1000) ·
/// T(eye) · lookAt(eye, 0, Y)`, the last two being the identity for the canonical camera.
fn canonical_vp(w: f32, h: f32) -> Mat4 {
    ortho(-w * 0.5, w * 0.5, -h * 0.5, h * 0.5, 0.0, 1000.0)
}

/// `perspective_we` (`Mat4::perspective_rh`: depth in [0, 1]).
fn perspective(fov_degrees: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let half = fov_degrees.to_radians() * 0.5;
    let h = half.cos() / half.sin();
    let w = h / aspect;
    let r = far / (near - far);
    [
        w,
        0.0,
        0.0,
        0.0,
        0.0,
        h,
        0.0,
        0.0,
        0.0,
        0.0,
        r,
        -1.0,
        0.0,
        0.0,
        r * near,
        0.0,
    ]
}

fn scale(x: f32, y: f32, z: f32) -> Mat4 {
    let mut m = IDENTITY;
    m[0] = x;
    m[5] = y;
    m[10] = z;
    m
}

/// `(x, y, z, u, v)`.
type Vertex = [f32; 5];

/// `math::quad`: bottom-left, top-left, bottom-right; bottom-right, top-left, top-right.
fn quad(x0: f32, x1: f32, y_top: f32, y_bottom: f32, u: f32, v: f32) -> [Vertex; 6] {
    let bl = [x0, y_bottom, 0.0, 0.0, v];
    let tl = [x0, y_top, 0.0, 0.0, 0.0];
    let br = [x1, y_bottom, 0.0, u, v];
    let tr = [x1, y_top, 0.0, u, 0.0];
    [bl, tl, br, br, tl, tr]
}

fn unit_quad() -> [Vertex; 6] {
    quad(-1.0, 1.0, -1.0, 1.0, 1.0, 1.0)
}

/// `math::image_rect` (centred alignment) → `(x0, x1, y_top, y_bottom)` in RC.
fn image_rect(
    origin: (f32, f32),
    scale: (f32, f32),
    size: (f32, f32),
    scene: (f32, f32),
) -> (f32, f32, f32, f32) {
    let (sw, sh) = (size.0 * scale.0, size.1 * scale.1);
    let (x0, x1) = (origin.0 - sw * 0.5, origin.0 + sw * 0.5);
    let (y0, y1) = (origin.1 - sh * 0.5, origin.1 + sh * 0.5);
    (
        x0 - scene.0 * 0.5,
        x1 - scene.0 * 0.5,
        scene.1 * 0.5 - y1,
        scene.1 * 0.5 - y0,
    )
}

fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

// ---------------------------------------------------------------------------------------------
// Shaders. 2D passes keep the upstream clip convention: `clip_fix` is what the front appends.

const CLIP_FIX: &str = r"
fn clip_fix(p: vec4<f32>) -> vec4<f32> { return vec4<f32>(p.x, -p.y, (p.z + p.w) * 0.5, p.w); }
";

/// `tests/gpu.rs`'s `SRC`.
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
    o.pos = clip_fix(u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0));
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

// ---------------------------------------------------------------------------------------------
// A stand-in for the neutral front: the plan, the shadow and the CPU state of the frame.

struct Prog {
    resources: Resources,
    /// Uniform block fields: `name → (offset, size)`.
    fields: HashMap<String, (u32, u32)>,
}

struct Tex {
    storage: (u32, u32),
    content: (u32, u32),
    sampler: SamplerKey,
}

struct StreamData {
    vertices: Vec<u8>,
    index_count: u32,
    written: bool,
}

#[derive(Default)]
struct Front {
    plan: ScenePlan,
    shadow: Vec<u8>,
    vertices: Vec<u8>,
    dirty: Option<Range<u32>>,
    variants: Vec<u16>,
    enabled: Vec<bool>,
    programs: Vec<Prog>,
    textures: Vec<Tex>,
    streams: Vec<StreamData>,
}

/// Options of a pass (`PassDesc` beyond label, shader, target and geometry).
#[derive(Clone)]
struct PassOpts {
    blend: BlendKey,
    raster: RasterKey,
    depth: DepthKey,
    slots: [Option<TextureSource>; 8],
    variants: Vec<TextureSource>,
}

impl Default for PassOpts {
    fn default() -> Self {
        Self {
            blend: BlendKey::OPAQUE,
            raster: RasterKey::default(),
            depth: DepthKey::default(),
            slots: [None; 8],
            variants: Vec::new(),
        }
    }
}

fn uniform_fields(module: &naga::Module) -> HashMap<String, (u32, u32)> {
    for (_, g) in module.global_variables.iter() {
        let Some(b) = g.binding else { continue };
        if (b.group, b.binding) != (0, 0) {
            continue;
        }
        if let naga::TypeInner::Struct { members, .. } = &module.types[g.ty].inner {
            return members
                .iter()
                .filter_map(|m| {
                    let size = module.types[m.ty].inner.size(module.to_ctx());
                    Some((m.name.clone()?, (m.offset, size)))
                })
                .collect();
        }
    }
    HashMap::new()
}

impl Front {
    /// A scene target and its clear, as `setup` in `tests/gpu.rs`.
    fn scene(w: u32, h: u32, clear: [f32; 4]) -> (Self, usize) {
        let mut f = Self::default();
        let full = f.target("_rt_FullFrameBuffer", (w, h));
        f.plan.ops.push(OpPlan::Clear {
            target: full,
            color: clear,
        });
        (f, full)
    }

    fn target(&mut self, label: &str, size: (u32, u32)) -> usize {
        self.plan.targets.push(TargetPlan {
            label: label.into(),
            size,
            format: TargetFormat::Rgba8Unorm,
            samples: 1,
            depth: false,
            shares: None,
        });
        self.plan.targets.len() - 1
    }

    fn size_of(&self, target: usize) -> (f32, f32) {
        let (w, h) = self.plan.targets[target].size;
        (w as f32, h as f32)
    }

    fn window(&mut self, bytes: u32) -> ConstantRegion {
        let size = bytes.max(1).next_multiple_of(CONSTANT_WINDOW_ALIGN);
        let w = ConstantRegion {
            offset: self.plan.constants_len,
            size,
        };
        self.plan.constants_len += size;
        self.shadow.resize(self.plan.constants_len as usize, 0);
        w
    }

    fn add_vertices(&mut self, v: &[Vertex]) -> GeometryPlan {
        let first = self.vertices.len() as u32 / 20;
        let bytes = f32_bytes(&v.concat());
        let start = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&bytes);
        self.plan.vertex_bytes = self.vertices.len() as u32;
        self.mark(start..self.vertices.len() as u32);
        GeometryPlan::Vertices {
            first,
            count: v.len() as u32,
        }
    }

    fn mark(&mut self, r: Range<u32>) {
        self.dirty = Some(match self.dirty.take() {
            Some(d) => d.start.min(r.start)..d.end.max(r.end),
            None => r,
        });
    }

    fn resolution(&self, s: TextureSource) -> [f32; 4] {
        match s {
            TextureSource::Texture(t) => {
                let t = &self.textures[t];
                [
                    t.storage.0 as f32,
                    t.storage.1 as f32,
                    t.content.0 as f32,
                    t.content.1 as f32,
                ]
            }
            TextureSource::Target(t) => {
                let (w, h) = self.size_of(t);
                [w, h, w, h]
            }
            TextureSource::Default2D | TextureSource::Default3D => [1.0; 4],
        }
    }

    /// The sampler the front resolves: the texture's own, `CLAMP_LINEAR` for targets.
    fn slot(&self, s: TextureSource) -> SlotPlan {
        SlotPlan {
            source: s,
            sampler: match s {
                TextureSource::Texture(t) => self.textures[t].sampler,
                _ => SamplerKey::CLAMP_LINEAR,
            },
        }
    }

    /// The scratch target of a size and format (`RenderTargetPool::scratch`).
    fn scratch(&mut self, like: usize) -> usize {
        let (size, format) = (self.plan.targets[like].size, self.plan.targets[like].format);
        if let Some(i) =
            self.plan.targets.iter().position(|t| {
                t.label == "lumenwall.scratch" && t.size == size && t.format == format
            })
        {
            return i;
        }
        let i = self.target("lumenwall.scratch", size);
        self.plan.targets[i].format = format;
        i
    }

    /// `SceneBuilder::add_pass`: uniform window with the engine defaults, hazard scratch when the
    /// program samples its own target.
    fn add_pass(
        &mut self,
        label: &str,
        program: usize,
        target: usize,
        geometry: GeometryPlan,
        opts: PassOpts,
    ) -> usize {
        let res = self.programs[program].resources.clone();
        let uniform = res.uniform_block.map(|s| self.window(s));
        let mut slots = [None; 8];
        for (n, s) in opts.slots.iter().enumerate() {
            slots[n] = s.map(|s| self.slot(s));
        }
        let sampled = |n: usize| res.textures & (1 << n) != 0;
        let own = (0..8).any(|n| {
            sampled(n)
                && (opts.slots[n] == Some(TextureSource::Target(target))
                    || (n == 0 && opts.variants.contains(&TextureSource::Target(target))))
        });
        let hazard_scratch = own.then(|| self.scratch(target));
        self.plan.passes.push(PassPlan {
            label: label.into(),
            program,
            target,
            blend: opts.blend,
            raster: opts.raster,
            depth: opts.depth,
            uniform,
            slots,
            slot0_variants: opts.variants.clone(),
            geometry,
            hazard_scratch,
            clear_first: false,
        });
        self.variants.push(0);
        self.enabled.push(true);
        let p = self.plan.passes.len() - 1;
        let fields: Vec<(String, u32)> = self.programs[program]
            .fields
            .iter()
            .map(|(n, &(_, s))| (n.clone(), s))
            .collect();
        for (name, size) in fields {
            if name == "g_ModelViewProjectionMatrix" && size == 64 {
                self.write(p, &name, &f32_bytes(&IDENTITY));
            } else if name == "g_Color" {
                self.write(p, &name, &f32_bytes(&[1.0; 4]));
            } else if let Some(n) = name
                .strip_prefix("g_Texture")
                .and_then(|r| r.strip_suffix("Resolution"))
                .and_then(|n| n.parse::<usize>().ok())
            {
                let source = match (n, opts.variants.first()) {
                    (0, Some(&v)) => Some(v),
                    _ => opts.slots.get(n).copied().flatten(),
                };
                if let Some(s) = source {
                    let r = self.resolution(s);
                    self.write(p, &name, &f32_bytes(&r));
                }
            }
        }
        p
    }

    /// Writes a uniform of a pass (truncated to the field), ignored when the program has none.
    fn write(&mut self, pass: usize, name: &str, bytes: &[u8]) {
        let Some(w) = self.plan.passes[pass].uniform else {
            return;
        };
        let program = self.plan.passes[pass].program;
        let Some(&(offset, size)) = self.programs[program].fields.get(name) else {
            return;
        };
        let n = bytes.len().min(size as usize);
        let at = (w.offset + offset) as usize;
        self.shadow[at..at + n].copy_from_slice(&bytes[..n]);
    }

    fn draw(&mut self, pass: usize) {
        self.plan.ops.push(OpPlan::Draw(pass));
    }
}

fn program(b: &mut SceneBuilder<'_>, f: &mut Front, src: &str, vs: &str, fs: &str) -> usize {
    let module = naga::front::wgsl::parse_str(&format!("{CLIP_FIX}{src}")).expect("test WGSL");
    let hlsl = hlsl::translate(&module, vs, fs).expect("HLSL");
    let i = b.add_program(&hlsl, None).expect("program");
    assert_eq!(i, f.programs.len());
    f.programs.push(Prog {
        resources: hlsl.resources.clone(),
        fields: uniform_fields(&module),
    });
    i
}

/// An RGBA8 texture with `TexFlags::CLAMP_UVS` (clamp, linear, the wgpu path's anisotropy).
fn texture(
    b: &mut SceneBuilder<'_>,
    f: &mut Front,
    (w, h): (u32, u32),
    content: Option<(u32, u32)>,
    pixel: impl Fn(u32, u32) -> [u8; 4],
) -> usize {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&pixel(x, y));
        }
    }
    let i = b
        .add_texture(
            GpuFormat::Rgba8Unorm,
            Dimension::D2,
            &[MipLevel {
                width: w,
                height: h,
                depth: 1,
                data: &data,
            }],
        )
        .expect("texture");
    assert_eq!(i, f.textures.len());
    f.textures.push(Tex {
        storage: (w, h),
        content: content.unwrap_or((w, h)),
        sampler: SamplerKey::new(AddressMode::Clamp, false, false, false),
    });
    i
}

fn build(b: SceneBuilder<'_>, f: &Front) -> Scene {
    b.build(&f.plan).expect("scene build")
}

fn render(scene: &mut Scene, d: &Device, f: &mut Front) -> FrameStats {
    {
        let streams: Vec<StreamFrame<'_>> = f
            .streams
            .iter()
            .map(|s| StreamFrame {
                vertices: s.written.then_some(&s.vertices[..]),
                index_count: s.index_count,
            })
            .collect();
        let input = FrameInput {
            shadow: &f.shadow,
            vertices: &f.vertices,
            vertices_dirty: f.dirty.clone(),
            variants: &f.variants,
            enabled: &f.enabled,
            streams: &streams,
        };
        scene.render(d, &input).expect("frame");
    }
    f.dirty = None;
    for s in &mut f.streams {
        s.written = false;
    }
    scene.last_frame()
}

fn read(d: &Device, scene: &Scene, target: usize) -> (u32, Vec<u8>) {
    let (w, _, px) = read_texture_rgba8(d, scene.target_texture(target).unwrap()).unwrap();
    (w, px)
}

fn px(data: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [data[i], data[i + 1], data[i + 2], data[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| x.abs_diff(*y) <= tol)
}

// ---------------------------------------------------------------------------------------------
// Images (`image::add_image` and `chain::plan_image_chain`, no explicit targets).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blend {
    Normal,
    Translucent,
}

fn blend_key(b: Blend, mask: u8) -> BlendKey {
    match b {
        Blend::Normal => BlendKey::OPAQUE,
        Blend::Translucent => BlendKey::translucent(),
    }
    .with_write_mask(mask)
}

#[derive(Clone)]
struct ImagePass {
    label: String,
    program: usize,
    blend: Blend,
    cull: Cull,
    slots: [Option<TextureSource>; 8],
    variants: Vec<TextureSource>,
}

impl ImagePass {
    fn new(label: &str, program: usize) -> Self {
        Self {
            label: label.into(),
            program,
            blend: Blend::Normal,
            cull: Cull::None,
            slots: [None; 8],
            variants: Vec::new(),
        }
    }
}

struct ImageSpec {
    id: u32,
    size: (f32, f32),
    main: usize,
    origin: (f32, f32),
    scale: (f32, f32),
    scene: usize,
}

struct Image {
    passes: Vec<usize>,
    a: usize,
    b: usize,
}

/// The passes of an image chain, placed at `spec.origin` (what `ImageHandle::update` writes).
fn add_image(f: &mut Front, spec: &ImageSpec, passes: &[ImagePass]) -> Image {
    let comp = (spec.size.0 as u32, spec.size.1 as u32);
    let a = f.target(&format!("_rt_imageLayerComposite_{}_a", spec.id), comp);
    let b = f.target(&format!("_rt_imageLayerComposite_{}_b", spec.id), comp);
    let tex = &f.textures[spec.main];
    let uv_max = (
        tex.content.0 as f32 / tex.storage.0 as f32,
        tex.content.1 as f32 / tex.storage.1 as f32,
    );
    let scene_size = f.size_of(spec.scene);
    let vp = canonical_vp(scene_size.0, scene_size.1);
    let (x0, x1, yt, yb) = image_rect(spec.origin, spec.scale, spec.size, scene_size);
    let n = passes.len();
    let (mut write, mut input) = (a, TextureSource::Texture(spec.main));
    let mut ids = Vec::with_capacity(n);
    for (k, p) in passes.iter().enumerate() {
        let (first, last) = (k == 0, k + 1 == n);
        let target = if last { spec.scene } else { write };
        let (verts, matrix) = match (first, last) {
            (true, true) => (quad(x0, x1, yt, yb, uv_max.0, uv_max.1), vp),
            (false, true) => (quad(x0, x1, yt, yb, 1.0, 1.0), vp),
            (true, false) => (
                quad(0.0, spec.size.0, 0.0, spec.size.1, uv_max.0, uv_max.1),
                ortho(0.0, spec.size.0, 0.0, spec.size.1, -1.0, 1.0),
            ),
            (false, false) => (unit_quad(), IDENTITY),
        };
        // Blend transfer (spec 04 §9.8): the last pass carries the first's, the first is normal.
        let blend = match (n > 1, first, last) {
            (true, _, true) => passes[0].blend,
            (true, true, _) => Blend::Normal,
            _ => p.blend,
        };
        let geometry = f.add_vertices(&verts);
        let mut slots = p.slots;
        let mut variants = Vec::new();
        if slots[0].is_none() {
            slots[0] = Some(input);
            if first {
                variants.clone_from(&p.variants);
            }
        }
        let id = f.add_pass(
            &p.label,
            p.program,
            target,
            geometry,
            PassOpts {
                blend: blend_key(
                    blend,
                    if last {
                        write_mask::RGB
                    } else {
                        write_mask::ALL
                    },
                ),
                raster: RasterKey {
                    cull: p.cull,
                    ..RasterKey::default()
                },
                slots,
                variants,
                ..PassOpts::default()
            },
        );
        f.write(id, "g_ModelViewProjectionMatrix", &f32_bytes(&matrix));
        f.draw(id);
        ids.push(id);
        input = TextureSource::Target(write);
        write = if write == a { b } else { a };
    }
    Image { passes: ids, a, b }
}

fn image(scene: usize, id: u32, size: f32, main: usize, at: f32) -> ImageSpec {
    ImageSpec {
        id,
        size: (size, size),
        main,
        origin: (at, at),
        scale: (1.0, 1.0),
        scene,
    }
}

// ---------------------------------------------------------------------------------------------
// tests/gpu.rs

/// Texel row 0 must land on scene row 0 (spec 04 §0.2, §10.2, §21.4). The composed output of the
/// wgpu test is task 4's (compositor).
#[test]
fn orientation_is_preserved_end_to_end() {
    let d = device();
    let (mut f, full) = Front::scene(64, 64, [0.0, 1.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let tex = texture(&mut b, &mut f, (8, 8), None, |_, y| {
        if y < 4 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        }
    });
    let sh = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let spec = image(full, 1, 64.0, tex, 32.0);
    add_image(&mut f, &spec, &[ImagePass::new("base", sh)]);
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, px_) = read(&d, &scene, full);
    assert!(
        near(px(&px_, w, 5, 2), [255, 0, 0, 255], 2),
        "top is red: {:?}",
        px(&px_, w, 5, 2)
    );
    assert!(
        near(px(&px_, w, 5, 60), [0, 0, 255, 255], 2),
        "bottom is blue"
    );
}

/// Power-of-two padding: only the content (top-left) is shown (spec 04 §8.7).
#[test]
fn padded_texture_uses_content_window() {
    let d = device();
    let (mut f, full) = Front::scene(32, 32, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let tex = texture(&mut b, &mut f, (8, 8), Some((4, 8)), |x, _| {
        if x < 4 {
            [255, 0, 0, 255]
        } else {
            [255, 255, 255, 255]
        }
    });
    let sh = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let spec = image(full, 1, 32.0, tex, 16.0);
    add_image(&mut f, &spec, &[ImagePass::new("base", sh)]);
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    // Bilinear filtering bleeds into the padding only at the last half texel.
    for x in [1, 16, 24] {
        assert!(
            near(px(&data, w, x, 16), [255, 0, 0, 255], 3),
            "x={x}: {:?}",
            px(&data, w, x, 16)
        );
    }
}

/// `cullmode normal` removes a mirrored image, `nocull` keeps it (spec 04 §11.5, §24.3).
#[test]
fn culling_and_front_face() {
    let d = device();
    for (scale_x, cull, drawn) in [
        (1.0, Cull::Back, true),
        (-1.0, Cull::Back, false),
        (-1.0, Cull::None, true),
    ] {
        let (mut f, full) = Front::scene(32, 32, [0.0, 1.0, 0.0, 1.0]);
        let mut b = SceneBuilder::new(&d, None);
        let tex = texture(&mut b, &mut f, (4, 4), None, |_, _| [255, 0, 0, 255]);
        let sh = program(&mut b, &mut f, SRC, "vs", "fs_copy");
        let mut spec = image(full, 1, 16.0, tex, 16.0);
        spec.scale = (scale_x, 1.0);
        let mut p = ImagePass::new("base", sh);
        p.cull = cull;
        add_image(&mut f, &spec, &[p]);
        let mut scene = build(b, &f);
        render(&mut scene, &d, &mut f);
        let (w, data) = read(&d, &scene, full);
        let c = px(&data, w, 16, 16);
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
    let d = device();
    let (mut f, full) = Front::scene(16, 16, [0.0, 0.0, 1.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let tex = texture(&mut b, &mut f, (4, 4), None, |_, _| [255, 0, 0, 128]);
    let sh = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let spec = image(full, 1, 16.0, tex, 8.0);
    let mut p = ImagePass::new("base", sh);
    p.blend = Blend::Translucent;
    add_image(&mut f, &spec, &[p]);
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert!(
        near(px(&data, w, 8, 8), [128, 0, 127, 255], 2),
        "{:?}",
        px(&data, w, 8, 8)
    );
}

/// Base pass + two effects through A/B: invert twice = identity, and the final pass carries the
/// model's blend (spec 04 §9.8, §10).
#[test]
fn ping_pong_chain() {
    let d = device();
    let (mut f, full) = Front::scene(32, 32, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let tex = texture(&mut b, &mut f, (4, 4), None, |x, _| {
        [40 * x as u8, 200, 10, 255]
    });
    let copy = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let inv = program(&mut b, &mut f, SRC, "vs", "fs_invert");
    let spec = image(full, 7, 32.0, tex, 16.0);
    let img = add_image(
        &mut f,
        &spec,
        &[
            ImagePass::new("base", copy),
            ImagePass::new("inv1", inv),
            ImagePass::new("inv2", inv),
        ],
    );
    let mut scene = build(b, &f);
    assert_eq!(scene.stats().passes, 3);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    // Column 0 of the texture (x = 0 → red 0).
    assert!(
        near(px(&data, w, 1, 16), [0, 200, 10, 255], 2),
        "{:?}",
        px(&data, w, 1, 16)
    );
    let (w, b_) = read(&d, &scene, img.b);
    assert!(
        near(px(&b_, w, 1, 16), [255, 55, 245, 255], 2),
        "B holds the first inversion: {:?}",
        px(&b_, w, 1, 16)
    );
}

/// A pass that samples its own destination reads a copy (spec 04 §24.6).
#[test]
fn read_write_hazard_uses_a_copy() {
    let d = device();
    let (mut f, full) = Front::scene(16, 16, [0.25, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let tex = texture(&mut b, &mut f, (4, 4), None, |_, _| [0, 64, 0, 255]);
    let add = program(&mut b, &mut f, SRC, "vs", "fs_add");
    let spec = image(full, 1, 16.0, tex, 8.0);
    let mut p = ImagePass::new("add", add);
    p.slots[1] = Some(TextureSource::Target(full));
    add_image(&mut f, &spec, &[p]);
    let mut scene = build(b, &f);
    let stats = render(&mut scene, &d, &mut f);
    assert_eq!(stats.copies, 1, "one hazard copy");
    let (w, data) = read(&d, &scene, full);
    assert!(
        near(px(&data, w, 8, 8), [64, 64, 0, 255], 2),
        "{:?}",
        px(&data, w, 8, 8)
    );
}

/// Attributes the engine does not feed read (0, 0, 0, 1) (spec 04 §11.5).
#[test]
fn unfed_attributes_default() {
    let src = r"
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(2) a_Color: vec4<f32>) -> VOut {
    var o: VOut;
    o.pos = clip_fix(vec4<f32>(a_Position, 1.0));
    o.c = a_Color + vec4<f32>(0.25, 0.0, 0.0, 0.0);
    return o;
}
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32> }
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> { return i.c; }
";
    let d = device();
    let (mut f, full) = Front::scene(8, 8, [1.0, 1.0, 1.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let sh = program(&mut b, &mut f, src, "vs", "fs");
    let v = f.add_vertices(&unit_quad());
    let id = f.add_pass("unfed", sh, full, v, PassOpts::default());
    f.draw(id);
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert!(
        near(px(&data, w, 4, 4), [64, 0, 0, 255], 1),
        "{:?}",
        px(&data, w, 4, 4)
    );
}

/// `SkinVertex` bytes: joints, weights, normal (+ pad), tangent.
fn skin_record(joints: [u32; 4], weights: [f32; 4], normal: [f32; 3]) -> Vec<u8> {
    let mut b: Vec<u8> = joints.iter().flat_map(|j| j.to_le_bytes()).collect();
    b.extend(f32_bytes(&weights));
    b.extend(f32_bytes(&normal));
    b.extend(f32_bytes(&[0.0; 5]));
    b
}

/// `mat4x3` bone palette: four 12-byte columns at a 16-byte stride per matrix.
fn bones(palette: &[[f32; 12]]) -> Vec<u8> {
    let mut b = Vec::new();
    for m in palette {
        for c in 0..4 {
            b.extend(f32_bytes(&m[c * 3..c * 3 + 3]));
            b.extend(f32_bytes(&[0.0]));
        }
    }
    b
}

/// Skinned meshes (spec 08 §9.8): skin attributes come from their own buffer at the mesh's first
/// record, the `mat4x3` palette keeps its column stride, and the mesh moves with it.
#[test]
fn skinned_geometry_follows_the_bone_palette() {
    let src = r"
struct Pass { g_Bones: array<mat4x3<f32>, 2> }
@group(0) @binding(0) var<uniform> u: Pass;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32> }
@vertex fn vs(
    @location(0) a_Position: vec3<f32>,
    @location(1) a_BlendIndices: vec4<u32>,
    @location(2) a_BlendWeights: vec4<f32>,
    @location(3) a_Normal: vec3<f32>,
) -> VOut {
    let m = u.g_Bones[a_BlendIndices.x] * a_BlendWeights.x
        + u.g_Bones[a_BlendIndices.y] * a_BlendWeights.y;
    var o: VOut;
    o.pos = clip_fix(vec4<f32>(m * vec4<f32>(a_Position, 1.0), 1.0));
    o.c = vec4<f32>(a_Normal, 1.0);
    return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> { return i.c; }
";
    let d = device();
    let (mut f, full) = Front::scene(8, 8, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let sh = program(&mut b, &mut f, src, "vs", "fs");
    // Unrelated vertices and skin records first: both buffers must start at the mesh.
    f.add_vertices(&unit_quad());
    f.plan.skins = skin_record([0; 4], [0.0; 4], [0.0; 3]).repeat(3);
    let strip = |x: f32, y: f32| [x, y, 0.0, 0.0, 0.0];
    let GeometryPlan::Vertices { first, .. } = f.add_vertices(&[
        strip(-1.0, -1.0),
        strip(-1.0, 1.0),
        strip(-0.5, -1.0),
        strip(-0.5, 1.0),
    ]) else {
        unreachable!()
    };
    f.plan.indices = vec![0, 1, 2, 2, 1, 3];
    let rec = skin_record([1, 0, 0, 0], [1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
    f.plan.skins.extend(rec.repeat(4));
    let id = f.add_pass(
        "skin",
        sh,
        full,
        GeometryPlan::Skinned {
            first_vertex: first,
            first_index: 0,
            index_count: 6,
            first_skin: 3,
        },
        PassOpts::default(),
    );
    f.draw(id);
    let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let mut palette = [identity, identity];
    f.write(id, "g_Bones", &bones(&palette));
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert!(
        near(px(&data, w, 0, 4), [0, 0, 255, 255], 1),
        "{:?}",
        px(&data, w, 0, 4)
    );
    assert!(near(px(&data, w, 7, 4), [0, 0, 0, 255], 1));
    // Bone 1 moves by 1.5 in x: the strip lands on the right edge.
    palette[1][9] = 1.5;
    f.write(id, "g_Bones", &bones(&palette));
    let stats = render(&mut scene, &d, &mut f);
    assert_eq!((stats.constant_maps, stats.vertex_maps), (1, 0));
    let (w, data) = read(&d, &scene, full);
    assert!(
        near(px(&data, w, 0, 4), [0, 0, 0, 255], 1),
        "{:?}",
        px(&data, w, 0, 4)
    );
    assert!(
        near(px(&data, w, 7, 4), [0, 0, 255, 255], 1),
        "{:?}",
        px(&data, w, 7, 4)
    );
}

/// Animated textures switch SRV tables; `g_Texture0Resolution` follows the variant (the front
/// writes it, the pass reads it).
#[test]
fn slot0_variants() {
    let d = device();
    let (mut f, full) = Front::scene(8, 8, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let a = texture(&mut b, &mut f, (4, 4), None, |_, _| [255, 0, 0, 255]);
    let bt = texture(&mut b, &mut f, (8, 8), None, |_, _| [0, 0, 255, 255]);
    let copy = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let res = program(&mut b, &mut f, SRC, "vs", "fs_resolution");
    let v = f.add_vertices(&unit_quad());
    let variants = vec![TextureSource::Texture(a), TextureSource::Texture(bt)];
    let p1 = f.add_pass(
        "anim",
        copy,
        full,
        v,
        PassOpts {
            variants: variants.clone(),
            ..PassOpts::default()
        },
    );
    f.draw(p1);
    // Second pass: disabled at first, drawn last to observe its uniform block.
    let p2 = f.add_pass(
        "res",
        res,
        full,
        v,
        PassOpts {
            variants,
            ..PassOpts::default()
        },
    );
    f.draw(p2);
    f.enabled[p2] = false;
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert!(near(px(&data, w, 4, 4), [255, 0, 0, 255], 1));
    f.variants[p1] = 1;
    let stats = render(&mut scene, &d, &mut f);
    assert_eq!(stats.constant_maps, 0, "a variant changes no constant");
    let (w, data) = read(&d, &scene, full);
    assert!(near(px(&data, w, 4, 4), [0, 0, 255, 255], 1));
    // The front's `set_variant`: the resolution of slot 0 follows the variant.
    f.variants[p2] = 1;
    f.write(p2, "g_Texture0Resolution", &f32_bytes(&[8.0; 4]));
    f.enabled[p2] = true;
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 4, 4), [8, 0, 0, 255]);
}

/// `lumenwall-render`'s `shaders/passes.wgsl` (bloom stand-ins), with the clip fix.
const BLOOM_WGSL: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_Texture0Resolution: vec4<f32>,
    g_BloomStrength: f32,
    g_BloomThreshold: f32,
}
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
@group(1) @binding(2) var g_Texture1: texture_2d<f32>;
@group(1) @binding(3) var g_Texture1_s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex
fn vs_generic(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = clip_fix(u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0));
    o.uv = a_TexCoord;
    return o;
}
@fragment fn fs_copy(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv);
}
fn texel() -> vec2<f32> { return 1.0 / max(u.g_Texture0Resolution.xy, vec2<f32>(1.0)); }
fn tap(uv: vec2<f32>) -> vec4<f32> { return textureSample(g_Texture0, g_Texture0_s, uv); }
@fragment fn fs_bloom_quarter(i: VOut) -> @location(0) vec4<f32> {
    let t = texel();
    let c = 0.25 * (tap(i.uv + t * vec2<f32>(-1.0, -1.0)) + tap(i.uv + t * vec2<f32>(1.0, -1.0))
        + tap(i.uv + t * vec2<f32>(-1.0, 1.0)) + tap(i.uv + t * vec2<f32>(1.0, 1.0)));
    let bright = max(c.rgb - vec3<f32>(u.g_BloomThreshold), vec3<f32>(0.0));
    return vec4<f32>(bright * u.g_BloomStrength, 1.0);
}
fn gauss(k: i32) -> f32 {
    var w = array<f32, 5>(0.2270, 0.1945, 0.1216, 0.0540, 0.0162);
    return w[abs(k)];
}
@fragment fn fs_bloom_eighth_v(i: VOut) -> @location(0) vec4<f32> {
    let t = texel();
    var c = vec3<f32>(0.0);
    for (var k = -4; k <= 4; k++) {
        c += gauss(k) * tap(i.uv + vec2<f32>(0.0, 2.0 * f32(k) * t.y)).rgb;
    }
    return vec4<f32>(c, 1.0);
}
@fragment fn fs_bloom_h(i: VOut) -> @location(0) vec4<f32> {
    let t = texel();
    var c = vec3<f32>(0.0);
    for (var k = -4; k <= 4; k++) {
        c += gauss(k) * tap(i.uv + vec2<f32>(f32(k) * t.x, 0.0)).rgb;
    }
    return vec4<f32>(c, 1.0);
}
@fragment fn fs_bloom_combine(i: VOut) -> @location(0) vec4<f32> {
    let a = textureSample(g_Texture0, g_Texture0_s, i.uv);
    let b = textureSample(g_Texture1, g_Texture1_s, i.uv);
    return vec4<f32>(a.rgb + b.rgb, a.a);
}
";

/// The five bloom passes run and spread light around a bright image (spec 04 §17).
#[test]
fn bloom_chain() {
    let d = device();
    let (mut f, full) = Front::scene(64, 64, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let tex = texture(&mut b, &mut f, (4, 4), None, |_, _| [255, 255, 255, 255]);
    let sh = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let spec = image(full, 1, 16.0, tex, 32.0);
    add_image(&mut f, &spec, &[ImagePass::new("base", sh)]);
    // `create_bloom_targets` and `create_bloom_composites` (B is never written: 1×1).
    let quarter = f.target("_rt_4FrameBuffer", (16, 16));
    let eighth = f.target("_rt_8FrameBuffer", (8, 8));
    let bloom = f.target("_rt_Bloom", (8, 8));
    let comp_a = f.target("_rt_imageLayerComposite_-1_a", (64, 64));
    f.target("_rt_imageLayerComposite_-1_b", (1, 1));
    let shaders: Vec<usize> = [
        "fs_copy",
        "fs_bloom_quarter",
        "fs_bloom_eighth_v",
        "fs_bloom_h",
        "fs_bloom_combine",
    ]
    .iter()
    .map(|fs| program(&mut b, &mut f, BLOOM_WGSL, "vs_generic", fs))
    .collect();
    // `add_bloom`: base copy into A (copy geometry and matrix), then unit quads.
    let copy_geom = f.add_vertices(&quad(0.0, 64.0, 0.0, 64.0, 1.0, 1.0));
    let unit_geom = f.add_vertices(&unit_quad());
    let targets = [comp_a, quarter, eighth, bloom, full];
    let slot0 = [full, full, quarter, eighth, comp_a];
    let labels = [
        "bloom.base",
        "bloom.quarter",
        "bloom.eighth_v",
        "bloom.blur_h",
        "bloom.combine",
    ];
    for (k, label) in labels.iter().enumerate() {
        let mut slots = [None; 8];
        slots[0] = Some(TextureSource::Target(slot0[k]));
        if k == 4 {
            slots[1] = Some(TextureSource::Target(bloom));
        }
        let id = f.add_pass(
            label,
            shaders[k],
            targets[k],
            if k == 0 { copy_geom } else { unit_geom },
            PassOpts {
                blend: BlendKey::OPAQUE.with_write_mask(if k == 4 {
                    write_mask::RGB
                } else {
                    write_mask::ALL
                }),
                slots,
                ..PassOpts::default()
            },
        );
        if k == 0 {
            let m = ortho(0.0, 64.0, 0.0, 64.0, -1.0, 1.0);
            f.write(id, "g_ModelViewProjectionMatrix", &f32_bytes(&m));
        }
        if (1..=3).contains(&k) {
            f.write(id, "g_BloomStrength", &f32_bytes(&[2.0]));
            f.write(id, "g_BloomThreshold", &f32_bytes(&[0.5]));
        }
        f.draw(id);
    }
    let mut scene = build(b, &f);
    assert_eq!(scene.stats().passes, 6);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    let glow = px(&data, w, 20, 32);
    assert!(glow[0] > 10, "bloom spreads outside the image: {glow:?}");
    assert_eq!(glow[3], 255, "combine writes RGB only");
    assert!(near(px(&data, w, 32, 32), [255, 255, 255, 255], 0));
}

/// A colour LUT (volume texture) in a `texture_3d` slot grades the image; a 2D-only pass given
/// the same volume reads the default texture instead.
#[test]
fn volume_lut_slot() {
    const LUT_SRC: &str = r"
struct Pass { g_ModelViewProjectionMatrix: mat4x4<f32> }
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
@group(1) @binding(2) var g_Texture1: texture_3d<f32>;
@group(1) @binding(3) var g_Texture1_s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = clip_fix(u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0));
    o.uv = a_TexCoord;
    return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(g_Texture0, g_Texture0_s, i.uv);
    return vec4<f32>(textureSample(g_Texture1, g_Texture1_s, c.rgb).rgb, 1.0);
}
";
    // 2×2×2 LUT that swaps red and blue.
    let mut lut = Vec::new();
    for bl in 0..2u8 {
        for g in 0..2u8 {
            for r in 0..2u8 {
                lut.extend_from_slice(&[bl * 255, g * 255, r * 255, 255]);
            }
        }
    }
    let lut_level = [MipLevel {
        width: 2,
        height: 2,
        depth: 2,
        data: &lut,
    }];
    let d = device();
    let (mut f, full) = Front::scene(8, 8, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let tex = texture(&mut b, &mut f, (8, 8), None, |x, _| {
        if x < 4 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        }
    });
    let vol = b
        .add_texture(GpuFormat::Rgba8Unorm, Dimension::D3, &lut_level)
        .unwrap();
    f.textures.push(Tex {
        storage: (2, 2),
        content: (2, 2),
        sampler: SamplerKey::new(AddressMode::Clamp, false, false, true),
    });
    let lut_shader = program(&mut b, &mut f, LUT_SRC, "vs", "fs");
    let v = f.add_vertices(&unit_quad());
    let mut slots = [None; 8];
    slots[0] = Some(TextureSource::Texture(tex));
    slots[1] = Some(TextureSource::Texture(vol));
    let id = f.add_pass(
        "lut",
        lut_shader,
        full,
        v,
        PassOpts {
            slots,
            ..PassOpts::default()
        },
    );
    f.draw(id);
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert!(
        near(px(&data, w, 1, 4), [0, 0, 255, 255], 2),
        "{:?}",
        px(&data, w, 1, 4)
    );
    assert!(
        near(px(&data, w, 6, 4), [255, 0, 0, 255], 2),
        "{:?}",
        px(&data, w, 6, 4)
    );

    // A volume bound to a 2D slot reads the 2D default (0, 0, 0, 1).
    let (mut f, full) = Front::scene(4, 4, [1.0, 1.0, 1.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let vol = b
        .add_texture(GpuFormat::Rgba8Unorm, Dimension::D3, &lut_level)
        .unwrap();
    f.textures.push(Tex {
        storage: (2, 2),
        content: (2, 2),
        sampler: SamplerKey::new(AddressMode::Repeat, false, false, true),
    });
    let sh = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let v = f.add_vertices(&unit_quad());
    let mut slots = [None; 8];
    slots[0] = Some(TextureSource::Texture(vol));
    let id = f.add_pass(
        "2d",
        sh,
        full,
        v,
        PassOpts {
            slots,
            ..PassOpts::default()
        },
    );
    f.draw(id);
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert!(near(px(&data, w, 2, 2), [0, 0, 0, 255], 0));
}

/// BC1 data is uploaded as-is (always supported at feature level 11_0).
#[test]
fn bc1_upload() {
    let d = device();
    let (mut f, full) = Front::scene(8, 8, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    // One 4×4 block: color0 = pure red (RGB565 0xF800), color1 = black, all indices 0.
    let block = [0x00, 0xF8, 0x00, 0x00, 0, 0, 0, 0];
    let tex = b
        .add_texture(
            GpuFormat::Bc1RgbaUnorm,
            Dimension::D2,
            &[MipLevel {
                width: 4,
                height: 4,
                depth: 1,
                data: &block,
            }],
        )
        .unwrap();
    f.textures.push(Tex {
        storage: (4, 4),
        content: (4, 4),
        sampler: SamplerKey::new(AddressMode::Repeat, false, false, false),
    });
    let sh = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let v = f.add_vertices(&unit_quad());
    let mut slots = [None; 8];
    slots[0] = Some(TextureSource::Texture(tex));
    let id = f.add_pass(
        "bc1",
        sh,
        full,
        v,
        PassOpts {
            slots,
            ..PassOpts::default()
        },
    );
    f.draw(id);
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert!(near(px(&data, w, 4, 4), [255, 0, 0, 255], 1));
}

/// Image visibility toggles its passes (spec 04 Q8: an image made visible later composes into
/// the scene).
#[test]
fn visibility_toggle() {
    let d = device();
    let (mut f, full) = Front::scene(16, 16, [0.0, 1.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let tex = texture(&mut b, &mut f, (4, 4), None, |_, _| [255, 0, 0, 255]);
    let sh = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let spec = image(full, 1, 16.0, tex, 8.0);
    let img = add_image(
        &mut f,
        &spec,
        &[ImagePass::new("base", sh), ImagePass::new("fx", sh)],
    );
    let mut scene = build(b, &f);
    for &p in &img.passes {
        f.enabled[p] = false;
    }
    let stats = render(&mut scene, &d, &mut f);
    assert_eq!(stats.draws, 0);
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 8, 8), [0, 255, 0, 255]);
    for &p in &img.passes {
        f.enabled[p] = true;
    }
    let stats = render(&mut scene, &d, &mut f);
    assert_eq!(stats.draws, 2);
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 8, 8), [255, 0, 0, 255]);
}

/// The extension of everything added to `f` since `base` (targets, passes, streams).
fn extension(f: &Front, base: (usize, usize, usize)) -> ScenePlanExtension {
    ScenePlanExtension {
        targets: f.plan.targets[base.0..].to_vec(),
        passes: f.plan.passes[base.1..].to_vec(),
        streams: f.plan.streams[base.2..].to_vec(),
        constants_len: f.plan.constants_len,
        vertex_bytes: f.plan.vertex_bytes,
    }
}

/// Layers added after `build` (`Scene::extend`): same programs and textures, larger shadow and
/// vertex region; `set_ops` places and reorders them and creates nothing.
#[test]
fn layers_added_after_build() {
    let d = device();
    let (mut f, full) = Front::scene(32, 32, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let red = texture(&mut b, &mut f, (4, 4), None, |_, _| [255, 0, 0, 255]);
    let blue = texture(&mut b, &mut f, (4, 4), None, |_, _| [0, 0, 255, 255]);
    let sh = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let spec = image(full, 1, 32.0, red, 16.0);
    add_image(&mut f, &spec, &[ImagePass::new("base", sh)]);
    let base_ops = f.plan.ops.clone();
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 16, 16), [255, 0, 0, 255]);

    // Four blue 16×16 layers in the centre, compiled against the built scene.
    let mut layers = Vec::new();
    for id in 2..6 {
        let base = (
            f.plan.targets.len(),
            f.plan.passes.len(),
            f.plan.streams.len(),
        );
        let ops_before = f.plan.ops.len();
        let spec = image(full, id, 16.0, blue, 16.0);
        let img = add_image(&mut f, &spec, &[ImagePass::new("layer", sh)]);
        // Not placed yet: the ops of the layer are kept aside.
        let layer_ops = f.plan.ops.split_off(ops_before);
        scene.extend(&d, extension(&f, base)).expect("extend");
        assert_eq!(scene.stats().passes, f.plan.passes.len());
        layers.push((img, layer_ops));
    }
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert_eq!(
        px(&data, w, 16, 16),
        [255, 0, 0, 255],
        "not placed: unchanged"
    );
    let place = |under: bool| {
        let mut ops = vec![base_ops[0]];
        let layer = layers[0].1.clone();
        if under {
            ops.extend(layer);
            ops.extend_from_slice(&base_ops[1..]);
        } else {
            ops.extend_from_slice(&base_ops[1..]);
            ops.extend(layer);
        }
        ops
    };
    scene.set_ops(&d, place(false)).unwrap();
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 16, 16), [0, 0, 255, 255], "layer on top");
    assert_eq!(px(&data, w, 2, 2), [255, 0, 0, 255], "base around it");

    let before = scene.stats().objects_created;
    scene.set_ops(&d, place(true)).unwrap();
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 16, 16), [255, 0, 0, 255], "layer under");
    assert_eq!(
        scene.stats().objects_created,
        before,
        "no object for a reorder"
    );
    scene.set_ops(&d, place(true)).unwrap();
    render(&mut scene, &d, &mut f);
    assert_eq!(scene.stats().objects_created, before);
}

/// Steady state: no Direct3D object created per frame, one constant `Map` per frame when the
/// shadow changed and none when it did not, `g_Time` reaches the pass block, timestamps, and the
/// per-segment profile.
#[test]
fn steady_state_frames() {
    let d = device();
    let (mut f, full) = Front::scene(32, 32, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let tex = texture(&mut b, &mut f, (4, 4), None, |_, _| [255, 255, 255, 255]);
    let copy = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let time = program(&mut b, &mut f, SRC, "vs", "fs_time");
    let spec = image(full, 1, 16.0, tex, 16.0);
    let img = add_image(
        &mut f,
        &spec,
        &[ImagePass::new("base", copy), ImagePass::new("timed", time)],
    );
    let timed = img.passes[1];
    let mut scene = build(b, &f);
    let mut timer = GpuTimer::new(&d).unwrap();
    let mut frame = |scene: &mut Scene, f: &mut Front, t: f32| {
        // The frame-global patch of `update_frame`: `g_Time` copied into the pass block.
        f.write(timed, "g_Time", &f32_bytes(&[t]));
        timer.begin(&d).unwrap();
        let stats = render(scene, &d, f);
        timer.end(&d);
        d.flush();
        let _ = timer.poll(&d).unwrap();
        stats
    };
    let first = frame(&mut scene, &mut f, 0.25);
    assert_eq!(
        (first.constant_maps, first.vertex_maps),
        (1, 1),
        "first frame"
    );
    assert_eq!(first.draws, 2);
    // Warm up, then measure.
    for i in 1..3 {
        frame(&mut scene, &mut f, 0.25 + i as f32 * 0.001);
    }
    // The scene counts every object it creates. (Process GPU memory is no witness here: the
    // other tests of this binary create devices at the same time; the 100-pass measurement reads
    // it alone in a child process.)
    let objects = scene.stats().objects_created;
    for i in 0..10 {
        let s = frame(&mut scene, &mut f, 0.25 + i as f32 * 0.01);
        assert_eq!(s.constant_maps, 1, "one Map per frame (time changed)");
        assert_eq!(s.vertex_maps + s.stream_maps, 0, "vertices unchanged");
        assert_eq!(s.draws, 2);
        assert!(d.wait_idle(Duration::from_secs(10)).unwrap());
    }
    assert_eq!(
        scene.stats().objects_created,
        objects,
        "no Direct3D object created per frame"
    );
    let (w, data) = read(&d, &scene, full);
    assert!(
        near(px(&data, w, 16, 16), [87, 0, 0, 255], 2),
        "g_Time reaches the pass block: {:?}",
        px(&data, w, 16, 16)
    );
    // Same inputs again: nothing mapped.
    let s = frame(&mut scene, &mut f, 0.34);
    assert_eq!(s.constant_maps, 0, "unchanged shadow, no Map");
    let deadline = Instant::now() + Duration::from_secs(5);
    while timer.last().is_none() && Instant::now() < deadline {
        timer.poll(&d).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(timer.last().is_some(), "GPU timer produced a measurement");

    // Per-segment profile: the clear of the scene, the base pass into A, the timed pass back on
    // the scene. The queries are created once.
    let profile = |scene: &mut Scene, f: &Front| {
        let streams: Vec<StreamFrame<'_>> = Vec::new();
        scene
            .profile_frame(
                &d,
                &FrameInput {
                    shadow: &f.shadow,
                    vertices: &f.vertices,
                    vertices_dirty: None,
                    variants: &f.variants,
                    enabled: &f.enabled,
                    streams: &streams,
                },
            )
            .unwrap()
    };
    let segments = profile(&mut scene, &f).expect("timestamps");
    let shape: Vec<(&str, usize, &str)> = segments
        .iter()
        .map(|s| (s.target.as_str(), s.passes, s.first_pass.as_str()))
        .collect();
    assert_eq!(
        shape,
        [
            ("_rt_FullFrameBuffer", 0, ""),
            ("_rt_imageLayerComposite_1_a", 1, "base"),
            ("_rt_FullFrameBuffer", 1, "timed"),
        ]
    );
    assert!(segments.iter().all(|s| s.ms >= 0.0 && s.ms < 1000.0));
    assert_eq!(segments[0].size, (32, 32));
    let objects = scene.stats().objects_created;
    profile(&mut scene, &f).expect("timestamps");
    assert_eq!(
        scene.stats().objects_created,
        objects,
        "profiling reuses its queries"
    );
}

// ---------------------------------------------------------------------------------------------
// tests/gpu3d.rs

const SRC_3D: &str = r"
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

/// F-3D1 quad (spec 09 §4.1) as a position + UV mesh.
fn quad_mesh(b: &mut SceneBuilder<'_>, z: f32, clockwise: bool) -> usize {
    let verts: [([f32; 3], [f32; 2]); 4] = [
        ([-1.0, -1.0, z], [0.0, 1.0]),
        ([1.0, -1.0, z], [1.0, 1.0]),
        ([1.0, 1.0, z], [1.0, 0.0]),
        ([-1.0, 1.0, z], [0.0, 0.0]),
    ];
    let bytes: Vec<u8> = verts
        .iter()
        .flat_map(|(p, uv)| f32_bytes(&[p[0], p[1], p[2], uv[0], uv[1]]))
        .collect();
    let indices: &[u16] = if clockwise {
        &[0, 2, 1, 0, 3, 2]
    } else {
        &[0, 1, 2, 0, 2, 3]
    };
    let layout = MeshLayoutPlan {
        stride: 20,
        attributes: vec![
            AttributePlan {
                name: "a_Position".into(),
                format: VertexFormat::Float32x3,
                offset: 0,
            },
            AttributePlan {
                name: "a_TexCoord".into(),
                format: VertexFormat::Float32x2,
                offset: 12,
            },
        ],
    };
    b.add_mesh(&layout, &bytes, indices).unwrap()
}

/// 3D shader: Wallpaper Engine clip convention, no correction (spec 09 §0.3).
fn program_3d(b: &mut SceneBuilder<'_>, f: &mut Front, vs: &str, fs: &str) -> usize {
    program(b, f, SRC_3D, vs, fs)
}

/// 3D passes: front faces counter-clockwise (`FrontFace::Ccw`).
fn raster_3d(cull: Cull) -> RasterKey {
    RasterKey {
        cull,
        front_ccw: true,
        ..RasterKey::default()
    }
}

#[test]
fn f_3d_o_orientation() {
    let d = device();
    let (mut f, full) = Front::scene(16, 16, [0.0, 0.0, 0.0, 1.0]);
    f.plan.targets[full].depth = true;
    let mut b = SceneBuilder::new(&d, None);
    let mesh = quad_mesh(&mut b, 0.0, false);
    let sh = program_3d(&mut b, &mut f, "vs_clip", "fs_v");
    let p = f.add_pass(
        "f3do",
        sh,
        full,
        GeometryPlan::Mesh(mesh),
        PassOpts {
            raster: raster_3d(Cull::None),
            ..PassOpts::default()
        },
    );
    f.draw(p);
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    let (w, data) = read(&d, &scene, full);
    let red = |x: u32, y: u32| px(&data, w, x, y)[0];
    // As in Wallpaper Engine: v = 1 on the first row.
    assert!(red(8, 0) > 230, "first row {}", red(8, 0));
    assert!(red(8, 15) < 25, "last row {}", red(8, 15));
}

#[test]
fn f_3d_o_with_matrices_and_depth() {
    let d = device();
    let (w, h) = (64u32, 36u32);
    let (mut f, full) = Front::scene(w, h, [0.0, 0.0, 0.0, 1.0]);
    f.plan.targets[full].depth = true;
    let mut b = SceneBuilder::new(&d, None);
    // `look_at(0, −Z, Y)` is the identity.
    let vp = perspective(50.0, w as f32 / h as f32, 0.01, 10000.0);
    let near_q = quad_mesh(&mut b, -3.0, false);
    let sh_uv = program_3d(&mut b, &mut f, "vs_mvp", "fs_uv");
    let sh_col = program_3d(&mut b, &mut f, "vs_mvp", "fs_color");
    let depth = DepthKey {
        test: true,
        write: true,
    };
    let p_near = f.add_pass(
        "near",
        sh_uv,
        full,
        GeometryPlan::Mesh(near_q),
        PassOpts {
            raster: raster_3d(Cull::Back),
            depth,
            ..PassOpts::default()
        },
    );
    // A larger quad behind, drawn after: hidden by the depth test where they overlap.
    let far_q = quad_mesh(&mut b, -6.0, false);
    let p_far = f.add_pass(
        "far",
        sh_col,
        full,
        GeometryPlan::Mesh(far_q),
        PassOpts {
            raster: raster_3d(Cull::Back),
            depth,
            ..PassOpts::default()
        },
    );
    // A clockwise (back-facing) quad in front of everything: culled.
    let back_q = quad_mesh(&mut b, -1.5, true);
    let p_back = f.add_pass(
        "back",
        sh_col,
        full,
        GeometryPlan::Mesh(back_q),
        PassOpts {
            raster: raster_3d(Cull::Back),
            ..PassOpts::default()
        },
    );
    for (p, m, colour) in [
        (p_near, vp, [0.0, 0.0, 0.0]),
        (p_far, mul(&vp, &scale(4.0, 4.0, 1.0)), [0.0, 0.0, 1.0]),
        (p_back, vp, [1.0, 1.0, 1.0]),
    ] {
        f.write(p, "g_ModelViewProjectionMatrix", &f32_bytes(&m));
        f.write(p, "g_Color", &f32_bytes(&colour));
        f.draw(p);
    }
    let mut scene = build(b, &f);
    render(&mut scene, &d, &mut f);
    // Rendered twice: the depth buffer is cleared every frame.
    render(&mut scene, &d, &mut f);
    let (wd, data) = read(&d, &scene, full);
    let at = |x: u32, y: u32| {
        let p = px(&data, wd, x, y);
        [p[0], p[1], p[2]]
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

// ---------------------------------------------------------------------------------------------
// tests/quality_gpu.rs

const SIDE: u32 = 32;

const QUALITY_SRC: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = clip_fix(u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0));
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

/// A white triangle on black whose slanted edge crosses pixels, with `samples` on the scene
/// target. Returns the samples in force and the pixels.
fn draw_triangle(d: &Device, samples: u32) -> (u32, Vec<u8>) {
    let (mut f, full) = Front::scene(SIDE, SIDE, [0.0, 0.0, 0.0, 1.0]);
    f.plan.targets[full].samples = samples;
    let mut b = SceneBuilder::new(d, None);
    let white = program(&mut b, &mut f, QUALITY_SRC, "vs", "fs_white");
    let v = |x: f32, y: f32| [x, y, 0.0, 0.0, 0.0];
    let tri = f.add_vertices(&[v(-0.9, -0.8), v(0.85, -0.3), v(-0.2, 0.9)]);
    let p = f.add_pass("tri", white, full, tri, PassOpts::default());
    f.draw(p);
    let mut scene = build(b, &f);
    let stats = render(&mut scene, d, &mut f);
    let level = scene.target_samples(full).unwrap();
    assert_eq!(stats.resolves, u32::from(level > 1));
    (level, read(d, &scene, full).1)
}

fn partial_pixels(px: &[u8]) -> usize {
    px.as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 8 && p[0] < 247)
        .count()
}

#[test]
fn multisampling_antialiases_the_edges_and_keeps_the_interior() {
    let d = device();
    let (flat_level, flat) = draw_triangle(&d, 1);
    let (level, ms) = draw_triangle(&d, 4);
    assert_eq!(flat_level, 1);
    if level == 1 {
        eprintln!("skipped: adapter has no 4x multisampling for Rgba8");
        return;
    }
    // A hard edge has no intermediate value; a multisampled one has many.
    assert_eq!(partial_pixels(&flat), 0, "single-sampled edge is hard");
    assert!(
        partial_pixels(&ms) >= 10,
        "multisampled edge has coverage values, got {}",
        partial_pixels(&ms)
    );
    // Centre of the triangle and a far corner are the same either way.
    let at = |px: &[u8], x: u32, y: u32| px[((y * SIDE + x) * 4) as usize];
    assert_eq!(at(&flat, 14, 17), 255);
    assert_eq!(at(&ms, 14, 17), 255);
    assert_eq!(at(&flat, 31, 0), 0);
    assert_eq!(at(&ms, 31, 0), 0);
}

#[test]
fn unsupported_multisampling_falls_back_to_a_supported_level() {
    let d = device();
    let (level, _) = draw_triangle(&d, 8);
    assert!(level <= 8);
    assert!(
        level == 1 || d.msaa_quality_levels(TargetFormat::Rgba8Unorm.dxgi(), level) > 0,
        "{level}× is supported"
    );
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
/// by a quarter: 8-bit targets clamp the first write to 1.0 and end at 0.25, float targets end at
/// 1.0.
fn scale_down_scene(d: &Device, hdr: bool) -> (Scene, usize) {
    let (mut f, full) = Front::scene(SIDE, SIDE, [0.0, 0.0, 0.0, 1.0]);
    f.plan.ops.clear();
    if hdr {
        f.plan.targets[full].format = TargetFormat::Rgba16Float;
    }
    let mut b = SceneBuilder::new(d, None);
    let four = program(&mut b, &mut f, QUALITY_SRC, "vs", "fs_four");
    let quarter = program(&mut b, &mut f, QUALITY_SRC, "vs", "fs_quarter");
    let quad = f.add_vertices(&unit_quad());
    let first = f.add_pass("four", four, full, quad, PassOpts::default());
    let mut slots = [None; 8];
    slots[0] = Some(TextureSource::Target(full));
    let second = f.add_pass(
        "quarter",
        quarter,
        full,
        quad,
        PassOpts {
            slots,
            ..PassOpts::default()
        },
    );
    assert!(f.plan.passes[second].hazard_scratch.is_some());
    f.draw(first);
    f.draw(second);
    let mut scene = build(b, &f);
    render(&mut scene, d, &mut f);
    (scene, full)
}

#[test]
fn hdr_targets_keep_values_above_one() {
    let d = device();
    let (eight, rt) = scale_down_scene(&d, false);
    let (_, px_) = read(&d, &eight, rt);
    assert!(
        (i32::from(px_[0]) - 64).abs() <= 1,
        "8-bit clamps: {}",
        px_[0]
    );
    let (float, rt) = scale_down_scene(&d, true);
    assert_eq!(float.target_format(rt), Some(TargetFormat::Rgba16Float));
    let resource: ID3D11Resource = float.target_texture(rt).unwrap().cast().unwrap();
    let back = read_subresource(&d, &resource, 0).unwrap();
    let red = half_to_f32(u16::from_le_bytes([back.data[0], back.data[1]]));
    assert!((red - 1.0).abs() < 1e-3, "float keeps 4.0: got {red}");
}

#[test]
fn default_quality_costs_nothing_more() {
    let d = device();
    let (mut f, full) = Front::scene(SIDE, SIDE, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let copy = program(&mut b, &mut f, QUALITY_SRC, "vs", "fs_copy");
    let quad = f.add_vertices(&unit_quad());
    let p = f.add_pass("p", copy, full, quad, PassOpts::default());
    f.draw(p);
    let scene = build(b, &f);
    assert_eq!(scene.target_samples(full), Some(1));
    assert_eq!(scene.target_format(full), Some(TargetFormat::Rgba8Unorm));
    let rt = scene.render_target(full).unwrap();
    assert_eq!(rt.desc().samples, 1);
    assert!(rt.depth().is_none());
}

// ---------------------------------------------------------------------------------------------
// The executor's own rules.

/// Transient targets sharing one texture (`plan_aliasing`): the first draw into each clears it
/// to transparent black (`clear_first`), so nothing of the other target shows through.
#[test]
fn aliased_targets_share_a_texture_and_clear_first() {
    let d = device();
    let (mut f, full) = Front::scene(16, 16, [0.0, 0.0, 0.0, 1.0]);
    let t1 = f.target("fx1", (16, 16));
    let t2 = f.target("fx2", (16, 16));
    f.plan.targets[t2].shares = Some(t1);
    let mut b = SceneBuilder::new(&d, None);
    let red = texture(&mut b, &mut f, (2, 2), None, |_, _| [255, 0, 0, 255]);
    let blue = texture(&mut b, &mut f, (2, 2), None, |_, _| [0, 0, 255, 255]);
    let copy = program(&mut b, &mut f, SRC, "vs", "fs_copy");
    let unit = f.add_vertices(&unit_quad());
    let left = f.add_vertices(&quad(-1.0, 0.0, -1.0, 1.0, 1.0, 1.0));
    let slots = |s: TextureSource| {
        let mut a = [None; 8];
        a[0] = Some(s);
        PassOpts {
            slots: a,
            ..PassOpts::default()
        }
    };
    // fx1: red everywhere, composed translucent over the scene (alpha 1: red).
    let a = f.add_pass("red", copy, t1, unit, slots(TextureSource::Texture(red)));
    let a_out = f.add_pass(
        "red.out",
        copy,
        full,
        unit,
        slots(TextureSource::Target(t1)),
    );
    // fx2: blue on the left half only, the right half must be the clear (transparent).
    let bp = f.add_pass("blue", copy, t2, left, slots(TextureSource::Texture(blue)));
    let mut out = slots(TextureSource::Target(t2));
    out.blend = BlendKey::translucent();
    let b_out = f.add_pass("blue.out", copy, full, unit, out);
    f.plan.passes[a].clear_first = true;
    f.plan.passes[bp].clear_first = true;
    for p in [a, a_out, bp, b_out] {
        f.draw(p);
    }
    let mut scene = build(b, &f);
    assert!(scene.shares_texture(t1, t2));
    assert_eq!(scene.stats().targets, 3);
    assert_eq!(scene.stats().target_textures, 2, "fx1 and fx2 share one");
    let stats = render(&mut scene, &d, &mut f);
    assert_eq!(stats.clears, 3, "the scene's clear and two first draws");
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 3, 8), [0, 0, 255, 255], "blue half");
    assert_eq!(
        px(&data, w, 12, 8),
        [255, 0, 0, 255],
        "red under the cleared half"
    );
    // A new order unshares the targets (lifetimes were planned on the old order).
    let objects = scene.stats().objects_created;
    scene.set_ops(&d, f.plan.ops.clone()).unwrap();
    assert!(!scene.shares_texture(t1, t2));
    assert!(scene.stats().objects_created > objects);
}

/// A particle stream: its vertices are written when the frame gives them, the index count is set
/// every frame, the layout is matched by name (the colour of the vertex, the position).
#[test]
fn particle_stream_draws_the_frame_count() {
    let src = r"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_Color: vec4<f32>) -> VOut {
    var o: VOut;
    o.pos = clip_fix(vec4<f32>(a_Position, 1.0));
    o.c = a_Color;
    return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> { return i.c; }
";
    let d = device();
    let (mut f, full) = Front::scene(8, 8, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(&d, None);
    let sh = program(&mut b, &mut f, src, "vs", "fs");
    f.plan.streams.push(StreamPlan {
        stride: 28,
        attributes: vec![
            AttributePlan {
                name: "a_Color".into(),
                format: VertexFormat::Float32x4,
                offset: 12,
            },
            AttributePlan {
                name: "a_Position".into(),
                format: VertexFormat::Float32x3,
                offset: 0,
            },
        ],
        max_vertices: 8,
        indices: vec![0, 1, 2, 2, 1, 3, 4, 5, 6, 6, 5, 7],
    });
    f.streams.push(StreamData {
        vertices: Vec::new(),
        index_count: 0,
        written: false,
    });
    let id = f.add_pass(
        "particles",
        sh,
        full,
        GeometryPlan::Stream(0),
        PassOpts::default(),
    );
    f.draw(id);
    let quad_of = |x0: f32, x1: f32, c: [f32; 4]| -> Vec<f32> {
        [(x0, -1.0), (x0, 1.0), (x1, -1.0), (x1, 1.0)]
            .iter()
            .flat_map(|&(x, y)| [x, y, 0.0, c[0], c[1], c[2], c[3]])
            .collect()
    };
    let mut scene = build(b, &f);
    // Frame 1: one red quad on the left.
    f.streams[0] = StreamData {
        vertices: f32_bytes(&quad_of(-1.0, 0.0, [1.0, 0.0, 0.0, 1.0])),
        index_count: 6,
        written: true,
    };
    let s = render(&mut scene, &d, &mut f);
    assert_eq!((s.stream_maps, s.draws), (1, 1));
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 1, 4), [255, 0, 0, 255]);
    assert_eq!(px(&data, w, 6, 4), [0, 0, 0, 255]);
    // Frame 2: two quads, green on the right.
    let mut v = quad_of(-1.0, 0.0, [1.0, 0.0, 0.0, 1.0]);
    v.extend(quad_of(0.0, 1.0, [0.0, 1.0, 0.0, 1.0]));
    f.streams[0] = StreamData {
        vertices: f32_bytes(&v),
        index_count: 12,
        written: true,
    };
    let s = render(&mut scene, &d, &mut f);
    assert_eq!(s.stream_maps, 1);
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 6, 4), [0, 255, 0, 255]);
    // Frame 3: nothing written, six indices: the left quad only, from the kept vertices.
    f.streams[0].index_count = 6;
    let s = render(&mut scene, &d, &mut f);
    assert_eq!(s.stream_maps, 0);
    let (w, data) = read(&d, &scene, full);
    assert_eq!(px(&data, w, 1, 4), [255, 0, 0, 255]);
    assert_eq!(px(&data, w, 6, 4), [0, 0, 0, 255]);
    // No index: nothing drawn.
    f.streams[0].index_count = 0;
    assert_eq!(render(&mut scene, &d, &mut f).draws, 0);
}

/// A plan that does not match the uploads, the frame or the executor's rules is an error value.
#[test]
fn bad_plans_and_frames_are_errors_not_panics() {
    fn setup(d: &Device) -> (Front, SceneBuilder<'_>, usize, usize) {
        let (mut f, full) = Front::scene(8, 8, [0.0; 4]);
        let mut b = SceneBuilder::new(d, None);
        let copy = program(&mut b, &mut f, SRC, "vs", "fs_copy");
        let v = f.add_vertices(&unit_quad());
        let p = f.add_pass("p", copy, full, v, PassOpts::default());
        f.draw(p);
        (f, b, full, p)
    }
    let d = device();
    // An unknown program.
    let (mut f, b, _, p) = setup(&d);
    f.plan.passes[p].program = 5;
    assert!(matches!(b.build(&f.plan), Err(Error::InvalidData(_))));
    // A pass sampling its own target without a scratch.
    let (mut f, b, full, p) = setup(&d);
    f.plan.passes[p].slots[0] = Some(f.slot(TextureSource::Target(full)));
    assert!(matches!(b.build(&f.plan), Err(Error::InvalidData(_))));
    // A window smaller than the program's block (80 bytes), or none.
    let (mut f, b, _, p) = setup(&d);
    f.plan.passes[p].uniform = None;
    assert!(matches!(b.build(&f.plan), Err(Error::InvalidData(_))));
    // A copy into a multisampled target.
    let (mut f, b, full, _) = setup(&d);
    let ms = f.target("ms", (8, 8));
    f.plan.targets[ms].samples = 4;
    f.plan.ops.push(OpPlan::Copy { src: full, dst: ms });
    let r = b.build(&f.plan);
    let supported = d.msaa_quality_levels(TargetFormat::Rgba8Unorm.dxgi(), 4) > 0;
    assert!(!supported || matches!(r, Err(Error::InvalidData(_))));
    // A frame that does not match the scene.
    let (mut f, b, _, _) = setup(&d);
    let mut scene = b.build(&f.plan).unwrap();
    let short = &f.shadow[..f.shadow.len() - 4];
    let r = scene.render(
        &d,
        &FrameInput {
            shadow: short,
            vertices: &f.vertices,
            vertices_dirty: None,
            variants: &f.variants,
            enabled: &f.enabled,
            streams: &[],
        },
    );
    assert!(matches!(r, Err(Error::InvalidData(_))));
    let r = scene.render(
        &d,
        &FrameInput {
            shadow: &f.shadow,
            vertices: &f.vertices,
            vertices_dirty: None,
            variants: &[],
            enabled: &f.enabled,
            streams: &[],
        },
    );
    assert!(matches!(r, Err(Error::InvalidData(_))));
    // Ops and extensions out of range; puppets after load.
    assert!(scene.set_ops(&d, vec![OpPlan::Draw(9)]).is_err());
    let mut ext = extension(&f, (1, 1, 0));
    let mut p = f.plan.passes[0].clone();
    p.geometry = GeometryPlan::Indexed {
        first_vertex: 0,
        first_index: 0,
        index_count: 0,
    };
    ext.passes.push(p);
    assert!(matches!(scene.extend(&d, ext), Err(Error::Unsupported(_))));
    let mut p = f.plan.passes[0].clone();
    p.target = 7;
    let mut ext = extension(&f, (1, 1, 0));
    ext.passes.push(p);
    assert!(matches!(scene.extend(&d, ext), Err(Error::InvalidData(_))));
    assert_eq!(scene.stats().passes, 1, "a failed extension adds nothing");
    // The scene still renders.
    render(&mut scene, &d, &mut f);
}

// ---------------------------------------------------------------------------------------------
// Shared GPU memory of a synthetic 100-pass scene.

/// Twenty images of five passes (copy into A, three tint effects through A/B, the last pass into
/// the scene, translucent), 1 KiB uniform blocks (a 100 KiB arena: windows past 64 KiB), a frame
/// block, and the composites of every image aliased onto image 0's (`clear_first`).
const SYNTHETIC_SRC: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_Color: vec4<f32>,
    g_Pad: array<vec4<f32>, 59>,
}
struct Frame { g_One: f32 }
@group(0) @binding(0) var<uniform> u: Pass;
@group(0) @binding(1) var<uniform> frame: Frame;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = clip_fix(u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0));
    o.uv = a_TexCoord;
    return o;
}
@fragment fn fs_copy(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv);
}
@fragment fn fs_tint(i: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(g_Texture0, g_Texture0_s, i.uv) * u.g_Color;
    return vec4<f32>(c.rgb * frame.g_One + u.g_Pad[58].xyz, c.a);
}
";

/// Colour of image `k` of the synthetic scene.
fn synthetic_colour(k: u32) -> [u8; 4] {
    [
        (k * 12 + 15) as u8,
        (255 - k * 11) as u8,
        ((k * 37) % 200 + 40) as u8,
        255,
    ]
}

/// The synthetic scene: `(scene, plan front, scene target, image centres)`.
fn synthetic_scene(
    d: &Device,
    size: (u32, u32),
    image: u32,
) -> (Scene, Front, usize, Vec<(u32, u32)>) {
    let (mut f, full) = Front::scene(size.0, size.1, [0.0, 0.0, 0.0, 1.0]);
    let mut b = SceneBuilder::new(d, None);
    let copy = program(&mut b, &mut f, SYNTHETIC_SRC, "vs", "fs_copy");
    let tint = program(&mut b, &mut f, SYNTHETIC_SRC, "vs", "fs_tint");
    let frame = f.window(4);
    f.plan.frame_block = Some(frame);
    let at = frame.offset as usize;
    f.shadow[at..at + 4].copy_from_slice(&1.0f32.to_le_bytes());
    let mut centres = Vec::new();
    let mut owners: Option<(usize, usize)> = None;
    for k in 0..20u32 {
        let c = synthetic_colour(k);
        let tex = texture(&mut b, &mut f, (image, image), None, move |_, _| c);
        let (col, row) = (k % 5, k / 5);
        let centre = (
            (col as f32 + 0.5) * size.0 as f32 / 5.0,
            (row as f32 + 0.5) * size.1 as f32 / 4.0,
        );
        let spec = ImageSpec {
            id: k,
            size: (image as f32, image as f32),
            main: tex,
            origin: centre,
            scale: (1.0, 1.0),
            scene: full,
        };
        let mut passes = vec![ImagePass::new("copy", copy)];
        for e in 0..4 {
            passes.push(ImagePass::new(&format!("tint{e}"), tint));
        }
        passes[0].blend = Blend::Translucent;
        let img = add_image(&mut f, &spec, &passes);
        // Transient composites: image 0's are the textures, the others alias them.
        match owners {
            None => owners = Some((img.a, img.b)),
            Some((a, b)) => {
                f.plan.targets[img.a].shares = Some(a);
                f.plan.targets[img.b].shares = Some(b);
            }
        }
        f.plan.passes[img.passes[0]].clear_first = true;
        f.plan.passes[img.passes[1]].clear_first = true;
        // `g_Pad[58]` stays zero; the frame block's `g_One` is 1.
        centres.push((centre.0 as u32, size.1 - centre.1 as u32));
    }
    assert_eq!(f.plan.passes.len(), 100);
    let scene = b.build(&f.plan).expect("100-pass scene");
    (scene, f, full, centres)
}

/// `GPU Process Memory(pid_<pid>_*)\Shared Usage` and `Dedicated Usage` (what Task Manager
/// shows), as `examples/clear_present.rs` reads them.
mod counters {
    use lumenwall_d3d11::windows::Win32::System::Performance::{
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW,
    };
    use lumenwall_d3d11::windows::core::{PCWSTR, w};

    pub struct Counters {
        query: PDH_HQUERY,
        shared: PDH_HCOUNTER,
        dedicated: PDH_HCOUNTER,
        prefix: String,
    }

    impl Counters {
        pub fn new() -> Option<Self> {
            let mut query = PDH_HQUERY::default();
            let (mut shared, mut dedicated) = (PDH_HCOUNTER::default(), PDH_HCOUNTER::default());
            // SAFETY: live out-parameters; static wide counter paths.
            unsafe {
                if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0
                    || PdhAddEnglishCounterW(
                        query,
                        w!("\\GPU Process Memory(*)\\Shared Usage"),
                        0,
                        &mut shared,
                    ) != 0
                    || PdhAddEnglishCounterW(
                        query,
                        w!("\\GPU Process Memory(*)\\Dedicated Usage"),
                        0,
                        &mut dedicated,
                    ) != 0
                {
                    return None;
                }
            }
            Some(Self {
                query,
                shared,
                dedicated,
                prefix: format!("pid_{}_", std::process::id()),
            })
        }

        /// `(shared, dedicated)` bytes.
        pub fn read(&self) -> (u64, u64) {
            // SAFETY: valid query handle.
            unsafe { PdhCollectQueryData(self.query) };
            (self.sum(self.shared), self.sum(self.dedicated))
        }

        fn sum(&self, counter: PDH_HCOUNTER) -> u64 {
            let (mut size, mut count) = (0u32, 0u32);
            // SAFETY: the first call asks for the size (PDH_MORE_DATA).
            let status = unsafe {
                PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut size, &mut count, None)
            };
            if status != 0x8000_07D2 {
                return 0;
            }
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let items = buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            // SAFETY: `buf` holds `size` 8-aligned bytes as requested.
            if unsafe {
                PdhGetFormattedCounterArrayW(
                    counter,
                    PDH_FMT_LARGE,
                    &mut size,
                    &mut count,
                    Some(items),
                )
            } != 0
            {
                return 0;
            }
            let mut total = 0u64;
            for i in 0..count as usize {
                // SAFETY: `count` items were written; their names point into `buf`.
                let item = unsafe { &*items.add(i) };
                // SAFETY: as above.
                let name = unsafe { item.szName.to_string() }.unwrap_or_default();
                if name.starts_with(&self.prefix) {
                    // SAFETY: the union member of PDH_FMT_LARGE.
                    total += unsafe { item.FmtValue.Anonymous.largeValue } as u64;
                }
            }
            total
        }
    }

    impl Drop for Counters {
        fn drop(&mut self) {
            // SAFETY: valid query handle.
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}

const MEMORY_CHILD: &str = "LUMENWALL_D3D11_SCENE_MEMORY_CHILD";

/// Shared GPU memory of the synthetic 100-pass scene after `trim` (the build trims), measured
/// like `examples/clear_present.rs`: GPU Process Memory counters and `QueryVideoMemoryInfo`, an
/// off-screen 2560×1440 window presenting at 30 fps, Idle priority. The counters are per process:
/// the measurement runs alone, in a child process.
///
/// On the hardware adapter it asserts shared memory under 1.5 MiB at the end of the window, on
/// both readings. `LUMENWALL_D3D11_TEST_ADAPTER=hardware cargo test --test scene hundred_pass --
/// --nocapture` prints the table.
///
/// Measured (RTX 4080 SUPER, driver 621.01, Windows 11, debug build of the test, 2560×1440 scene,
/// 256×256 images, 3 runs of 3 s one after the other, 5 November 2026), MiB, shared = GPU Process
/// Memory counter, equal to `QueryVideoMemoryInfo` non-local in every reading:
///
/// | Step | Shared | Dedicated | Local (QVMI) |
/// |---|---|---|---|
/// | Device | 0.72 | 10.79 | 8.31 |
/// | Scene built and trimmed (100 passes, 20 textures, 3 target textures) | 0.75 | 15.87 | 13.36 |
/// | 30 frames presented | 1.05–1.22 | 61.39–61.40 | 58.88 |
/// | 90 frames presented | **1.22–1.28** | 61.40 | 58.88 |
///
/// Two more runs over 20 s (`LUMENWALL_D3D11_MEMORY_SECONDS=20`, 600 frames, all presented):
/// shared 1.17 / 1.09 at 30 frames, 1.19 / 1.28 at 90, 1.22 / 1.28 at 300 and **1.22 / 1.28 at
/// 600**: flat after the first seconds, under the 1.5 MiB target and Wallpaper Engine's 3 MiB.
///
/// The scene adds 0.03 MiB to the device until frames run, then 0.3–0.5 MiB while they run (the
/// 2560×1440 swap chain alone stays at 0.80 in the foundation's measurement). 66 Direct3D objects:
/// 3 buffers (constant arena of 112 KiB, vertex region, static vertex data), 2 input layouts,
/// 6 state objects; the 40 composites alias two textures.
#[test]
fn hundred_pass_scene_shared_memory() {
    if std::env::var_os(MEMORY_CHILD).is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "hundred_pass_scene_shared_memory",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(MEMORY_CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        println!("{stdout}");
        eprintln!("{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the measurement failed");
        assert!(stdout.contains("1 passed"), "the measurement ran");
        return;
    }
    use lumenwall_d3d11::windows::Win32::System::Threading::{
        GetCurrentProcess, IDLE_PRIORITY_CLASS, SetPriorityClass,
    };
    use lumenwall_d3d11::{OffscreenWindow, PresentStatus, SwapChain};

    const MIB: f64 = 1024.0 * 1024.0;
    let hw = hardware();
    // SAFETY: the current process's pseudo-handle.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), IDLE_PRIORITY_CLASS) };
    let counters = counters::Counters::new();
    let stage = |name: &str, d: &Device| -> (u64, u64) {
        let (shared, dedicated) = counters.as_ref().map_or((0, 0), counters::Counters::read);
        let m = d.memory().unwrap();
        println!(
            "| {name} | {:.2} | {:.2} | {:.2} | {:.2} |",
            shared as f64 / MIB,
            dedicated as f64 / MIB,
            m.non_local.usage as f64 / MIB,
            m.local.usage as f64 / MIB
        );
        (shared, m.non_local.usage)
    };
    // `LUMENWALL_D3D11_MEMORY_SECONDS` (hardware, 1 to 60, default 3): a longer window, as the
    // foundation's 20 s measurement.
    let seconds = std::env::var("LUMENWALL_D3D11_MEMORY_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(3)
        .clamp(1, 60);
    let (size, image, frames) = if hw {
        ((2560, 1440), 256, seconds * 30)
    } else {
        ((640, 360), 64, 6)
    };
    println!(
        "| Step | Shared (counters) MiB | Dedicated (counters) MiB | Non-local (QVMI) MiB | Local (QVMI) MiB |"
    );
    println!("|---|---|---|---|---|");
    let d = device();
    stage("device", &d);
    let window = OffscreenWindow::new(size.0, size.1).unwrap();
    let swap = SwapChain::for_hwnd(&d, window.hwnd(), size).unwrap();
    let t = Instant::now();
    let (mut scene, mut f, full, centres) = synthetic_scene(&d, size, image);
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    stage("scene built (trimmed)", &d);
    let interval = Duration::from_secs_f64(1.0 / 30.0);
    let mut next = Instant::now() + interval;
    let mut shown = 0;
    for frame in 0..frames {
        window.pump();
        let stats = render(&mut scene, &d, &mut f);
        assert!(stats.constant_maps <= 1);
        assert_eq!(stats.draws, 100);
        let ctx = d.context();
        // SAFETY: the swap chain's view on its own device.
        unsafe {
            ctx.OMSetRenderTargets(Some(&[Some(swap.rtv().clone())]), None);
            ctx.ClearRenderTargetView(swap.rtv(), &[0.0, 0.0, 0.0, 1.0]);
        }
        if matches!(swap.present(&d).unwrap(), PresentStatus::Presented) {
            shown += 1;
        }
        if [29, 89, 299].contains(&frame) && frame + 1 < frames {
            stage(&format!("{} frames", frame + 1), &d);
        }
        if hw {
            let now = Instant::now();
            if now < next {
                std::thread::sleep(next - now);
                next += interval;
            } else {
                next = now + interval;
            }
        }
    }
    let (shared, non_local) = stage(&format!("end ({frames} frames)"), &d);
    let s = scene.stats();
    println!(
        "adapter: {}; build {build_ms:.0} ms; {} passes, {} targets on {} textures, {} buffers, \
         {} input layouts, states {:?}, {} objects; arena {} KiB; presents shown: {shown}",
        d.info().name,
        s.passes,
        s.targets,
        s.target_textures,
        s.buffers,
        s.input_layouts,
        s.states,
        s.objects_created,
        s.buffer_bytes / 1024,
    );
    if hw {
        assert!(
            (shared.max(non_local) as f64) < 1.5 * MIB,
            "shared GPU memory {:.2} MiB (counters) / {:.2} MiB (QVMI)",
            shared as f64 / MIB,
            non_local as f64 / MIB
        );
    }
    // Every image lands where it belongs, with its colour (windows past 64 KiB included).
    let (w, data) = read(&d, &scene, full);
    for (k, &(x, y)) in centres.iter().enumerate() {
        assert!(
            near(px(&data, w, x, y), synthetic_colour(k as u32), 2),
            "image {k}: {:?}",
            px(&data, w, x, y)
        );
    }
}

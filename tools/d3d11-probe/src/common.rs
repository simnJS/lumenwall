//! What both backends share: uniform contents, quad vertices, texture row layout and the report.

use lumenwall_shader::{PassShader, ScalarKind, UniformType, UniformValue};

use crate::plan::{OUTPUT, Src, Workload};

/// Rounds a constant buffer size up to 16 bytes (a Direct3D 11 rule), at least 16.
pub fn cb_size(bytes: u32) -> u32 {
    bytes.max(16).div_ceil(16) * 16
}

fn put_f32(buf: &mut [u8], offset: usize, v: &[f32]) {
    for (i, f) in v.iter().enumerate() {
        if let Some(dst) = buf.get_mut(offset + i * 4..offset + i * 4 + 4) {
            dst.copy_from_slice(&f.to_le_bytes());
        }
    }
}

/// The frame block (`@group(0) @binding(1)`): `g_Time`, `g_Daytime`, pointer, texel sizes, audio
/// spectra (zero).
pub fn fill_frame(buf: &mut [u8], time: f32) {
    put_f32(buf, 0, &[time, 0.5]);
    put_f32(buf, 8, &[0.5, 0.5]);
    put_f32(buf, 16, &[0.5, 0.5]);
    let (tx, ty) = (1.0 / OUTPUT.0 as f32, 1.0 / OUTPUT.1 as f32);
    put_f32(buf, 24, &[tx, ty]);
    put_f32(buf, 32, &[tx * 0.5, ty * 0.5]);
}

fn default_values(v: UniformValue) -> [f32; 4] {
    match v {
        UniformValue::Float(f) => [f, 0.0, 0.0, 0.0],
        UniformValue::Int(i) => [i as f32, 0.0, 0.0, 0.0],
        UniformValue::Vec2([x, y]) => [x, y, 0.0, 0.0],
        UniformValue::Vec3([x, y, z]) => [x, y, z, 0.0],
        UniformValue::Vec4(v) => v,
    }
}

/// The pass block: identity matrices, opaque white, texture resolutions, annotation defaults for
/// the material constants. `dims(slot)` is the size of the texture bound to a slot.
pub fn fill_pass(buf: &mut [u8], shader: &PassShader, time: f32, dims: &dyn Fn(u32) -> (u32, u32)) {
    let Some(block) = &shader.program.reflection.uniform_block else {
        return;
    };
    let defaults: Vec<_> = shader
        .vertex_params
        .uniforms
        .iter()
        .chain(&shader.fragment_params.uniforms)
        .collect();
    for m in &block.members {
        let off = m.offset as usize;
        let n = m.array_len.max(1) as usize;
        let stride = if m.array_len > 0 {
            m.array_stride as usize
        } else {
            0
        };
        match m.ty {
            UniformType::Matrix { columns, rows } => {
                for a in 0..n {
                    for c in 0..columns as usize {
                        let mut col = [0.0f32; 4];
                        if c < rows as usize {
                            col[c] = 1.0;
                        }
                        put_f32(
                            buf,
                            off + a * stride + c * UniformType::MATRIX_COLUMN_STRIDE as usize,
                            &col[..rows as usize],
                        );
                    }
                }
                continue;
            }
            UniformType::Other => continue,
            _ => {}
        }
        let mut v = match m.name.as_str() {
            "g_Time" => [time, 0.0, 0.0, 0.0],
            "g_Alpha" | "g_UserAlpha" | "g_Brightness" | "g_Color" | "g_Color4"
            | "g_CompositeColor" => [1.0; 4],
            "g_TextureReductionScale" => [1.0, 0.0, 0.0, 0.0],
            name => {
                if let Some(slot) = name
                    .strip_prefix("g_Texture")
                    .and_then(|r| r.strip_suffix("Resolution"))
                    .and_then(|n| n.parse::<u32>().ok())
                {
                    let (w, h) = dims(slot);
                    [w as f32, h as f32, w as f32, h as f32]
                } else if let Some(p) = defaults.iter().find(|p| p.uniform == name) {
                    default_values(p.default)
                } else {
                    [0.0; 4]
                }
            }
        };
        let comps = match m.ty {
            UniformType::Scalar(_) => 1,
            UniformType::Vector(_, n) => n as usize,
            _ => 1,
        };
        if matches!(
            m.ty,
            UniformType::Scalar(ScalarKind::Int)
                | UniformType::Scalar(ScalarKind::Uint)
                | UniformType::Scalar(ScalarKind::Bool)
        ) {
            if let Some(dst) = buf.get_mut(off..off + 4) {
                dst.copy_from_slice(&(v[0] as i32).to_le_bytes());
            }
            continue;
        }
        if matches!(m.ty, UniformType::Vector(k, _) if k != ScalarKind::Float) {
            v = [0.0; 4];
        }
        put_f32(buf, off, &v[..comps]);
    }
}

/// Texture size bound to a slot of a draw.
pub fn slot_dims(w: &Workload, d: &crate::plan::Draw, slot: u32) -> (u32, u32) {
    match d.slots.iter().find(|(s, _)| *s == slot) {
        Some((_, Src::Tex(i))) => (w.textures[*i].width, w.textures[*i].height),
        Some((_, Src::Target(i))) => (w.targets[*i].width, w.targets[*i].height),
        None => (1, 1),
    }
}

/// One vertex attribute of a program: location, components (1..=4), scalar kind.
#[derive(Clone, Debug)]
pub struct Attr {
    pub name: String,
    pub location: u32,
    pub comps: u32,
    pub kind: ScalarKind,
    pub offset: u32,
}

/// The attributes of a program, ordered by location, tightly interleaved. Returns them and the
/// vertex stride in bytes.
pub fn attributes(shader: &PassShader) -> (Vec<Attr>, u32) {
    let mut v: Vec<_> = shader.program.reflection.attributes.iter().collect();
    v.sort_by_key(|a| a.location);
    let mut offset = 0;
    let mut out = Vec::new();
    for a in v {
        let (comps, kind) = match a.ty {
            UniformType::Scalar(k) => (1, k),
            UniformType::Vector(k, n) => (u32::from(n), k),
            _ => (4, ScalarKind::Float),
        };
        out.push(Attr {
            name: a.name.clone(),
            location: a.location,
            comps,
            kind,
            offset,
        });
        offset += comps * 4;
    }
    (out, offset)
}

/// Four vertices (a triangle strip) of a quad covering `coverage` of the target, centred, for a
/// program's attributes: positions in clip space, texture coordinates over the whole texture.
pub fn quad(attrs: &[Attr], stride: u32, coverage: (f32, f32)) -> Vec<u8> {
    let corners: [(f32, f32); 4] = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)];
    let mut data = vec![0u8; stride as usize * 4];
    for (i, (cx, cy)) in corners.iter().enumerate() {
        let base = i * stride as usize;
        for a in attrs {
            if a.kind != ScalarKind::Float {
                continue;
            }
            let lower = a.name.to_ascii_lowercase();
            let v: [f32; 4] = if lower.contains("position") {
                [cx * coverage.0, cy * coverage.1, 0.0, 1.0]
            } else if lower.contains("texcoord") {
                [(cx + 1.0) * 0.5, (1.0 - cy) * 0.5, 0.0, 1.0]
            } else {
                [0.0; 4]
            };
            put_f32(&mut data, base + a.offset as usize, &v[..a.comps as usize]);
        }
    }
    data
}

/// Everything the report prints for one run.
#[derive(Default, Debug)]
pub struct Report {
    pub backend: String,
    pub adapter: String,
    pub wallpaper: String,
    /// Device + swap chain (Vulkan: instance, adapter, device, surface configuration).
    pub create_ms: f64,
    /// Textures created and uploaded, GPU idle afterwards.
    pub upload_ms: f64,
    /// Render targets created.
    pub targets_ms: f64,
    /// Shader objects (D3D11: from the bytecode cache, or D3DCompile on a miss; Vulkan: naga
    /// modules).
    pub shaders_ms: f64,
    /// `D3DCompile` when the cache missed (cold), else 0.
    pub compile_ms: f64,
    /// Pipeline / state objects.
    pub pipelines_ms: f64,
    pub frames: u64,
    pub late_frames: u64,
    pub cpu_percent: f64,
    pub gpu_3d_percent: f64,
    pub shared: u64,
    pub dedicated: u64,
    pub private: u64,
    pub working_set: u64,
    /// Shared / dedicated right after device + swap chain creation, before any resource.
    pub shared_device: u64,
    pub dedicated_device: u64,
    pub draws: usize,
    pub render_passes: usize,
}

pub fn mib(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

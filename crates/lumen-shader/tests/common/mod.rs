//! Checks of the lumen-render binding contract (crates/lumen-render/README.md,
//! "Integration contract" > "Shaders"). The local checks always run; with
//! `--features render-contract` the renderer's own reflection, clip-space fix
//! and validation run too, and the wgpu module is created when a GPU exists.

#![allow(dead_code)]

use lumen_shader::{FRAGMENT_ENTRY, PassShader, VERTEX_ENTRY};
use naga::{AddressSpace, Binding, ImageClass, ImageDimension, Module, ShaderStage, TypeInner};

/// Frame block members and offsets of the contract (`dynbuf::frame_globals`).
const FRAME_FIELDS: &[(&str, u32)] = &[
    ("g_Time", 0),
    ("g_Daytime", 4),
    ("g_PointerPosition", 8),
    ("g_PointerPositionLast", 16),
    ("g_TexelSize", 24),
    ("g_TexelSizeHalf", 32),
    ("g_AudioSpectrum16Left", 48),
    ("g_AudioSpectrum16Right", 112),
    ("g_AudioSpectrum32Left", 176),
    ("g_AudioSpectrum32Right", 304),
    ("g_AudioSpectrum64Left", 432),
    ("g_AudioSpectrum64Right", 688),
];
const FRAME_SIZE: u32 = 944;

fn struct_members(m: &Module, ty: naga::Handle<naga::Type>) -> Option<(&[naga::StructMember], u32)> {
    match &m.types[ty].inner {
        TypeInner::Struct { members, span } => Some((members, *span)),
        _ => None,
    }
}

pub fn check_render_contract(shader: &PassShader, label: &str) -> Result<(), String> {
    let m = &shader.program.module;
    let r = shader.reflection();
    // Entry points.
    let vep = m.entry_points.iter().find(|e| e.name == VERTEX_ENTRY && e.stage == ShaderStage::Vertex);
    let fep = m.entry_points.iter().find(|e| e.name == FRAGMENT_ENTRY && e.stage == ShaderStage::Fragment);
    let (Some(vep), Some(fep)) = (vep, fep) else { return Err("missing vs_main / fs_main".into()) };
    // Bound resources.
    let mut slots = 0u8;
    for (_, g) in m.global_variables.iter() {
        let Some(rb) = g.binding else { continue };
        match (rb.group, rb.binding, g.space) {
            (0, 0, AddressSpace::Uniform) => {
                let (members, span) = struct_members(m, g.ty).ok_or("pass block is not a struct")?;
                let b = r.uniform_block.as_ref().ok_or("pass block not reflected")?;
                if b.size != span || b.members.len() != members.len() {
                    return Err("pass block reflection differs from the module".into());
                }
                for (rm, mm) in b.members.iter().zip(members) {
                    if mm.name.as_deref() != Some(rm.name.as_str()) || mm.offset != rm.offset {
                        return Err(format!("pass member {} differs", rm.name));
                    }
                }
            }
            (0, 1, AddressSpace::Uniform) => {
                let (members, span) = struct_members(m, g.ty).ok_or("frame block is not a struct")?;
                if span != FRAME_SIZE {
                    return Err(format!("frame block size {span}"));
                }
                for mm in members {
                    let name = mm.name.as_deref().unwrap_or("");
                    if let Some(f) = FRAME_FIELDS.iter().find(|f| f.0 == name)
                        && f.1 != mm.offset
                    {
                        return Err(format!("frame member {name} at {} instead of {}", mm.offset, f.1));
                    }
                }
                for u in &r.frame_uniforms {
                    if !members.iter().any(|mm| mm.name.as_deref() == Some(u)) {
                        return Err(format!("frame uniform {u} not in the frame block"));
                    }
                }
            }
            (1, b, AddressSpace::Handle) if b < 16 => {
                let ok = if b % 2 == 0 {
                    matches!(
                        m.types[g.ty].inner,
                        TypeInner::Image {
                            dim: ImageDimension::D2 | ImageDimension::D3,
                            arrayed: false,
                            class: ImageClass::Sampled { kind: naga::ScalarKind::Float, multi: false }
                        }
                    )
                } else {
                    matches!(m.types[g.ty].inner, TypeInner::Sampler { comparison: false })
                };
                if !ok {
                    return Err(format!("@group(1) @binding({b}) has the wrong type"));
                }
                slots |= 1 << (b / 2);
            }
            (g, b, _) => return Err(format!("unexpected resource @group({g}) @binding({b})")),
        }
    }
    let reflected: u8 = r.textures.iter().fold(0, |acc, t| acc | 1 << t.unit);
    if reflected != slots {
        return Err(format!("slots {slots:#b} vs reflection {reflected:#b}"));
    }
    if r.uses_frame_block()
        != m.global_variables.iter().any(|(_, g)| g.binding.as_ref().is_some_and(|b| b.group == 0 && b.binding == 1))
    {
        return Err("frame block presence differs".into());
    }
    // Vertex inputs are found by name.
    for a in &vep.function.arguments {
        if let Some(Binding::Location { location, .. }) = a.binding {
            // Names match except for GLSL 4.50 keywords renamed on emission.
            let name = a.name.as_deref().unwrap_or("");
            if !r.attributes.iter().any(|x| x.location == location && (x.name == name || format!("{}_l", x.name) == name)) {
                return Err(format!("vertex input {name} at {location} not reflected"));
            }
        }
    }
    // One fragment output at location 0.
    let res = fep.function.result.as_ref().ok_or("fragment entry point without output")?;
    let locations: Vec<u32> = match &res.binding {
        Some(Binding::Location { location, .. }) => vec![*location],
        _ => struct_members(m, res.ty)
            .map(|(members, _)| {
                members
                    .iter()
                    .filter_map(|mm| match mm.binding {
                        Some(Binding::Location { location, .. }) => Some(location),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    if locations != [0] {
        return Err(format!("fragment outputs at {locations:?}"));
    }
    renderer::check(shader, label)
}

#[cfg(not(feature = "render-contract"))]
mod renderer {
    pub fn check(_: &lumen_shader::PassShader, _: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(feature = "render-contract")]
mod renderer {
    use std::sync::OnceLock;

    use lumen_render::shader::{ShaderDesc, apply_clip_space_fix, reflect, validate};
    use lumen_render::{ContextOptions, GpuContext};
    use lumen_shader::{FRAGMENT_ENTRY, PassShader, VERTEX_ENTRY};

    fn gpu() -> Option<&'static GpuContext> {
        static CTX: OnceLock<Option<GpuContext>> = OnceLock::new();
        CTX.get_or_init(|| match GpuContext::new_headless(&ContextOptions::default()) {
            Ok(c) => {
                eprintln!("render contract: wgpu modules are created on a real device");
                Some(c)
            }
            Err(e) => {
                eprintln!("render contract: no GPU adapter ({e}); wgpu module creation skipped");
                None
            }
        })
        .as_ref()
    }

    pub fn check(shader: &PassShader, label: &str) -> Result<(), String> {
        let m = &shader.program.module;
        let r = shader.reflection();
        let iface = reflect(m, VERTEX_ENTRY, label).map_err(|e| format!("renderer reflection: {e}"))?;
        if iface.uniform_size != r.uniform_block.as_ref().map(|b| b.size) {
            return Err("renderer sees another pass block size".into());
        }
        for mem in r.uniform_block.iter().flat_map(|b| &b.members) {
            match iface.field(&mem.name) {
                Some(f) if f.offset == mem.offset => {}
                _ => return Err(format!("renderer sees member {} elsewhere", mem.name)),
            }
        }
        if iface.uses_frame_globals != r.uses_frame_block() {
            return Err("renderer sees another frame block usage".into());
        }
        let mut fixed = m.clone();
        apply_clip_space_fix(&mut fixed, VERTEX_ENTRY).map_err(|e| format!("clip fix: {e}"))?;
        validate(&fixed, label).map_err(|e| format!("validation after clip fix: {e}"))?;
        if let Some(ctx) = gpu() {
            let desc = ShaderDesc {
                label: label.to_string(),
                module: m.clone(),
                vertex_entry: VERTEX_ENTRY.into(),
                fragment_entry: FRAGMENT_ENTRY.into(),
                upstream_clip_space: true,
            };
            lumen_render::shader::compile(ctx, desc).map_err(|e| format!("wgpu: {e}"))?;
        }
        Ok(())
    }
}

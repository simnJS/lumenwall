//! naga stage: parse the strict GLSL 4.50 of both stages, merge them into one
//! module (entry points `vs_main` / `fs_main`), validate, and read the uniform
//! block layout back from the module.

use naga::front::glsl::{Frontend, Options};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::{AddressSpace, Module, ShaderStage, TypeInner};
use serde::{Deserialize, Serialize};

use crate::ShaderError;
use crate::glsl::Translated;
use crate::glsl::link::{UStorage, UniformSlot};
use crate::glsl::types::{Scalar, Type};
use crate::reflect::*;

/// A validated program, ready for `wgpu::ShaderSource::Naga` (one module with
/// both entry points, binding contract of lumen-render).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompiledProgram {
    pub module: Module,
    pub reflection: ProgramReflection,
}

fn stage_name(stage: ShaderStage) -> &'static str {
    if stage == ShaderStage::Vertex { "vertex" } else { "fragment" }
}

fn parse_stage(src: &str, stage: ShaderStage, entry: &str) -> Result<Module, ShaderError> {
    let mut module = Frontend::default()
        .parse(&Options::from(stage), src)
        .map_err(|e| ShaderError::Backend { stage: stage_name(stage), message: e.emit_to_string(src) })?;
    for ep in module.entry_points.iter_mut() {
        ep.name = entry.to_string();
    }
    Ok(module)
}

fn validator() -> Validator {
    Validator::new(ValidationFlags::all(), Capabilities::default())
}

fn scalar_kind(s: Scalar) -> ScalarKind {
    match s {
        Scalar::Float => ScalarKind::Float,
        Scalar::Int => ScalarKind::Int,
        Scalar::Uint => ScalarKind::Uint,
        Scalar::Bool => ScalarKind::Bool,
    }
}

pub(crate) fn uniform_type(t: &Type) -> UniformType {
    match t {
        Type::Scalar(s) => UniformType::Scalar(scalar_kind(*s)),
        Type::Vector(s, n) => UniformType::Vector(scalar_kind(*s), *n),
        Type::Matrix(c, r) => UniformType::Matrix { columns: *c, rows: *r },
        Type::Array(e, _) => uniform_type(e),
        _ => UniformType::Other,
    }
}

fn stages(v: Option<u32>, f: Option<u32>) -> Stages {
    Stages { vertex: v.is_some(), fragment: f.is_some() }
}

/// Member offsets (and array strides) of the pass block.
fn block_layout(m: &Module) -> Option<(u32, Vec<(u32, u32)>)> {
    let gv = m.global_variables.iter().map(|(_, g)| g).find(|g| {
        g.space == AddressSpace::Uniform
            && g.binding.as_ref().is_some_and(|b| b.group == BIND_GROUP && b.binding == UNIFORM_BLOCK_BINDING)
    })?;
    let TypeInner::Struct { members, span } = &m.types[gv.ty].inner else {
        return None;
    };
    let v = members
        .iter()
        .map(|mem| {
            let stride = match m.types[mem.ty].inner {
                TypeInner::Array { stride, .. } => stride,
                _ => 0,
            };
            (mem.offset, stride)
        })
        .collect();
    Some((*span, v))
}

fn member(u: &UniformSlot, offset: u32, naga_stride: u32) -> UniformMember {
    let (array_len, array_stride) = match (&u.storage, &u.ty) {
        (UStorage::Packed(_, n), _) => (*n, 4),
        (_, Type::Array(_, n)) => (*n, naga_stride),
        _ => (0, 0),
    };
    UniformMember {
        name: u.name.clone(),
        ty: uniform_type(&u.ty),
        offset,
        array_len,
        array_stride,
        stages: stages(u.vertex, u.fragment),
    }
}

pub fn compile_translated(t: &Translated) -> Result<CompiledProgram, ShaderError> {
    let vertex = parse_stage(&t.vertex, ShaderStage::Vertex, VERTEX_ENTRY)?;
    let fragment = parse_stage(&t.fragment, ShaderStage::Fragment, FRAGMENT_ENTRY)?;
    let merged = crate::merge::merge(vertex.clone(), fragment.clone())
        .map_err(|message| ShaderError::Backend { stage: "merge", message })?;
    if let Err(e) = validator().validate(&merged) {
        // Locate the error in the stage sources when possible.
        for (m, src, stage) in [(&vertex, &t.vertex, "vertex"), (&fragment, &t.fragment, "fragment")] {
            if let Err(e) = validator().validate(m) {
                return Err(ShaderError::Backend { stage, message: e.emit_to_string(src) });
            }
        }
        return Err(ShaderError::Backend { stage: "merge", message: format!("{e:?}") });
    }
    let layout = &t.layout;
    let uniform_block = if layout.uniforms.is_empty() {
        None
    } else {
        let (size, offsets) = block_layout(&merged).ok_or_else(|| ShaderError::Backend {
            stage: "merge",
            message: "uniform block missing from the module".into(),
        })?;
        if offsets.len() != layout.uniforms.len() {
            return Err(ShaderError::Backend { stage: "merge", message: "uniform block member count mismatch".into() });
        }
        let members =
            layout.uniforms.iter().zip(offsets).map(|(u, (off, stride))| member(u, off, stride)).collect();
        Some(UniformBlock { group: BIND_GROUP, binding: UNIFORM_BLOCK_BINDING, size, members })
    };
    let mut textures: Vec<TextureBinding> = Vec::new();
    for s in &layout.samplers {
        let st = stages(s.vertex, s.fragment);
        match textures.iter_mut().find(|t| t.unit == s.unit) {
            Some(t) => {
                t.names.push(s.name.clone());
                t.stages.vertex |= st.vertex;
                t.stages.fragment |= st.fragment;
            }
            None => textures.push(TextureBinding {
                unit: s.unit,
                names: vec![s.name.clone()],
                group: TEXTURE_GROUP,
                texture_binding: 2 * s.unit,
                sampler_binding: 2 * s.unit + 1,
                stages: st,
            }),
        }
    }
    let attributes = layout
        .attributes
        .iter()
        .map(|a| VertexAttribute { name: a.name.clone(), location: a.location, ty: uniform_type(&a.ty) })
        .collect();
    let frame = layout.frame_uniforms.iter().map(|u| u.name.clone()).collect();
    Ok(CompiledProgram {
        module: merged,
        reflection: ProgramReflection::new(uniform_block, textures, attributes, frame),
    })
}

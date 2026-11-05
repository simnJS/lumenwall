//! naga module → HLSL for shader model 5.0 (what `D3DCompile` turns into Direct3D 11 bytecode).
//!
//! The module follows the binding contract of `lumenwall-render` (crate README, "Shaders"):
//!
//! | naga binding | HLSL register | Content |
//! |---|---|---|
//! | `@group(0) @binding(0)` | `b0` | pass uniform block (a window of the frame's constant arena) |
//! | `@group(0) @binding(1)` | `b1` | frame block |
//! | `@group(1) @binding(2N)` | `tN` | texture of slot N (`texture_2d` or `texture_3d`), N < 8 |
//! | `@group(1) @binding(2N + 1)` | `sN` | its sampler |
//!
//! Vertex inputs keep their location as semantic index: naga writes `LOC<n>`, the input layout
//! uses the semantic name [`INPUT_SEMANTIC`] and index `n`.
//!
//! naga 30's HLSL writer declares every sampler through a Direct3D 12 sampler heap (an array of
//! 2048 samplers in register space 0 and a `StructuredBuffer<uint>` of indices per group), which
//! shader model 5.0 cannot compile. [`plain_samplers`] rewrites those declarations into plain
//! `SamplerState name : register(sN);`.
//!
//! The clip-space correction of `lumenwall-render` (`(x, −y, (z + w)/2, w)` appended to the
//! vertex entry point) is a backend-neutral transform: Direct3D 11's clip space is wgpu's. It is
//! applied to the module before it reaches this function (see the crate README, "Seam").

use std::collections::BTreeMap;

use naga::back::hlsl;
use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::{AddressSpace, Binding, ImageDimension, Module, ScalarKind, ShaderStage, TypeInner};

use crate::error::{Error, Result};

/// Texture slots of the binding contract.
pub const MAX_SLOTS: u32 = 8;

/// Semantic name of every vertex input (`LOC0`, `LOC1`… in the HLSL naga writes).
pub const INPUT_SEMANTIC: &str = "LOC";

/// Version of what this module generates; part of every bytecode cache key, bumped whenever the
/// generated HLSL changes for the same module (options, rewrites).
pub const GENERATOR_VERSION: u32 = 1;

/// Where a resource of the binding contract goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Register {
    /// Constant buffer `bN`.
    ConstantBuffer(u32),
    /// Shader resource view `tN`.
    Texture(u32),
    /// Sampler `sN`.
    Sampler(u32),
}

/// The register of `@group(group) @binding(binding)` under the binding contract, `None` when
/// the pair is outside it.
#[must_use]
pub fn register_of(group: u32, binding: u32) -> Option<Register> {
    match (group, binding) {
        (0, 0 | 1) => Some(Register::ConstantBuffer(binding)),
        (1, b) if b < 2 * MAX_SLOTS => Some(if b % 2 == 0 {
            Register::Texture(b / 2)
        } else {
            Register::Sampler(b / 2)
        }),
        _ => None,
    }
}

/// Scalar type of a vertex input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputKind {
    Float,
    Sint,
    Uint,
}

/// One vertex input of the vertex entry point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VertexInput {
    pub location: u32,
    /// Argument or member name in the module (`a_Position`, `a_TexCoord`…).
    pub name: String,
    pub kind: InputKind,
    /// 1 to 4.
    pub components: u32,
}

/// Resources a program uses, as the draw code binds them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resources {
    /// Byte size of the pass uniform block (`b0`), if declared.
    pub uniform_block: Option<u32>,
    /// Byte size of the frame block (`b1`), if declared.
    pub frame_block: Option<u32>,
    /// Bit N: slot N declares a texture.
    pub textures: u8,
    /// Bit N: the texture of slot N is a `texture_3d` (volume).
    pub volumes: u8,
    /// Bit N: slot N declares a sampler.
    pub samplers: u8,
}

/// One program in HLSL, ready for `D3DCompile`.
#[derive(Clone, Debug)]
pub struct HlslProgram {
    pub source: String,
    /// Entry point names in `source` (naga may rename reserved words).
    pub vertex_entry: String,
    pub fragment_entry: String,
    /// Vertex inputs by location.
    pub vertex_inputs: Vec<VertexInput>,
    pub resources: Resources,
}

/// Translates a validated-or-not naga module with a vertex and a fragment entry point.
pub fn translate(module: &Module, vertex_entry: &str, fragment_entry: &str) -> Result<HlslProgram> {
    let info = Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(module)
        .map_err(|e| Error::Shader(format!("naga validation: {}", e.as_inner())))?;
    let resources = resources(module)?;
    let mut binding_map = BTreeMap::new();
    for (_, g) in module.global_variables.iter() {
        let Some(b) = g.binding else { continue };
        let register = match register_of(b.group, b.binding) {
            Some(Register::ConstantBuffer(n) | Register::Texture(n) | Register::Sampler(n)) => n,
            None => unreachable!("checked by `resources`"),
        };
        binding_map.insert(
            b,
            hlsl::BindTarget {
                space: 0,
                register,
                binding_array_size: None,
                dynamic_storage_buffer_offsets_index: None,
                restrict_indexing: false,
            },
        );
    }
    // Every resource of the module is in `binding_map` (`resources` refused the others): the
    // only binding naga fakes is the sampler index buffer, which `plain_samplers` removes.
    let options = hlsl::Options {
        shader_model: hlsl::ShaderModel::V5_0,
        binding_map,
        fake_missing_bindings: true,
        ..hlsl::Options::default()
    };
    let pipeline = hlsl::PipelineOptions::default();
    let mut source = String::new();
    let reflection = hlsl::Writer::new(&mut source, &options, &pipeline)
        .write(module, &info, None)
        .map_err(|e| Error::Shader(format!("naga HLSL writer: {e}")))?;
    let name_of = |wanted: &str, stage: ShaderStage| -> Result<String> {
        let (_, name) = module
            .entry_points
            .iter()
            .zip(&reflection.entry_point_names)
            .find(|(ep, _)| ep.name == wanted && ep.stage == stage)
            .ok_or_else(|| Error::Shader(format!("no {stage:?} entry point `{wanted}`")))?;
        name.clone()
            .map_err(|e| Error::Shader(format!("entry point `{wanted}`: {e}")))
    };
    Ok(HlslProgram {
        vertex_entry: name_of(vertex_entry, ShaderStage::Vertex)?,
        fragment_entry: name_of(fragment_entry, ShaderStage::Fragment)?,
        vertex_inputs: vertex_inputs(module, vertex_entry)?,
        resources,
        source: plain_samplers(&source)?,
    })
}

/// Resources of the module, checked against the binding contract.
pub fn resources(module: &Module) -> Result<Resources> {
    let mut r = Resources::default();
    for (_, g) in module.global_variables.iter() {
        let Some(b) = g.binding else { continue };
        let name = g.name.as_deref().unwrap_or("?");
        let inner = &module.types[g.ty].inner;
        let outside = || {
            Error::Shader(format!(
                "`{name}` at @group({}) @binding({}) is outside the binding contract",
                b.group, b.binding
            ))
        };
        match register_of(b.group, b.binding).ok_or_else(outside)? {
            Register::ConstantBuffer(n) => {
                if g.space != AddressSpace::Uniform {
                    return Err(outside());
                }
                let size = inner.size(module.to_ctx());
                if n == 0 {
                    r.uniform_block = Some(size);
                } else {
                    r.frame_block = Some(size);
                }
            }
            Register::Texture(n) => match *inner {
                TypeInner::Image {
                    dim: dim @ (ImageDimension::D2 | ImageDimension::D3),
                    arrayed: false,
                    ..
                } => {
                    r.textures |= 1 << n;
                    if dim == ImageDimension::D3 {
                        r.volumes |= 1 << n;
                    }
                }
                _ => return Err(outside()),
            },
            Register::Sampler(n) => match *inner {
                TypeInner::Sampler { .. } => r.samplers |= 1 << n,
                _ => return Err(outside()),
            },
        }
    }
    Ok(r)
}

/// Vertex inputs of `entry` (arguments and members of struct arguments with a location).
pub fn vertex_inputs(module: &Module, entry: &str) -> Result<Vec<VertexInput>> {
    let ep = module
        .entry_points
        .iter()
        .find(|ep| ep.name == entry && ep.stage == ShaderStage::Vertex)
        .ok_or_else(|| Error::Shader(format!("no vertex entry point `{entry}`")))?;
    let mut out = Vec::new();
    let mut push = |name: Option<&str>, ty: naga::Handle<naga::Type>, location: u32| -> Result<()> {
        let (scalar, components) = match module.types[ty].inner {
            TypeInner::Scalar(s) => (s, 1),
            TypeInner::Vector { size, scalar } => (scalar, size as u32),
            ref other => {
                return Err(Error::Shader(format!(
                    "vertex input at location {location} has type {other:?}"
                )));
            }
        };
        let kind = match scalar.kind {
            ScalarKind::Float => InputKind::Float,
            ScalarKind::Sint => InputKind::Sint,
            ScalarKind::Uint => InputKind::Uint,
            k => {
                return Err(Error::Shader(format!(
                    "vertex input at location {location} has scalar kind {k:?}"
                )));
            }
        };
        out.push(VertexInput {
            location,
            name: name.unwrap_or_default().to_owned(),
            kind,
            components,
        });
        Ok(())
    };
    for arg in &ep.function.arguments {
        match (&arg.binding, &module.types[arg.ty].inner) {
            (Some(Binding::Location { location, .. }), _) => {
                push(arg.name.as_deref(), arg.ty, *location)?;
            }
            (None, TypeInner::Struct { members, .. }) => {
                for m in members {
                    if let Some(Binding::Location { location, .. }) = m.binding {
                        push(m.name.as_deref(), m.ty, location)?;
                    }
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|i| i.location);
    Ok(out)
}

/// Rewrites naga's sampler heap declarations into plain shader model 5.0 samplers. A sampler the
/// rewrite cannot place (binding arrays of samplers, outside the contract) is an error rather
/// than a shader that fails later in `D3DCompile`.
pub fn plain_samplers(source: &str) -> Result<String> {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("SamplerState nagaSamplerHeap[")
            || trimmed.starts_with("SamplerComparisonState nagaComparisonSamplerHeap[")
            || (trimmed.starts_with("StructuredBuffer<uint> nagaGroup")
                && trimmed.contains("SamplerIndexArray"))
        {
            continue;
        }
        if let Some((ty, rest)) = trimmed
            .strip_prefix("static const SamplerState ")
            .map(|r| ("SamplerState", r))
            .or_else(|| {
                trimmed
                    .strip_prefix("static const SamplerComparisonState ")
                    .map(|r| ("SamplerComparisonState", r))
            })
        {
            // `name = nagaSamplerHeap[nagaGroup1SamplerIndexArray[3]];`
            let parsed = rest.split_once(" = ").and_then(|(name, init)| {
                let init = init.strip_suffix("]];")?;
                let (_, register) = init.rsplit_once('[')?;
                let register: u32 = register.parse().ok()?;
                (init.starts_with("nagaSamplerHeap[")
                    || init.starts_with("nagaComparisonSamplerHeap["))
                .then_some((name, register))
            });
            let (name, register) = parsed.ok_or_else(|| {
                Error::Shader(format!("unexpected sampler declaration: {trimmed}"))
            })?;
            out.push_str(&format!("{ty} {name} : register(s{register});\n"));
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.contains("SamplerHeap") || out.contains("SamplerIndexArray") {
        return Err(Error::Shader(
            "sampler heap left after the rewrite (sampler binding arrays are not supported)".into(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHADER: &str = r"
struct Pass { g_Color: vec4<f32>, g_Scale: vec2<f32>, g_Count: i32, }
struct Frame { g_Time: f32, }
@group(0) @binding(0) var<uniform> pass_block: Pass;
@group(0) @binding(1) var<uniform> frame: Frame;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0Sampler: sampler;
@group(1) @binding(6) var g_Texture3: texture_3d<f32>;
@group(1) @binding(7) var g_Texture3Sampler: sampler;
struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) tint: vec4<f32>, }
struct VsIn { @location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>, }
@vertex fn vs_main(v: VsIn, @location(4) a_BlendIndices: vec4<u32>) -> VsOut {
    var o: VsOut;
    o.pos = vec4<f32>(v.a_Position * vec3<f32>(pass_block.g_Scale, 1.0), 1.0);
    o.uv = v.a_TexCoord;
    o.tint = pass_block.g_Color * f32(a_BlendIndices.x) * frame.g_Time;
    return o;
}
@fragment fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
    let a = textureSample(g_Texture0, g_Texture0Sampler, i.uv);
    let b = textureSample(g_Texture3, g_Texture3Sampler, vec3<f32>(i.uv, 0.5));
    return a * b * i.tint;
}
";

    fn module(src: &str) -> Module {
        naga::front::wgsl::parse_str(src).expect("test WGSL")
    }

    #[test]
    fn registers_follow_the_binding_contract() {
        assert_eq!(register_of(0, 0), Some(Register::ConstantBuffer(0)));
        assert_eq!(register_of(0, 1), Some(Register::ConstantBuffer(1)));
        assert_eq!(register_of(1, 0), Some(Register::Texture(0)));
        assert_eq!(register_of(1, 1), Some(Register::Sampler(0)));
        assert_eq!(register_of(1, 14), Some(Register::Texture(7)));
        assert_eq!(register_of(1, 15), Some(Register::Sampler(7)));
        assert_eq!(register_of(1, 16), None);
        assert_eq!(register_of(0, 2), None);
        assert_eq!(register_of(2, 0), None);
    }

    #[test]
    fn translation_maps_every_resource_and_input() {
        let p = translate(&module(SHADER), "vs_main", "fs_main").unwrap();
        let s = &p.source;
        // naga's namer may add a trailing `_` to names ending in a digit.
        let declared = |prefix: &str, register: &str| {
            s.lines()
                .any(|l| l.starts_with(prefix) && l.ends_with(&format!(": register({register});")))
        };
        assert!(declared("cbuffer pass_block", "b0") || s.contains("register(b0)"), "{s}");
        assert!(s.contains("register(b1)"), "{s}");
        assert!(declared("Texture2D<float4> g_Texture0", "t0"), "{s}");
        assert!(declared("Texture3D<float4> g_Texture3", "t3"), "{s}");
        assert!(declared("SamplerState g_Texture0Sampler", "s0"), "{s}");
        assert!(declared("SamplerState g_Texture3Sampler", "s3"), "{s}");
        assert!(!s.contains("SamplerHeap") && !s.contains("space"), "{s}");
        assert!(s.contains(": LOC0") && s.contains(": LOC4"), "{s}");
        assert_eq!(
            p.vertex_inputs,
            vec![
                VertexInput {
                    location: 0,
                    name: "a_Position".into(),
                    kind: InputKind::Float,
                    components: 3
                },
                VertexInput {
                    location: 1,
                    name: "a_TexCoord".into(),
                    kind: InputKind::Float,
                    components: 2
                },
                VertexInput {
                    location: 4,
                    name: "a_BlendIndices".into(),
                    kind: InputKind::Uint,
                    components: 4
                },
            ]
        );
        assert_eq!(
            p.resources,
            Resources {
                uniform_block: Some(32),
                frame_block: Some(4),
                textures: 0b1001,
                volumes: 0b1000,
                samplers: 0b1001,
            }
        );
        assert!(!p.vertex_entry.is_empty() && !p.fragment_entry.is_empty());
    }

    #[test]
    fn bindings_outside_the_contract_are_refused() {
        let src = SHADER.replace("@group(1) @binding(6)", "@group(2) @binding(6)");
        let e = translate(&module(&src), "vs_main", "fs_main").unwrap_err();
        assert!(e.to_string().contains("outside the binding contract"), "{e}");
        let src = SHADER.replace("@group(1) @binding(0) var g_Texture0", "@group(1) @binding(1) var g_Texture0")
            .replace("@group(1) @binding(1) var g_Texture0Sampler", "@group(1) @binding(0) var g_Texture0Sampler");
        let e = translate(&module(&src), "vs_main", "fs_main").unwrap_err();
        assert!(e.to_string().contains("outside the binding contract"), "{e}");
    }

    #[test]
    fn missing_entry_point_is_an_error() {
        let e = translate(&module(SHADER), "nope", "fs_main").unwrap_err();
        assert!(e.to_string().contains("nope"), "{e}");
    }

    #[test]
    fn sampler_rewrite_handles_both_kinds_and_renamed_index_arrays() {
        let src = "SamplerState nagaSamplerHeap[2048]: register(s0, space0);
SamplerComparisonState nagaComparisonSamplerHeap[2048]: register(s0, space1);
StructuredBuffer<uint> nagaGroup1SamplerIndexArray_1 : register(t1, space255);
static const SamplerState s_a = nagaSamplerHeap[nagaGroup1SamplerIndexArray_1[5]];
static const SamplerComparisonState s_b = nagaComparisonSamplerHeap[nagaGroup1SamplerIndexArray_1[2]];
float4 f() { return 0; }
";
        let out = plain_samplers(src).unwrap();
        assert_eq!(
            out,
            "SamplerState s_a : register(s5);
SamplerComparisonState s_b : register(s2);
float4 f() { return 0; }
"
        );
        let bad = "float x = nagaSamplerHeap[i];\n";
        assert!(plain_samplers(bad).is_err());
    }
}

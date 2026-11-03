//! Pass shaders: the binding contract, reflection from the naga IR, the clip
//! space correction of spec 04 §24.2 and GPU module creation.
//!
//! # Binding contract (what a shader translator must emit)
//!
//! * `@group(0) @binding(0)`: optional `var<uniform>` struct with the pass
//!   uniforms (engine `g_*` uniforms and material constants). Member names are
//!   the WE uniform names; offsets come from the naga struct layout. It is
//!   bound with a dynamic offset.
//! * `@group(0) @binding(1)`: optional frame globals block, layout
//!   [`crate::dynbuf::FRAME_GLOBALS_WGSL`].
//! * `@group(1) @binding(2N)`: `texture_2d<f32>` of slot N (`g_TextureN`, N < 8);
//!   `@group(1) @binding(2N + 1)`: its filtering `sampler`.
//! * Vertex inputs by name: `a_Position` (3 floats) and `a_TexCoord` (2 floats)
//!   are fed; any other `@location` input receives (0, 0, 0, 1).
//! * The vertex position is in the **upstream** clip convention (GLSL); the
//!   correction `(x, −y, (z + w)/2, w)` is appended here, translators must not
//!   apply it themselves.
//! * One fragment output at `@location(0)`.

use std::borrow::Cow;

use naga::{
    AddressSpace, Binding, BuiltIn, Expression, Handle, Literal, Module, Scalar, Span, Statement,
    Type, TypeInner, VectorSize,
};

use crate::context::GpuContext;
use crate::dynbuf::{ScalarKind, UniformSlot, UniformType, frame_globals};
use crate::error::{RenderError, Result};

/// Number of addressable texture slots (`g_Texture0` … `g_Texture7`).
pub const MAX_SLOTS: usize = 8;

/// A vertex input of a pass shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VertexAttr {
    /// `a_Position`, fed with 3 floats.
    Position,
    /// `a_TexCoord`, fed with 2 floats.
    TexCoord,
    /// Declared but not fed: reads (0, 0, 0, 1).
    Unfed { components: u8, kind: ScalarKind },
}

/// `@location` of a vertex input and what feeds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexInput {
    pub location: u32,
    pub attr: VertexAttr,
}

/// One member of the pass uniform block (offset relative to the block).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniformField {
    pub name: String,
    pub offset: u32,
    pub ty: UniformType,
}

/// Interface of a pass shader, reflected from its naga module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShaderInterface {
    /// Byte size of the pass block, if any.
    pub uniform_size: Option<u32>,
    pub uniforms: Vec<UniformField>,
    pub uses_frame_globals: bool,
    /// Bit N set = slot N sampled.
    pub slots: u8,
    pub vertex_inputs: Vec<VertexInput>,
    /// `(location, name)` of every vertex input, for vertex streams that feed
    /// attributes by name (particles, see `graph::StreamLayout`).
    pub vertex_input_names: Vec<(u32, String)>,
}

impl ShaderInterface {
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&UniformField> {
        self.uniforms.iter().find(|f| f.name == name)
    }

    /// Name of the vertex input at `location`.
    #[must_use]
    pub fn vertex_input_name(&self, location: u32) -> Option<&str> {
        self.vertex_input_names
            .iter()
            .find(|(l, _)| *l == location)
            .map(|(_, n)| n.as_str())
    }

    #[must_use]
    pub fn uses_slot(&self, slot: usize) -> bool {
        slot < MAX_SLOTS && self.slots & (1 << slot) != 0
    }
}

fn shader_err(label: &str, reason: impl Into<String>) -> RenderError {
    RenderError::Shader {
        label: label.to_owned(),
        reason: reason.into(),
    }
}

fn scalar_kind(s: Scalar) -> Option<ScalarKind> {
    match s.kind {
        naga::ScalarKind::Float if s.width == 4 => Some(ScalarKind::Float),
        naga::ScalarKind::Sint if s.width == 4 => Some(ScalarKind::Sint),
        naga::ScalarKind::Uint if s.width == 4 => Some(ScalarKind::Uint),
        _ => None,
    }
}

fn uniform_type(module: &Module, ty: Handle<Type>) -> UniformType {
    let opaque = |module: &Module| UniformType::Opaque {
        size: module.types[ty].inner.size(module.to_ctx()),
    };
    match &module.types[ty].inner {
        TypeInner::Scalar(s) => scalar_kind(*s).map_or_else(|| opaque(module), UniformType::Scalar),
        TypeInner::Vector { size, scalar } => scalar_kind(*scalar)
            .map_or_else(|| opaque(module), |k| UniformType::Vector(k, *size as u8)),
        TypeInner::Matrix {
            columns,
            rows,
            scalar,
        } if scalar.width == 4 => UniformType::Matrix {
            columns: *columns as u8,
            rows: *rows as u8,
        },
        TypeInner::Array {
            base,
            size: naga::ArraySize::Constant(len),
            stride,
        } => UniformType::Array {
            element: Box::new(uniform_type(module, *base)),
            len: len.get(),
            stride: *stride,
        },
        _ => opaque(module),
    }
}

fn components_of(module: &Module, ty: Handle<Type>) -> Option<(u8, ScalarKind)> {
    match &module.types[ty].inner {
        TypeInner::Scalar(s) => Some((1, scalar_kind(*s)?)),
        TypeInner::Vector { size, scalar } => Some((*size as u8, scalar_kind(*scalar)?)),
        _ => None,
    }
}

/// Reflect the binding contract from a module.
pub fn reflect(module: &Module, vertex_entry: &str, label: &str) -> Result<ShaderInterface> {
    let mut out = ShaderInterface::default();
    for (_, gv) in module.global_variables.iter() {
        let Some(rb) = gv.binding else {
            continue;
        };
        match (rb.group, rb.binding, gv.space) {
            (0, 0, AddressSpace::Uniform) => {
                let TypeInner::Struct { members, span } = &module.types[gv.ty].inner else {
                    return Err(shader_err(
                        label,
                        "@group(0) @binding(0) must be a uniform struct",
                    ));
                };
                out.uniform_size = Some(*span);
                for m in members {
                    out.uniforms.push(UniformField {
                        name: m.name.clone().unwrap_or_default(),
                        offset: m.offset,
                        ty: uniform_type(module, m.ty),
                    });
                }
            }
            (0, 1, AddressSpace::Uniform) => {
                let size = module.types[gv.ty].inner.size(module.to_ctx());
                if size > frame_globals::SIZE {
                    return Err(shader_err(
                        label,
                        "frame globals block larger than the engine block",
                    ));
                }
                out.uses_frame_globals = true;
            }
            (1, b, AddressSpace::Handle) if (b as usize) < 2 * MAX_SLOTS => {
                let is_texture = matches!(module.types[gv.ty].inner, TypeInner::Image { .. });
                let is_sampler = matches!(
                    module.types[gv.ty].inner,
                    TypeInner::Sampler { comparison: false }
                );
                if (b % 2 == 0 && !is_texture) || (b % 2 == 1 && !is_sampler) {
                    return Err(shader_err(
                        label,
                        format!("@group(1) @binding({b}) has the wrong resource type"),
                    ));
                }
                out.slots |= 1 << (b / 2);
            }
            (g, b, _) => {
                return Err(shader_err(
                    label,
                    format!("unexpected resource @group({g}) @binding({b})"),
                ));
            }
        }
    }

    let ep = module
        .entry_points
        .iter()
        .find(|e| e.name == vertex_entry && e.stage == naga::ShaderStage::Vertex)
        .ok_or_else(|| shader_err(label, format!("no vertex entry point `{vertex_entry}`")))?;
    let mut push =
        |name: Option<&String>, ty: Handle<Type>, binding: Option<&Binding>| -> Result<()> {
            let Some(Binding::Location { location, .. }) = binding else {
                return Ok(());
            };
            let attr = match name.map(String::as_str) {
                Some("a_Position") => VertexAttr::Position,
                Some("a_TexCoord") => VertexAttr::TexCoord,
                _ => {
                    let (components, kind) = components_of(module, ty).ok_or_else(|| {
                        shader_err(
                            label,
                            format!("unsupported vertex input at location {location}"),
                        )
                    })?;
                    VertexAttr::Unfed { components, kind }
                }
            };
            out.vertex_inputs.push(VertexInput {
                location: *location,
                attr,
            });
            if let Some(n) = name {
                out.vertex_input_names.push((*location, n.clone()));
            }
            Ok(())
        };
    for arg in &ep.function.arguments {
        if let TypeInner::Struct { members, .. } = &module.types[arg.ty].inner {
            for m in members {
                push(m.name.as_ref(), m.ty, m.binding.as_ref())?;
            }
        } else {
            push(arg.name.as_ref(), arg.ty, arg.binding.as_ref())?;
        }
    }
    out.vertex_inputs.sort_by_key(|v| v.location);
    Ok(out)
}

/// Append `(x, y, z, w) → (x, −y, (z + w)/2, w)` to every `return` of the
/// vertex entry point (spec 04 §24.2), working on the naga IR.
pub fn apply_clip_space_fix(module: &mut Module, vertex_entry: &str) -> Result<()> {
    let label = vertex_entry;
    let vec4 = module.types.insert(
        Type {
            name: None,
            inner: TypeInner::Vector {
                size: VectorSize::Quad,
                scalar: Scalar::F32,
            },
        },
        Span::UNDEFINED,
    );
    let ep = module
        .entry_points
        .iter_mut()
        .find(|e| e.name == vertex_entry && e.stage == naga::ShaderStage::Vertex)
        .ok_or_else(|| shader_err(label, "no such vertex entry point"))?;
    let result = ep
        .function
        .result
        .as_ref()
        .ok_or_else(|| shader_err(label, "vertex entry point without result"))?;
    let is_position =
        |b: Option<&Binding>| matches!(b, Some(Binding::BuiltIn(BuiltIn::Position { .. })));
    let target = if is_position(result.binding.as_ref()) {
        PositionAt::Whole
    } else if let TypeInner::Struct { members, .. } = &module.types[result.ty].inner {
        let idx = members
            .iter()
            .position(|m| is_position(m.binding.as_ref()))
            .ok_or_else(|| shader_err(label, "no @builtin(position) output"))?;
        PositionAt::Member {
            index: idx as u32,
            count: members.len() as u32,
            ty: result.ty,
        }
    } else {
        return Err(shader_err(label, "no @builtin(position) output"));
    };
    let func = &mut ep.function;
    let body = std::mem::take(&mut func.body);
    func.body = rewrite_block(body, &mut func.expressions, target, vec4);
    Ok(())
}

#[derive(Clone, Copy)]
enum PositionAt {
    Whole,
    Member {
        index: u32,
        count: u32,
        ty: Handle<Type>,
    },
}

fn rewrite_block(
    block: naga::Block,
    exprs: &mut naga::Arena<Expression>,
    target: PositionAt,
    vec4: Handle<Type>,
) -> naga::Block {
    let mut out = naga::Block::with_capacity(block.len());
    for (stmt, span) in block.span_into_iter() {
        match stmt {
            Statement::Return { value: Some(v) } => {
                // Literals are pre-emitted; everything else must be in the Emit range.
                let half = exprs.append(Expression::Literal(Literal::F32(0.5)), span);
                let start = exprs.len();
                let pos = match target {
                    PositionAt::Whole => v,
                    PositionAt::Member { index, .. } => {
                        exprs.append(Expression::AccessIndex { base: v, index }, span)
                    }
                };
                let c: [Handle<Expression>; 4] = std::array::from_fn(|i| {
                    exprs.append(
                        Expression::AccessIndex {
                            base: pos,
                            index: i as u32,
                        },
                        span,
                    )
                });
                let ny = exprs.append(
                    Expression::Unary {
                        op: naga::UnaryOperator::Negate,
                        expr: c[1],
                    },
                    span,
                );
                let zw = exprs.append(
                    Expression::Binary {
                        op: naga::BinaryOperator::Add,
                        left: c[2],
                        right: c[3],
                    },
                    span,
                );
                let nz = exprs.append(
                    Expression::Binary {
                        op: naga::BinaryOperator::Multiply,
                        left: zw,
                        right: half,
                    },
                    span,
                );
                let fixed = exprs.append(
                    Expression::Compose {
                        ty: vec4,
                        components: vec![c[0], ny, nz, c[3]],
                    },
                    span,
                );
                let value = match target {
                    PositionAt::Whole => fixed,
                    PositionAt::Member { index, count, ty } => {
                        let components = (0..count)
                            .map(|i| {
                                if i == index {
                                    fixed
                                } else {
                                    exprs
                                        .append(Expression::AccessIndex { base: v, index: i }, span)
                                }
                            })
                            .collect();
                        exprs.append(Expression::Compose { ty, components }, span)
                    }
                };
                out.push(Statement::Emit(exprs.range_from(start)), span);
                out.push(Statement::Return { value: Some(value) }, span);
            }
            Statement::Block(b) => out.push(
                Statement::Block(rewrite_block(b, exprs, target, vec4)),
                span,
            ),
            Statement::If {
                condition,
                accept,
                reject,
            } => out.push(
                Statement::If {
                    condition,
                    accept: rewrite_block(accept, exprs, target, vec4),
                    reject: rewrite_block(reject, exprs, target, vec4),
                },
                span,
            ),
            Statement::Switch { selector, cases } => {
                let cases = cases
                    .into_iter()
                    .map(|mut c| {
                        c.body = rewrite_block(c.body, exprs, target, vec4);
                        c
                    })
                    .collect();
                out.push(Statement::Switch { selector, cases }, span);
            }
            Statement::Loop {
                body,
                continuing,
                break_if,
            } => out.push(
                Statement::Loop {
                    body: rewrite_block(body, exprs, target, vec4),
                    continuing,
                    break_if,
                },
                span,
            ),
            other => out.push(other, span),
        }
    }
    out
}

/// Validate a module with naga (structural errors are reported here instead
/// of reaching the device).
pub fn validate(module: &Module, label: &str) -> Result<naga::valid::ModuleInfo> {
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| shader_err(label, format!("{e:?}")))
}

/// Render-ready shader of one pass.
#[derive(Clone, Debug)]
pub struct ShaderDesc {
    pub label: String,
    pub module: Module,
    pub vertex_entry: String,
    pub fragment_entry: String,
    /// The vertex stage outputs upstream clip coordinates (every WE shader):
    /// apply the wgpu correction. `false` only for shaders already written
    /// for wgpu.
    pub upstream_clip_space: bool,
}

impl ShaderDesc {
    /// Parse WGSL written in the upstream clip convention (internal shaders, tests).
    pub fn from_wgsl(
        label: &str,
        source: &str,
        vertex_entry: &str,
        fragment_entry: &str,
    ) -> Result<Self> {
        let module = naga::front::wgsl::parse_str(source)
            .map_err(|e| shader_err(label, e.emit_to_string(source)))?;
        Ok(Self {
            label: label.to_owned(),
            module,
            vertex_entry: vertex_entry.to_owned(),
            fragment_entry: fragment_entry.to_owned(),
            upstream_clip_space: true,
        })
    }
}

/// A shader module on the GPU with its reflected interface.
#[derive(Debug)]
pub struct PassShader {
    pub(crate) module: wgpu::ShaderModule,
    pub(crate) vertex_entry: String,
    pub(crate) fragment_entry: String,
    pub(crate) interface: ShaderInterface,
    pub(crate) label: String,
}

impl PassShader {
    #[must_use]
    pub fn interface(&self) -> &ShaderInterface {
        &self.interface
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Reflect, fix, validate and create the GPU module.
pub fn compile(ctx: &GpuContext, mut desc: ShaderDesc) -> Result<PassShader> {
    let interface = reflect(&desc.module, &desc.vertex_entry, &desc.label)?;
    if !desc
        .module
        .entry_points
        .iter()
        .any(|e| e.name == desc.fragment_entry && e.stage == naga::ShaderStage::Fragment)
    {
        return Err(shader_err(
            &desc.label,
            format!("no fragment entry point `{}`", desc.fragment_entry),
        ));
    }
    if desc.upstream_clip_space {
        apply_clip_space_fix(&mut desc.module, &desc.vertex_entry)?;
    }
    validate(&desc.module, &desc.label)?;
    let scope = ctx.device().push_error_scope(wgpu::ErrorFilter::Validation);
    let module = ctx
        .device()
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&desc.label),
            source: wgpu::ShaderSource::Naga(Cow::Owned(desc.module)),
        });
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(RenderError::Validation {
            label: desc.label,
            message: e.to_string(),
        });
    }
    ctx.note_resources(1);
    Ok(PassShader {
        module,
        vertex_entry: desc.vertex_entry,
        fragment_entry: desc.fragment_entry,
        interface,
        label: desc.label,
    })
}

impl UniformField {
    /// Write handle for a block placed at `block_offset`.
    #[must_use]
    pub fn slot(&self, block_offset: u32) -> UniformSlot {
        UniformSlot {
            offset: block_offset + self.offset,
            ty: self.ty.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r"
struct Pass { g_ModelViewProjectionMatrix: mat4x4<f32>, g_Time: f32, g_Tint: vec3<f32>, g_Arr: array<vec4<f32>, 4> }
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
@group(1) @binding(4) var g_Texture2: texture_2d<f32>;
@group(1) @binding(5) var g_Texture2_s: sampler;
struct VOut { @location(0) uv: vec2<f32>, @builtin(position) pos: vec4<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>, @location(3) a_Extra: vec4<f32>) -> VOut {
    var o: VOut;
    o.uv = a_TexCoord + a_Extra.xy;
    if (u.g_Time > 100.0) {
        o.pos = vec4<f32>(0.0);
        return o;
    }
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv) + textureSample(g_Texture2, g_Texture2_s, i.uv);
}
";

    #[test]
    fn reflection() {
        let m = naga::front::wgsl::parse_str(SRC).unwrap();
        let i = reflect(&m, "vs", "t").unwrap();
        assert_eq!(i.slots, 0b101);
        assert!(i.uses_slot(0) && !i.uses_slot(1) && i.uses_slot(2));
        assert!(!i.uses_frame_globals);
        assert_eq!(i.field("g_Time").unwrap().offset, 64);
        assert_eq!(
            i.field("g_Time").unwrap().ty,
            UniformType::Scalar(ScalarKind::Float)
        );
        assert_eq!(i.field("g_Tint").unwrap().offset, 80);
        assert_eq!(
            i.field("g_ModelViewProjectionMatrix").unwrap().ty,
            UniformType::Matrix {
                columns: 4,
                rows: 4
            }
        );
        assert_eq!(i.field("g_Arr").unwrap().ty.size(), 64);
        assert_eq!(
            i.vertex_inputs,
            vec![
                VertexInput {
                    location: 0,
                    attr: VertexAttr::Position
                },
                VertexInput {
                    location: 1,
                    attr: VertexAttr::TexCoord
                },
                VertexInput {
                    location: 3,
                    attr: VertexAttr::Unfed {
                        components: 4,
                        kind: ScalarKind::Float
                    }
                },
            ]
        );
    }

    #[test]
    fn clip_fix_rewrites_every_return_and_validates() {
        let mut m = naga::front::wgsl::parse_str(SRC).unwrap();
        apply_clip_space_fix(&mut m, "vs").unwrap();
        validate(&m, "t").unwrap();
        let ep = m.entry_points.iter().find(|e| e.name == "vs").unwrap();
        let negates = ep
            .function
            .expressions
            .iter()
            .filter(|(_, e)| {
                matches!(
                    e,
                    Expression::Unary {
                        op: naga::UnaryOperator::Negate,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(negates, 2, "both returns are rewritten");

        // Direct @builtin(position) result.
        let src = r"@vertex fn v(@location(0) a_Position: vec3<f32>) -> @builtin(position) vec4<f32> { return vec4<f32>(a_Position, 1.0); }
@fragment fn f() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }";
        let mut m = naga::front::wgsl::parse_str(src).unwrap();
        apply_clip_space_fix(&mut m, "v").unwrap();
        validate(&m, "t").unwrap();
        assert!(apply_clip_space_fix(&mut m, "nope").is_err());
    }

    #[test]
    fn contract_violations_are_reported() {
        let src = r"@group(2) @binding(0) var t: texture_2d<f32>;
@vertex fn v() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }";
        let m = naga::front::wgsl::parse_str(src).unwrap();
        assert!(reflect(&m, "v", "t").is_err());
        let src = r"@group(1) @binding(1) var t: texture_2d<f32>;
@vertex fn v() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }";
        let m = naga::front::wgsl::parse_str(src).unwrap();
        assert!(reflect(&m, "v", "t").is_err(), "odd bindings are samplers");
    }
}

//! Inter-stage linking: varying matching and widening (R8), the shared
//! uniform block layout, texture/sampler bindings and attribute locations.

use std::collections::HashMap;

use super::ast::*;
use super::typer::{GStorage, Stage, TypedUnit};
use super::types::{Interner, SamplerKind, Scalar, Type, basic_type_name};

#[derive(Debug, Clone)]
pub struct LinkError(pub String);

/// How a uniform is stored in the block.
#[derive(Clone, Debug, PartialEq)]
pub enum UStorage {
    /// Stored with its logical type.
    Plain,
    /// `bool`/`bvecN` stored as `uint`/`uvecN` (bool is not host-shareable).
    BoolAsUint,
    /// `matCx2` stored as `vec4[C]` (naga rejects two-row matrices in std140).
    Mat2Rows(u8),
    /// `float[N]` / `int[N]` / `uint[N]` stored tightly as `vec4[ceil(N/4)]`.
    Packed(Scalar, u32),
}

#[derive(Clone, Debug)]
pub struct UniformSlot {
    pub name: String,
    /// Logical (widest) type.
    pub ty: Type,
    pub storage: UStorage,
    pub vertex: Option<u32>,
    pub fragment: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct SamplerSlot {
    pub name: String,
    pub ty: Type,
    /// Texture unit (spec 03 §8.1): N for `g_Texture0`…`g_Texture7`, else 0.
    pub unit: u32,
    pub vertex: Option<u32>,
    pub fragment: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct VaryingSlot {
    pub name: String,
    pub ty: Type,
    pub location: u32,
    pub interp: Interp,
    pub vertex: Option<u32>,
    pub fragment: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct AttributeSlot {
    pub name: String,
    pub ty: Type,
    pub location: u32,
    pub global: u32,
}

#[derive(Clone, Debug)]
pub struct OutputSlot {
    pub ty: Type,
    pub location: u32,
    pub global: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ProgramLayout {
    /// Members of the pass block (`@group(0) @binding(0)`), in block order.
    pub uniforms: Vec<UniformSlot>,
    /// Engine globals served by the frame block (`@group(0) @binding(1)`).
    pub frame_uniforms: Vec<UniformSlot>,
    pub samplers: Vec<SamplerSlot>,
    pub varyings: Vec<VaryingSlot>,
    pub attributes: Vec<AttributeSlot>,
    pub frag_outputs: Vec<OutputSlot>,
}

pub const UNIFORM_BINDING: u32 = 0;
pub const FRAME_BINDING: u32 = 1;
/// Bind group of the textures: slot N uses bindings 2N (texture) and 2N + 1 (sampler).
pub const TEXTURE_GROUP: u32 = 1;

/// Texture unit of a sampler name (spec 03 §8.1).
pub fn texture_unit(name: &str) -> u32 {
    match name.strip_prefix("g_Texture") {
        Some(d) if d.len() == 1 && matches!(d.as_bytes()[0], b'0'..=b'7') => (d.as_bytes()[0] - b'0') as u32,
        _ => 0,
    }
}

/// True if the uniform can live in the renderer's frame block with its declared type.
fn frame_candidate(u: &UniformSlot) -> bool {
    match u.name.as_str() {
        "g_Time" | "g_Daytime" => u.storage == UStorage::Plain && u.ty == Type::FLOAT,
        "g_PointerPosition" | "g_PointerPositionLast" | "g_TexelSize" | "g_TexelSizeHalf" => {
            u.storage == UStorage::Plain && u.ty == Type::Vector(Scalar::Float, 2)
        }
        "g_AudioSpectrum16Left" | "g_AudioSpectrum16Right" => u.storage == UStorage::Packed(Scalar::Float, 16),
        "g_AudioSpectrum32Left" | "g_AudioSpectrum32Right" => u.storage == UStorage::Packed(Scalar::Float, 32),
        "g_AudioSpectrum64Left" | "g_AudioSpectrum64Right" => u.storage == UStorage::Packed(Scalar::Float, 64),
        _ => false,
    }
}

fn widen(a: &Type, b: &Type) -> Option<Type> {
    if a == b {
        return Some(a.clone());
    }
    match (a.scalar(), b.scalar(), a.vec_size(), b.vec_size()) {
        (Some(x), Some(y), Some(n), Some(m)) if x == y => Some(Type::vec(x, n.max(m))),
        _ => None,
    }
}

/// Visits every global reference in reachable code; `indexed` is true when the
/// reference is directly the base of an index expression.
fn scan_globals(tu: &TypedUnit, f: &mut dyn FnMut(u32, bool)) {
    fn expr(e: &Expr, indexed: bool, f: &mut dyn FnMut(u32, bool)) {
        match &e.kind {
            ExprKind::Ident(_, Sym::Global(g)) => f(*g, indexed),
            ExprKind::Index(b, i) => {
                expr(b, true, f);
                expr(i, false, f);
            }
            ExprKind::Unary(_, a)
            | ExprKind::Field(a, _)
            | ExprKind::Swizzle(a, _)
            | ExprKind::Length(a)
            | ExprKind::Convert(a) => expr(a, false, f),
            ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) | ExprKind::Comma(a, b) => {
                expr(a, false, f);
                expr(b, false, f);
            }
            ExprKind::Ternary(a, b, c) => {
                expr(a, false, f);
                expr(b, false, f);
                expr(c, false, f);
            }
            ExprKind::Call { args, .. } => args.iter().for_each(|a| expr(a, false, f)),
            _ => {}
        }
    }
    fn stmt(s: &Stmt, f: &mut dyn FnMut(u32, bool)) {
        match s {
            Stmt::Decl(d) => d.vars.iter().filter_map(|v| v.init.as_ref()).for_each(|e| expr(e, false, f)),
            Stmt::Expr(e) | Stmt::Case(e) => expr(e, false, f),
            Stmt::Block(v) | Stmt::Switch(_, v) => {
                if let Stmt::Switch(c, _) = s {
                    expr(c, false, f);
                }
                v.iter().for_each(|s| stmt(s, f));
            }
            Stmt::If(c, a, b) => {
                expr(c, false, f);
                stmt(a, f);
                if let Some(b) = b {
                    stmt(b, f);
                }
            }
            Stmt::While(c, b) | Stmt::DoWhile(b, c) => {
                expr(c, false, f);
                stmt(b, f);
            }
            Stmt::For { init, cond, step, body } => {
                if let Some(i) = init {
                    stmt(i, f);
                }
                if let Some(c) = cond {
                    expr(c, false, f);
                }
                if let Some(s) = step {
                    expr(s, false, f);
                }
                stmt(body, f);
            }
            Stmt::Return(Some(e), _) => expr(e, false, f),
            _ => {}
        }
    }
    for item in &tu.unit.items {
        match item {
            Item::Decl(d) => d.vars.iter().filter_map(|v| v.init.as_ref()).for_each(|e| expr(e, false, f)),
            Item::Function(fd) if !fd.skip && tu.reachable[fd.fn_index as usize] => {
                if let Some(b) = &fd.body {
                    b.iter().for_each(|s| stmt(s, f));
                }
            }
            _ => {}
        }
    }
}

pub fn link(vs: &TypedUnit, fs: &TypedUnit, int: &Interner, frame_globals: bool) -> Result<ProgramLayout, LinkError> {
    let mut layout = ProgramLayout::default();
    let tn = |t: &Type| basic_type_name(t);

    // Varyings (R8).
    let mut loc = 0u32;
    let mut fs_inputs: HashMap<&str, u32> = HashMap::new();
    for (g, gi) in fs.globals.iter().enumerate() {
        if gi.storage == GStorage::In && fs.used_globals[g] {
            fs_inputs.insert(int.get(gi.name), g as u32);
        }
    }
    let mut matched: Vec<u32> = Vec::new();
    for (g, gi) in vs.globals.iter().enumerate() {
        if gi.storage != GStorage::Out {
            continue;
        }
        let name = int.get(gi.name);
        let fsg = fs_inputs.get(name).copied();
        let ty = match fsg {
            Some(f) => {
                let ft = &fs.globals[f as usize].ty;
                matched.push(f);
                widen(&gi.ty, ft).ok_or_else(|| {
                    LinkError(format!(
                        "varying '{name}' has incompatible types between stages ({} vs {})",
                        tn(&gi.ty),
                        tn(ft)
                    ))
                })?
            }
            None => gi.ty.clone(),
        };
        let mut interp = fsg.map_or(gi.interp, |f| fs.globals[f as usize].interp);
        if interp == Interp::Default {
            interp = gi.interp;
        }
        if ty.scalar() == Some(Scalar::Bool) {
            return Err(LinkError(format!("boolean varying '{name}'")));
        }
        if matches!(ty.scalar(), Some(Scalar::Int | Scalar::Uint)) {
            interp = Interp::Flat;
        }
        layout.varyings.push(VaryingSlot {
            name: name.to_string(),
            ty: ty.clone(),
            location: loc,
            interp,
            vertex: Some(g as u32),
            fragment: fsg,
        });
        loc += ty.location_count();
    }
    let mut missing: Vec<(&str, u32)> = fs_inputs.iter().filter(|(_, g)| !matched.contains(g)).map(|(n, g)| (*n, *g)).collect();
    missing.sort_by_key(|m| m.1);
    for (name, g) in missing {
        let gi = &fs.globals[g as usize];
        let mut interp = gi.interp;
        if matches!(gi.ty.scalar(), Some(Scalar::Int | Scalar::Uint)) {
            interp = Interp::Flat;
        }
        layout.varyings.push(VaryingSlot {
            name: name.to_string(),
            ty: gi.ty.clone(),
            location: loc,
            interp,
            vertex: None,
            fragment: Some(g),
        });
        loc += gi.ty.location_count();
    }

    // Attributes.
    let mut aloc = 0;
    for (g, gi) in vs.globals.iter().enumerate() {
        if gi.storage == GStorage::In && vs.used_globals[g] {
            layout.attributes.push(AttributeSlot {
                name: int.get(gi.name).to_string(),
                ty: gi.ty.clone(),
                location: aloc,
                global: g as u32,
            });
            aloc += gi.ty.location_count();
        }
    }

    // Fragment outputs: `out_FragColor` first.
    let mut outs: Vec<u32> =
        (0..fs.globals.len() as u32).filter(|&g| fs.globals[g as usize].storage == GStorage::Out).collect();
    outs.sort_by_key(|&g| int.get(fs.globals[g as usize].name) != "out_FragColor");
    if outs.len() > 1 {
        return Err(LinkError("only one fragment output (`gl_FragColor`) is supported".into()));
    }
    for g in outs {
        let t = fs.globals[g as usize].ty.clone();
        layout.frag_outputs.push(OutputSlot { ty: t, location: 0, global: g });
    }

    // Uniforms and samplers, shared by both stages.
    let mut uni_index: HashMap<String, usize> = HashMap::new();
    let mut samp_index: HashMap<String, usize> = HashMap::new();
    let mut whole_use: HashMap<String, bool> = HashMap::new();
    for (stage, tu) in [(Stage::Vertex, vs), (Stage::Fragment, fs)] {
        let mut whole = vec![false; tu.globals.len()];
        scan_globals(tu, &mut |g, indexed| {
            if !indexed {
                whole[g as usize] = true;
            }
        });
        for (g, gi) in tu.globals.iter().enumerate() {
            if gi.storage != GStorage::Uniform || !tu.used_globals[g] {
                continue;
            }
            let name = int.get(gi.name).to_string();
            let set = |v: &mut Option<u32>| *v = Some(g as u32);
            if gi.ty.is_sampler() {
                match samp_index.get(&name) {
                    Some(&i) => {
                        let s = &mut layout.samplers[i];
                        if s.ty != gi.ty {
                            return Err(LinkError(format!("sampler '{name}' has different types between stages")));
                        }
                        if stage == Stage::Vertex { set(&mut s.vertex) } else { set(&mut s.fragment) }
                    }
                    None => {
                        samp_index.insert(name.clone(), layout.samplers.len());
                        if gi.ty != Type::Sampler(SamplerKind::S2D) {
                            return Err(LinkError(format!(
                                "sampler '{name}': only 2D textures are bound (spec 03 §8.1)"
                            )));
                        }
                        let mut s = SamplerSlot {
                            unit: texture_unit(&name),
                            name,
                            ty: gi.ty.clone(),
                            vertex: None,
                            fragment: None,
                        };
                        if stage == Stage::Vertex { set(&mut s.vertex) } else { set(&mut s.fragment) }
                        layout.samplers.push(s);
                    }
                }
                continue;
            }
            if matches!(gi.ty, Type::Struct(_)) || matches!(&gi.ty, Type::Array(e, _) if matches!(**e, Type::Struct(_) | Type::Array(..))) {
                return Err(LinkError(format!("uniform '{name}' of struct type is not supported")));
            }
            *whole_use.entry(name.clone()).or_default() |= whole[g];
            match uni_index.get(&name) {
                Some(&i) => {
                    let u = &mut layout.uniforms[i];
                    // R8 also applies to uniforms declared with different vector sizes.
                    u.ty = widen(&u.ty, &gi.ty).ok_or_else(|| {
                        LinkError(format!(
                            "uniform '{name}' has incompatible types between stages ({} vs {})",
                            tn(&u.ty),
                            tn(&gi.ty)
                        ))
                    })?;
                    if stage == Stage::Vertex { set(&mut u.vertex) } else { set(&mut u.fragment) }
                }
                None => {
                    uni_index.insert(name.clone(), layout.uniforms.len());
                    let mut u = UniformSlot { name, ty: gi.ty.clone(), storage: UStorage::Plain, vertex: None, fragment: None };
                    if stage == Stage::Vertex { set(&mut u.vertex) } else { set(&mut u.fragment) }
                    layout.uniforms.push(u);
                }
            }
        }
    }
    for u in layout.uniforms.iter_mut() {
        u.storage = match &u.ty {
            t if t.scalar() == Some(Scalar::Bool) && t.is_scalar_or_vector() => UStorage::BoolAsUint,
            Type::Matrix(c, 2) => UStorage::Mat2Rows(*c),
            Type::Array(e, n) if !whole_use.get(&u.name).copied().unwrap_or(true) => match **e {
                Type::Scalar(s @ (Scalar::Float | Scalar::Int | Scalar::Uint)) => UStorage::Packed(s, *n),
                Type::Scalar(Scalar::Bool) => return Err(LinkError(format!("bool array uniform '{}'", u.name))),
                _ => UStorage::Plain,
            },
            Type::Array(e, _) if matches!(**e, Type::Matrix(_, 2)) || e.scalar() == Some(Scalar::Bool) => {
                return Err(LinkError(format!("unsupported array uniform '{}'", u.name)));
            }
            _ => UStorage::Plain,
        };
    }
    if frame_globals {
        let (frame, pass): (Vec<_>, Vec<_>) = std::mem::take(&mut layout.uniforms).into_iter().partition(frame_candidate);
        layout.uniforms = pass;
        layout.frame_uniforms = frame;
    }
    order_uniforms(&mut layout.uniforms);
    layout.samplers.sort_by_key(|s| s.unit);
    Ok(layout)
}

/// std140 size class of a stored member: 16 for anything 16-aligned with a size
/// multiple of 16, 12 for vec3, 8 for vec2, 4 for scalars.
fn size_class(u: &UniformSlot) -> u32 {
    match (&u.storage, &u.ty) {
        (UStorage::Plain | UStorage::BoolAsUint, Type::Scalar(_)) => 4,
        (UStorage::Plain | UStorage::BoolAsUint, Type::Vector(_, 2)) => 8,
        (UStorage::Plain | UStorage::BoolAsUint, Type::Vector(_, 3)) => 12,
        _ => 16,
    }
}

/// Orders members to minimise std140 padding: 16-byte members, then each vec3
/// followed by a scalar, then vec2s, then the remaining scalars.
fn order_uniforms(u: &mut Vec<UniformSlot>) {
    let all = std::mem::take(u);
    let (mut big, mut v3, mut v2, mut sc) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for s in all {
        match size_class(&s) {
            16 => big.push(s),
            12 => v3.push(s),
            8 => v2.push(s),
            _ => sc.push(s),
        }
    }
    sc.reverse();
    u.extend(big);
    for s in v3 {
        u.push(s);
        if let Some(x) = sc.pop() {
            u.push(x);
        }
    }
    u.extend(v2);
    sc.reverse();
    u.extend(sc);
}

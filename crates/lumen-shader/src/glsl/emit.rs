//! Emission of strictly typed Vulkan GLSL 4.50 from a typed, linked unit.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::OnceLock;

use super::ast::*;
use super::builtins::{Builtins, explicit_lod_name};
use super::link::{FRAME_BINDING, ProgramLayout, TEXTURE_GROUP, UNIFORM_BINDING, UStorage};
use super::typer::{GStorage, Stage, TypedUnit};
use super::types::{Interner, Name, RESERVED_450, Scalar, Type, basic_type_name};

pub const UNIFORM_BLOCK_NAME: &str = "lumen_Uniforms";
pub const FRAME_BLOCK_NAME: &str = "lumen_Frame";

/// Members of the renderer's frame block, in order (std140 offsets 0, 4, 8, 16,
/// 24, 32, 40 (padding), 48, 112, 176, 304, 432, 688; size 944).
pub const FRAME_FIELDS: &[(&str, &str)] = &[
    ("g_Time", "float"),
    ("g_Daytime", "float"),
    ("g_PointerPosition", "vec2"),
    ("g_PointerPositionLast", "vec2"),
    ("g_TexelSize", "vec2"),
    ("g_TexelSizeHalf", "vec2"),
    ("pad0", "vec2"),
    ("g_AudioSpectrum16Left", "vec4[4]"),
    ("g_AudioSpectrum16Right", "vec4[4]"),
    ("g_AudioSpectrum32Left", "vec4[8]"),
    ("g_AudioSpectrum32Right", "vec4[8]"),
    ("g_AudioSpectrum64Left", "vec4[16]"),
    ("g_AudioSpectrum64Right", "vec4[16]"),
];

fn view_of(u: &super::link::UniformSlot, local_ty: &Type) -> UView {
    match &u.storage {
        UStorage::Plain if *local_ty == u.ty => UView::Direct,
        UStorage::Plain => UView::Truncate(local_ty.vec_size().unwrap_or(1)),
        UStorage::BoolAsUint => {
            UView::BoolAsUint { stored: u.ty.vec_size().unwrap_or(1), used: local_ty.vec_size().unwrap_or(1) }
        }
        UStorage::Mat2Rows(c) => UView::Mat2Rows(*c),
        UStorage::Packed(s, _) => UView::Packed(*s),
    }
}

fn reserved() -> &'static HashSet<&'static str> {
    static R: OnceLock<HashSet<&'static str>> = OnceLock::new();
    R.get_or_init(|| RESERVED_450.iter().copied().collect())
}

/// Identifier as emitted (GLSL 4.50 keywords that were free in 3.30 get a suffix).
pub fn safe_ident(s: &str) -> String {
    if reserved().contains(s) || s.starts_with("lumen_") {
        format!("{s}_l")
    } else {
        s.to_string()
    }
}

#[derive(Clone)]
enum UView {
    Direct,
    Truncate(u8),
    BoolAsUint { stored: u8, used: u8 },
    Mat2Rows(u8),
    Packed(Scalar),
}

struct VaryingIo {
    /// Interface variable name.
    io: String,
    io_ty: Type,
    /// User-visible private variable (when adapted).
    private: Option<(String, Type)>,
    location: u32,
    interp: Interp,
}

pub struct Emitter<'a> {
    tu: &'a TypedUnit,
    int: &'a Interner,
    stage: Stage,
    out: String,
    uniform_view: HashMap<u32, (String, UView)>,
    samplers: HashMap<u32, (String, String)>,
    fn_names: Vec<String>,
    wrapper: bool,
    emitted_structs: HashSet<u32>,
    emitted_globals: HashSet<u32>,
    /// Globals emitted as GLSL `const`.
    const_globals: HashSet<u32>,
    /// Statements run by the wrapper `main` before the user's main.
    pre_main: Vec<String>,
    /// Polyfills needed by the emitted code (functions naga's HLSL back-end lacks).
    helpers: RefCell<BTreeSet<Helper>>,
    post_main: Vec<String>,
}

fn fmt_float(f: f32) -> String {
    if !f.is_finite() {
        return if f.is_nan() { "(0.0 / 0.0)".into() } else if f > 0.0 { "3.4028235e38".into() } else { "(-3.4028235e38)".into() };
    }
    let s = format!("{f:?}");
    if s.contains('.') || s.contains('e') || s.contains("inf") { s } else { format!("{s}.0") }
}

fn fmt_int(v: i64) -> String {
    let v = v as i32;
    if v == i32::MIN {
        "(-2147483647 - 1)".into()
    } else if v < 0 {
        format!("({v})")
    } else {
        v.to_string()
    }
}

pub fn emit_stage(tu: &TypedUnit, layout: &ProgramLayout, int: &Interner) -> String {
    let stage = tu.stage;
    let mut e = Emitter {
        tu,
        int,
        stage,
        out: String::with_capacity(8192),
        uniform_view: HashMap::new(),
        samplers: HashMap::new(),
        fn_names: Vec::new(),
        wrapper: false,
        emitted_structs: HashSet::new(),
        emitted_globals: HashSet::new(),
        const_globals: HashSet::new(),
        pre_main: Vec::new(),
        post_main: Vec::new(),
        helpers: RefCell::new(BTreeSet::new()),
    };
    e.out.push_str("#version 450\n");
    // Function names: overloaded, builtin-colliding or reserved names are mangled.
    let bt = Builtins::get();
    let mut counts: HashMap<Name, u32> = HashMap::new();
    for f in &tu.functions {
        *counts.entry(f.name).or_default() += 1;
    }
    for (i, f) in tu.functions.iter().enumerate() {
        let n = int.get(f.name);
        let name = if i as u32 == tu.main_fn {
            "main".to_string()
        } else if counts[&f.name] > 1 || !bt.overloads(n).is_empty() || reserved().contains(n) || n.starts_with("lumen_") {
            format!("{n}_lf{i}")
        } else {
            n.to_string()
        };
        e.fn_names.push(name);
    }

    // Pass block.
    let mine = |v: &Option<u32>, f: &Option<u32>| if stage == Stage::Vertex { *v } else { *f };
    if !layout.uniforms.is_empty() {
        let _ = writeln!(e.out, "layout(std140, set = 0, binding = {UNIFORM_BINDING}) uniform {UNIFORM_BLOCK_NAME} {{");
        for u in &layout.uniforms {
            let member = safe_ident(&u.name);
            let decl = match (&u.storage, &u.ty) {
                (UStorage::Plain, Type::Array(t, n)) => format!("{} {}[{}]", e.type_name(t), member, n),
                (UStorage::Plain, t) => format!("{} {}", e.type_name(t), member),
                (UStorage::BoolAsUint, t) => format!("{} {}", basic_type_name(&t.with_scalar(Scalar::Uint)), member),
                (UStorage::Mat2Rows(c), _) => format!("vec4 {member}[{c}]"),
                (UStorage::Packed(s, n), _) => {
                    format!("{} {}[{}]", basic_type_name(&Type::Vector(*s, 4)), member, n.div_ceil(4))
                }
            };
            let _ = writeln!(e.out, "    {decl};");
            if let Some(g) = mine(&u.vertex, &u.fragment) {
                e.uniform_view.insert(g, (member, view_of(u, &tu.globals[g as usize].ty)));
            }
        }
        e.out.push_str("};\n");
    }
    // Frame block: the full renderer layout; members the shader does not use
    // get private names so they cannot clash with pass-block members.
    if !layout.frame_uniforms.is_empty() {
        let _ = writeln!(e.out, "layout(std140, set = 0, binding = {FRAME_BINDING}) uniform {FRAME_BLOCK_NAME} {{");
        for (name, decl) in FRAME_FIELDS {
            let used = layout.frame_uniforms.iter().find(|u| u.name == *name);
            let member = if used.is_some() { safe_ident(name) } else { format!("lumen_f_{name}") };
            let (ty, arr) = decl.split_once('[').map_or((*decl, String::new()), |(t, a)| (t, format!("[{a}")));
            let _ = writeln!(e.out, "    {ty} {member}{arr};");
            if let Some(u) = used
                && let Some(g) = mine(&u.vertex, &u.fragment)
            {
                e.uniform_view.insert(g, (member, view_of(u, &tu.globals[g as usize].ty)));
            }
        }
        e.out.push_str("};\n");
    }
    // Textures: one texture/sampler pair per unit, at @group(1) @binding(2N, 2N + 1).
    let mut units_done = Vec::new();
    for s in &layout.samplers {
        let Type::Sampler(k) = s.ty else { continue };
        let tex = format!("lumen_tex{}", s.unit);
        let smp = format!("lumen_smp{}", s.unit);
        if !units_done.contains(&s.unit) {
            units_done.push(s.unit);
            let (tb, sb) = (2 * s.unit, 2 * s.unit + 1);
            let _ = writeln!(e.out, "layout(set = {TEXTURE_GROUP}, binding = {tb}) uniform {} {tex};", k.texture_name());
            let _ = writeln!(e.out, "layout(set = {TEXTURE_GROUP}, binding = {sb}) uniform {} {smp};", k.sampler_name());
        }
        if let Some(g) = mine(&s.vertex, &s.fragment) {
            e.samplers.insert(g, (tex, smp));
        }
    }

    // Interface variables.
    let mut ios: Vec<VaryingIo> = Vec::new();
    match stage {
        Stage::Vertex => {
            for a in &layout.attributes {
                let _ = writeln!(e.out, "layout(location = {}) in {};", a.location, e.decl(&a.ty, &safe_ident(&a.name)));
            }
            for v in &layout.varyings {
                let user = v.vertex.map(|g| &tu.globals[g as usize]);
                let name = safe_ident(&v.name);
                match user {
                    Some(gi) if gi.ty == v.ty => {
                        ios.push(VaryingIo { io: name, io_ty: v.ty.clone(), private: None, location: v.location, interp: v.interp })
                    }
                    Some(gi) => ios.push(VaryingIo {
                        io: format!("lumen_io_{}", v.name),
                        io_ty: v.ty.clone(),
                        private: Some((name, gi.ty.clone())),
                        location: v.location,
                        interp: v.interp,
                    }),
                    None => {
                        let io = format!("lumen_io_{}", v.name);
                        e.post_main.push(format!("{io} = {};", e.zero(&v.ty)));
                        ios.push(VaryingIo { io, io_ty: v.ty.clone(), private: None, location: v.location, interp: v.interp });
                    }
                }
            }
        }
        Stage::Fragment => {
            for v in &layout.varyings {
                let Some(g) = v.fragment else { continue };
                let gi = &tu.globals[g as usize];
                let name = safe_ident(&v.name);
                if gi.ty == v.ty {
                    ios.push(VaryingIo { io: name, io_ty: v.ty.clone(), private: None, location: v.location, interp: v.interp });
                } else {
                    ios.push(VaryingIo {
                        io: format!("lumen_io_{}", v.name),
                        io_ty: v.ty.clone(),
                        private: Some((name, gi.ty.clone())),
                        location: v.location,
                        interp: v.interp,
                    });
                }
            }
            for o in &layout.frag_outputs {
                let name = safe_ident(e.int.get(tu.globals[o.global as usize].name));
                let _ = writeln!(e.out, "layout(location = {}) out {};", o.location, e.decl(&o.ty, &name));
            }
        }
    }
    let dir = if stage == Stage::Vertex { "out" } else { "in" };
    for io in &ios {
        let interp = match io.interp {
            Interp::Flat => "flat ",
            Interp::NoPerspective => "noperspective ",
            _ => "",
        };
        let _ = writeln!(e.out, "layout(location = {}) {}{} {};", io.location, interp, dir, e.decl(&io.io_ty, &io.io));
        if let Some((pname, pty)) = &io.private {
            let _ = writeln!(e.out, "{};", e.decl(pty, pname));
            if stage == Stage::Vertex {
                e.post_main.push(format!("{} = {};", io.io, pad(pname, pty, &io.io_ty)));
            } else {
                e.pre_main.push(format!("{} = {};", pname, truncate(&io.io, &io.io_ty, pty)));
            }
        }
    }

    // User items.
    let mut body = String::new();
    std::mem::swap(&mut e.out, &mut body);
    for item in &tu.unit.items {
        match item {
            Item::Decl(d) => e.global_decl(d),
            Item::Function(f) => e.function(f),
            Item::Ignored => {}
        }
    }
    std::mem::swap(&mut e.out, &mut body);
    e.wrapper = !e.pre_main.is_empty() || !e.post_main.is_empty();
    if e.wrapper {
        // The user's main was emitted under the name `main`; rename it.
        body = rename_main(&body);
    }
    for h in e.helpers.borrow().iter() {
        e.out.push_str(&h.source());
    }
    e.out.push_str(&body);
    if e.wrapper {
        e.out.push_str("void main() {\n");
        for s in &e.pre_main {
            let _ = writeln!(e.out, "    {s}");
        }
        e.out.push_str("    lumen_user_main();\n");
        for s in &e.post_main {
            let _ = writeln!(e.out, "    {s}");
        }
        e.out.push_str("}\n");
    }
    e.out
}

/// Renames the definition `void main()` emitted by `function` (marked) to
/// `lumen_user_main`.
fn rename_main(body: &str) -> String {
    body.replacen("void main/*lumen_main*/()", "void lumen_user_main()", 1)
}

fn pad(src: &str, from: &Type, to: &Type) -> String {
    let (n, m) = (from.vec_size().unwrap_or(1), to.vec_size().unwrap_or(1));
    let s = to.scalar().unwrap_or(Scalar::Float);
    let (zero, one) = match s {
        Scalar::Float => ("0.0", "1.0"),
        Scalar::Uint => ("0u", "1u"),
        _ => ("0", "1"),
    };
    let mut parts = vec![src.to_string()];
    for k in n..m {
        parts.push(if k == 3 { one.into() } else { zero.into() });
    }
    format!("{}({})", basic_type_name(to), parts.join(", "))
}

fn truncate(src: &str, from: &Type, to: &Type) -> String {
    let (n, m) = (from.vec_size().unwrap_or(1), to.vec_size().unwrap_or(1));
    if n == m {
        return src.to_string();
    }
    format!("{src}.{}", &"xyzw"[..m as usize])
}

impl Emitter<'_> {
    fn name(&self, n: Name) -> String {
        safe_ident(self.int.get(n))
    }

    fn type_name(&self, t: &Type) -> String {
        match t {
            Type::Struct(id) => self.name(self.tu.structs[*id as usize].name),
            Type::Array(e, n) => format!("{}[{}]", self.type_name(e), n),
            t => basic_type_name(t),
        }
    }

    /// Declarator text `T name` / `T name[N]`.
    fn decl(&self, t: &Type, name: &str) -> String {
        match t {
            Type::Array(e, n) => format!("{} {}[{}]", self.type_name(e), name, n),
            _ => format!("{} {}", self.type_name(t), name),
        }
    }

    fn zero(&self, t: &Type) -> String {
        match t {
            Type::Scalar(Scalar::Float) => "0.0".into(),
            Type::Scalar(Scalar::Int) => "0".into(),
            Type::Scalar(Scalar::Uint) => "0u".into(),
            Type::Scalar(Scalar::Bool) => "false".into(),
            Type::Vector(s, _) => format!("{}({})", basic_type_name(t), self.zero(&Type::Scalar(*s))),
            Type::Matrix(..) => format!("{}(0.0)", basic_type_name(t)),
            Type::Array(e, n) => {
                let z = self.zero(e);
                format!("{}[{}]({})", self.type_name(e), n, vec![z; *n as usize].join(", "))
            }
            Type::Struct(id) => {
                let fields: Vec<String> = self.tu.structs[*id as usize].fields.iter().map(|f| self.zero(&f.1)).collect();
                format!("{}({})", self.type_name(t), fields.join(", "))
            }
            _ => "0".into(),
        }
    }

    fn struct_def(&mut self, d: &FullType) {
        let TypeSpec::StructDef(i) = d.spec else { return };
        let Some(id) = self.tu.unit.structs[i as usize].resolved else { return };
        if !self.emitted_structs.insert(id) {
            return;
        }
        let s = &self.tu.structs[id as usize];
        let mut txt = format!("struct {} {{\n", self.name(s.name));
        for (n, t) in &s.fields {
            let _ = writeln!(txt, "    {};", self.decl(t, &self.name(*n)));
        }
        txt.push_str("};\n");
        self.out.push_str(&txt);
    }

    fn global_decl(&mut self, d: &Decl) {
        self.struct_def(&d.ty);
        for v in &d.vars {
            let Sym::Global(g) = v.sym else { continue };
            if !self.emitted_globals.insert(g) {
                continue;
            }
            let gi = &self.tu.globals[g as usize];
            let name = self.name(v.name);
            match gi.storage {
                GStorage::Uniform | GStorage::In | GStorage::Out => {}
                GStorage::Const => {
                    let init = v.init.as_ref().expect("const has init");
                    if is_const_expr(init, &self.const_globals) {
                        self.const_globals.insert(g);
                        let txt = format!("const {} = {};\n", self.decl(&gi.ty, &name), self.expr(init));
                        self.out.push_str(&txt);
                    } else {
                        let txt = format!("{};\n", self.decl(&gi.ty, &name));
                        self.out.push_str(&txt);
                        let init = self.expr_top(init);
                        self.pre_main.push(format!("{name} = {init};"));
                    }
                }
                GStorage::Private => {
                    let txt = format!("{};\n", self.decl(&gi.ty, &name));
                    self.out.push_str(&txt);
                    if let Some(init) = &v.init {
                        let init = self.expr_top(init);
                        self.pre_main.push(format!("{name} = {init};"));
                    }
                }
            }
        }
    }

    fn params(&self, f: &FunctionDef) -> String {
        let mut ps = Vec::new();
        for p in &f.params {
            let pname = p.name.map(|n| self.name(n)).unwrap_or_else(|| format!("lumen_unnamed{}", p.local));
            if let Type::Sampler(k) = p.rty {
                ps.push(format!("{} {}_lt", k.texture_name(), pname));
                ps.push(format!("{} {}_ls", k.sampler_name(), pname));
                continue;
            }
            let dir = match p.dir {
                ParamDir::In => "",
                ParamDir::Out => "out ",
                ParamDir::InOut => "inout ",
            };
            ps.push(format!("{dir}{}", self.decl(&p.rty, &pname)));
        }
        ps.join(", ")
    }

    fn function(&mut self, f: &FunctionDef) {
        if f.skip || !self.tu.reachable[f.fn_index as usize] {
            return;
        }
        self.struct_def(&f.ret);
        let fi = &self.tu.functions[f.fn_index as usize];
        let is_main = f.fn_index == self.tu.main_fn;
        let name = if is_main { "main/*lumen_main*/".to_string() } else { self.fn_names[f.fn_index as usize].clone() };
        let head = format!("{} {}({})", self.type_name(&fi.ret), name, self.params(f));
        match &f.body {
            None => {
                if !is_main {
                    let _ = writeln!(self.out, "{head};");
                }
            }
            Some(body) => {
                let mut txt = format!("{head} {{\n");
                for s in body {
                    self.stmt(s, 1, &mut txt);
                }
                txt.push_str("}\n");
                self.out.push_str(&txt);
            }
        }
    }

    // ---- statements --------------------------------------------------------

    fn local_decl_text(&mut self, d: &Decl) -> Vec<String> {
        let mut v = Vec::new();
        for var in &d.vars {
            let name = self.name(var.name);
            match &var.init {
                Some(init) => v.push(format!("{} = {}", self.decl(&var.ty, &name), self.expr(init))),
                None => v.push(self.decl(&var.ty, &name)),
            }
        }
        v
    }

    fn stmt(&mut self, s: &Stmt, depth: usize, out: &mut String) {
        let ind = "    ".repeat(depth);
        match s {
            Stmt::Decl(d) => {
                if let TypeSpec::StructDef(_) = d.ty.spec {
                    let mut tmp = String::new();
                    std::mem::swap(&mut self.out, &mut tmp);
                    self.struct_def(&d.ty);
                    std::mem::swap(&mut self.out, &mut tmp);
                    out.push_str(&tmp);
                }
                for l in self.local_decl_text(d) {
                    let _ = writeln!(out, "{ind}{l};");
                }
            }
            Stmt::Expr(e) => {
                let _ = writeln!(out, "{ind}{};", self.expr_top(e));
            }
            Stmt::Block(v) => {
                let _ = writeln!(out, "{ind}{{");
                for s in v {
                    self.stmt(s, depth + 1, out);
                }
                let _ = writeln!(out, "{ind}}}");
            }
            Stmt::If(c, t, e) => {
                let _ = writeln!(out, "{ind}if ({}) {{", self.expr_top(c));
                self.stmt(t, depth + 1, out);
                if let Some(e) = e {
                    let _ = writeln!(out, "{ind}}} else {{");
                    self.stmt(e, depth + 1, out);
                }
                let _ = writeln!(out, "{ind}}}");
            }
            Stmt::Switch(c, body) => {
                let _ = writeln!(out, "{ind}switch ({}) {{", self.expr_top(c));
                for s in body {
                    self.stmt(s, depth + 1, out);
                }
                let _ = writeln!(out, "{ind}}}");
            }
            Stmt::Case(e) => {
                let _ = writeln!(out, "{ind}case {}:", self.expr_top(e));
            }
            Stmt::Default => {
                let _ = writeln!(out, "{ind}default:");
            }
            Stmt::While(c, b) => {
                let _ = writeln!(out, "{ind}while ({}) {{", self.expr_top(c));
                self.stmt(b, depth + 1, out);
                let _ = writeln!(out, "{ind}}}");
            }
            Stmt::DoWhile(b, c) => {
                let _ = writeln!(out, "{ind}do {{");
                self.stmt(b, depth + 1, out);
                let _ = writeln!(out, "{ind}}} while ({});", self.expr_top(c));
            }
            Stmt::For { init, cond, step, body } => {
                let init_txt = match init.as_deref() {
                    None => String::new(),
                    Some(Stmt::Decl(d)) => {
                        let parts = self.local_decl_text(d);
                        // `T a = 1, b = 2` shares the type of the first declarator.
                        let mut txt = parts.first().cloned().unwrap_or_default();
                        for (p, v) in parts.iter().zip(d.vars.iter()).skip(1) {
                            let tn = self.decl(&v.ty, "");
                            let rest = p.strip_prefix(tn.trim_end()).unwrap_or(p).trim_start();
                            let _ = write!(txt, ", {rest}");
                        }
                        txt
                    }
                    Some(Stmt::Expr(e)) => self.expr_top(e),
                    Some(_) => String::new(),
                };
                let c = cond.as_ref().map(|c| self.expr_top(c)).unwrap_or_default();
                let st = step.as_ref().map(|s| self.expr_top(s)).unwrap_or_default();
                let _ = writeln!(out, "{ind}for ({init_txt}; {c}; {st}) {{");
                self.stmt(body, depth + 1, out);
                let _ = writeln!(out, "{ind}}}");
            }
            Stmt::Break => {
                let _ = writeln!(out, "{ind}break;");
            }
            Stmt::Continue => {
                let _ = writeln!(out, "{ind}continue;");
            }
            Stmt::Discard => {
                let _ = writeln!(out, "{ind}discard;");
            }
            Stmt::Return(e, _) => match e {
                Some(e) => {
                    let _ = writeln!(out, "{ind}return {};", self.expr_top(e));
                }
                None => {
                    let _ = writeln!(out, "{ind}return;");
                }
            },
            Stmt::Empty => {}
        }
    }

    // ---- expressions -------------------------------------------------------

    /// Expression without redundant outer parentheses.
    fn expr_top(&self, e: &Expr) -> String {
        let s = self.expr(e);
        if s.starts_with('(') && s.ends_with(')') && balanced_outer(&s) {
            s[1..s.len() - 1].to_string()
        } else {
            s
        }
    }

    fn sampler_pair(&self, e: &Expr) -> (String, String) {
        match &e.kind {
            ExprKind::Ident(_, Sym::Global(g)) => {
                self.samplers.get(g).cloned().unwrap_or_else(|| ("lumen_missing_texture".into(), "lumen_missing_sampler".into()))
            }
            ExprKind::Ident(n, Sym::Local(_)) => {
                let p = self.name(*n);
                (format!("{p}_lt"), format!("{p}_ls"))
            }
            _ => ("lumen_invalid_texture".into(), "lumen_invalid_sampler".into()),
        }
    }

    fn postfix_base(&self, b: &Expr) -> String {
        let s = self.expr(b);
        match b.kind {
            ExprKind::Ident(..) | ExprKind::Index(..) | ExprKind::Field(..) | ExprKind::Call { .. } => s,
            ExprKind::Swizzle(ref inner, _) if inner.ty.vec_size() != Some(1) => s,
            _ if s.starts_with('(') && s.ends_with(')') && balanced_outer(&s) => s,
            _ => format!("({s})"),
        }
    }

    fn expr(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Ident(n, sym) => match sym {
                Sym::Global(g) => match self.uniform_view.get(g) {
                    Some((m, view)) => match view {
                        UView::Direct | UView::Packed(_) => m.clone(),
                        UView::Truncate(n) => {
                            if *n == 1 {
                                format!("{m}.x")
                            } else {
                                format!("{m}.{}", &"xyzw"[..*n as usize])
                            }
                        }
                        UView::BoolAsUint { stored, used } => {
                            let src = if stored == used {
                                m.clone()
                            } else if *used == 1 {
                                format!("{m}.x")
                            } else {
                                format!("{m}.{}", &"xyzw"[..*used as usize])
                            };
                            if *used == 1 {
                                format!("({src} != 0u)")
                            } else {
                                format!("notEqual({src}, uvec{used}(0u))")
                            }
                        }
                        UView::Mat2Rows(c) => {
                            let cols: Vec<String> = (0..*c).map(|k| format!("{m}[{k}].xy")).collect();
                            let t = if *c == 2 { "mat2".to_string() } else { format!("mat{c}x2") };
                            format!("{t}({})", cols.join(", "))
                        }
                    },
                    None => self.name(*n),
                },
                Sym::Builtin(b) => match b {
                    BuiltinVar::Position => "gl_Position".into(),
                    BuiltinVar::PointSize => "gl_PointSize".into(),
                    BuiltinVar::VertexId => format!("{}(gl_VertexIndex)", basic_type_name(&e.ty)),
                    BuiltinVar::InstanceId => format!("{}(gl_InstanceIndex)", basic_type_name(&e.ty)),
                    BuiltinVar::FragCoord => "gl_FragCoord".into(),
                    BuiltinVar::FrontFacing => "gl_FrontFacing".into(),
                    BuiltinVar::PointCoord => "gl_PointCoord".into(),
                    BuiltinVar::FragDepth => "gl_FragDepth".into(),
                },
                _ => self.name(*n),
            },
            ExprKind::IntLit(v) => fmt_int(*v),
            ExprKind::UintLit(v) => format!("{}u", *v as u32),
            ExprKind::FloatLit(f) => {
                if *f < 0.0 { format!("({})", fmt_float(*f)) } else { fmt_float(*f) }
            }
            ExprKind::BoolLit(b) => b.to_string(),
            ExprKind::Unary(op, x) => {
                let s = self.expr(x);
                match op {
                    UnOp::Neg => format!("(-{s})"),
                    UnOp::Plus => s,
                    UnOp::Not => format!("(!{s})"),
                    UnOp::BitNot => format!("(~{s})"),
                    UnOp::PreInc => format!("(++{s})"),
                    UnOp::PreDec => format!("(--{s})"),
                    UnOp::PostInc => format!("({s}++)"),
                    UnOp::PostDec => format!("({s}--)"),
                }
            }
            ExprKind::Binary(op, a, b) => {
                let (x, y) = (self.expr(a), self.expr(b));
                match op {
                    BinOp::FMod => format!("mod({}, {})", strip(&x), strip(&y)),
                    BinOp::Xor => format!("({x} != {y})"),
                    _ => format!("({x} {} {y})", op.text()),
                }
            }
            ExprKind::Assign(op, l, r) => {
                let l = self.expr(l);
                let r = self.expr(r);
                match op {
                    None => format!("({l} = {})", strip(&r)),
                    Some(o) => format!("({l} {}= {})", o.text(), strip(&r)),
                }
            }
            ExprKind::Ternary(c, a, b) => format!("({} ? {} : {})", self.expr(c), self.expr(a), self.expr(b)),
            ExprKind::Index(b, i) => {
                if let ExprKind::Ident(_, Sym::Global(g)) = &b.kind
                    && let Some((m, UView::Packed(s))) = self.uniform_view.get(g)
                {
                    let unsigned = i.ty == Type::UINT;
                    debug_assert!(matches!(s, Scalar::Float | Scalar::Int | Scalar::Uint));
                    if let ExprKind::IntLit(k) = i.kind {
                        return format!("{m}[{}].{}", k / 4, &"xyzw"[(k % 4) as usize..(k % 4) as usize + 1]);
                    }
                    let idx = strip(&self.expr(i)).to_string();
                    let (two, three) = if unsigned { ("2u", "3u") } else { ("2", "3") };
                    return format!("{m}[({idx}) >> {two}][({idx}) & {three}]");
                }
                format!("{}[{}]", self.postfix_base(b), strip(&self.expr(i)))
            }
            ExprKind::Field(b, n) => format!("{}.{}", self.postfix_base(b), self.name(*n)),
            ExprKind::Swizzle(b, comps) => {
                if b.ty.vec_size() == Some(1) {
                    let s = self.expr(b);
                    if comps.len() == 1 {
                        s
                    } else {
                        format!("{}({})", basic_type_name(&e.ty), strip(&s))
                    }
                } else {
                    let sel: String = comps.iter().map(|&c| b"xyzw"[c as usize] as char).collect();
                    format!("{}.{}", self.postfix_base(b), sel)
                }
            }
            ExprKind::Call { args, target, .. } => self.call(args, target),
            ExprKind::Length(b) => format!("{}.length()", self.postfix_base(b)),
            ExprKind::Comma(a, b) => format!("({}, {})", self.expr(a), self.expr(b)),
            ExprKind::Convert(x) => self.convert(x, &e.ty),
        }
    }

    fn convert(&self, x: &Expr, to: &Type) -> String {
        let s = self.expr(x);
        let from = &x.ty;
        let (Some(fb), Some(n), Some(tb), Some(m)) = (from.scalar(), from.vec_size(), to.scalar(), to.vec_size()) else {
            return format!("{}({})", self.type_name(to), strip(&s));
        };
        // Shape first (reduce), then base type.
        let shaped = if n > m {
            let sel = &"xyzw"[..m as usize];
            let base = if s.starts_with('(') || matches!(x.kind, ExprKind::Ident(..) | ExprKind::Call { .. } | ExprKind::Index(..) | ExprKind::Field(..)) {
                s.clone()
            } else {
                format!("({s})")
            };
            format!("{base}.{sel}")
        } else {
            s.clone()
        };
        let shaped_ty = Type::vec(fb, n.min(m));
        let based = if fb == tb {
            shaped
        } else if tb == Scalar::Bool {
            let z = self.zero(&shaped_ty);
            if shaped_ty.vec_size() == Some(1) {
                format!("({} != {z})", shaped)
            } else {
                format!("notEqual({}, {z})", strip(&shaped))
            }
        } else {
            format!("{}({})", basic_type_name(&shaped_ty.with_scalar(tb)), strip(&shaped))
        };
        if n == 1 && m > 1 {
            format!("{}({})", basic_type_name(to), strip(&based))
        } else {
            based
        }
    }

    fn call(&self, args: &[Expr], target: &CallTarget) -> String {
        match target {
            CallTarget::Ctor(t) => {
                let a: Vec<String> = args.iter().map(|a| strip(&self.expr(a)).to_string()).collect();
                format!("{}({})", self.type_name(t), a.join(", "))
            }
            CallTarget::Builtin(i) => {
                let b = &Builtins::get().list[*i as usize];
                let mut a: Vec<String> = Vec::with_capacity(args.len() + 1);
                for arg in args {
                    if let Type::Sampler(k) = arg.ty {
                        let (t, s) = self.sampler_pair(arg);
                        a.push(format!("{}({t}, {s})", k.combined_name()));
                    } else {
                        a.push(strip(&self.expr(arg)).to_string());
                    }
                }
                let mut name = b.emit;
                // naga's HLSL back-end (wgpu DX12) has no `inverse`/`outerProduct`.
                let helper = match (b.name, &b.ret) {
                    ("inverse", Type::Matrix(n, _)) => Some(Helper::Inverse(*n)),
                    ("outerProduct", Type::Matrix(c, r)) => Some(Helper::Outer(*c, *r)),
                    _ => None,
                };
                if let Some(h) = helper {
                    let n = h.name();
                    self.helpers.borrow_mut().insert(h);
                    return format!("{n}({})", a.join(", "));
                }
                if b.implicit_lod && self.stage != Stage::Fragment {
                    // Implicit-LOD lookups are fragment-only in naga: use level 0.
                    if let Some(explicit) = explicit_lod_name(b.name) {
                        name = explicit;
                        let has_offset = b.name == "textureOffset";
                        let mut na: Vec<String> = a[..2].to_vec();
                        na.push("0.0".into());
                        if has_offset {
                            na.push(a[2].clone());
                        }
                        a = na;
                    }
                }
                format!("{name}({})", a.join(", "))
            }
            CallTarget::User(f) => {
                let mut a: Vec<String> = Vec::with_capacity(args.len());
                for arg in args {
                    if arg.ty.is_sampler() {
                        let (t, s) = self.sampler_pair(arg);
                        a.push(t);
                        a.push(s);
                    } else {
                        a.push(strip(&self.expr(arg)).to_string());
                    }
                }
                let name = if *f == self.tu.main_fn { "main" } else { &self.fn_names[*f as usize] };
                format!("{name}({})", a.join(", "))
            }
            CallTarget::Unresolved => "/*unresolved*/0".into(),
        }
    }
}

fn balanced_outer(s: &str) -> bool {
    // True if the first '(' closes at the last character.
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && i != s.len() - 1 {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

fn strip(s: &str) -> &str {
    if s.starts_with('(') && s.ends_with(')') && balanced_outer(s) { &s[1..s.len() - 1] } else { s }
}

/// True if `e` can initialise a `const` global in naga (no calls besides constructors).
fn is_const_expr(e: &Expr, consts: &HashSet<u32>) -> bool {
    match &e.kind {
        ExprKind::IntLit(_) | ExprKind::UintLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) => true,
        ExprKind::Ident(_, Sym::Global(g)) => consts.contains(g),
        ExprKind::Unary(op, x) => {
            matches!(op, UnOp::Neg | UnOp::Plus | UnOp::Not | UnOp::BitNot) && is_const_expr(x, consts)
        }
        ExprKind::Binary(op, a, b) => *op != BinOp::FMod && is_const_expr(a, consts) && is_const_expr(b, consts),
        ExprKind::Convert(x) | ExprKind::Swizzle(x, _) => is_const_expr(x, consts),
        ExprKind::Call { target: CallTarget::Ctor(_), args, .. } => args.iter().all(|a| is_const_expr(a, consts)),
        _ => false,
    }
}

/// Helper functions emitted on demand.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Helper {
    /// `inverse(matN)`.
    Inverse(u8),
    /// `outerProduct` returning `matCxR`.
    Outer(u8, u8),
}

impl Helper {
    fn name(self) -> String {
        match self {
            Helper::Inverse(n) => format!("lumen_inverse{n}"),
            Helper::Outer(c, r) => format!("lumen_outer{c}x{r}"),
        }
    }

    fn source(self) -> String {
        match self {
            Helper::Inverse(2) => INVERSE2.to_string(),
            Helper::Inverse(3) => INVERSE3.to_string(),
            Helper::Inverse(_) => INVERSE4.to_string(),
            Helper::Outer(c, r) => {
                let m = basic_type_name(&Type::Matrix(c, r));
                let cols: Vec<String> = (0..c).map(|k| format!("c * r.{}", &"xyzw"[k as usize..k as usize + 1])).collect();
                format!("{m} {}(vec{r} c, vec{c} r) {{ return {m}({}); }}\n", self.name(), cols.join(", "))
            }
        }
    }
}

/// Inverse of a 2×2 matrix (adjugate over determinant).
pub const INVERSE2: &str = "mat2 lumen_inverse2(mat2 m) {
    float d = m[0][0] * m[1][1] - m[1][0] * m[0][1];
    return mat2(m[1][1], -m[0][1], -m[1][0], m[0][0]) / d;
}
";

/// Inverse of a 3×3 matrix: rows of the inverse are the cross products of
/// the columns, divided by the determinant.
pub const INVERSE3: &str = "mat3 lumen_inverse3(mat3 m) {
    vec3 r0 = cross(m[1], m[2]);
    vec3 r1 = cross(m[2], m[0]);
    vec3 r2 = cross(m[0], m[1]);
    float d = dot(m[0], r0);
    return transpose(mat3(r0, r1, r2)) / d;
}
";

/// Inverse of a 4×4 matrix by 2×2 sub-determinants (Laplace expansion).
/// `aCR` is column C, row R; the result is listed column by column.
pub const INVERSE4: &str = "mat4 lumen_inverse4(mat4 m) {
    float a00 = m[0][0]; float a01 = m[0][1]; float a02 = m[0][2]; float a03 = m[0][3];
    float a10 = m[1][0]; float a11 = m[1][1]; float a12 = m[1][2]; float a13 = m[1][3];
    float a20 = m[2][0]; float a21 = m[2][1]; float a22 = m[2][2]; float a23 = m[2][3];
    float a30 = m[3][0]; float a31 = m[3][1]; float a32 = m[3][2]; float a33 = m[3][3];
    float b00 = a00 * a11 - a01 * a10;
    float b01 = a00 * a12 - a02 * a10;
    float b02 = a00 * a13 - a03 * a10;
    float b03 = a01 * a12 - a02 * a11;
    float b04 = a01 * a13 - a03 * a11;
    float b05 = a02 * a13 - a03 * a12;
    float b06 = a20 * a31 - a21 * a30;
    float b07 = a20 * a32 - a22 * a30;
    float b08 = a20 * a33 - a23 * a30;
    float b09 = a21 * a32 - a22 * a31;
    float b10 = a21 * a33 - a23 * a31;
    float b11 = a22 * a33 - a23 * a32;
    float det = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
    return mat4(
        a11 * b11 - a12 * b10 + a13 * b09,
        a02 * b10 - a01 * b11 - a03 * b09,
        a31 * b05 - a32 * b04 + a33 * b03,
        a22 * b04 - a21 * b05 - a23 * b03,
        a12 * b08 - a10 * b11 - a13 * b07,
        a00 * b11 - a02 * b08 + a03 * b07,
        a32 * b02 - a30 * b05 - a33 * b01,
        a20 * b05 - a22 * b02 + a23 * b01,
        a10 * b10 - a11 * b08 + a13 * b06,
        a01 * b08 - a00 * b10 - a03 * b06,
        a30 * b04 - a31 * b02 + a33 * b00,
        a21 * b02 - a20 * b04 - a23 * b00,
        a11 * b07 - a10 * b09 - a12 * b06,
        a00 * b09 - a01 * b07 + a02 * b06,
        a31 * b01 - a30 * b03 - a32 * b00,
        a20 * b03 - a21 * b01 + a22 * b00) / det;
}
";

#[cfg(test)]
mod tests {
    use super::INVERSE4;
    use std::collections::HashMap;

    /// Evaluates `+ - *` expressions over named scalars (enough for INVERSE4).
    fn eval(expr: &str, vars: &HashMap<String, f64>) -> f64 {
        let toks: Vec<String> = expr
            .replace('*', " * ")
            .replace('+', " + ")
            .replace('-', " - ")
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut sum = 0.0;
        let mut sign = 1.0;
        let mut prod = 1.0;
        for t in toks {
            match t.as_str() {
                "+" | "-" => {
                    sum += sign * prod;
                    sign = if t == "+" { 1.0 } else { -1.0 };
                    prod = 1.0;
                }
                "*" => {}
                name => prod *= vars[name],
            }
        }
        sum + sign * prod
    }

    #[test]
    fn inverse4_formula() {
        // Column-major test matrix with a known inverse check: M · M⁻¹ = I.
        let m: [[f64; 4]; 4] =
            [[2.0, 0.5, 0.0, 1.0], [0.0, 3.0, 1.0, 0.0], [1.0, 0.0, 4.0, 0.25], [0.0, 2.0, 0.0, 1.5]];
        let mut vars = HashMap::new();
        for (c, col) in m.iter().enumerate() {
            for (r, v) in col.iter().enumerate() {
                vars.insert(format!("a{c}{r}"), *v);
            }
        }
        let body: Vec<&str> = INVERSE4.lines().collect();
        for l in body.iter().filter(|l| l.trim_start().starts_with("float b") || l.trim_start().starts_with("float det")) {
            let (lhs, rhs) = l.trim().trim_start_matches("float ").trim_end_matches(';').split_once(" = ").unwrap();
            let v = eval(rhs, &vars);
            vars.insert(lhs.to_string(), v);
        }
        let start = body.iter().position(|l| l.contains("return mat4(")).unwrap();
        let exprs: Vec<f64> = body[start + 1..start + 17]
            .iter()
            .map(|l| eval(l.trim().trim_end_matches(") / det;").trim_end_matches(','), &vars) / vars["det"])
            .collect();
        let inv = |c: usize, r: usize| exprs[c * 4 + r];
        for (i, j) in (0..4).flat_map(|i| (0..4).map(move |j| (i, j))) {
            {
                // (M · N)[col j][row i] = Σ_k M[col k][row i] · N[col j][row k]
                let v: f64 = m.iter().enumerate().map(|(k, col)| col[i] * inv(j, k)).sum();
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((v - want).abs() < 1e-9, "({i},{j}) = {v}");
            }
        }
    }
}

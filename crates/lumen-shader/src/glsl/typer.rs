//! Semantic analysis with the Wallpaper Engine relaxations (spec 03 §10.2,
//! rules R1–R7). Every implicit conversion is made explicit in the tree so the
//! emitted GLSL 4.50 is strictly typed.

use std::collections::{BTreeSet, HashMap};

use super::ast::*;
use super::builtins::Builtins;
use super::types::{Interner, Name, Scalar, Type, basic_type_name};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Stage {
    Vertex,
    Fragment,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Vertex => "vertex",
            Stage::Fragment => "fragment",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TypeError {
    pub line: u32,
    pub msg: String,
}

type TResult<T> = Result<T, TypeError>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GStorage {
    Uniform,
    In,
    Out,
    Const,
    Private,
}

#[derive(Clone, Debug)]
pub struct StructInfo {
    pub name: Name,
    pub fields: Vec<(Name, Type)>,
}

#[derive(Clone, Debug)]
pub struct GlobalInfo {
    pub name: Name,
    pub ty: Type,
    pub storage: GStorage,
    pub interp: Interp,
    pub line: u32,
    pub const_value: Option<ConstVal>,
}

#[derive(Clone, Debug)]
pub struct FnInfo {
    pub name: Name,
    pub ret: Type,
    pub params: Vec<(ParamDir, Type)>,
    pub defined: bool,
    pub uses: BTreeSet<u32>,
    pub calls: BTreeSet<u32>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ConstVal {
    Int(i64),
    Uint(u64),
    Float(f64),
    Bool(bool),
}

impl ConstVal {
    fn as_i64(self) -> i64 {
        match self {
            ConstVal::Int(v) => v,
            ConstVal::Uint(v) => v as i64,
            ConstVal::Float(v) => v as i64,
            ConstVal::Bool(b) => b as i64,
        }
    }
    fn as_f64(self) -> f64 {
        match self {
            ConstVal::Int(v) => v as f64,
            ConstVal::Uint(v) => v as f64,
            ConstVal::Float(v) => v,
            ConstVal::Bool(b) => b as i64 as f64,
        }
    }
}

/// Result of typing one stage.
pub struct TypedUnit {
    pub stage: Stage,
    pub unit: Unit,
    pub structs: Vec<StructInfo>,
    pub globals: Vec<GlobalInfo>,
    pub functions: Vec<FnInfo>,
    pub main_fn: u32,
    /// Functions reachable from `main` (and from global initializers).
    pub reachable: Vec<bool>,
    /// Globals referenced by reachable code or by global initializers.
    pub used_globals: Vec<bool>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy)]
struct Coerce {
    /// R1 operator-restricted conversions (float → int, uint → int).
    restricted: bool,
    /// R2 shape adaptations.
    shape: bool,
    /// Constructor semantics: any scalar conversion.
    explicit: bool,
}

const ASSIGN: Coerce = Coerce { restricted: true, shape: true, explicit: false };
const ARG: Coerce = Coerce { restricted: false, shape: true, explicit: false };
const STD: Coerce = Coerce { restricted: false, shape: false, explicit: false };
const EXPLICIT: Coerce = Coerce { restricted: true, shape: false, explicit: true };

/// Standard implicit conversions of GLSL 3.30 plus R1 `bool → float`.
fn std_conv(from: Scalar, to: Scalar) -> bool {
    from == to || (to == Scalar::Float && matches!(from, Scalar::Int | Scalar::Uint | Scalar::Bool))
}
/// R1 conversions restricted to `=`, `+ - * / %` and `*=`. `int → uint` is
/// added (GLSL 4.00 implicit conversion) so `uint u = int_expr;` compiles; it
/// never takes part in overload resolution.
fn restricted_conv(from: Scalar, to: Scalar) -> bool {
    matches!(
        (from, to),
        (Scalar::Float, Scalar::Int | Scalar::Uint) | (Scalar::Uint, Scalar::Int) | (Scalar::Int, Scalar::Uint)
    )
}

struct LocalScope {
    vars: HashMap<Name, (u32, Type, bool, Option<ConstVal>)>,
}

#[derive(Clone)]
struct Cand {
    params: Vec<(ParamDir, Type)>,
    ret: Type,
    target: CallTarget,
}

pub struct Typer<'a> {
    int: &'a Interner,
    stage: Stage,
    ast_structs: Vec<StructDefAst>,
    structs: Vec<StructInfo>,
    struct_by_name: HashMap<Name, u32>,
    globals: Vec<GlobalInfo>,
    global_by_name: HashMap<Name, u32>,
    functions: Vec<FnInfo>,
    fn_by_name: HashMap<Name, Vec<u32>>,
    scopes: Vec<LocalScope>,
    local_count: u32,
    cur_fn: Option<u32>,
    cur_ret: Type,
    init_uses: BTreeSet<u32>,
    init_calls: BTreeSet<u32>,
    /// `in uint gl_InstanceID;`-style redeclarations of vertex builtins.
    builtin_types: HashMap<BuiltinVar, Type>,
    warnings: Vec<String>,
}

fn terr<T>(line: u32, msg: impl Into<String>) -> TResult<T> {
    Err(TypeError { line, msg: msg.into() })
}

pub fn type_unit(unit: Unit, int: &Interner, stage: Stage) -> TResult<TypedUnit> {
    let Unit { mut items, structs } = unit;
    let mut t = Typer {
        int,
        stage,
        ast_structs: structs,
        structs: Vec::new(),
        struct_by_name: HashMap::new(),
        globals: Vec::new(),
        global_by_name: HashMap::new(),
        functions: Vec::new(),
        fn_by_name: HashMap::new(),
        scopes: Vec::new(),
        local_count: 0,
        cur_fn: None,
        cur_ret: Type::Void,
        init_uses: BTreeSet::new(),
        init_calls: BTreeSet::new(),
        builtin_types: HashMap::new(),
        warnings: Vec::new(),
    };
    for item in items.iter_mut() {
        match item {
            Item::Decl(d) => t.global_decl(d)?,
            Item::Function(f) => t.function(f)?,
            Item::Ignored => {}
        }
    }
    let main_name = int.lookup("main");
    let main_fn = main_name
        .and_then(|n| t.fn_by_name.get(&n))
        .and_then(|v| v.iter().copied().find(|&i| t.functions[i as usize].params.is_empty()))
        .filter(|&i| t.functions[i as usize].defined);
    let Some(main_fn) = main_fn else {
        return terr(0, "no definition of main()");
    };
    // Reachability.
    let mut reachable = vec![false; t.functions.len()];
    let mut stack: Vec<u32> = vec![main_fn];
    stack.extend(t.init_calls.iter().copied());
    while let Some(f) = stack.pop() {
        if std::mem::replace(&mut reachable[f as usize], true) {
            continue;
        }
        if !t.functions[f as usize].defined {
            let name = int.get(t.functions[f as usize].name).to_string();
            return terr(0, format!("function '{name}' is called but never defined"));
        }
        stack.extend(t.functions[f as usize].calls.iter().copied());
    }
    let mut used_globals = vec![false; t.globals.len()];
    for &g in &t.init_uses {
        used_globals[g as usize] = true;
    }
    for (f, r) in reachable.iter().enumerate() {
        if *r {
            for &g in &t.functions[f].uses {
                used_globals[g as usize] = true;
            }
        }
    }
    Ok(TypedUnit {
        stage,
        unit: Unit { items, structs: t.ast_structs },
        structs: t.structs,
        globals: t.globals,
        functions: t.functions,
        main_fn,
        reachable,
        used_globals,
        warnings: t.warnings,
    })
}

impl<'a> Typer<'a> {
    fn name(&self, n: Name) -> &str {
        self.int.get(n)
    }
    fn tname(&self, t: &Type) -> String {
        match t {
            Type::Struct(id) => self.name(self.structs[*id as usize].name).to_string(),
            Type::Array(e, n) => format!("{}[{}]", self.tname(e), n),
            _ => basic_type_name(t),
        }
    }

    // ---- types -----------------------------------------------------------

    fn resolve_full_type(&mut self, ft: &mut FullType) -> TResult<Type> {
        let base = match &ft.spec {
            TypeSpec::Builtin(t) => t.clone(),
            TypeSpec::Named(n) => match self.struct_by_name.get(n) {
                Some(&id) => Type::Struct(id),
                None => return terr(ft.line, format!("unknown type '{}'", self.name(*n))),
            },
            TypeSpec::StructDef(i) => Type::Struct(self.declare_struct(*i as usize)?),
        };
        match &mut ft.array {
            None => Ok(base),
            Some(Some(e)) => {
                let n = self.array_size(e)?;
                Ok(Type::Array(Box::new(base), n))
            }
            Some(None) => Ok(Type::Array(Box::new(base), 0)),
        }
    }

    fn array_size(&mut self, e: &mut Expr) -> TResult<u32> {
        self.expr(e)?;
        match self.const_eval(e) {
            Some(v) if v.as_i64() > 0 && v.as_i64() <= 65536 => Ok(v.as_i64() as u32),
            _ => terr(e.line, "array size must be a positive constant integer expression"),
        }
    }

    fn apply_array(&mut self, base: Type, arr: &mut ArraySpec, line: u32) -> TResult<Type> {
        match arr {
            None => Ok(base),
            Some(_) if matches!(base, Type::Array(..)) => terr(line, "arrays of arrays are not supported"),
            Some(Some(e)) => {
                let n = self.array_size(e)?;
                Ok(Type::Array(Box::new(base), n))
            }
            Some(None) => Ok(Type::Array(Box::new(base), 0)),
        }
    }

    fn declare_struct(&mut self, i: usize) -> TResult<u32> {
        if let Some(id) = self.ast_structs[i].resolved {
            return Ok(id);
        }
        let mut def = self.ast_structs[i].clone();
        let mut fields = Vec::new();
        for (ft, names) in def.fields.iter_mut() {
            let base = self.resolve_full_type(ft)?;
            if base.is_sampler() {
                return terr(def.line, "samplers in structs are not supported");
            }
            for (n, arr) in names.iter_mut() {
                let t = self.apply_array(base.clone(), arr, def.line)?;
                if matches!(&t, Type::Array(_, 0)) {
                    return terr(def.line, "unsized array in struct");
                }
                fields.push((*n, t));
            }
        }
        let name = def.name.unwrap_or_else(|| self.int.lookup("main").unwrap_or(0));
        if let Some(&prev) = def.name.and_then(|n| self.struct_by_name.get(&n)) {
            if self.structs[prev as usize].fields == fields {
                self.warnings.push(format!(
                    "line {}: struct '{}' redefined identically; first definition kept",
                    def.line,
                    self.name(name)
                ));
                self.ast_structs[i].resolved = Some(prev);
                return Ok(prev);
            }
            return terr(def.line, format!("struct '{}' redefined", self.name(name)));
        }
        let id = self.structs.len() as u32;
        self.structs.push(StructInfo { name, fields });
        if let Some(n) = def.name {
            self.struct_by_name.insert(n, id);
        }
        self.ast_structs[i].resolved = Some(id);
        Ok(id)
    }

    // ---- declarations ----------------------------------------------------

    fn global_decl(&mut self, d: &mut Decl) -> TResult<()> {
        let base = self.resolve_full_type(&mut d.ty)?;
        let storage = match d.quals.storage {
            Storage::Uniform => GStorage::Uniform,
            Storage::In => GStorage::In,
            Storage::Out => GStorage::Out,
            Storage::Const => GStorage::Const,
            Storage::None => GStorage::Private,
            Storage::Attribute if self.stage == Stage::Vertex => GStorage::In,
            Storage::Varying => {
                if self.stage == Stage::Vertex {
                    GStorage::Out
                } else {
                    GStorage::In
                }
            }
            s => return terr(d.line, format!("unsupported storage qualifier {s:?} at global scope")),
        };
        if base.is_sampler() && storage != GStorage::Uniform {
            return terr(d.line, "samplers must be uniforms");
        }
        for v in d.vars.iter_mut() {
            // A shader may redeclare the instance/vertex index as an input with
            // its own integer type; it stays the builtin.
            if storage == GStorage::In && self.stage == Stage::Vertex && v.array.is_none() {
                let b = match self.name(v.name) {
                    "gl_InstanceID" => Some(BuiltinVar::InstanceId),
                    "gl_VertexID" => Some(BuiltinVar::VertexId),
                    _ => None,
                };
                if let Some(b) = b {
                    if !matches!(base, Type::Scalar(Scalar::Int | Scalar::Uint)) {
                        return terr(v.line, "builtin index redeclared with a non-integer type");
                    }
                    self.builtin_types.insert(b, base.clone());
                    v.sym = Sym::Builtin(b);
                    v.ty = base.clone();
                    continue;
                }
            }
            let mut ty = self.apply_array(base.clone(), &mut v.array, v.line)?;
            if let Some(init) = v.init.as_mut() {
                if matches!(storage, GStorage::In | GStorage::Out) {
                    return terr(v.line, "shader inputs and outputs cannot have initializers");
                }
                self.cur_fn = None;
                self.expr(init)?;
                if let (Type::Array(b, 0), Type::Array(_, n)) = (&ty, &init.ty) {
                    ty = Type::Array(b.clone(), *n);
                }
                self.coerce(init, &ty, ASSIGN)?;
                if storage == GStorage::Uniform {
                    self.warnings.push(format!(
                        "line {}: uniform initializer of '{}' ignored",
                        v.line,
                        self.name(v.name)
                    ));
                }
            } else if storage == GStorage::Const {
                return terr(v.line, "const variable without initializer");
            }
            if matches!(&ty, Type::Array(_, 0)) {
                return terr(v.line, "unsized array");
            }
            if matches!(storage, GStorage::In | GStorage::Out)
                && ty.scalar().is_none()
                && !matches!(&ty, Type::Array(e, _) if e.scalar().is_some())
            {
                return terr(v.line, "unsupported type for a shader input/output");
            }
            let const_value = if storage == GStorage::Const {
                v.init.as_ref().and_then(|e| self.const_eval(e))
            } else {
                None
            };
            if let Some(&prev) = self.global_by_name.get(&v.name) {
                let p = &self.globals[prev as usize];
                if p.ty == ty && p.storage == storage {
                    // Tolerated: duplicated includes redeclare the same uniforms.
                    self.warnings.push(format!(
                        "line {}: '{}' redeclared with the same type; first declaration kept",
                        v.line,
                        self.name(v.name)
                    ));
                    v.sym = Sym::Global(prev);
                    v.ty = ty;
                    continue;
                }
                return terr(v.line, format!("redeclaration of '{}'", self.name(v.name)));
            }
            let id = self.globals.len() as u32;
            self.globals.push(GlobalInfo {
                name: v.name,
                ty: ty.clone(),
                storage,
                interp: d.quals.interp,
                line: v.line,
                const_value,
            });
            self.global_by_name.insert(v.name, id);
            v.sym = Sym::Global(id);
            v.ty = ty;
        }
        Ok(())
    }

    fn function(&mut self, f: &mut FunctionDef) -> TResult<()> {
        let ret = self.resolve_full_type(&mut f.ret)?;
        let mut params = Vec::new();
        for p in f.params.iter_mut() {
            let base = self.resolve_full_type(&mut p.ty)?;
            let t = self.apply_array(base, &mut p.array, f.line)?;
            if t.is_sampler() && p.dir != ParamDir::In {
                return terr(f.line, "sampler parameters must be 'in'");
            }
            if matches!(&t, Type::Array(_, 0)) {
                return terr(f.line, "unsized array parameter");
            }
            p.rty = t.clone();
            params.push((p.dir, t));
        }
        if params.len() == 1 && params[0].1 == Type::Void {
            params.clear();
            f.params.clear();
        }
        let existing = self.fn_by_name.get(&f.name).and_then(|v| {
            v.iter().copied().find(|&i| {
                let fi = &self.functions[i as usize];
                fi.params.iter().map(|p| &p.1).eq(params.iter().map(|p| &p.1))
            })
        });
        let idx = match existing {
            Some(i) => {
                let fi = &self.functions[i as usize];
                if fi.ret != ret {
                    return terr(f.line, format!("'{}' redeclared with another return type", self.name(f.name)));
                }
                if f.body.is_some() && fi.defined {
                    self.warnings.push(format!(
                        "line {}: function '{}' defined twice; first definition kept",
                        f.line,
                        self.name(f.name)
                    ));
                    f.skip = true;
                    f.fn_index = i;
                    return Ok(());
                }
                i
            }
            None => {
                let i = self.functions.len() as u32;
                self.functions.push(FnInfo {
                    name: f.name,
                    ret: ret.clone(),
                    params: params.clone(),
                    defined: false,
                    uses: BTreeSet::new(),
                    calls: BTreeSet::new(),
                });
                self.fn_by_name.entry(f.name).or_default().push(i);
                i
            }
        };
        f.fn_index = idx;
        let Some(body) = f.body.as_mut() else {
            return Ok(());
        };
        self.functions[idx as usize].defined = true;
        self.cur_fn = Some(idx);
        self.cur_ret = ret;
        self.local_count = 0;
        self.scopes.push(LocalScope { vars: HashMap::new() });
        for p in f.params.iter_mut() {
            let id = self.local_count;
            self.local_count += 1;
            p.local = id;
            if let Some(n) = p.name {
                self.scopes
                    .last_mut()
                    .expect("scope")
                    .vars
                    .insert(n, (id, p.rty.clone(), p.is_const, None));
            }
        }
        for s in body.iter_mut() {
            self.stmt(s)?;
        }
        self.scopes.pop();
        self.cur_fn = None;
        Ok(())
    }

    fn local_decl(&mut self, d: &mut Decl) -> TResult<()> {
        let base = self.resolve_full_type(&mut d.ty)?;
        let is_const = match d.quals.storage {
            Storage::Const => true,
            Storage::None => false,
            s => return terr(d.line, format!("storage qualifier {s:?} not allowed on a local")),
        };
        if base.is_sampler() {
            return terr(d.line, "local sampler variables are not supported");
        }
        for v in d.vars.iter_mut() {
            let mut ty = self.apply_array(base.clone(), &mut v.array, v.line)?;
            if let Some(init) = v.init.as_mut() {
                self.expr(init)?;
                if let (Type::Array(b, 0), Type::Array(_, n)) = (&ty, &init.ty) {
                    ty = Type::Array(b.clone(), *n);
                }
                self.coerce(init, &ty, ASSIGN)?;
            } else if is_const {
                return terr(v.line, "const variable without initializer");
            }
            if matches!(&ty, Type::Array(_, 0)) {
                return terr(v.line, "unsized array");
            }
            let cv = if is_const { v.init.as_ref().and_then(|e| self.const_eval(e)) } else { None };
            let id = self.local_count;
            self.local_count += 1;
            self.scopes
                .last_mut()
                .expect("scope")
                .vars
                .insert(v.name, (id, ty.clone(), is_const, cv));
            v.sym = Sym::Local(id);
            v.ty = ty;
        }
        Ok(())
    }

    // ---- statements ------------------------------------------------------

    fn scoped(&mut self, s: &mut Stmt) -> TResult<()> {
        self.scopes.push(LocalScope { vars: HashMap::new() });
        let r = self.stmt(s);
        self.scopes.pop();
        r
    }

    fn stmt(&mut self, s: &mut Stmt) -> TResult<()> {
        match s {
            Stmt::Decl(d) => self.local_decl(d),
            Stmt::Expr(e) => self.expr(e),
            Stmt::Block(v) => {
                self.scopes.push(LocalScope { vars: HashMap::new() });
                for s in v.iter_mut() {
                    self.stmt(s)?;
                }
                self.scopes.pop();
                Ok(())
            }
            Stmt::If(c, t, e) => {
                self.condition(c)?;
                self.scoped(t)?;
                if let Some(e) = e {
                    self.scoped(e)?;
                }
                Ok(())
            }
            Stmt::Switch(c, body) => {
                self.expr(c)?;
                if !matches!(c.ty, Type::Scalar(Scalar::Int | Scalar::Uint)) {
                    if c.ty == Type::FLOAT {
                        self.wrap(c, Type::INT);
                    } else {
                        return terr(c.line, "switch selector must be an integer");
                    }
                }
                let sel = c.ty.clone();
                self.scopes.push(LocalScope { vars: HashMap::new() });
                for s in body.iter_mut() {
                    if let Stmt::Case(e) = s {
                        self.expr(e)?;
                        self.coerce(e, &sel, EXPLICIT)?;
                        match self.const_eval(e) {
                            Some(v) => {
                                e.kind = if sel == Type::UINT {
                                    ExprKind::UintLit(v.as_i64() as u64)
                                } else {
                                    ExprKind::IntLit(v.as_i64())
                                };
                                e.ty = sel.clone();
                            }
                            None => return terr(e.line, "case label must be a constant expression"),
                        }
                    } else {
                        self.stmt(s)?;
                    }
                }
                self.scopes.pop();
                Ok(())
            }
            Stmt::Case(e) => terr(e.line, "case label outside of switch"),
            Stmt::While(c, b) => {
                self.scopes.push(LocalScope { vars: HashMap::new() });
                self.condition(c)?;
                let r = self.stmt(b);
                self.scopes.pop();
                r
            }
            Stmt::DoWhile(b, c) => {
                self.scoped(b)?;
                self.condition(c)
            }
            Stmt::For { init, cond, step, body } => {
                self.scopes.push(LocalScope { vars: HashMap::new() });
                if let Some(i) = init {
                    self.stmt(i)?;
                }
                if let Some(c) = cond {
                    self.condition(c)?;
                }
                if let Some(s) = step {
                    self.expr(s)?;
                }
                let r = self.scoped(body);
                self.scopes.pop();
                r
            }
            Stmt::Return(e, line) => {
                let ret = self.cur_ret.clone();
                match e {
                    Some(e) => {
                        self.expr(e)?;
                        if ret == Type::Void {
                            return terr(*line, "void function cannot return a value");
                        }
                        self.coerce(e, &ret, ARG)
                    }
                    None if ret != Type::Void => terr(*line, "missing return value"),
                    None => Ok(()),
                }
            }
            Stmt::Default | Stmt::Break | Stmt::Continue | Stmt::Discard | Stmt::Empty => {
                if matches!(s, Stmt::Discard) && self.stage != Stage::Fragment {
                    return terr(0, "'discard' is only allowed in fragment shaders");
                }
                Ok(())
            }
        }
    }

    /// R4: integer conditions are accepted; statements use `!= 0`
    /// (open question 7, HLSL semantics).
    fn condition(&mut self, c: &mut Expr) -> TResult<()> {
        self.expr(c)?;
        match c.ty {
            Type::Scalar(Scalar::Bool) => Ok(()),
            Type::Scalar(s @ (Scalar::Int | Scalar::Uint)) => {
                self.compare_zero(c, s, BinOp::Ne);
                Ok(())
            }
            _ => terr(c.line, format!("condition must be a boolean, found {}", self.tname(&c.ty))),
        }
    }

    fn compare_zero(&mut self, c: &mut Expr, s: Scalar, op: BinOp) {
        let line = c.line;
        let zero = if s == Scalar::Uint {
            ExprKind::UintLit(0)
        } else {
            ExprKind::IntLit(0)
        };
        let old = std::mem::replace(c, Expr::new(ExprKind::BoolLit(false), line));
        let z = Expr { kind: zero, ty: Type::Scalar(s), line };
        *c = Expr { kind: ExprKind::Binary(op, Box::new(old), Box::new(z)), ty: Type::BOOL, line };
    }

    // ---- conversions -----------------------------------------------------

    /// Wraps `e` in a conversion to `to` (folding literals).
    fn wrap(&mut self, e: &mut Expr, to: Type) {
        if let Type::Scalar(ts) = to {
            let folded = match (&e.kind, ts) {
                (ExprKind::IntLit(v), Scalar::Float) => Some(ExprKind::FloatLit(*v as f32)),
                (ExprKind::UintLit(v), Scalar::Float) => Some(ExprKind::FloatLit(*v as f32)),
                (ExprKind::BoolLit(b), Scalar::Float) => Some(ExprKind::FloatLit(*b as i32 as f32)),
                (ExprKind::FloatLit(f), Scalar::Int) => Some(ExprKind::IntLit(f.trunc() as i64)),
                (ExprKind::FloatLit(f), Scalar::Uint) => Some(ExprKind::UintLit(f.trunc().max(0.0) as u64)),
                (ExprKind::UintLit(v), Scalar::Int) => Some(ExprKind::IntLit(*v as u32 as i32 as i64)),
                (ExprKind::IntLit(v), Scalar::Uint) => Some(ExprKind::UintLit(*v as i32 as u32 as u64)),
                _ => None,
            };
            if let Some(k) = folded {
                e.kind = k;
                e.ty = to;
                return;
            }
        }
        let line = e.line;
        let old = std::mem::replace(e, Expr::new(ExprKind::BoolLit(false), line));
        *e = Expr { kind: ExprKind::Convert(Box::new(old)), ty: to, line };
    }

    fn coerce(&mut self, e: &mut Expr, to: &Type, c: Coerce) -> TResult<()> {
        if e.ty == *to {
            return Ok(());
        }
        let fail = |s: &Self, e: &Expr| {
            terr(e.line, format!("cannot convert {} to {}", s.tname(&e.ty), s.tname(to)))
        };
        let (Some(fb), Some(n)) = (e.ty.scalar(), e.ty.vec_size()) else {
            return fail(self, e);
        };
        let (Some(tb), Some(m)) = (to.scalar(), to.vec_size()) else {
            return fail(self, e);
        };
        let base_ok = std_conv(fb, tb) || (c.restricted && restricted_conv(fb, tb)) || c.explicit;
        let shape_ok = n == m || (c.shape && ((n == 1 && m > 1) || n > m));
        if !(base_ok && shape_ok) {
            return fail(self, e);
        }
        self.wrap(e, to.clone());
        Ok(())
    }

    // ---- expressions -----------------------------------------------------

    fn note_global_use(&mut self, g: u32) {
        match self.cur_fn {
            Some(f) => {
                self.functions[f as usize].uses.insert(g);
            }
            None => {
                self.init_uses.insert(g);
            }
        }
    }

    fn lookup_local(&self, n: Name) -> Option<(u32, Type, bool)> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.vars.get(&n))
            .map(|(id, t, c, _)| (*id, t.clone(), *c))
    }

    fn is_lvalue(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Ident(n, Sym::Local(_)) => !self.lookup_local(*n).is_some_and(|l| l.2),
            ExprKind::Ident(_, Sym::Global(g)) => {
                matches!(self.globals[*g as usize].storage, GStorage::Private | GStorage::Out)
            }
            ExprKind::Ident(_, Sym::Builtin(b)) => {
                matches!(b, BuiltinVar::Position | BuiltinVar::PointSize | BuiltinVar::FragDepth)
            }
            ExprKind::Index(b, _) | ExprKind::Field(b, _) => self.is_lvalue(b),
            ExprKind::Swizzle(b, c) => {
                let mut seen = [false; 4];
                for &k in c {
                    if std::mem::replace(&mut seen[k as usize], true) {
                        return false;
                    }
                }
                self.is_lvalue(b)
            }
            _ => false,
        }
    }

    fn require_lvalue(&self, e: &Expr) -> TResult<()> {
        if self.is_lvalue(e) {
            Ok(())
        } else {
            terr(e.line, "expression is not assignable")
        }
    }

    fn expr(&mut self, e: &mut Expr) -> TResult<()> {
        let line = e.line;
        match &mut e.kind {
            ExprKind::Ident(n, sym) => {
                let n = *n;
                if let Some((id, t, _)) = self.lookup_local(n) {
                    *sym = Sym::Local(id);
                    e.ty = t;
                } else if let Some(&g) = self.global_by_name.get(&n) {
                    *sym = Sym::Global(g);
                    e.ty = self.globals[g as usize].ty.clone();
                    self.note_global_use(g);
                } else if let Some((b, t)) = self.builtin_var(n) {
                    *sym = Sym::Builtin(b);
                    e.ty = self.builtin_types.get(&b).cloned().unwrap_or(t);
                } else {
                    return terr(line, format!("undeclared identifier '{}'", self.name(n)));
                }
            }
            ExprKind::IntLit(_) => e.ty = Type::INT,
            ExprKind::UintLit(_) => e.ty = Type::UINT,
            ExprKind::FloatLit(_) => e.ty = Type::FLOAT,
            ExprKind::BoolLit(_) => e.ty = Type::BOOL,
            ExprKind::Unary(op, x) => {
                let op = *op;
                self.expr(x)?;
                match op {
                    UnOp::Neg | UnOp::Plus => {
                        if x.ty.scalar() == Some(Scalar::Bool) && x.ty.is_scalar_or_vector() {
                            let t = x.ty.with_scalar(Scalar::Float);
                            self.wrap(x, t);
                        }
                        if !(x.ty.is_numeric_sv() || matches!(x.ty, Type::Matrix(..))) {
                            return terr(line, format!("cannot negate {}", self.tname(&x.ty)));
                        }
                        e.ty = x.ty.clone();
                    }
                    UnOp::Not => {
                        if x.ty.scalar() != Some(Scalar::Bool) || !x.ty.is_scalar_or_vector() {
                            return terr(line, format!("'!' needs a boolean, found {}", self.tname(&x.ty)));
                        }
                        // R2: bvecN → bool (first component).
                        self.coerce(x, &Type::BOOL, ARG)?;
                        e.ty = Type::BOOL;
                    }
                    UnOp::BitNot => {
                        if !matches!(x.ty.scalar(), Some(Scalar::Int | Scalar::Uint)) || !x.ty.is_scalar_or_vector() {
                            return terr(line, "'~' needs an integer operand");
                        }
                        e.ty = x.ty.clone();
                    }
                    UnOp::PreInc | UnOp::PreDec | UnOp::PostInc | UnOp::PostDec => {
                        self.require_lvalue(x)?;
                        if !(x.ty.is_numeric_sv() || matches!(x.ty, Type::Matrix(..))) {
                            return terr(line, "'++'/'--' need a numeric operand");
                        }
                        e.ty = x.ty.clone();
                    }
                }
            }
            ExprKind::Binary(..) => self.binary(e)?,
            ExprKind::Assign(..) => self.assign(e)?,
            ExprKind::Ternary(c, a, b) => {
                self.expr(c)?;
                match c.ty {
                    Type::Scalar(Scalar::Bool) => {}
                    // R4: a value-producing ternary tests `cond > 0`.
                    // parity: negative integer conditions are false, as in the reference.
                    Type::Scalar(s @ (Scalar::Int | Scalar::Uint)) => self.compare_zero(c, s, BinOp::Gt),
                    _ => return terr(line, "ternary condition must be a boolean"),
                }
                self.expr(a)?;
                self.expr(b)?;
                if a.ty != b.ty {
                    match (a.ty.scalar(), b.ty.scalar(), a.ty.vec_size(), b.ty.vec_size()) {
                        (Some(x), Some(y), Some(n), Some(m)) if n == m => {
                            let common = if std_conv(x, y) {
                                y
                            } else if std_conv(y, x) {
                                x
                            } else {
                                return terr(line, "ternary branches have incompatible types");
                            };
                            let t = Type::vec(common, n);
                            self.coerce(a, &t, STD)?;
                            self.coerce(b, &t, STD)?;
                        }
                        _ => {
                            return terr(
                                line,
                                format!(
                                    "ternary branches have different types ({} and {})",
                                    self.tname(&a.ty),
                                    self.tname(&b.ty)
                                ),
                            );
                        }
                    }
                }
                e.ty = a.ty.clone();
            }
            ExprKind::Index(b, i) => {
                self.expr(b)?;
                self.expr(i)?;
                match i.ty {
                    Type::Scalar(Scalar::Int | Scalar::Uint) => {}
                    // R6: float indices are truncated toward zero.
                    Type::Scalar(Scalar::Float) => self.wrap(i, Type::INT),
                    _ => return terr(line, "array index must be a scalar integer"),
                }
                e.ty = match &b.ty {
                    Type::Array(t, _) => (**t).clone(),
                    Type::Vector(s, _) => Type::Scalar(*s),
                    Type::Matrix(_, r) => Type::Vector(Scalar::Float, *r),
                    t => return terr(line, format!("cannot index {}", self.tname(t))),
                };
            }
            ExprKind::Field(b, n) => {
                let n = *n;
                self.expr(b)?;
                match b.ty.clone() {
                    Type::Struct(id) => {
                        let s = &self.structs[id as usize];
                        match s.fields.iter().find(|f| f.0 == n) {
                            Some((_, t)) => e.ty = t.clone(),
                            None => {
                                return terr(line, format!("no field '{}' in struct", self.name(n)));
                            }
                        }
                    }
                    Type::Scalar(s) | Type::Vector(s, _) => {
                        let size = b.ty.vec_size().unwrap_or(1);
                        let comps = self.swizzle(n, size, line)?;
                        let ty = Type::vec(s, comps.len() as u8);
                        let inner = std::mem::replace(b, Box::new(Expr::new(ExprKind::BoolLit(false), line)));
                        e.kind = ExprKind::Swizzle(inner, comps);
                        e.ty = ty;
                    }
                    t => return terr(line, format!("cannot select a field of {}", self.tname(&t))),
                }
            }
            ExprKind::Swizzle(..) | ExprKind::Convert(..) => {}
            ExprKind::Length(b) => {
                self.expr(b)?;
                let n = match &b.ty {
                    Type::Array(_, n) => *n as i64,
                    Type::Vector(_, n) => *n as i64,
                    Type::Matrix(c, _) => *c as i64,
                    _ => return terr(line, "length() needs an array, vector or matrix"),
                };
                e.kind = ExprKind::IntLit(n);
                e.ty = Type::INT;
            }
            ExprKind::Comma(a, b) => {
                self.expr(a)?;
                self.expr(b)?;
                e.ty = b.ty.clone();
            }
            ExprKind::Call { .. } => self.call(e)?,
        }
        Ok(())
    }

    /// Parses a swizzle selector. R7: out-of-range components wrap modulo the size.
    fn swizzle(&mut self, n: Name, size: u8, line: u32) -> TResult<Vec<u8>> {
        let s = self.name(n).to_string();
        if s.is_empty() || s.len() > 4 {
            return terr(line, format!("invalid swizzle '{s}'"));
        }
        let sets = ["xyzw", "rgba", "stpq"];
        let set = sets
            .iter()
            .find(|set| s.chars().all(|c| set.contains(c)))
            .ok_or_else(|| TypeError { line, msg: format!("invalid swizzle '{s}'") })?;
        let mut out = Vec::with_capacity(s.len());
        for c in s.chars() {
            let k = set.find(c).expect("checked") as u8;
            if k >= size {
                self.warnings.push(format!(
                    "line {line}: swizzle '{s}' exceeds a {size}-component value; component wrapped"
                ));
            }
            out.push(k % size);
        }
        Ok(out)
    }

    fn builtin_var(&self, n: Name) -> Option<(BuiltinVar, Type)> {
        let s = self.name(n);
        let v4 = Type::Vector(Scalar::Float, 4);
        Some(match (self.stage, s) {
            (Stage::Vertex, "gl_Position") => (BuiltinVar::Position, v4),
            (Stage::Vertex, "gl_PointSize") => (BuiltinVar::PointSize, Type::FLOAT),
            (Stage::Vertex, "gl_VertexID") => (BuiltinVar::VertexId, Type::INT),
            (Stage::Vertex, "gl_InstanceID") => (BuiltinVar::InstanceId, Type::INT),
            (Stage::Fragment, "gl_FragCoord") => (BuiltinVar::FragCoord, v4),
            (Stage::Fragment, "gl_FrontFacing") => (BuiltinVar::FrontFacing, Type::BOOL),
            (Stage::Fragment, "gl_PointCoord") => (BuiltinVar::PointCoord, Type::Vector(Scalar::Float, 2)),
            (Stage::Fragment, "gl_FragDepth") => (BuiltinVar::FragDepth, Type::FLOAT),
            _ => return None,
        })
    }

    fn binary(&mut self, e: &mut Expr) -> TResult<()> {
        let line = e.line;
        let ExprKind::Binary(op, l, r) = &mut e.kind else { unreachable!() };
        let op = *op;
        self.expr(l)?;
        self.expr(r)?;
        let ty = match op {
            BinOp::And | BinOp::Or | BinOp::Xor => {
                if l.ty != Type::BOOL || r.ty != Type::BOOL {
                    return terr(line, format!("'{}' needs boolean operands", op.text()));
                }
                Type::BOOL
            }
            BinOp::Eq | BinOp::Ne => {
                if l.ty != r.ty {
                    self.unify_scalar_bases(l, r, line, true)?;
                    if l.ty != r.ty {
                        return terr(
                            line,
                            format!("cannot compare {} with {}", self.tname(&l.ty), self.tname(&r.ty)),
                        );
                    }
                }
                Type::BOOL
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                if !(l.ty.vec_size() == Some(1) && r.ty.vec_size() == Some(1)) {
                    return terr(line, "relational operators need scalar operands");
                }
                self.unify_scalar_bases(l, r, line, true)?;
                if l.ty == Type::BOOL {
                    let (a, b) = (l.ty.with_scalar(Scalar::Float), r.ty.with_scalar(Scalar::Float));
                    self.wrap(l, a);
                    self.wrap(r, b);
                }
                Type::BOOL
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                let (ty, fmod) = self.arith(op, l, r, line)?;
                if fmod {
                    let ExprKind::Binary(o, ..) = &mut e.kind else { unreachable!() };
                    *o = BinOp::FMod;
                }
                ty
            }
            BinOp::FMod => e.ty.clone(),
            BinOp::Shl | BinOp::Shr => {
                let ok = |t: &Type| matches!(t.scalar(), Some(Scalar::Int | Scalar::Uint)) && t.is_scalar_or_vector();
                if !ok(&l.ty) || !ok(&r.ty) {
                    return terr(line, "shift operators need integer operands");
                }
                let (n, m) = (l.ty.vec_size().unwrap_or(1), r.ty.vec_size().unwrap_or(1));
                if n != m && m != 1 {
                    return terr(line, "shift operand sizes do not match");
                }
                l.ty.clone()
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let ok = |t: &Type| matches!(t.scalar(), Some(Scalar::Int | Scalar::Uint)) && t.is_scalar_or_vector();
                if !ok(&l.ty) || !ok(&r.ty) || l.ty.scalar() != r.ty.scalar() {
                    return terr(line, format!("'{}' needs integer operands of the same type", op.text()));
                }
                let (n, m) = (l.ty.vec_size().unwrap_or(1), r.ty.vec_size().unwrap_or(1));
                match (n, m) {
                    _ if n == m => l.ty.clone(),
                    (1, _) => r.ty.clone(),
                    (_, 1) => l.ty.clone(),
                    _ => return terr(line, "operand sizes do not match"),
                }
            }
        };
        e.ty = ty;
        Ok(())
    }

    /// Converts two scalar/vector operands to a common base type
    /// (standard conversions plus R1 `bool → float`).
    fn unify_scalar_bases(&mut self, l: &mut Expr, r: &mut Expr, line: u32, allow_bool: bool) -> TResult<()> {
        let (Some(a), Some(b)) = (l.ty.scalar(), r.ty.scalar()) else {
            return Ok(());
        };
        if !l.ty.is_scalar_or_vector() || !r.ty.is_scalar_or_vector() || a == b {
            return Ok(());
        }
        let common = if std_conv(a, b) {
            b
        } else if std_conv(b, a) {
            a
        } else if matches!((a, b), (Scalar::Int, Scalar::Uint) | (Scalar::Uint, Scalar::Int)) {
            Scalar::Int
        } else {
            return terr(line, "operand types are incompatible");
        };
        if common == Scalar::Bool && !allow_bool {
            return terr(line, "boolean operands are not allowed here");
        }
        let (lt, rt) = (l.ty.with_scalar(common), r.ty.with_scalar(common));
        self.wrap(l, lt);
        self.wrap(r, rt);
        Ok(())
    }

    /// Arithmetic operators. Returns (result type, is floating-point `%`).
    fn arith(&mut self, op: BinOp, l: &mut Expr, r: &mut Expr, line: u32) -> TResult<(Type, bool)> {
        let numeric = |t: &Type| t.is_scalar_or_vector() || matches!(t, Type::Matrix(..));
        if !numeric(&l.ty) || !numeric(&r.ty) {
            return terr(
                line,
                format!("invalid operands for '{}': {} and {}", op.text(), self.tname(&l.ty), self.tname(&r.ty)),
            );
        }
        let (lb, rb) = (l.ty.scalar().expect("numeric"), r.ty.scalar().expect("numeric"));
        // R1: mixed int/float stays float; bool → float; uint → int for these operators.
        let common = if lb == rb && lb != Scalar::Bool {
            lb
        } else if matches!(lb, Scalar::Float | Scalar::Bool) || matches!(rb, Scalar::Float | Scalar::Bool) {
            Scalar::Float
        } else {
            Scalar::Int
        };
        let is_mat = |t: &Type| matches!(t, Type::Matrix(..));
        if (is_mat(&l.ty) || is_mat(&r.ty)) && common != Scalar::Float {
            return terr(line, "integer matrix arithmetic is not supported");
        }
        if !is_mat(&l.ty) && lb != common {
            let t = l.ty.with_scalar(common);
            self.wrap(l, t);
        }
        if !is_mat(&r.ty) && rb != common {
            let t = r.ty.with_scalar(common);
            self.wrap(r, t);
        }
        let fmod = op == BinOp::Mod && common == Scalar::Float;
        let mismatch = |s: &Self, l: &Expr, r: &Expr| {
            terr(
                line,
                format!("mismatched operands for '{}': {} and {}", op.text(), s.tname(&l.ty), s.tname(&r.ty)),
            )
        };
        let ty = match (&l.ty, &r.ty) {
            (Type::Matrix(c1, r1), Type::Matrix(c2, r2)) => match op {
                BinOp::Mul if c1 == r2 => Type::Matrix(*c2, *r1),
                BinOp::Add | BinOp::Sub | BinOp::Div if c1 == c2 && r1 == r2 => l.ty.clone(),
                _ => return mismatch(self, l, r),
            },
            (Type::Matrix(c, rr), Type::Vector(_, n)) if op == BinOp::Mul => {
                if *c != *n {
                    return mismatch(self, l, r);
                }
                Type::Vector(Scalar::Float, *rr)
            }
            (Type::Vector(_, n), Type::Matrix(_c, rr)) if op == BinOp::Mul => {
                if *rr != *n {
                    return mismatch(self, l, r);
                }
                Type::Vector(Scalar::Float, *_c)
            }
            (Type::Matrix(..), Type::Scalar(_)) if op != BinOp::Mod => l.ty.clone(),
            (Type::Scalar(_), Type::Matrix(..)) if op != BinOp::Mod => r.ty.clone(),
            (Type::Matrix(..), _) | (_, Type::Matrix(..)) => return mismatch(self, l, r),
            _ => {
                let (n, m) = (l.ty.vec_size().expect("sv"), r.ty.vec_size().expect("sv"));
                if n == m || m == 1 {
                    l.ty.clone()
                } else if n == 1 {
                    if fmod {
                        // mod(genType, float) exists, mod(float, genType) does not.
                        let t = r.ty.clone();
                        self.wrap(l, t);
                    }
                    r.ty.clone()
                } else {
                    return mismatch(self, l, r);
                }
            }
        };
        Ok((ty, fmod))
    }

    fn assign(&mut self, e: &mut Expr) -> TResult<()> {
        let line = e.line;
        let ExprKind::Assign(op, l, r) = &mut e.kind else { unreachable!() };
        let op = *op;
        self.expr(l)?;
        self.expr(r)?;
        self.require_lvalue(l)?;
        let lt = l.ty.clone();
        match op {
            None => self.coerce(r, &lt, ASSIGN)?,
            Some(BinOp::Mul) if matches!((&lt, &r.ty), (Type::Vector(Scalar::Float, n), Type::Matrix(c, rr)) if n == c && n == rr) =>
                {}
            Some(BinOp::Mul | BinOp::Add | BinOp::Sub | BinOp::Div) if matches!(lt, Type::Matrix(..)) => {
                match &r.ty {
                    t if *t == lt => {}
                    Type::Scalar(_) => self.coerce(r, &Type::FLOAT, STD)?,
                    _ => return terr(line, "invalid matrix compound assignment"),
                }
            }
            Some(o @ (BinOp::Add | BinOp::Sub | BinOp::Div | BinOp::Mul | BinOp::Mod)) => {
                let Some(lb) = lt.scalar().filter(|_| lt.is_scalar_or_vector()) else {
                    return terr(line, "invalid compound assignment");
                };
                let (Some(rb), Some(rn)) = (r.ty.scalar(), r.ty.vec_size()) else {
                    return terr(line, "invalid compound assignment");
                };
                let ln = lt.vec_size().expect("sv");
                // `*=` also takes the R1 float → int conversion (0.5 becomes 0).
                let restricted = o == BinOp::Mul;
                let base_ok = std_conv(rb, lb) || (restricted && restricted_conv(rb, lb));
                if !base_ok {
                    return terr(
                        line,
                        format!("cannot apply '{}=' to {} with {}", o.text(), self.tname(&lt), self.tname(&r.ty)),
                    );
                }
                let target = if rn == 1 || rn == ln { r.ty.with_scalar(lb) } else { lt.clone() };
                self.coerce(r, &target, Coerce { restricted, shape: true, explicit: false })?;
                if o == BinOp::Mod && lb == Scalar::Float {
                    // R5: `a %= b` on floats is `a = mod(a, b)`.
                    if !side_effect_free(l) {
                        return terr(line, "'%=' on a float needs a simple left operand");
                    }
                    let lhs = (**l).clone();
                    let rhs = std::mem::replace(r, Box::new(Expr::new(ExprKind::BoolLit(false), line)));
                    let mut m = Expr {
                        kind: ExprKind::Binary(BinOp::FMod, Box::new(lhs), rhs),
                        ty: lt.clone(),
                        line,
                    };
                    if let ExprKind::Binary(_, a, b) = &mut m.kind
                        && a.ty.vec_size() == Some(1)
                        && b.ty.vec_size() != Some(1)
                    {
                        let t = b.ty.clone();
                        self.wrap(a, t);
                    }
                    **r = m;
                    let ExprKind::Assign(op, ..) = &mut e.kind else { unreachable!() };
                    *op = None;
                }
            }
            Some(o) => {
                // Shifts and bitwise operators: integers (R5 same base type).
                let int_ok = |t: &Type| matches!(t.scalar(), Some(Scalar::Int | Scalar::Uint)) && t.is_scalar_or_vector();
                if !int_ok(&lt) || !int_ok(&r.ty) {
                    return terr(line, format!("'{}=' needs integer operands", o.text()));
                }
                if !matches!(o, BinOp::Shl | BinOp::Shr) && lt.scalar() != r.ty.scalar() {
                    return terr(line, format!("'{}=' needs operands of the same type", o.text()));
                }
                if r.ty.vec_size() != Some(1) && r.ty.vec_size() != lt.vec_size() {
                    let t = if matches!(o, BinOp::Shl | BinOp::Shr) {
                        lt.with_scalar(r.ty.scalar().expect("int"))
                    } else {
                        lt.clone()
                    };
                    self.coerce(r, &t, ARG)?;
                }
            }
        }
        e.ty = lt;
        Ok(())
    }

    // ---- calls -----------------------------------------------------------

    fn call(&mut self, e: &mut Expr) -> TResult<()> {
        let line = e.line;
        let ExprKind::Call { callee, args, target } = &mut e.kind else { unreachable!() };
        for a in args.iter_mut() {
            self.expr(a)?;
            if a.ty == Type::Void {
                return terr(a.line, "void value used as an argument");
            }
        }
        match callee {
            Callee::Type(ft) => {
                let mut ft = ft.clone();
                let mut ty = self.resolve_full_type(&mut ft)?;
                if let Type::Array(b, 0) = &ty {
                    ty = Type::Array(b.clone(), args.len() as u32);
                }
                self.constructor(&ty, args, line)?;
                *target = CallTarget::Ctor(ty.clone());
                e.ty = ty;
                self.simplify_ctor(e);
                Ok(())
            }
            Callee::Name(n) => {
                let n = *n;
                if let Some(&sid) = self.struct_by_name.get(&n) {
                    let ty = Type::Struct(sid);
                    self.constructor(&ty, args, line)?;
                    *target = CallTarget::Ctor(ty.clone());
                    e.ty = ty;
                    return Ok(());
                }
                let mut cands: Vec<Cand> = Vec::new();
                if let Some(v) = self.fn_by_name.get(&n) {
                    for &i in v {
                        let f = &self.functions[i as usize];
                        cands.push(Cand { params: f.params.clone(), ret: f.ret.clone(), target: CallTarget::User(i) });
                    }
                }
                let bt = Builtins::get();
                for &i in bt.overloads(self.name(n)) {
                    let b = &bt.list[i as usize];
                    cands.push(Cand { params: b.params.clone(), ret: b.ret.clone(), target: CallTarget::Builtin(i) });
                }
                if cands.is_empty() {
                    return terr(line, format!("no function named '{}'", self.name(n)));
                }
                let arg_types: Vec<Type> = args.iter().map(|a| a.ty.clone()).collect();
                let chosen = match resolve_overload(&cands, &arg_types) {
                    Ok(i) => cands[i].clone(),
                    Err(msg) => {
                        let sig: Vec<String> = arg_types.iter().map(|t| self.tname(t)).collect();
                        return terr(line, format!("{msg} for call {}({})", self.name(n), sig.join(", ")));
                    }
                };
                for (a, (dir, pt)) in args.iter_mut().zip(chosen.params.iter()) {
                    match dir {
                        ParamDir::In => self.coerce(a, pt, ARG)?,
                        ParamDir::Out | ParamDir::InOut => {
                            self.require_lvalue(a)?;
                            if a.ty != *pt {
                                // Only a narrowing swizzle keeps an assignable argument.
                                match (a.ty.scalar(), pt.scalar(), a.ty.vec_size(), pt.vec_size()) {
                                    (Some(x), Some(y), Some(n), Some(m)) if x == y && n > m && m > 1 => {
                                        let line = a.line;
                                        let inner = std::mem::replace(a, Expr::new(ExprKind::BoolLit(false), line));
                                        *a = Expr {
                                            kind: ExprKind::Swizzle(Box::new(inner), (0..m).collect()),
                                            ty: pt.clone(),
                                            line,
                                        };
                                    }
                                    _ => {
                                        return terr(
                                            a.line,
                                            format!(
                                                "cannot pass {} to an out parameter of type {}",
                                                self.tname(&a.ty),
                                                self.tname(pt)
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                match chosen.target {
                    CallTarget::User(i) => {
                        if let Some(f) = self.cur_fn {
                            self.functions[f as usize].calls.insert(i);
                        } else {
                            self.init_calls.insert(i);
                        }
                    }
                    CallTarget::Builtin(i) if bt.list[i as usize].fragment_only && self.stage != Stage::Fragment => {
                        return terr(line, format!("'{}' is only available in fragment shaders", self.name(n)));
                    }
                    _ => {}
                }
                *target = chosen.target;
                e.ty = chosen.ret;
                Ok(())
            }
        }
    }

    /// Checks and converts constructor arguments (any scalar conversion allowed).
    fn constructor(&mut self, ty: &Type, args: &mut Vec<Expr>, line: u32) -> TResult<()> {
        match ty {
            Type::Scalar(s) => {
                if args.len() != 1 {
                    return terr(line, "scalar constructor takes one argument");
                }
                let a = &mut args[0];
                let Some(_) = a.ty.scalar() else {
                    return terr(line, format!("cannot construct {} from {}", s.name(), self.tname(&a.ty)));
                };
                if let Type::Matrix(..) = a.ty {
                    let l = a.line;
                    let inner = std::mem::replace(a, Expr::new(ExprKind::BoolLit(false), l));
                    let col = Expr {
                        kind: ExprKind::Index(Box::new(inner), Box::new(Expr { kind: ExprKind::IntLit(0), ty: Type::INT, line: l })),
                        ty: Type::Vector(Scalar::Float, 2),
                        line: l,
                    };
                    *a = col;
                }
                if a.ty.vec_size() != Some(1) {
                    let base = a.ty.scalar().expect("checked");
                    self.coerce(a, &Type::Scalar(base), ARG)?;
                }
                self.coerce(a, ty, EXPLICIT)
            }
            Type::Vector(s, n) => {
                let (s, n) = (*s, *n as u32);
                if args.len() == 1 && args[0].ty.vec_size() == Some(1) {
                    return self.coerce(&mut args[0], &Type::Scalar(s), EXPLICIT);
                }
                if args.iter().any(|a| !a.ty.is_scalar_or_vector()) {
                    return terr(line, "vector constructors take scalars or vectors");
                }
                let mut have = 0u32;
                let count = args.len();
                for (k, a) in args.iter_mut().enumerate() {
                    if have >= n {
                        return terr(line, "too many arguments to vector constructor");
                    }
                    let an = a.ty.vec_size().expect("sv") as u32;
                    let take = an.min(n - have);
                    let t = Type::vec(s, take as u8);
                    if take < an {
                        if k + 1 != count {
                            return terr(line, "too many arguments to vector constructor");
                        }
                        let src = a.ty.scalar().expect("sv");
                        self.coerce(a, &Type::vec(src, take as u8), ARG)?;
                    }
                    self.coerce(a, &t, EXPLICIT)?;
                    have += take;
                }
                if have < n {
                    return terr(line, "not enough components for vector constructor");
                }
                Ok(())
            }
            Type::Matrix(c, r) => {
                let (c, r) = (*c, *r);
                if args.len() == 1 {
                    match args[0].ty {
                        Type::Matrix(..) => return Ok(()),
                        Type::Scalar(_) => return self.coerce(&mut args[0], &Type::FLOAT, EXPLICIT),
                        _ => {}
                    }
                }
                if args.iter().any(|a| !a.ty.is_scalar_or_vector()) {
                    return terr(line, "matrix constructors take scalars, vectors or one matrix");
                }
                for a in args.iter_mut() {
                    let t = a.ty.with_scalar(Scalar::Float);
                    self.coerce(a, &t, EXPLICIT)?;
                }
                let columns_ok = args.len() == c as usize && args.iter().all(|a| a.ty.vec_size() == Some(r));
                let scalars_ok = args.len() == (c as usize * r as usize) && args.iter().all(|a| a.ty.vec_size() == Some(1));
                if columns_ok || scalars_ok {
                    return Ok(());
                }
                // Regroup components into columns (naga only accepts columns or scalars).
                let mut comps: Vec<Expr> = Vec::new();
                for a in args.iter() {
                    let n = a.ty.vec_size().expect("sv");
                    if n == 1 {
                        comps.push(a.clone());
                    } else {
                        if !side_effect_free(a) {
                            return terr(line, "matrix constructor argument with side effects cannot be split");
                        }
                        for k in 0..n {
                            comps.push(Expr {
                                kind: ExprKind::Swizzle(Box::new(a.clone()), vec![k]),
                                ty: Type::FLOAT,
                                line: a.line,
                            });
                        }
                    }
                }
                let need = c as usize * r as usize;
                if comps.len() < need {
                    return terr(line, "not enough components for matrix constructor");
                }
                comps.truncate(need);
                let col_ty = Type::Vector(Scalar::Float, r);
                let mut cols = Vec::new();
                for chunk in comps.chunks(r as usize) {
                    cols.push(Expr {
                        kind: ExprKind::Call {
                            callee: Callee::Type(FullType { spec: TypeSpec::Builtin(col_ty.clone()), array: None, line }),
                            args: chunk.to_vec(),
                            target: CallTarget::Ctor(col_ty.clone()),
                        },
                        ty: col_ty.clone(),
                        line,
                    });
                }
                *args = cols;
                Ok(())
            }
            Type::Struct(id) => {
                let fields = self.structs[*id as usize].fields.clone();
                if fields.len() != args.len() {
                    return terr(line, "wrong number of arguments to struct constructor");
                }
                for (a, (_, t)) in args.iter_mut().zip(fields.iter()) {
                    self.coerce(a, t, ARG)?;
                }
                Ok(())
            }
            Type::Array(t, n) => {
                if *n as usize != args.len() {
                    return terr(line, "wrong number of arguments to array constructor");
                }
                for a in args.iter_mut() {
                    self.coerce(a, t, STD)?;
                }
                Ok(())
            }
            _ => terr(line, format!("cannot construct {}", self.tname(ty))),
        }
    }

    /// Replaces a single-argument scalar/vector constructor that only converts
    /// by the conversion itself (keeps the output minimal).
    fn simplify_ctor(&mut self, e: &mut Expr) {
        let ExprKind::Call { args, target: CallTarget::Ctor(t), .. } = &mut e.kind else { return };
        if !t.is_scalar_or_vector() || args.len() != 1 {
            return;
        }
        if args[0].ty == *t {
            let a = args.pop().expect("one");
            *e = a;
        }
    }

    // ---- constant evaluation ----------------------------------------------

    pub fn const_eval(&self, e: &Expr) -> Option<ConstVal> {
        Some(match &e.kind {
            ExprKind::IntLit(v) => ConstVal::Int(*v),
            ExprKind::UintLit(v) => ConstVal::Uint(*v),
            ExprKind::FloatLit(v) => ConstVal::Float(*v as f64),
            ExprKind::BoolLit(b) => ConstVal::Bool(*b),
            ExprKind::Ident(n, Sym::Local(_)) => {
                return self.scopes.iter().rev().find_map(|s| s.vars.get(n)).and_then(|v| v.3);
            }
            ExprKind::Ident(_, Sym::Global(g)) => return self.globals[*g as usize].const_value,
            ExprKind::Convert(x) => {
                let v = self.const_eval(x)?;
                match e.ty {
                    Type::Scalar(Scalar::Int) => ConstVal::Int(v.as_f64().trunc() as i64),
                    Type::Scalar(Scalar::Uint) => ConstVal::Uint(v.as_f64().trunc() as u64),
                    Type::Scalar(Scalar::Float) => ConstVal::Float(v.as_f64()),
                    Type::Scalar(Scalar::Bool) => ConstVal::Bool(v.as_f64() != 0.0),
                    _ => return None,
                }
            }
            ExprKind::Unary(op, x) => {
                let v = self.const_eval(x)?;
                match (op, v) {
                    (UnOp::Neg, ConstVal::Int(i)) => ConstVal::Int(-i),
                    (UnOp::Neg, ConstVal::Float(f)) => ConstVal::Float(-f),
                    (UnOp::Plus, v) => v,
                    (UnOp::Not, ConstVal::Bool(b)) => ConstVal::Bool(!b),
                    (UnOp::BitNot, ConstVal::Int(i)) => ConstVal::Int(!i),
                    _ => return None,
                }
            }
            ExprKind::Binary(op, a, b) => {
                let (x, y) = (self.const_eval(a)?, self.const_eval(b)?);
                match (x, y) {
                    (ConstVal::Int(x), ConstVal::Int(y)) => match op {
                        BinOp::Add => ConstVal::Int(x.wrapping_add(y)),
                        BinOp::Sub => ConstVal::Int(x.wrapping_sub(y)),
                        BinOp::Mul => ConstVal::Int(x.wrapping_mul(y)),
                        BinOp::Div if y != 0 => ConstVal::Int(x / y),
                        BinOp::Mod if y != 0 => ConstVal::Int(x % y),
                        BinOp::Shl => ConstVal::Int(x.wrapping_shl(y as u32)),
                        BinOp::Shr => ConstVal::Int(x.wrapping_shr(y as u32)),
                        BinOp::BitAnd => ConstVal::Int(x & y),
                        BinOp::BitOr => ConstVal::Int(x | y),
                        BinOp::BitXor => ConstVal::Int(x ^ y),
                        BinOp::Lt => ConstVal::Bool(x < y),
                        BinOp::Gt => ConstVal::Bool(x > y),
                        BinOp::Le => ConstVal::Bool(x <= y),
                        BinOp::Ge => ConstVal::Bool(x >= y),
                        BinOp::Eq => ConstVal::Bool(x == y),
                        BinOp::Ne => ConstVal::Bool(x != y),
                        _ => return None,
                    },
                    (ConstVal::Float(x), ConstVal::Float(y)) => match op {
                        BinOp::Add => ConstVal::Float(x + y),
                        BinOp::Sub => ConstVal::Float(x - y),
                        BinOp::Mul => ConstVal::Float(x * y),
                        BinOp::Div => ConstVal::Float(x / y),
                        BinOp::Lt => ConstVal::Bool(x < y),
                        BinOp::Gt => ConstVal::Bool(x > y),
                        _ => return None,
                    },
                    (ConstVal::Uint(x), ConstVal::Uint(y)) => match op {
                        BinOp::Add => ConstVal::Uint(x.wrapping_add(y)),
                        BinOp::Sub => ConstVal::Uint(x.wrapping_sub(y)),
                        BinOp::Mul => ConstVal::Uint(x.wrapping_mul(y)),
                        BinOp::Div if y != 0 => ConstVal::Uint(x / y),
                        _ => return None,
                    },
                    _ => return None,
                }
            }
            ExprKind::Call { target: CallTarget::Ctor(Type::Scalar(s)), args, .. } if args.len() == 1 => {
                let v = self.const_eval(&args[0])?;
                match s {
                    Scalar::Int => ConstVal::Int(v.as_f64().trunc() as i64),
                    Scalar::Uint => ConstVal::Uint(v.as_f64().trunc() as u64),
                    Scalar::Float => ConstVal::Float(v.as_f64()),
                    Scalar::Bool => ConstVal::Bool(v.as_f64() != 0.0),
                }
            }
            _ => return None,
        })
    }
}

/// True if evaluating `e` twice is harmless.
pub fn side_effect_free(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Ident(..) | ExprKind::IntLit(_) | ExprKind::UintLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) => {
            true
        }
        ExprKind::Field(b, _) | ExprKind::Swizzle(b, _) | ExprKind::Convert(b) | ExprKind::Length(b) => side_effect_free(b),
        ExprKind::Index(b, i) => side_effect_free(b) && side_effect_free(i),
        ExprKind::Unary(op, x) => {
            !matches!(op, UnOp::PreInc | UnOp::PreDec | UnOp::PostInc | UnOp::PostDec) && side_effect_free(x)
        }
        ExprKind::Binary(_, a, b) => side_effect_free(a) && side_effect_free(b),
        ExprKind::Ternary(a, b, c) => side_effect_free(a) && side_effect_free(b) && side_effect_free(c),
        ExprKind::Call { target: CallTarget::Builtin(_) | CallTarget::Ctor(_), args, .. } => {
            args.iter().all(side_effect_free)
        }
        _ => false,
    }
}

/// R3: two-phase overload resolution. Returns the index of the chosen candidate.
fn resolve_overload(cands: &[Cand], args: &[Type]) -> Result<usize, String> {
    // Phase 1: strict (same shapes, standard conversions + bool → float).
    let mut viable: Vec<(usize, usize)> = Vec::new();
    for (i, c) in cands.iter().enumerate() {
        if c.params.len() != args.len() {
            continue;
        }
        let mut ok = true;
        let mut conversions = 0;
        for ((dir, p), a) in c.params.iter().zip(args) {
            if p == a {
                continue;
            }
            conversions += 1;
            let same_shape = a.is_scalar_or_vector() && p.is_scalar_or_vector() && a.vec_size() == p.vec_size();
            let (Some(ab), Some(pb)) = (a.scalar(), p.scalar()) else {
                ok = false;
                break;
            };
            ok &= same_shape
                && match dir {
                    ParamDir::In => std_conv(ab, pb),
                    ParamDir::Out => std_conv(pb, ab),
                    ParamDir::InOut => false,
                };
            if !ok {
                break;
            }
        }
        if ok {
            if conversions == 0 {
                return Ok(i);
            }
            viable.push((i, conversions));
        }
    }
    if viable.len() == 1 {
        return Ok(viable[0].0);
    }
    if viable.len() > 1 {
        // GLSL 3.30 reports an ambiguity here; Lumen keeps the candidate needing
        // the fewest conversions when it is unique (superset, see README).
        let min = viable.iter().map(|v| v.1).min().expect("non-empty");
        let best: Vec<_> = viable.iter().filter(|v| v.1 == min).collect();
        if best.len() == 1 {
            return Ok(best[0].0);
        }
        return Err("ambiguous overload".into());
    }
    // Phase 2: loose (broadcast scalar → vector, reduce vector → smaller vector).
    let mut best: Option<(usize, u32)> = None;
    let mut ties = 0;
    for (i, c) in cands.iter().enumerate() {
        if c.params.len() != args.len() {
            continue;
        }
        let mut ok = true;
        let mut adapt = 0u32;
        for ((dir, p), a) in c.params.iter().zip(args) {
            if p == a {
                continue;
            }
            if !a.is_scalar_or_vector() || !p.is_scalar_or_vector() {
                ok = false;
                break;
            }
            let (n, m) = (a.vec_size().expect("sv"), p.vec_size().expect("sv"));
            let shape_ok = n == m || (n == 1 && m > 1) || (n > m && m > 1);
            let (ab, pb) = (a.scalar().expect("sv"), p.scalar().expect("sv"));
            let base_ok = match dir {
                ParamDir::In => std_conv(ab, pb),
                ParamDir::Out => std_conv(pb, ab),
                ParamDir::InOut => ab == pb,
            };
            if !(shape_ok && base_ok) {
                ok = false;
                break;
            }
            if n != m {
                adapt += 1;
            }
        }
        if !ok {
            continue;
        }
        match best {
            None => {
                best = Some((i, adapt));
                ties = 1;
            }
            Some((_, b)) if adapt < b => {
                best = Some((i, adapt));
                ties = 1;
            }
            Some((_, b)) if adapt == b => ties += 1,
            _ => {}
        }
    }
    match best {
        // The reference only reports an ambiguity from three tied candidates.
        Some(_) if ties >= 3 => Err("ambiguous overload".into()),
        Some((i, _)) => Ok(i),
        None => Err("no matching overload".into()),
    }
}

//! Syntax tree of the tolerant GLSL front-end. The typer annotates it in place
//! (types, symbol resolution, inserted conversions) and the emitter prints it.

use super::types::{Name, Type};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    /// Floating-point `%` (R5): emitted as `mod(a, b)`.
    FMod,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Xor,
}

impl BinOp {
    pub fn text(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Mod | FMod => "%",
            Shl => "<<",
            Shr => ">>",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            Lt => "<",
            Gt => ">",
            Le => "<=",
            Ge => ">=",
            Eq => "==",
            Ne => "!=",
            And => "&&",
            Or => "||",
            Xor => "^^",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
}

/// Symbol an identifier resolves to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sym {
    Unresolved,
    Global(u32),
    Local(u32),
    Builtin(BuiltinVar),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum BuiltinVar {
    Position,
    PointSize,
    VertexId,
    InstanceId,
    FragCoord,
    FrontFacing,
    PointCoord,
    FragDepth,
}

/// What a call expression resolves to.
#[derive(Clone, PartialEq, Debug)]
pub enum CallTarget {
    Unresolved,
    /// Builtin function: index into the builtin table.
    Builtin(u32),
    /// User function: index into the stage function table.
    User(u32),
    /// Type constructor (scalar, vector, matrix, array or struct).
    Ctor(Type),
}

#[derive(Clone, Debug)]
pub enum TypeSpec {
    Builtin(Type),
    Named(Name),
    /// Struct defined inline; index into the parsed struct list.
    StructDef(u32),
}

#[derive(Clone, Debug)]
pub struct FullType {
    pub spec: TypeSpec,
    /// Array dimension written after the type (`float[3] x`): `Some(None)` = unsized.
    pub array: ArraySpec,
    pub line: u32,
}

#[derive(Clone, Debug)]
pub enum Callee {
    Name(Name),
    Type(FullType),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub line: u32,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Ident(Name, Sym),
    IntLit(i64),
    UintLit(u64),
    FloatLit(f32),
    BoolLit(bool),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Assign(Option<BinOp>, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Index(Box<Expr>, Box<Expr>),
    /// `.name` before typing; struct field after typing.
    Field(Box<Expr>, Name),
    /// Vector swizzle (component indices), created by the typer.
    Swizzle(Box<Expr>, Vec<u8>),
    Call { callee: Callee, args: Vec<Expr>, target: CallTarget },
    /// `expr.length()` (replaced by a literal when typed).
    Length(Box<Expr>),
    Comma(Box<Expr>, Box<Expr>),
    /// Implicit conversion inserted by the typer; target type is `Expr::ty`.
    Convert(Box<Expr>),
}

impl Expr {
    pub fn new(kind: ExprKind, line: u32) -> Expr {
        Expr { kind, ty: Type::Void, line }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Storage {
    #[default]
    None,
    Const,
    In,
    Out,
    InOut,
    Uniform,
    Attribute,
    Varying,
    Buffer,
    Shared,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Interp {
    #[default]
    Default,
    Flat,
    Smooth,
    NoPerspective,
}

#[derive(Clone, Debug, Default)]
pub struct Qualifiers {
    pub storage: Storage,
    /// `const` given together with `in` (parameters).
    pub is_const: bool,
    pub interp: Interp,
    pub centroid: bool,
    pub invariant: bool,
    /// `layout(...)` was present (contents are ignored; Lumen assigns its own).
    pub layout: bool,
}

#[derive(Clone, Debug)]
pub struct Declarator {
    pub name: Name,
    pub array: ArraySpec,
    pub init: Option<Expr>,
    pub line: u32,
    /// Resolved type (typer).
    pub ty: Type,
    /// Resolved symbol (typer).
    pub sym: Sym,
}

#[derive(Clone, Debug)]
pub struct Decl {
    pub quals: Qualifiers,
    pub ty: FullType,
    pub vars: Vec<Declarator>,
    pub line: u32,
}

/// Array dimension as written: `None` = not an array, `Some(None)` = unsized.
pub type ArraySpec = Option<Option<Box<Expr>>>;

/// One struct member line: type and declarators.
pub type FieldDecl = (FullType, Vec<(Name, ArraySpec)>);

#[derive(Clone, Debug)]
pub struct StructDefAst {
    pub name: Option<Name>,
    pub fields: Vec<FieldDecl>,
    pub line: u32,
    /// Index in the typer's struct table once declared.
    pub resolved: Option<u32>,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Decl(Decl),
    Expr(Expr),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    Switch(Expr, Vec<Stmt>),
    Case(Expr),
    Default,
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    For { init: Option<Box<Stmt>>, cond: Option<Expr>, step: Option<Expr>, body: Box<Stmt> },
    Break,
    Continue,
    Discard,
    Return(Option<Expr>, u32),
    Empty,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParamDir {
    In,
    Out,
    InOut,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub dir: ParamDir,
    pub is_const: bool,
    pub ty: FullType,
    pub name: Option<Name>,
    pub array: ArraySpec,
    /// Resolved type and local id (typer).
    pub rty: Type,
    pub local: u32,
}

#[derive(Clone, Debug)]
pub struct FunctionDef {
    pub ret: FullType,
    pub name: Name,
    pub params: Vec<Param>,
    pub body: Option<Vec<Stmt>>,
    pub line: u32,
    /// Index in the typer's function table (typer).
    pub fn_index: u32,
    /// Skip on emission (duplicate definition tolerated by the typer).
    pub skip: bool,
}

#[derive(Clone, Debug)]
pub enum Item {
    Decl(Decl),
    Function(FunctionDef),
    /// Declaration with only qualifiers (`invariant gl_Position;`, `layout(...) in;`).
    Ignored,
}

#[derive(Clone, Debug, Default)]
pub struct Unit {
    pub items: Vec<Item>,
    /// Struct definitions in parse order, referenced by `TypeSpec::StructDef`.
    pub structs: Vec<StructDefAst>,
}

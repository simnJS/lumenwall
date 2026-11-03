//! Types of the tolerant GLSL front-end, and the identifier interner.

use std::collections::HashMap;
use std::fmt;

/// Interned identifier.
pub type Name = u32;

macro_rules! keywords {
    ($($v:ident = $s:literal,)*) => {
        /// Keywords recognised by the parser. Their ids are the first interned names.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Kw { $($v,)* }
        const KEYWORDS: &[&str] = &[$($s,)*];
        const ALL_KW: &[Kw] = &[$(Kw::$v,)*];
    };
}

keywords! {
    Void = "void", Bool = "bool", Int = "int", Uint = "uint", Float = "float", Double = "double",
    Vec2 = "vec2", Vec3 = "vec3", Vec4 = "vec4",
    BVec2 = "bvec2", BVec3 = "bvec3", BVec4 = "bvec4",
    IVec2 = "ivec2", IVec3 = "ivec3", IVec4 = "ivec4",
    UVec2 = "uvec2", UVec3 = "uvec3", UVec4 = "uvec4",
    Mat2 = "mat2", Mat3 = "mat3", Mat4 = "mat4",
    Mat2x2 = "mat2x2", Mat2x3 = "mat2x3", Mat2x4 = "mat2x4",
    Mat3x2 = "mat3x2", Mat3x3 = "mat3x3", Mat3x4 = "mat3x4",
    Mat4x2 = "mat4x2", Mat4x3 = "mat4x3", Mat4x4 = "mat4x4",
    Sampler2D = "sampler2D", Sampler3D = "sampler3D", SamplerCube = "samplerCube",
    Sampler2DShadow = "sampler2DShadow", Sampler2DArray = "sampler2DArray",
    Sampler2DComparison = "sampler2DComparison",
    Struct = "struct",
    Const = "const", In = "in", Out = "out", Inout = "inout", Uniform = "uniform",
    Attribute = "attribute", Varying = "varying", Flat = "flat", Smooth = "smooth",
    Noperspective = "noperspective", Centroid = "centroid", Invariant = "invariant",
    Precise = "precise", Highp = "highp", Mediump = "mediump", Lowp = "lowp",
    Precision = "precision", Layout = "layout", Buffer = "buffer", Shared = "shared",
    If = "if", Else = "else", Switch = "switch", Case = "case", Default = "default",
    While = "while", Do = "do", For = "for", Break = "break", Continue = "continue",
    Return = "return", Discard = "discard", True = "true", False = "false",
}

impl Kw {
    pub fn id(self) -> Name {
        self as u32
    }
    pub fn from_id(id: Name) -> Option<Kw> {
        ALL_KW.get(id as usize).copied()
    }
}

/// Identifier interner shared by both stages of a program.
pub struct Interner {
    map: HashMap<Box<str>, Name>,
    names: Vec<Box<str>>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub fn new() -> Self {
        let mut i = Interner {
            map: HashMap::with_capacity(512),
            names: Vec::with_capacity(512),
        };
        for k in KEYWORDS {
            i.intern(k);
        }
        i
    }
    pub fn intern(&mut self, s: &str) -> Name {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.names.len() as Name;
        self.names.push(s.into());
        self.map.insert(s.into(), id);
        id
    }
    pub fn lookup(&self, s: &str) -> Option<Name> {
        self.map.get(s).copied()
    }
    pub fn get(&self, id: Name) -> &str {
        &self.names[id as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Scalar {
    Bool,
    Int,
    Uint,
    Float,
}

impl Scalar {
    pub fn is_integer(self) -> bool {
        matches!(self, Scalar::Int | Scalar::Uint)
    }
    fn prefix(self) -> &'static str {
        match self {
            Scalar::Bool => "b",
            Scalar::Int => "i",
            Scalar::Uint => "u",
            Scalar::Float => "",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Scalar::Bool => "bool",
            Scalar::Int => "int",
            Scalar::Uint => "uint",
            Scalar::Float => "float",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SamplerKind {
    S2D,
    S2DShadow,
    S3D,
    Cube,
    S2DArray,
}

impl SamplerKind {
    /// Combined sampler type name (source dialect, and the Vulkan GLSL constructor).
    pub fn combined_name(self) -> &'static str {
        match self {
            SamplerKind::S2D => "sampler2D",
            SamplerKind::S2DShadow => "sampler2DShadow",
            SamplerKind::S3D => "sampler3D",
            SamplerKind::Cube => "samplerCube",
            SamplerKind::S2DArray => "sampler2DArray",
        }
    }
    /// Separate texture type name (Vulkan GLSL).
    pub fn texture_name(self) -> &'static str {
        match self {
            SamplerKind::S2D | SamplerKind::S2DShadow => "texture2D",
            SamplerKind::S3D => "texture3D",
            SamplerKind::Cube => "textureCube",
            SamplerKind::S2DArray => "texture2DArray",
        }
    }
    /// Separate sampler type name (Vulkan GLSL).
    pub fn sampler_name(self) -> &'static str {
        match self {
            SamplerKind::S2DShadow => "samplerShadow",
            _ => "sampler",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Type {
    Void,
    Scalar(Scalar),
    /// Vector of 2 to 4 components.
    Vector(Scalar, u8),
    /// Float matrix: columns, rows.
    Matrix(u8, u8),
    Sampler(SamplerKind),
    /// Index into the stage's struct table.
    Struct(u32),
    Array(Box<Type>, u32),
}

impl Type {
    pub const FLOAT: Type = Type::Scalar(Scalar::Float);
    pub const INT: Type = Type::Scalar(Scalar::Int);
    pub const UINT: Type = Type::Scalar(Scalar::Uint);
    pub const BOOL: Type = Type::Scalar(Scalar::Bool);

    /// Scalar (n == 1) or vector type.
    pub fn vec(s: Scalar, n: u8) -> Type {
        if n == 1 { Type::Scalar(s) } else { Type::Vector(s, n) }
    }
    /// Base scalar of a scalar, vector or matrix.
    pub fn scalar(&self) -> Option<Scalar> {
        match *self {
            Type::Scalar(s) | Type::Vector(s, _) => Some(s),
            Type::Matrix(..) => Some(Scalar::Float),
            _ => None,
        }
    }
    /// Component count of a scalar or vector (1 for scalars).
    pub fn vec_size(&self) -> Option<u8> {
        match *self {
            Type::Scalar(_) => Some(1),
            Type::Vector(_, n) => Some(n),
            _ => None,
        }
    }
    pub fn is_scalar_or_vector(&self) -> bool {
        matches!(self, Type::Scalar(_) | Type::Vector(..))
    }
    pub fn is_numeric_sv(&self) -> bool {
        matches!(self.scalar(), Some(s) if s != Scalar::Bool) && self.is_scalar_or_vector()
    }
    pub fn is_sampler(&self) -> bool {
        matches!(self, Type::Sampler(_))
    }
    /// Same shape, other scalar. Only meaningful for scalars and vectors.
    pub fn with_scalar(&self, s: Scalar) -> Type {
        match *self {
            Type::Scalar(_) => Type::Scalar(s),
            Type::Vector(_, n) => Type::Vector(s, n),
            ref t => t.clone(),
        }
    }
    /// Number of scalar components (scalars, vectors, matrices).
    pub fn component_count(&self) -> Option<u32> {
        match *self {
            Type::Scalar(_) => Some(1),
            Type::Vector(_, n) => Some(n as u32),
            Type::Matrix(c, r) => Some(c as u32 * r as u32),
            _ => None,
        }
    }
    /// Number of interface locations this type occupies.
    pub fn location_count(&self) -> u32 {
        match self {
            Type::Matrix(c, _) => *c as u32,
            Type::Array(t, n) => t.location_count() * n,
            _ => 1,
        }
    }
}

/// Display helper for types (needs the struct names).
pub struct TypeDisplay<'a> {
    pub ty: &'a Type,
    pub struct_names: &'a dyn Fn(u32) -> String,
}

impl fmt::Display for TypeDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ty {
            Type::Array(t, n) => {
                write!(f, "{}[{}]", TypeDisplay { ty: t, struct_names: self.struct_names }, n)
            }
            Type::Struct(id) => f.write_str(&(self.struct_names)(*id)),
            t => f.write_str(&basic_type_name(t)),
        }
    }
}

/// GLSL spelling of a non-struct, non-array type.
pub fn basic_type_name(t: &Type) -> String {
    match *t {
        Type::Void => "void".into(),
        Type::Scalar(s) => s.name().into(),
        Type::Vector(s, n) => format!("{}vec{}", s.prefix(), n),
        Type::Matrix(c, r) => {
            if c == r {
                format!("mat{c}")
            } else {
                format!("mat{c}x{r}")
            }
        }
        Type::Sampler(k) => k.combined_name().into(),
        Type::Struct(id) => format!("struct#{id}"),
        Type::Array(ref t, n) => format!("{}[{}]", basic_type_name(t), n),
    }
}

/// Builtin type named by a keyword, if any.
pub fn keyword_type(kw: Kw) -> Option<Type> {
    use Kw::*;
    use Scalar as S;
    Some(match kw {
        Void => Type::Void,
        Bool => Type::BOOL,
        Int => Type::INT,
        Uint => Type::UINT,
        Float => Type::FLOAT,
        Vec2 => Type::Vector(S::Float, 2),
        Vec3 => Type::Vector(S::Float, 3),
        Vec4 => Type::Vector(S::Float, 4),
        BVec2 => Type::Vector(S::Bool, 2),
        BVec3 => Type::Vector(S::Bool, 3),
        BVec4 => Type::Vector(S::Bool, 4),
        IVec2 => Type::Vector(S::Int, 2),
        IVec3 => Type::Vector(S::Int, 3),
        IVec4 => Type::Vector(S::Int, 4),
        UVec2 => Type::Vector(S::Uint, 2),
        UVec3 => Type::Vector(S::Uint, 3),
        UVec4 => Type::Vector(S::Uint, 4),
        Mat2 | Mat2x2 => Type::Matrix(2, 2),
        Mat3 | Mat3x3 => Type::Matrix(3, 3),
        Mat4 | Mat4x4 => Type::Matrix(4, 4),
        Mat2x3 => Type::Matrix(2, 3),
        Mat2x4 => Type::Matrix(2, 4),
        Mat3x2 => Type::Matrix(3, 2),
        Mat3x4 => Type::Matrix(3, 4),
        Mat4x2 => Type::Matrix(4, 2),
        Mat4x3 => Type::Matrix(4, 3),
        Sampler2D => Type::Sampler(SamplerKind::S2D),
        // Open question 10: `sampler2DComparison` is not GLSL; Lumen treats it as a
        // plain 2D sampler unless the shader defines it as a macro.
        Sampler2DComparison => Type::Sampler(SamplerKind::S2D),
        Sampler2DShadow => Type::Sampler(SamplerKind::S2DShadow),
        Sampler3D => Type::Sampler(SamplerKind::S3D),
        SamplerCube => Type::Sampler(SamplerKind::Cube),
        Sampler2DArray => Type::Sampler(SamplerKind::S2DArray),
        _ => return None,
    })
}

/// Words that are reserved in GLSL 4.50 / Vulkan GLSL but not in GLSL 3.30, so a
/// 3.30 shader may use them as identifiers. They are renamed on emission.
pub const RESERVED_450: &[&str] = &[
    "sample", "patch", "subroutine", "precise", "coherent", "volatile", "restrict", "readonly",
    "writeonly", "buffer", "shared", "double", "dvec2", "dvec3", "dvec4", "dmat2", "dmat3",
    "dmat4", "atomic_uint", "texture1D", "texture2D", "texture3D", "textureCube",
    "texture2DArray", "texture2DMS", "textureBuffer", "texture1DArray", "textureCubeArray",
    "sampler", "samplerShadow", "subpassInput", "image1D", "image2D", "image3D", "imageCube",
    "image2DArray", "image2DMS", "imageBuffer", "samplerCubeArray", "resource", "common",
    "partition", "active", "filter", "superp", "input", "output", "hvec2", "hvec3", "hvec4",
    "fvec2", "fvec3", "fvec4", "sizeof", "cast", "namespace", "using", "noinline", "inline",
    "public", "static", "extern", "external", "interface", "long", "short", "half", "fixed",
    "unsigned", "goto", "template", "this", "typedef", "union", "enum", "class", "asm",
    "dmat2x2", "dmat2x3", "dmat2x4", "dmat3x2", "dmat3x3", "dmat3x4", "dmat4x2", "dmat4x3",
    "dmat4x4", "isampler2D", "usampler2D", "attribute", "varying", "gl_VertexID",
    "gl_InstanceID",
];

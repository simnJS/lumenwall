//! Table of GLSL builtin functions (concrete overloads), in GLSL 3.30 core
//! order: for each family float, vec2, vec3, vec4 – the order matters for the
//! "first candidate wins" rule of R3.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::ast::ParamDir;
use super::types::{SamplerKind, Scalar, Type};

pub struct Builtin {
    pub name: &'static str,
    /// Name used in the emitted GLSL 4.50.
    pub emit: &'static str,
    pub ret: Type,
    pub params: Vec<(ParamDir, Type)>,
    /// Only valid in fragment shaders (implicit derivatives).
    pub fragment_only: bool,
    /// Texture lookup with implicit level of detail (needs rewriting in vertex shaders).
    pub implicit_lod: bool,
}

pub struct Builtins {
    pub list: Vec<Builtin>,
    by_name: HashMap<&'static str, Vec<u32>>,
}

impl Builtins {
    pub fn get() -> &'static Builtins {
        static B: OnceLock<Builtins> = OnceLock::new();
        B.get_or_init(build)
    }
    pub fn overloads(&self, name: &str) -> &[u32] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }
}

use Scalar::{Bool as B, Float as F, Int as I, Uint as U};

fn v(s: Scalar, n: u8) -> Type {
    Type::vec(s, n)
}

struct Builder {
    list: Vec<Builtin>,
}

impl Builder {
    fn add(&mut self, name: &'static str, ret: Type, params: &[Type]) -> &mut Builtin {
        self.list.push(Builtin {
            name,
            emit: name,
            ret,
            params: params.iter().map(|t| (ParamDir::In, t.clone())).collect(),
            fragment_only: false,
            implicit_lod: false,
        });
        self.list.last_mut().expect("just pushed")
    }
    fn add_dirs(&mut self, name: &'static str, ret: Type, params: &[(ParamDir, Type)]) {
        self.list.push(Builtin {
            name,
            emit: name,
            ret,
            params: params.to_vec(),
            fragment_only: false,
            implicit_lod: false,
        });
    }
    /// genType f(genType)
    fn un(&mut self, name: &'static str, s: Scalar) {
        for n in 1..=4 {
            self.add(name, v(s, n), &[v(s, n)]);
        }
    }
    /// genType f(genType, genType)
    fn bin(&mut self, name: &'static str, s: Scalar) {
        for n in 1..=4 {
            self.add(name, v(s, n), &[v(s, n), v(s, n)]);
        }
    }
    /// genType f(genType, scalar) for vectors only
    fn bin_vs(&mut self, name: &'static str, s: Scalar) {
        for n in 2..=4 {
            self.add(name, v(s, n), &[v(s, n), v(s, 1)]);
        }
    }
}

fn build() -> Builtins {
    let mut b = Builder {
        list: Vec::with_capacity(900),
    };
    for name in [
        "radians",
        "degrees",
        "sin",
        "cos",
        "tan",
        "asin",
        "acos",
        "atan",
        "sinh",
        "cosh",
        "tanh",
        "asinh",
        "acosh",
        "atanh",
        "exp",
        "log",
        "exp2",
        "log2",
        "sqrt",
        "inversesqrt",
        "floor",
        "trunc",
        "round",
        "roundEven",
        "ceil",
        "fract",
        "normalize",
    ] {
        b.un(name, F);
    }
    for name in ["abs", "sign"] {
        b.un(name, F);
        b.un(name, I);
    }
    for name in ["atan", "pow", "step", "mod", "min", "max", "reflect"] {
        b.bin(name, F);
        if name == "mod" || name == "min" || name == "max" {
            b.bin_vs(name, F);
        }
    }
    for n in 2..=4 {
        b.add("step", v(F, n), &[Type::FLOAT, v(F, n)]);
    }
    for s in [I, U] {
        b.bin("min", s);
        b.bin_vs("min", s);
        b.bin("max", s);
        b.bin_vs("max", s);
    }
    for s in [F, I, U] {
        for n in 1..=4 {
            b.add("clamp", v(s, n), &[v(s, n), v(s, n), v(s, n)]);
        }
        for n in 2..=4 {
            b.add("clamp", v(s, n), &[v(s, n), v(s, 1), v(s, 1)]);
        }
    }
    for n in 1..=4 {
        b.add("mix", v(F, n), &[v(F, n), v(F, n), v(F, n)]);
    }
    for n in 2..=4 {
        b.add("mix", v(F, n), &[v(F, n), v(F, n), Type::FLOAT]);
    }
    for s in [F, I, U, B] {
        for n in 1..=4 {
            b.add("mix", v(s, n), &[v(s, n), v(s, n), v(B, n)]);
        }
    }
    for n in 1..=4 {
        b.add("smoothstep", v(F, n), &[v(F, n), v(F, n), v(F, n)]);
    }
    for n in 2..=4 {
        b.add("smoothstep", v(F, n), &[Type::FLOAT, Type::FLOAT, v(F, n)]);
    }
    for n in 1..=4 {
        b.add("fma", v(F, n), &[v(F, n), v(F, n), v(F, n)]);
        b.add_dirs(
            "modf",
            v(F, n),
            &[(ParamDir::In, v(F, n)), (ParamDir::Out, v(F, n))],
        );
        b.add_dirs(
            "frexp",
            v(F, n),
            &[(ParamDir::In, v(F, n)), (ParamDir::Out, v(I, n))],
        );
        b.add("ldexp", v(F, n), &[v(F, n), v(I, n)]);
        b.add("isnan", v(B, n), &[v(F, n)]);
        b.add("isinf", v(B, n), &[v(F, n)]);
        b.add("floatBitsToInt", v(I, n), &[v(F, n)]);
        b.add("floatBitsToUint", v(U, n), &[v(F, n)]);
        b.add("intBitsToFloat", v(F, n), &[v(I, n)]);
        b.add("uintBitsToFloat", v(F, n), &[v(U, n)]);
        b.add("length", Type::FLOAT, &[v(F, n)]);
        b.add("distance", Type::FLOAT, &[v(F, n), v(F, n)]);
        b.add("dot", Type::FLOAT, &[v(F, n), v(F, n)]);
        b.add("faceforward", v(F, n), &[v(F, n), v(F, n), v(F, n)]);
        b.add("refract", v(F, n), &[v(F, n), v(F, n), Type::FLOAT]);
    }
    b.add("cross", v(F, 3), &[v(F, 3), v(F, 3)]);
    // Integer functions.
    for s in [I, U] {
        for n in 1..=4 {
            b.add("bitCount", v(I, n), &[v(s, n)]);
            b.add("findLSB", v(I, n), &[v(s, n)]);
            b.add("findMSB", v(I, n), &[v(s, n)]);
            b.add("bitfieldReverse", v(s, n), &[v(s, n)]);
            b.add("bitfieldExtract", v(s, n), &[v(s, n), Type::INT, Type::INT]);
            b.add(
                "bitfieldInsert",
                v(s, n),
                &[v(s, n), v(s, n), Type::INT, Type::INT],
            );
        }
    }
    // Packing.
    b.add("packUnorm2x16", Type::UINT, &[v(F, 2)]);
    b.add("packSnorm2x16", Type::UINT, &[v(F, 2)]);
    b.add("packUnorm4x8", Type::UINT, &[v(F, 4)]);
    b.add("packSnorm4x8", Type::UINT, &[v(F, 4)]);
    b.add("packHalf2x16", Type::UINT, &[v(F, 2)]);
    b.add("unpackUnorm2x16", v(F, 2), &[Type::UINT]);
    b.add("unpackSnorm2x16", v(F, 2), &[Type::UINT]);
    b.add("unpackUnorm4x8", v(F, 4), &[Type::UINT]);
    b.add("unpackSnorm4x8", v(F, 4), &[Type::UINT]);
    b.add("unpackHalf2x16", v(F, 2), &[Type::UINT]);
    // Matrix functions.
    for c in 2..=4u8 {
        for r in 2..=4u8 {
            let m = Type::Matrix(c, r);
            b.add("matrixCompMult", m.clone(), &[m.clone(), m.clone()]);
            b.add("outerProduct", m.clone(), &[v(F, r), v(F, c)]);
            b.add("transpose", Type::Matrix(r, c), std::slice::from_ref(&m));
        }
        b.add("determinant", Type::FLOAT, &[Type::Matrix(c, c)]);
        b.add("inverse", Type::Matrix(c, c), &[Type::Matrix(c, c)]);
    }
    // Vector relational.
    for name in [
        "lessThan",
        "lessThanEqual",
        "greaterThan",
        "greaterThanEqual",
        "equal",
        "notEqual",
    ] {
        let with_bool = name == "equal" || name == "notEqual";
        for s in [F, I, U, B] {
            if s == B && !with_bool {
                continue;
            }
            for n in 2..=4 {
                b.add(name, v(B, n), &[v(s, n), v(s, n)]);
            }
        }
    }
    for n in 2..=4 {
        b.add("any", Type::BOOL, &[v(B, n)]);
        b.add("all", Type::BOOL, &[v(B, n)]);
        b.add("not", v(B, n), &[v(B, n)]);
    }
    // Derivatives (fragment only).
    for name in [
        "dFdx",
        "dFdy",
        "fwidth",
        "dFdxFine",
        "dFdyFine",
        "dFdxCoarse",
        "dFdyCoarse",
        "fwidthFine",
        "fwidthCoarse",
    ] {
        for n in 1..=4 {
            b.add(name, v(F, n), &[v(F, n)]).fragment_only = true;
        }
    }
    textures(&mut b);

    let mut by_name: HashMap<&'static str, Vec<u32>> = HashMap::new();
    for (i, f) in b.list.iter().enumerate() {
        by_name.entry(f.name).or_default().push(i as u32);
    }
    Builtins {
        list: b.list,
        by_name,
    }
}

fn textures(b: &mut Builder) {
    use SamplerKind::*;
    for k in [S2D, S3D, Cube, S2DArray, S2DShadow] {
        let s = Type::Sampler(k);
        let (coord, size, deriv, off, res): (Type, Type, Type, Option<Type>, Type) = match k {
            S2D => (v(F, 2), v(I, 2), v(F, 2), Some(v(I, 2)), v(F, 4)),
            S3D => (v(F, 3), v(I, 3), v(F, 3), Some(v(I, 3)), v(F, 4)),
            Cube => (v(F, 3), v(I, 2), v(F, 3), None, v(F, 4)),
            S2DArray => (v(F, 3), v(I, 3), v(F, 2), Some(v(I, 2)), v(F, 4)),
            S2DShadow => (v(F, 3), v(I, 2), v(F, 2), Some(v(I, 2)), Type::FLOAT),
        };
        b.add("textureSize", size, &[s.clone(), Type::INT]);
        b.add("textureQueryLevels", Type::INT, std::slice::from_ref(&s));
        let t = b.add("texture", res.clone(), &[s.clone(), coord.clone()]);
        t.implicit_lod = true;
        t.fragment_only = false;
        b.add(
            "texture",
            res.clone(),
            &[s.clone(), coord.clone(), Type::FLOAT],
        )
        .implicit_lod = true;
        b.add(
            "textureLod",
            res.clone(),
            &[s.clone(), coord.clone(), Type::FLOAT],
        );
        b.add(
            "textureGrad",
            res.clone(),
            &[s.clone(), coord.clone(), deriv.clone(), deriv.clone()],
        );
        if let Some(off) = off {
            b.add(
                "textureOffset",
                res.clone(),
                &[s.clone(), coord.clone(), off.clone()],
            )
            .implicit_lod = true;
            b.add(
                "textureOffset",
                res.clone(),
                &[s.clone(), coord.clone(), off.clone(), Type::FLOAT],
            )
            .implicit_lod = true;
            b.add(
                "textureLodOffset",
                res.clone(),
                &[s.clone(), coord.clone(), Type::FLOAT, off.clone()],
            );
            b.add(
                "textureGradOffset",
                res.clone(),
                &[
                    s.clone(),
                    coord.clone(),
                    deriv.clone(),
                    deriv.clone(),
                    off.clone(),
                ],
            );
            if k != S2DShadow {
                let icoord = coord.with_scalar(I);
                b.add(
                    "texelFetch",
                    res.clone(),
                    &[s.clone(), icoord.clone(), Type::INT],
                );
                b.add(
                    "texelFetchOffset",
                    res.clone(),
                    &[s.clone(), icoord, Type::INT, off],
                );
            }
        }
        match k {
            S2D => {
                for pc in [v(F, 3), v(F, 4)] {
                    b.add("textureProj", res.clone(), &[s.clone(), pc.clone()])
                        .implicit_lod = true;
                    b.add(
                        "textureProj",
                        res.clone(),
                        &[s.clone(), pc.clone(), Type::FLOAT],
                    )
                    .implicit_lod = true;
                    b.add("textureProjLod", res.clone(), &[s.clone(), pc, Type::FLOAT]);
                }
                b.add("textureGather", v(F, 4), &[s.clone(), v(F, 2)]);
                b.add("textureGather", v(F, 4), &[s.clone(), v(F, 2), Type::INT]);
                // Legacy names, accepted as a superset of GLSL 3.30 core.
                let t = b.add("texture2D", res.clone(), &[s.clone(), v(F, 2)]);
                t.emit = "texture";
                t.implicit_lod = true;
                let t = b.add("texture2D", res.clone(), &[s.clone(), v(F, 2), Type::FLOAT]);
                t.emit = "texture";
                t.implicit_lod = true;
                b.add(
                    "texture2DLod",
                    res.clone(),
                    &[s.clone(), v(F, 2), Type::FLOAT],
                )
                .emit = "textureLod";
            }
            S2DArray | Cube => {
                b.add("textureGather", v(F, 4), &[s.clone(), coord.clone()]);
                b.add(
                    "textureGather",
                    v(F, 4),
                    &[s.clone(), coord.clone(), Type::INT],
                );
            }
            S2DShadow => {
                b.add("textureGather", v(F, 4), &[s.clone(), v(F, 2), Type::FLOAT]);
            }
            S3D => {
                b.add("textureProj", res.clone(), &[s.clone(), v(F, 4)])
                    .implicit_lod = true;
                b.add(
                    "textureProjLod",
                    res.clone(),
                    &[s.clone(), v(F, 4), Type::FLOAT],
                );
            }
        }
    }
}

/// Explicit-LOD replacement of an implicit-LOD lookup, for vertex shaders.
pub fn explicit_lod_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "texture" | "texture2D" => "textureLod",
        "textureOffset" => "textureLodOffset",
        "textureProj" => "textureProjLod",
        _ => return None,
    })
}

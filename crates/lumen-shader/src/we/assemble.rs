//! Final source of a unit (spec 03 §9): header with the dialect macros,
//! unit-specific definitions, combo `#define`s, body, varying adaptations.

use std::fmt::Write as _;

use crate::glsl::Stage;

/// Dialect macros of spec 03 §3.3 (written for Lumen; textual, C semantics).
fn header(stage: Stage, negate_ddy: bool) -> String {
    let mut h = String::with_capacity(1024);
    h.push_str("// Lumen: Wallpaper Engine shader dialect\nprecision highp float;\n");
    // `mul(a, b)` is `b * a` (HLSL row-vector convention).
    h.push_str("#define mul(a, b) ((b) * (a))\n");
    // Arguments swapped so that `max(0.0, v)` works with a vector `v`.
    h.push_str("#define max(a, b) max(b, a)\n");
    h.push_str("#define lerp mix\n#define frac fract\n");
    h.push_str("#define saturate(x) clamp(x, 0.0, 1.0)\n");
    h.push_str(
        "#define CAST2(x) (vec2(x))\n#define CAST3(x) (vec3(x))\n#define CAST4(x) (vec4(x))\n",
    );
    h.push_str("#define CAST3X3(x) (mat3(x))\n");
    h.push_str("#define float2 vec2\n#define float3 vec3\n#define float4 vec4\n");
    h.push_str("#define int2 ivec2\n#define int3 ivec3\n#define int4 ivec4\n");
    h.push_str("#define texSample2D texture\n#define texSample2DLod textureLod\n");
    // Not in §3.3: volume lookups of `sampler3D` slots (colour LUTs, `ccsimple.frag` with `LUT`).
    h.push_str("#define texSample3D texture\n#define texSample3DLod textureLod\n");
    // log10(2) = 0.301029995663981
    h.push_str("#define log10(x) (log2(x) * 0.301029995663981)\n");
    h.push_str("#define atan2 atan\n");
    // HLSL fmod: result has the sign of x.
    h.push_str("#define fmod(x, y) ((x) - (y) * trunc((x) / (y)))\n");
    h.push_str("#define ddx dFdx\n");
    if negate_ddy {
        h.push_str("#define ddy(x) dFdy(-(x))\n");
    } else {
        // wgpu framebuffer y already points down like Direct3D (spec 03 §16).
        h.push_str("#define ddy dFdy\n");
    }
    h.push_str("#define GLSL 1\n\n");
    match stage {
        Stage::Fragment => h.push_str("out vec4 out_FragColor;\n#define varying in\n"),
        Stage::Vertex => h.push_str("#define attribute in\n#define varying out\n"),
    }
    h
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Parses `varying <ty> <name> ;` starting at `pos` (which points at "varying").
fn parse_varying_decl(text: &str, pos: usize) -> Option<(&str, &str, usize)> {
    let b = text.as_bytes();
    if pos > 0 && is_ident(b[pos - 1]) {
        return None;
    }
    let mut i = pos + "varying".len();
    let ws = |i: &mut usize| {
        let s = *i;
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\r' | b'\n') {
            *i += 1;
        }
        *i > s
    };
    let word = |i: &mut usize| {
        let s = *i;
        while *i < b.len() && is_ident(b[*i]) {
            *i += 1;
        }
        &text[s..*i]
    };
    if !ws(&mut i) {
        return None;
    }
    let ty = word(&mut i);
    if !ws(&mut i) {
        return None;
    }
    let name = word(&mut i);
    ws(&mut i);
    if name.is_empty() || b.get(i) != Some(&b';') {
        return None;
    }
    Some((ty, name, i + 1))
}

fn varying_decls(text: &str) -> Vec<(String, String, usize, usize)> {
    let mut v = Vec::new();
    let mut from = 0;
    while let Some(off) = text[from..].find("varying") {
        let pos = from + off;
        if let Some((ty, name, end)) = parse_varying_decl(text, pos) {
            v.push((ty.to_string(), name.to_string(), pos, end));
        }
        from = pos + 7;
    }
    v
}

/// §9.1: vertex `varying vec2 X` read as `vec4 X` by the fragment unit.
pub fn widen_vertex_varyings(vertex: &str, fragment: &str) -> String {
    let wide: Vec<String> = varying_decls(fragment)
        .into_iter()
        .filter(|d| d.0 == "vec4")
        .map(|d| d.1)
        .collect();
    let mut out = vertex.to_string();
    for name in wide {
        let decls: Vec<_> = varying_decls(&out)
            .into_iter()
            .filter(|d| d.0 == "vec2" && d.1 == name)
            .collect();
        if decls.is_empty() {
            continue;
        }
        for d in decls.iter().rev() {
            out.replace_range(d.2..d.3, &format!("varying vec4 {name};"));
        }
        let mut res = String::with_capacity(out.len() + 64);
        for line in out.split_inclusive('\n') {
            res.push_str(&rewrite_assignment(line, &name).unwrap_or_else(|| line.to_string()));
        }
        out = res;
    }
    out
}

fn rewrite_assignment(line: &str, name: &str) -> Option<String> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent..];
    let after = rest.strip_prefix(name)?;
    if after.as_bytes().first().is_some_and(|&c| is_ident(c)) {
        return None;
    }
    let after_ws = after.trim_start_matches([' ', '\t']);
    let expr_part = after_ws.strip_prefix('=')?;
    if expr_part.starts_with('=') {
        return None;
    }
    let semi = expr_part.find(';')?;
    if expr_part[..semi].contains('\n') {
        return None;
    }
    let expr = expr_part[..semi].trim();
    let tail = &expr_part[semi + 1..];
    Some(format!(
        "{}{name} = vec4({expr}, 0.0, 1.0);{tail}",
        &line[..indent]
    ))
}

/// §9.2: `v_TexCoord` (vec3/vec4) combined with `CAST2(…)` becomes `v_TexCoord.xy`.
pub fn adapt_texcoord(fragment: &str) -> String {
    if !varying_decls(fragment)
        .iter()
        .any(|d| d.1 == "v_TexCoord" && (d.0 == "vec3" || d.0 == "vec4"))
    {
        return fragment.to_string();
    }
    let b = fragment.as_bytes();
    let name = "v_TexCoord";
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(off) = fragment[from..].find(name) {
        let pos = from + off;
        let end = pos + name.len();
        from = end;
        if (pos > 0 && is_ident(b[pos - 1])) || b.get(end).is_some_and(|&c| is_ident(c)) {
            continue;
        }
        // v_TexCoord op CAST2(
        let right = fragment[end..].trim_start_matches([' ', '\t']);
        let right_ok = right.starts_with(['+', '-', '*', '/'])
            && !right[1..].starts_with('=')
            && right[1..]
                .trim_start_matches([' ', '\t'])
                .starts_with("CAST2(");
        // CAST2(...) op v_TexCoord, without ')' inside the argument.
        let left = fragment[..pos].trim_end_matches([' ', '\t']);
        let left_ok = left.ends_with(['+', '-', '*', '/']) && {
            let l2 = left[..left.len() - 1].trim_end_matches([' ', '\t']);
            l2.ends_with(')')
                && l2[..l2.len() - 1]
                    .rfind("CAST2(")
                    .is_some_and(|c| !l2[c + 6..l2.len() - 1].contains(')'))
        };
        if right_ok || left_ok {
            hits.push(end);
        }
    }
    let mut out = fragment.to_string();
    for end in hits.into_iter().rev() {
        out.insert_str(end, ".xy");
    }
    out
}

/// Builds the complete source of a unit.
pub fn assemble(stage: Stage, defines: &str, body: &str, negate_ddy: bool) -> String {
    let mut s = header(stage, negate_ddy);
    let _ = write!(s, "{defines}");
    s.push_str(body);
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widened_vertex_varyings() {
        let vs = "varying vec2 v_TexCoord;\nvoid main() {\n    v_TexCoord = a_TexCoord;\n    v_TexCoord.x += 1.0;\n    v_TexCoord += 1.0;\n}\n";
        let fs = "varying  vec4   v_TexCoord ;\n";
        let out = widen_vertex_varyings(vs, fs);
        assert!(out.contains("varying vec4 v_TexCoord;"));
        assert!(out.contains("    v_TexCoord = vec4(a_TexCoord, 0.0, 1.0);"));
        assert!(out.contains("v_TexCoord.x += 1.0;"));
        assert!(out.contains("    v_TexCoord += 1.0;"));
    }

    #[test]
    fn texcoord_with_cast2() {
        let fs = "varying vec4 v_TexCoord;\nvoid main() { vec2 a = v_TexCoord + CAST2(1.0); vec2 b = CAST2(g) * v_TexCoord; vec2 c = v_TexCoord.xy * CAST2(2.0); vec2 d = CAST2(f(x)) * v_TexCoord; }\n";
        let out = adapt_texcoord(fs);
        assert!(out.contains("a = v_TexCoord.xy + CAST2(1.0)"));
        assert!(out.contains("b = CAST2(g) * v_TexCoord.xy"));
        assert!(out.contains("c = v_TexCoord.xy * CAST2(2.0)"));
        assert!(out.contains("d = CAST2(f(x)) * v_TexCoord;"));
    }
}

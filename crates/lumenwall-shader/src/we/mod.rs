//! Wallpaper Engine textual preprocessing of a shader unit (spec 03 §4):
//! includes, `#require`, line scan for combos and annotations, `gl_FragColor`.
//! These are raw substring operations that ignore comments and conditionals,
//! exactly like the reference.

pub mod annotations;
pub mod assemble;
pub mod combos;
pub mod material;

use serde_json::{Map, Value};

use crate::ShaderError;
use crate::fs::{ShaderFs, read_shader_file, with_extension};

/// Maximum include nesting (cycles are detected separately).
const MAX_INCLUDE_DEPTH: usize = 64;

/// A `// [COMBO] {…}` declaration or a sampler annotation, in line order.
#[derive(Clone, Debug)]
pub enum ScanItem {
    Combo(Map<String, Value>),
    Annotation(RawAnnotation),
}

/// `uniform <type> <name>; // {json}` as extracted by spec 03 §4.3.
#[derive(Clone, Debug)]
pub struct RawAnnotation {
    pub glsl_type: String,
    pub name: String,
    pub json: Map<String, Value>,
}

/// One unit after textual preprocessing.
#[derive(Clone, Debug)]
pub struct ScannedUnit {
    pub body: String,
    pub items: Vec<ScanItem>,
    pub warnings: Vec<String>,
}

fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map_or(0, |i| i + 1)
}

fn next_line_start(text: &str, pos: usize) -> usize {
    text[pos..].find('\n').map_or(text.len(), |i| pos + i + 1)
}

/// Name between the first `"` after `from` and the next `"` (empty if none).
fn quoted_name(text: &str) -> &str {
    let Some(a) = text.find('"') else { return "" };
    let rest = &text[a + 1..];
    rest.find('"').map_or("", |b| &rest[..b])
}

fn include_block(
    fs: &dyn ShaderFs,
    name: &str,
    stack: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Result<String, ShaderError> {
    let path = with_extension(name, "h");
    let Some(content) = read_shader_file(fs, &path) else {
        warnings.push(format!("include '{name}' not found"));
        return Ok(format!("// [lumenwall] include not found: {name}\n"));
    };
    if stack.contains(&path) {
        return Err(ShaderError::Preprocess(format!(
            "include cycle through '{path}'"
        )));
    }
    if stack.len() >= MAX_INCLUDE_DEPTH {
        return Err(ShaderError::Preprocess(format!(
            "includes nested too deeply at '{path}'"
        )));
    }
    stack.push(path);
    let inner = expand_nested(fs, &content, stack, warnings)?;
    stack.pop();
    let nl = if inner.ends_with('\n') { "" } else { "\n" };
    Ok(format!(
        "// [lumenwall] begin include {name}\n{inner}{nl}// [lumenwall] end include {name}\n"
    ))
}

/// §4.1.2: every line containing `#include` is replaced from the directive to
/// the end of the line by the included content, expanded recursively in place.
fn expand_nested(
    fs: &dyn ShaderFs,
    content: &str,
    stack: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Result<String, ShaderError> {
    let mut out = String::with_capacity(content.len());
    for line in content.split_inclusive('\n') {
        match line.find("#include") {
            Some(p) => {
                out.push_str(&line[..p]);
                if p > 0 && !line[..p].ends_with('\n') {
                    out.push('\n');
                }
                let name = quoted_name(&line[p..]).to_string();
                out.push_str(&include_block(fs, &name, stack, warnings)?);
            }
            None => out.push_str(line),
        }
    }
    Ok(out)
}

/// §4.1.3: where the include block goes in the main file.
fn insertion_point(text: &str) -> Result<usize, ShaderError> {
    let b = text.as_bytes();
    let mut from = 0;
    let main_pos = loop {
        let Some(i) = text[from..].find(" main") else {
            return Err(ShaderError::Preprocess(
                "no recognisable main() function".into(),
            ));
        };
        let p = from + i;
        match b.get(p + 5) {
            Some(b' ') | Some(b'(') => break p,
            _ => from = p + 1,
        }
    };
    let main_line = line_start(text, main_pos);
    let before = &text[..main_pos];
    let k = ["attribute", "varying", "uniform"]
        .iter()
        .filter_map(|w| before.rfind(w))
        .max();
    let mut p = match k {
        Some(k) => next_line_start(text, k),
        None => main_line,
    };
    // Leave conditional blocks: pair #if/#endif with a stack, in #endif order.
    let mut stack: Vec<usize> = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find('#') {
        let pos = i + off;
        let rest = &text[pos..];
        if rest.starts_with("#if") {
            stack.push(pos);
        } else if rest.starts_with("#endif")
            && let Some(if_pos) = stack.pop()
            && p > if_pos
            && p <= pos
        {
            // Open question 12: an `#if` on the first line moves P to the very start.
            p = line_start(text, if_pos);
        }
        i = pos + 1;
    }
    Ok(p.min(main_line))
}

const LIGHTING_V1: &str = "vec3 PerformLighting_V1(vec3 worldPos, vec3 albedo, vec3 normal, vec3 viewDir, vec3 specularTint, vec3 baseReflectance, float roughness, float metallic) { return vec3(0.0); }\n";

/// §4.2 `#require` modules.
fn apply_requires(text: &mut String, warnings: &mut Vec<String>) {
    let mut from = 0;
    while let Some(off) = text[from..].find("#require") {
        let pos = from + off;
        let after = pos + "#require".len();
        let line_end = text[after..].find('\n').map_or(text.len(), |i| after + i);
        let rest = &text[after..line_end];
        let Some(name) = rest.strip_prefix(' ') else {
            warnings.push("#require without module name".into());
            from = after;
            continue;
        };
        let name = name.trim_end_matches([' ', '\r']).to_string();
        if name.is_empty() {
            warnings.push("#require without module name".into());
            from = after;
            continue;
        }
        text.replace_range(pos..pos + 2, "//");
        if name == "LightingV1" {
            text.insert_str(pos, LIGHTING_V1);
            from = pos + LIGHTING_V1.len() + 2;
        } else {
            warnings.push(format!("unknown #require module '{name}'"));
            from = pos + 2;
        }
    }
}

/// §4.3 line scan.
fn scan_lines(text: &str, warnings: &mut Vec<String>) -> Vec<ScanItem> {
    let mut items = Vec::new();
    for line in text.split_inclusive('\n') {
        let Some(l) = line.strip_suffix('\n') else {
            continue;
        };
        if let Some(p) = l.find("// [COMBO] ") {
            match serde_json::from_str::<Value>(&l[p + 11..]) {
                Ok(Value::Object(m)) => items.push(ScanItem::Combo(m)),
                _ => warnings.push(format!("invalid [COMBO] declaration: {}", l.trim())),
            }
            continue;
        }
        if !(l.contains("uniform ") && l.contains("// ") && l.contains(';')) {
            continue;
        }
        let semi = l.find(';').expect("contains ;");
        let cmt = l.find("// ").expect("contains //");
        if semi > cmt {
            continue;
        }
        let before = &l[..semi];
        let Some(last_sp) = before.rfind(' ') else {
            continue;
        };
        let prev = before[..last_sp].rfind(' ').map_or(0, |i| i + 1);
        let name = before[last_sp + 1..].to_string();
        let glsl_type = before[prev..last_sp].to_string();
        let json_txt = &l[l.find("//").expect("contains //") + 2..];
        match serde_json::from_str::<Value>(json_txt) {
            Ok(Value::Object(json)) => items.push(ScanItem::Annotation(RawAnnotation {
                glsl_type,
                name,
                json,
            })),
            _ => warnings.push(format!("invalid uniform annotation: {}", l.trim())),
        }
    }
    items
}

/// Runs §4.1–§4.4 on one unit.
pub fn preprocess_unit(fs: &dyn ShaderFs, source: &str) -> Result<ScannedUnit, ShaderError> {
    let mut warnings = Vec::new();
    let mut text = source.to_string();
    let mut block = String::new();
    let mut stack = Vec::new();
    let mut from = 0;
    while let Some(off) = text[from..].find("#include") {
        let pos = from + off;
        let name = quoted_name(&text[pos + 8..]).to_string();
        text.replace_range(pos..pos + 2, "//");
        block.push_str(&include_block(fs, &name, &mut stack, &mut warnings)?);
        from = pos + 8;
    }
    let p = insertion_point(&text)?;
    if !block.is_empty() {
        block.push('\n');
        text.insert_str(p, &block);
    }
    apply_requires(&mut text, &mut warnings);
    let items = scan_lines(&text, &mut warnings);
    let body = text.replace("gl_FragColor", "out_FragColor");
    Ok(ScannedUnit {
        body,
        items,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;

    #[test]
    fn include_insertion_after_last_declaration() {
        let mut fs = MemFs::default();
        fs.insert("shaders/common.h", "float common_fn() { return 1.0; }\n");
        let src = "#include \"common.h\"\nuniform float g_A;\nvarying vec2 v_UV;\n\nfloat helper() { return 2.0; }\nvoid main() {}\n";
        let u = preprocess_unit(&fs, src).unwrap();
        let inc = u.body.find("common_fn").unwrap();
        assert!(u.body.find("varying vec2 v_UV").unwrap() < inc);
        assert!(inc < u.body.find("float helper").unwrap());
        assert!(u.body.starts_with("//nclude"));
    }

    #[test]
    fn include_leaves_conditional_blocks() {
        let mut fs = MemFs::default();
        fs.insert("shaders/a.h", "float A;\n");
        let src = "#include \"a.h\"\n#if GS_ENABLED\nuniform float x;\nvoid main() {}\n#else\nvoid main() {}\n#endif\n";
        let u = preprocess_unit(&fs, src).unwrap();
        assert!(u.body.find("float A;").unwrap() < u.body.find("#if GS_ENABLED").unwrap());
    }

    #[test]
    fn nested_includes_and_cycles() {
        let mut fs = MemFs::default();
        fs.insert("shaders/a.h", "// a\n#include \"b.h\"\nfloat afn;\n");
        fs.insert("shaders/b.h", "float bfn;\n");
        let u = preprocess_unit(&fs, "#include \"a.h\"\nvoid main() {}\n").unwrap();
        assert!(u.body.find("bfn").unwrap() < u.body.find("afn").unwrap());
        let mut fs = MemFs::default();
        fs.insert("shaders/a.h", "#include \"b.h\"\n");
        fs.insert("shaders/b.h", "#include \"a.h\"\n");
        assert!(preprocess_unit(&fs, "#include \"a.h\"\nvoid main() {}\n").is_err());
    }

    #[test]
    fn missing_include_is_a_comment_and_main_is_required() {
        let fs = MemFs::default();
        let u = preprocess_unit(&fs, "#include \"nope.h\"\nvoid main() {}\n").unwrap();
        assert!(u.body.contains("include not found"));
        assert!(preprocess_unit(&fs, "void\tmain() {}\n").is_err());
    }

    #[test]
    fn require_lighting() {
        let fs = MemFs::default();
        let u =
            preprocess_unit(&fs, "#require LightingV1\n#require Other\nvoid main() {}\n").unwrap();
        assert!(u.body.contains("vec3 PerformLighting_V1("));
        assert!(u.body.contains("//equire LightingV1"));
        assert!(u.body.contains("//equire Other"));
        let u = preprocess_unit(&fs, "#require\nvoid main() {}\n").unwrap();
        assert!(u.body.contains("#require\n"));
    }

    #[test]
    fn scan_annotations_and_combos() {
        let fs = MemFs::default();
        let src = "// [COMBO] {\"combo\":\"A\",\"default\":2}\nuniform highp float g_X; // {\"material\":\"x\",\"default\":1}\n// uniform float g_Y; // {\"material\":\"y\"}\nuniform float g_Z; // {bad json}\nvoid main() { gl_FragColor = vec4(0); }\n// [COMBO] {\"combo\":\"LAST\"}";
        let u = preprocess_unit(&fs, src).unwrap();
        assert_eq!(u.items.len(), 2);
        match &u.items[1] {
            ScanItem::Annotation(a) => {
                assert_eq!(a.name, "g_X");
                assert_eq!(a.glsl_type, "float");
            }
            _ => panic!(),
        }
        assert!(u.body.contains("out_FragColor = vec4(0)"));
        assert_eq!(u.warnings.len(), 1);
    }
}

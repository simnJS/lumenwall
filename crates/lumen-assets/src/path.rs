//! Lexical path normalization (spec 01 §4.6), per-asset path conventions (§4.10)
//! and the cleanup of playlist paths from `config.json` (§5.6).

use std::borrow::Cow;
use std::path::Path;

/// Normalizes a requested path lexically, without touching the disk (§4.6).
///
/// `/` is the separator. On Windows `\` is converted to `/` first (QO-25); on other
/// platforms it is an ordinary character. Already-normal paths are returned
/// borrowed.
pub fn normalize(path: &str) -> Cow<'_, str> {
    normalize_with(path, cfg!(windows))
}

/// [`normalize`] with an explicit choice for `\` (separator or ordinary character).
pub fn normalize_with(path: &str, backslash_is_separator: bool) -> Cow<'_, str> {
    if backslash_is_separator && path.contains('\\') {
        let converted = path.replace('\\', "/");
        return Cow::Owned(normalize_slow(&converted));
    }
    if is_normal(path) {
        Cow::Borrowed(path)
    } else {
        Cow::Owned(normalize_slow(path))
    }
}

/// Fast check: no empty, `.` or `..` component (a single trailing `/` is allowed).
fn is_normal(path: &str) -> bool {
    let body = path.strip_prefix('/').unwrap_or(path);
    let mut components = body.split('/').peekable();
    while let Some(component) = components.next() {
        let last = components.peek().is_none();
        match component {
            "" if last => {}
            "" | "." | ".." => return false,
            _ => {}
        }
    }
    true
}

fn normalize_slow(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    // Whether the result ends with a separator: "a/b/" and "a/./" keep one, and so
    // does "a/b/.." (only "b/.." is removed).
    let mut trailing = path.ends_with('/');
    let mut components = path.split('/').filter(|c| !c.is_empty()).peekable();
    while let Some(component) = components.next() {
        let last = components.peek().is_none();
        match component {
            "." => trailing |= last,
            ".." => match stack.last() {
                Some(&previous) if previous != ".." => {
                    stack.pop();
                    trailing |= last;
                }
                // Rule 5: `..` right after the root disappears.
                _ if absolute => trailing |= last,
                // Rule 6: leading `..` of a relative path are kept.
                _ => stack.push(".."),
            },
            other => stack.push(other),
        }
    }

    let mut out = String::with_capacity(path.len());
    if absolute {
        out.push('/');
    }
    out.push_str(&stack.join("/"));
    // A trailing separator never follows `..` and never stands alone.
    if trailing && stack.last().is_some_and(|&c| c != "..") {
        out.push('/');
    }
    if out.is_empty() {
        out.push('.');
    }
    out
}

/// `"/" + n`, normalized, without the leading `/`, for a normalized relative `n`
/// (§4.7 step 3): leading `..` components disappear and `.` becomes empty.
pub(crate) fn rooted_relative(normalized: &str) -> &str {
    if normalized == "." {
        return "";
    }
    let mut rest = normalized;
    loop {
        if rest == ".." {
            return "";
        }
        match rest.strip_prefix("../") {
            Some(tail) => rest = tail,
            None => return rest,
        }
    }
}

/// `materials/<name>.tex`: `.tex` is appended to the logical name, which is kept
/// as is (`foo.png` gives `materials/foo.png.tex`).
pub fn texture_path(name: &str) -> String {
    format!("materials/{name}.tex")
}

/// `materials/<name>.tex-json` (spritesheet metadata).
pub fn texture_json_path(name: &str) -> String {
    format!("materials/{name}.tex-json")
}

/// Shader stage, which selects the extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    /// `.vert`
    Vertex,
    /// `.frag`
    Fragment,
    /// `.h`
    Include,
}

impl ShaderStage {
    /// Extension with its dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Vertex => ".vert",
            Self::Fragment => ".frag",
            Self::Include => ".h",
        }
    }
}

/// Shader name relative to `shaders/`: the last extension of the file name is
/// replaced (or added) — `effects/blur` gives `effects/blur.frag`, and `a/b.c`
/// gives `a/b.frag` (QO-28).
pub fn shader_relative_name(name: &str, stage: ShaderStage) -> String {
    let file_start = name.rfind('/').map_or(0, |i| i + 1);
    let file = &name[file_start..];
    // As with `std::path`, a leading dot (".hidden") is not an extension, and
    // "." / ".." have none.
    let stem_len = match file.rfind('.') {
        Some(dot) if dot > 0 && file != ".." => dot,
        _ => file.len(),
    };
    let mut out = String::with_capacity(file_start + stem_len + 5);
    out.push_str(&name[..file_start + stem_len]);
    out.push_str(stage.extension());
    out
}

/// `shaders/` + [`shader_relative_name`].
pub fn shader_path(name: &str, stage: ShaderStage) -> String {
    format!("shaders/{}", shader_relative_name(name, stage))
}

/// Workshop compatibility substitute for a shader name relative to `shaders/`
/// (§4.10.1): `workshop/<id>/<x>/<f>[/…]` gives `zcompat/scene/shaders/<id>/<f>`.
/// Names with fewer than 4 components have none (QO-28).
pub fn workshop_compat_path(relative_name: &str) -> Option<String> {
    let mut components = relative_name.split('/').filter(|c| !c.is_empty());
    if components.next()? != "workshop" {
        return None;
    }
    let id = components.next()?;
    let _third = components.next()?;
    let file = components.next()?;
    Some(format!("zcompat/scene/shaders/{id}/{file}"))
}

/// Converts a Windows playlist path from `config.json` (§5.6, steps 1 to 6).
/// Step 7, which needs the disk, is [`clean_config_path`].
pub fn clean_config_path_lexical(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let without_prefix = raw.strip_prefix(r"\\?\").unwrap_or(raw);
    let mut path = without_prefix.replace('\\', "/");
    // A `:` byte at index 1 implies an ASCII first character (UTF-8 continuation
    // bytes are never `:`), so index 2 is a character boundary.
    if path.as_bytes().get(1) == Some(&b':') && path.is_char_boundary(2) {
        path.drain(..2);
    }
    if !path.is_empty() && !path.starts_with('/') {
        path.insert(0, '/');
    }
    normalize_with(&path, false).into_owned()
}

/// Full §5.6 cleanup: lexical steps, then a path naming an existing regular file is
/// replaced by its parent folder.
pub fn clean_config_path(raw: &str) -> String {
    let cleaned = clean_config_path_lexical(raw);
    let as_path = Path::new(&cleaned);
    if as_path.is_file()
        && let Some(parent) = as_path.parent()
    {
        return parent.to_string_lossy().into_owned();
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(path: &str) -> String {
        normalize_with(path, false).into_owned()
    }

    #[test]
    fn spec_examples() {
        assert_eq!(n("./project.json"), "project.json");
        assert_eq!(n("materials//x/../y.tex"), "materials/y.tex");
        assert_eq!(n("/../a"), "/a");
        assert_eq!(n("../a"), "../a");
        assert_eq!(n("a/.."), ".");
        assert_eq!(n("a/b/"), "a/b/");
        assert_eq!(n("a/./"), "a/");
        assert_eq!(n("./materials//x.tex"), "materials/x.tex");
    }

    #[test]
    fn more_cases() {
        assert_eq!(n(""), "");
        assert_eq!(n("/"), "/");
        assert_eq!(n("//a//b"), "/a/b");
        assert_eq!(n("a/b/.."), "a/");
        assert_eq!(n("a/b/../.."), ".");
        assert_eq!(n("a/../../b"), "../b");
        assert_eq!(n("../"), "..");
        assert_eq!(n("/.."), "/");
        assert_eq!(n("/a/../.."), "/");
        assert_eq!(n("."), ".");
        assert_eq!(n("a/."), "a/");
        assert_eq!(n("a\\b/../c"), "c");
        assert_eq!(normalize_with("a\\b/../c", true), "a/c");
    }

    #[test]
    fn normal_paths_are_borrowed() {
        assert!(matches!(
            normalize_with("materials/x.tex", false),
            Cow::Borrowed(_)
        ));
        assert!(matches!(normalize_with("/a/b/", false), Cow::Borrowed(_)));
        assert!(matches!(normalize_with("a/./b", false), Cow::Owned(_)));
    }

    #[test]
    fn rooted() {
        assert_eq!(rooted_relative("../x"), "x");
        assert_eq!(rooted_relative("../../x/y"), "x/y");
        assert_eq!(rooted_relative(".."), "");
        assert_eq!(rooted_relative("."), "");
        assert_eq!(rooted_relative("a/b"), "a/b");
    }

    #[test]
    fn asset_paths() {
        assert_eq!(texture_path("foo.png"), "materials/foo.png.tex");
        assert_eq!(texture_json_path("a/b"), "materials/a/b.tex-json");
        assert_eq!(
            shader_path("effects/blur", ShaderStage::Fragment),
            "shaders/effects/blur.frag"
        );
        assert_eq!(
            shader_relative_name("a/b.c", ShaderStage::Fragment),
            "a/b.frag"
        );
        assert_eq!(
            shader_relative_name("a.b/c", ShaderStage::Vertex),
            "a.b/c.vert"
        );
        assert_eq!(
            shader_relative_name("common", ShaderStage::Include),
            "common.h"
        );
        assert_eq!(
            shader_relative_name(".hidden", ShaderStage::Include),
            ".hidden.h"
        );
        assert_eq!(shader_relative_name("x.", ShaderStage::Include), "x.h");
    }

    #[test]
    fn workshop_compat() {
        assert_eq!(
            workshop_compat_path("workshop/123/effects/foo.frag").as_deref(),
            Some("zcompat/scene/shaders/123/foo.frag")
        );
        assert_eq!(
            workshop_compat_path("workshop/123/effects/foo.frag/extra").as_deref(),
            Some("zcompat/scene/shaders/123/foo.frag")
        );
        assert_eq!(workshop_compat_path("workshop/123/foo.frag"), None);
        assert_eq!(workshop_compat_path("workshop/123"), None);
        assert_eq!(workshop_compat_path("effects/a/b/c.frag"), None);
    }

    #[test]
    fn config_paths() {
        assert_eq!(clean_config_path_lexical(""), "");
        assert_eq!(
            clean_config_path_lexical(r"\\?\C:\Steam\steamapps\workshop\content\431960\123"),
            "/Steam/steamapps/workshop/content/431960/123"
        );
        assert_eq!(clean_config_path_lexical(r"D:\a\.\b\..\c"), "/a/c");
        assert_eq!(clean_config_path_lexical("relative/x"), "/relative/x");
    }

    #[test]
    fn config_path_file_becomes_parent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("project.json");
        std::fs::write(&file, b"{}").unwrap();
        let raw = file.to_string_lossy().into_owned();
        if raw.starts_with('/') {
            assert_eq!(clean_config_path(&raw), dir.path().to_string_lossy());
        }
    }
}

//! Shader file access (spec 03 §2): the caller's virtual file system, the
//! engine's virtual `commands/copy` shader and the workshop compatibility
//! substitution.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

/// Read access to the wallpaper's virtual file system. Paths are relative,
/// `/`-separated (`shaders/common.h`). The implementation applies the search
/// order of spec 03 §2.2 (wallpaper folder, `scene.pkg`, assets…).
pub trait ShaderFs: Sync {
    fn read(&self, path: &str) -> Option<Cow<'_, [u8]>>;
}

/// Plain directory.
pub struct DirFs(pub PathBuf);

impl ShaderFs for DirFs {
    fn read(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        if path.split('/').any(|c| c == "..") {
            return None;
        }
        std::fs::read(self.0.join(path)).ok().map(Cow::Owned)
    }
}

/// Several file systems searched in order (first found wins).
pub struct LayeredFs<'a>(pub Vec<&'a dyn ShaderFs>);

impl ShaderFs for LayeredFs<'_> {
    fn read(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        self.0.iter().find_map(|fs| fs.read(path))
    }
}

/// In-memory files (tests, generated content).
#[derive(Default)]
pub struct MemFs(pub HashMap<String, Vec<u8>>);

impl MemFs {
    pub fn insert(&mut self, path: impl Into<String>, content: impl Into<Vec<u8>>) {
        self.0.insert(path.into(), content.into());
    }
}

impl ShaderFs for MemFs {
    fn read(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        self.0.get(path).map(|v| Cow::Borrowed(v.as_slice()))
    }
}

/// Engine-provided copy shader (spec 03 §2.4), written for this crate.
const COPY_VERT: &str = "in vec3 a_Position;\nin vec2 a_TexCoord;\nout vec2 v_TexCoord;\n\nvoid main() {\n    gl_Position = vec4(a_Position, 1.0);\n    v_TexCoord = a_TexCoord;\n}\n";
const COPY_FRAG: &str = "uniform sampler2D g_Texture0;\nin vec2 v_TexCoord;\n\nvoid main() {\n    out_FragColor = texture(g_Texture0, v_TexCoord);\n}\n";

/// Replaces the extension of the last path component, or appends one
/// (spec 03 §2.1). A leading dot does not start an extension.
pub fn with_extension(path: &str, ext: &str) -> String {
    let start = path.rfind('/').map_or(0, |i| i + 1);
    let last = &path[start..];
    match last.rfind('.') {
        Some(d) if d > 0 => format!("{}.{}", &path[..start + d], ext),
        _ => format!("{path}.{ext}"),
    }
}

/// Reads `shaders/<rel>` with the virtual files and the workshop substitution.
/// `rel` already carries its extension.
pub fn read_shader_file(fs: &dyn ShaderFs, rel: &str) -> Option<String> {
    match rel {
        "commands/copy.vert" => return Some(COPY_VERT.to_string()),
        "commands/copy.frag" => return Some(COPY_FRAG.to_string()),
        _ => {}
    }
    let comps: Vec<&str> = rel.split('/').collect();
    if comps.len() >= 4 && comps[0] == "workshop" {
        let alt = format!("zcompat/scene/shaders/{}/{}", comps[1], comps[3]);
        if let Some(b) = fs.read(&alt) {
            tracing::info!("shader '{rel}' replaced by compatibility file '{alt}'");
            return Some(String::from_utf8_lossy(&b).into_owned());
        }
    }
    fs.read(&format!("shaders/{rel}")).map(|b| String::from_utf8_lossy(&b).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(with_extension("common", "h"), "common.h");
        assert_eq!(with_extension("foo.inc", "h"), "foo.h");
        assert_eq!(with_extension("a/b.c", "vert"), "a/b.vert");
        assert_eq!(with_extension("a.b/c", "frag"), "a.b/c.frag");
        assert_eq!(with_extension(".hidden", "h"), ".hidden.h");
    }

    #[test]
    fn workshop_substitution() {
        let mut fs = MemFs::default();
        fs.insert("zcompat/scene/shaders/123/foo.frag", "A");
        fs.insert("shaders/workshop/123/effects/foo.frag", "B");
        fs.insert("shaders/workshop/123/foo.frag", "C");
        assert_eq!(read_shader_file(&fs, "workshop/123/effects/foo.frag").as_deref(), Some("A"));
        // Three components: no substitution.
        assert_eq!(read_shader_file(&fs, "workshop/123/foo.frag").as_deref(), Some("C"));
        assert!(read_shader_file(&fs, "commands/copy.vert").is_some());
    }
}

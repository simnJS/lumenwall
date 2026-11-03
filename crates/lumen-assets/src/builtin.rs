//! Files every wallpaper VFS exposes from memory (spec 01 §4.4.1).
//!
//! Content written for Lumen from the spec's description; JSON is compact.

use crate::vfs::MemoryFiles;

const BLOOM_EFFECT: &str = concat!(
    r#"{"name":"camerabloom_wpengine_linux","group":"wpengine_linux_camera","dependencies":[],"passes":["#,
    r#"{"material":"materials/util/downsample_quarter_bloom.json","target":"_rt_4FrameBuffer","bind":[{"name":"_rt_FullFrameBuffer","index":0}]},"#,
    r#"{"material":"materials/util/downsample_eighth_blur_v.json","target":"_rt_8FrameBuffer","bind":[{"name":"_rt_4FrameBuffer","index":0}]},"#,
    r#"{"material":"materials/util/blur_h_bloom.json","target":"_rt_Bloom","bind":[{"name":"_rt_8FrameBuffer","index":0}]},"#,
    r#"{"material":"materials/util/combine.json","target":"_rt_FullFrameBuffer","bind":[{"name":"_rt_imageLayerComposite_-1_a","index":0},{"name":"_rt_Bloom","index":1}]}"#,
    r#"]}"#
);

const MODEL: &str = r#"{"material":"materials/wpenginelinux.json"}"#;

const MATERIAL: &str = concat!(
    r#"{"passes":[{"blending":"normal","cullmode":"nocull","depthtest":"disabled","depthwrite":"disabled","#,
    r#""shader":"genericimage2","textures":["_rt_FullFrameBuffer"]}]}"#
);

const COPY_VERT: &str = "\
in vec3 a_Position;
in vec2 a_TexCoord;
out vec2 v_TexCoord;

void main() {
\tgl_Position = vec4(a_Position, 1.0);
\tv_TexCoord = a_TexCoord;
}
";

// `out_FragColor` is the output variable of the shader translation pipeline
// (spec 01 §4.4.1; exact convention owned by the shader spec).
const COPY_FRAG: &str = "\
uniform sampler2D g_Texture0;
in vec2 v_TexCoord;

void main() {
\tout_FragColor = texture(g_Texture0, v_TexCoord);
}
";

/// `(path, content)` of the five built-in files, keys without a leading `/`.
pub const BUILTIN_FILES: [(&str, &str); 5] = [
    ("effects/wpenginelinux/bloomeffect.json", BLOOM_EFFECT),
    ("models/wpenginelinux.json", MODEL),
    ("materials/wpenginelinux.json", MATERIAL),
    ("shaders/commands/copy.vert", COPY_VERT),
    ("shaders/commands/copy.frag", COPY_FRAG),
];

/// Memory adapter content mounted first in every wallpaper VFS.
pub fn builtin_files() -> MemoryFiles {
    let mut files = MemoryFiles::default();
    for (path, content) in BUILTIN_FILES {
        files.insert_static(path, content.as_bytes());
    }
    files
}

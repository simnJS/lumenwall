//! The passes of one file set read and preprocess each shader unit and include once
//! (`ShaderFs::identity`), and get exactly what a compiler without that memo prepares.
//! The shaders were written for these tests.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Mutex;

use lumenwall_shader::{
    CompileOptions, MemFs, PassCombos, PassRequest, PreparedPass, ShaderCompiler, ShaderError,
    ShaderFs,
};

const COMMON: &str = "uniform float g_Time;\nfloat pulse(float x) { return sin(x * g_Time); }\n";

const VS: &str = "#include \"common.h\"
attribute vec3 a_Position;
attribute vec2 a_TexCoord;
varying vec2 v_TexCoord;
uniform mat4 g_ModelViewProjectionMatrix;

void main() {
    gl_Position = mul(vec4(a_Position, 1.0), g_ModelViewProjectionMatrix);
    v_TexCoord = a_TexCoord;
}
";

const FS: &str = "#include \"common.h\"
// [COMBO] {\"material\":\"Fast\",\"combo\":\"FAST\",\"type\":\"options\",\"default\":0}
varying vec2 v_TexCoord;
uniform sampler2D g_Texture0; // {\"material\":\"framebuffer\",\"label\":\"Image\"}
uniform float u_Speed; // {\"material\":\"speed\",\"default\":2}

void main() {
#if FAST
    gl_FragColor = texSample2D(g_Texture0, v_TexCoord);
#else
    gl_FragColor = texSample2D(g_Texture0, v_TexCoord) * pulse(u_Speed);
#endif
}
";

/// `MemFs` that counts the reads of each path and reports a chosen identity.
struct CountingFs {
    files: MemFs,
    identity: Option<u64>,
    reads: Mutex<HashMap<String, usize>>,
}

impl CountingFs {
    fn new(identity: Option<u64>, files: &[(&str, &str)]) -> Self {
        let mut fs = MemFs::default();
        for (p, c) in files {
            fs.insert(*p, c.as_bytes());
        }
        Self {
            files: fs,
            identity,
            reads: Mutex::new(HashMap::new()),
        }
    }

    fn reads(&self, path: &str) -> usize {
        self.reads.lock().unwrap().get(path).copied().unwrap_or(0)
    }

    fn total_reads(&self) -> usize {
        self.reads.lock().unwrap().values().sum()
    }
}

impl ShaderFs for CountingFs {
    fn read(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        *self
            .reads
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .or_default() += 1;
        self.files.read(path)
    }

    fn identity(&self) -> Option<u64> {
        self.identity
    }
}

fn files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("shaders/a.vert", VS),
        ("shaders/a.frag", FS),
        ("shaders/b.vert", VS),
        ("shaders/b.frag", FS),
        ("shaders/common.h", COMMON),
    ]
}

fn combos(fast: i64) -> PassCombos {
    let mut c = PassCombos::default();
    c.material.insert("FAST".into(), fast);
    c
}

fn prepare(
    c: &ShaderCompiler,
    fs: &dyn ShaderFs,
    shader: &str,
    combos: &PassCombos,
) -> Result<PreparedPass, ShaderError> {
    c.prepare(fs, &PassRequest { shader, combos })
}

/// Everything a prepared pass carries that the rest of the pipeline reads.
fn summary(p: &PreparedPass) -> String {
    format!(
        "{}\n{}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
        p.vertex_source,
        p.fragment_source,
        p.vertex_params,
        p.fragment_params,
        p.vertex_combos,
        p.fragment_combos,
        p.warnings,
        p.key()
    )
}

/// The passes of a scene: one shader with two combo values, a second shader on the same
/// include, and repeats.
fn requests() -> Vec<(&'static str, PassCombos)> {
    vec![
        ("a", combos(0)),
        ("a", combos(1)),
        ("b", combos(0)),
        ("a", combos(0)),
        ("b", combos(1)),
    ]
}

#[test]
fn one_file_set_reads_each_unit_and_include_once() {
    let c = ShaderCompiler::new(CompileOptions::default(), None);
    let fs = CountingFs::new(Some(7), &files());
    let reference = ShaderCompiler::new(CompileOptions::default(), None);
    let plain = CountingFs::new(None, &files());
    for (shader, combos) in requests() {
        let got = prepare(&c, &fs, shader, &combos).unwrap();
        let want = prepare(&reference, &plain, shader, &combos).unwrap();
        assert_eq!(summary(&got), summary(&want), "{shader} {combos:?}");
    }
    for path in [
        "shaders/a.vert",
        "shaders/a.frag",
        "shaders/b.vert",
        "shaders/b.frag",
        "shaders/common.h",
    ] {
        assert_eq!(fs.reads(path), 1, "{path}");
    }
    // Without an identity every pass reads its two units and their includes.
    assert_eq!(plain.total_reads(), requests().len() * 4);
}

#[test]
fn another_identity_or_a_cleared_cache_reads_again() {
    let c = ShaderCompiler::new(CompileOptions::default(), None);
    let first = CountingFs::new(Some(1), &files());
    prepare(&c, &first, "a", &combos(0)).unwrap();
    prepare(&c, &first, "a", &combos(1)).unwrap();
    // a.vert, its include, a.frag (whose include is already read).
    assert_eq!(first.total_reads(), 3);

    // Another file set: its own files, not the first one's (`b` differs here).
    let mut changed = files();
    changed[3] = (
        "shaders/b.frag",
        "varying vec2 v_TexCoord;\nvoid main() {\n    gl_FragColor = vec4(v_TexCoord, 0.0, 1.0);\n}\n",
    );
    let second = CountingFs::new(Some(2), &changed);
    let b = prepare(&c, &second, "b", &combos(0)).unwrap();
    assert!(b.fragment_source.contains("vec4(v_TexCoord, 0.0, 1.0)"));
    assert!(!b.fragment_source.contains("pulse("));
    assert_eq!(second.reads("shaders/b.frag"), 1);

    c.clear_memory_cache();
    prepare(&c, &second, "b", &combos(0)).unwrap();
    assert_eq!(second.reads("shaders/b.frag"), 2);
}

#[test]
fn errors_are_the_same_and_kept() {
    // The vertex unit has no main() (preprocessing error) and the fragment unit is
    // missing: reading both files comes first, so the missing file is the error.
    let files = [("shaders/c.vert", "attribute vec3 a_Position;\n")];
    let c = ShaderCompiler::new(CompileOptions::default(), None);
    let memo = CountingFs::new(Some(3), &files);
    let plain = CountingFs::new(None, &files);
    for _ in 0..2 {
        let got = prepare(&c, &memo, "c", &PassCombos::default()).unwrap_err();
        let want = prepare(&c, &plain, "c", &PassCombos::default()).unwrap_err();
        assert!(
            matches!(&got, ShaderError::NotFound(p) if p == "c.frag"),
            "{got}"
        );
        assert_eq!(got.to_string(), want.to_string());
    }
    assert_eq!(memo.reads("shaders/c.vert"), 1);
    assert_eq!(memo.reads("shaders/c.frag"), 1);

    // With both units present, the vertex preprocessing error comes out.
    let files = [
        ("shaders/c.vert", "attribute vec3 a_Position;\n"),
        ("shaders/c.frag", FS),
        ("shaders/common.h", COMMON),
    ];
    let memo = CountingFs::new(Some(4), &files);
    let got = prepare(&c, &memo, "c", &PassCombos::default()).unwrap_err();
    assert!(matches!(got, ShaderError::Preprocess(_)), "{got}");
}

#[test]
fn passes_load_in_parallel_with_the_memo() {
    let c = ShaderCompiler::new(CompileOptions::default(), None);
    let fs = CountingFs::new(Some(5), &files());
    let combos: Vec<PassCombos> = (0..16).map(|i| combos(i % 2)).collect();
    let requests: Vec<PassRequest> = combos
        .iter()
        .enumerate()
        .map(|(i, combos)| PassRequest {
            shader: if i % 3 == 0 { "b" } else { "a" },
            combos,
        })
        .collect();
    let sequential: Vec<String> = requests
        .iter()
        .map(|r| summary(&c.prepare(&fs, r).unwrap()))
        .collect();
    assert_eq!(fs.total_reads(), 5);
    let parallel = c.load_passes(&fs, &requests);
    assert_eq!(fs.total_reads(), 5, "the parallel loads read nothing again");
    for (r, (p, want)) in requests.iter().zip(parallel.iter().zip(&sequential)) {
        let p = p.as_ref().unwrap();
        let again = summary(&c.prepare(&fs, r).unwrap());
        assert_eq!(&again, want);
        assert_eq!(p.key, *c.prepare(&fs, r).unwrap().key().as_bytes());
    }
}

#[test]
fn equal_requests_return_the_same_pass() {
    let c = ShaderCompiler::new(CompileOptions::default(), None);
    let fs = CountingFs::new(Some(6), &files());
    let reference = ShaderCompiler::new(CompileOptions::default(), None);
    let plain = CountingFs::new(None, &files());
    let shown = |p: &lumenwall_shader::PassShader| {
        format!(
            "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
            p.key,
            p.vertex_params,
            p.fragment_params,
            p.vertex_combos,
            p.fragment_combos,
            p.warnings
        )
    };
    let mut keys = Vec::new();
    for (shader, combos) in requests() {
        let req = PassRequest {
            shader,
            combos: &combos,
        };
        let got = c.load_pass(&fs, &req).unwrap();
        let want = reference.load_pass(&plain, &req).unwrap();
        assert_eq!(shown(&got), shown(&want), "{shader} {combos:?}");
        keys.push(got.key);
    }
    // a/0 and a/1 differ, a/0 asked twice is one program.
    assert_ne!(keys[0], keys[1]);
    assert_eq!(keys[0], keys[3]);
    assert_eq!(fs.total_reads(), 5);
    // Another identity loads its own passes.
    let other = CountingFs::new(Some(8), &files());
    c.load_pass(
        &other,
        &PassRequest {
            shader: "a",
            combos: &combos(0),
        },
    )
    .unwrap();
    assert_eq!(other.total_reads(), 3);
}

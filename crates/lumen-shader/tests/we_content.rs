//! Compiles real Wallpaper Engine content (never committed; see docs/TESTING.md).
//!
//! `LUMEN_WE_ASSETS=/abs/path/to/wallpaper_engine/assets cargo test -p lumen-shader -- --ignored --nocapture`
//! `LUMEN_TEST_WALLPAPER` (optional) points to an unpacked scene wallpaper; it
//! defaults to `testdata/we/3018340632` when that folder exists.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lumen_shader::{
    CompileOptions, DirFs, LayeredFs, PassCombos, PassRequest, ShaderCompiler, ShaderFs,
};
use serde_json::Value;

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

fn wallpaper_dir() -> Option<PathBuf> {
    env_dir("LUMEN_TEST_WALLPAPER").or_else(|| {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/we/3018340632");
        p.is_dir().then_some(p)
    })
}

fn read_json(fs: &dyn ShaderFs, path: &str) -> Option<Value> {
    serde_json::from_slice(&fs.read(path)?).ok()
}

fn combos_of(v: Option<&Value>) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, v) in o {
            let i = match v {
                Value::Bool(b) => *b as i64,
                Value::Number(n) => n.as_f64().unwrap_or(0.0).trunc() as i64,
                _ => continue,
            };
            m.insert(k.clone(), i);
        }
    }
    m
}

fn slots_of(v: Option<&Value>, out: &mut BTreeSet<u32>) {
    if let Some(Value::Array(a)) = v {
        for (i, t) in a.iter().enumerate() {
            let used = match t {
                Value::String(s) => !s.is_empty(),
                Value::Object(o) => o.get("name").is_some_and(Value::is_string),
                _ => false,
            };
            if used {
                out.insert(i as u32);
            }
        }
    }
}

struct Job {
    label: String,
    shader: String,
    combos: PassCombos,
}

fn material_jobs(
    fs: &dyn ShaderFs,
    path: &str,
    ov: Option<&Value>,
    extra: &BTreeMap<String, i64>,
    label: &str,
    jobs: &mut Vec<Job>,
) {
    let Some(mat) = read_json(fs, path) else {
        println!("  missing material {path}");
        return;
    };
    let Some(passes) = mat.get("passes").and_then(Value::as_array) else {
        return;
    };
    for (i, p) in passes.iter().enumerate() {
        let Some(shader) = p.get("shader").and_then(Value::as_str) else {
            continue;
        };
        let mut used = BTreeSet::new();
        slots_of(p.get("textures"), &mut used);
        slots_of(p.get("usertextures"), &mut used);
        let mut overrides = combos_of(ov.and_then(|o| o.get("combos")));
        if let Some(o) = ov {
            slots_of(o.get("textures"), &mut used);
        }
        for (k, v) in extra {
            overrides.insert(k.clone(), *v);
        }
        let shader = if extra.contains_key("ROPE") {
            "genericropeparticle".to_string()
        } else {
            shader.to_string()
        };
        overrides.remove("ROPE");
        jobs.push(Job {
            label: format!("{label} {path}#{i}"),
            shader,
            combos: PassCombos {
                overrides,
                material: combos_of(p.get("combos")),
                used_slots: used,
            },
        });
    }
}

fn scene_jobs(fs: &dyn ShaderFs) -> Vec<Job> {
    let mut jobs = Vec::new();
    let scene = read_json(fs, "scene.json").expect("scene.json");
    for o in scene
        .get("objects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = o.get("name").and_then(Value::as_str).unwrap_or("?");
        if let Some(model) = o.get("image").and_then(Value::as_str) {
            if let Some(m) = read_json(fs, model)
                .and_then(|m| m.get("material").and_then(Value::as_str).map(str::to_owned))
            {
                material_jobs(fs, &m, None, &BTreeMap::new(), name, &mut jobs);
            }
            for e in o
                .get("effects")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(file) = e.get("file").and_then(Value::as_str) else {
                    continue;
                };
                let Some(effect) = read_json(fs, file) else {
                    println!("  missing effect {file}");
                    continue;
                };
                let overrides: Vec<&Value> = e
                    .get("passes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .collect();
                let mut c = 0;
                for p in effect
                    .get("passes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(mat) = p.get("material").and_then(Value::as_str) else {
                        continue;
                    };
                    material_jobs(
                        fs,
                        mat,
                        overrides.get(c).copied(),
                        &BTreeMap::new(),
                        &format!("{name}/{file}"),
                        &mut jobs,
                    );
                    if c < overrides.len() {
                        c += 1;
                    }
                }
            }
        }
        if let Some(pfile) = o.get("particle").and_then(Value::as_str) {
            let Some(p) = read_json(fs, pfile) else {
                continue;
            };
            let mut extra = BTreeMap::from([("THICKFORMAT".to_string(), 1)]);
            for r in p
                .get("renderer")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match r.get("name").and_then(Value::as_str) {
                    Some("spritetrail") | Some("ropetrail") => {
                        extra.insert("TRAILRENDERER".into(), 1);
                    }
                    Some("rope") => {
                        extra.insert("ROPE".into(), 1);
                    }
                    _ => {}
                }
            }
            if let Some(m) = p.get("material").and_then(Value::as_str) {
                material_jobs(fs, m, None, &extra, &format!("{name}/{pfile}"), &mut jobs);
            }
        }
    }
    jobs
}

/// Also checks that naga's SPIR-V and HLSL back-ends (wgpu's Vulkan and DX12
/// paths) accept the module, and the lumen-render binding contract.
fn backends(s: &lumen_shader::PassShader, label: &str) -> Result<(), String> {
    let m = &s.program.module;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(m)
    .map_err(|e| format!("revalidation: {e}"))?;
    naga::back::spv::write_vec(m, &info, &naga::back::spv::Options::default(), None)
        .map_err(|e| format!("SPIR-V back-end: {e}"))?;
    let (opts, popts) = (
        naga::back::hlsl::Options::default(),
        naga::back::hlsl::PipelineOptions::default(),
    );
    let mut out = String::new();
    naga::back::hlsl::Writer::new(&mut out, &opts, &popts)
        .write(m, &info, None)
        .map_err(|e| format!("HLSL back-end: {e}"))?;
    common::check_render_contract(s, label).map_err(|e| format!("render contract: {e}"))
}

fn run(fs: &dyn ShaderFs, jobs: &[Job]) -> (usize, Vec<(String, String)>) {
    let compiler = ShaderCompiler::new(CompileOptions::default(), None);
    let reqs: Vec<PassRequest> = jobs
        .iter()
        .map(|j| PassRequest {
            shader: &j.shader,
            combos: &j.combos,
        })
        .collect();
    let t = std::time::Instant::now();
    let results = compiler.load_passes(fs, &reqs);
    println!("compiled {} passes in {:?}", jobs.len(), t.elapsed());
    let mut failures = Vec::new();
    let mut ok = 0;
    for (j, r) in jobs.iter().zip(results) {
        match r
            .map_err(|e| e.to_string())
            .and_then(|s| backends(&s, &j.label))
        {
            Ok(()) => ok += 1,
            Err(e) => failures.push((format!("{} [{}]", j.label, j.shader), e)),
        }
    }
    (ok, failures)
}

fn report(what: &str, ok: usize, failures: &[(String, String)]) {
    println!("{what}: {ok}/{} passes compiled", ok + failures.len());
    for (l, e) in failures {
        let first: String = e.lines().take(6).collect::<Vec<_>>().join("\n      ");
        println!("  FAIL {l}\n      {first}");
    }
}

#[test]
#[ignore = "needs LUMEN_WE_ASSETS and the reference wallpaper (docs/TESTING.md)"]
fn reference_wallpaper_shaders() {
    let (Some(assets), Some(wp)) = (env_dir("LUMEN_WE_ASSETS"), wallpaper_dir()) else {
        println!("LUMEN_WE_ASSETS or the reference wallpaper is missing; skipped");
        return;
    };
    let wfs = DirFs(wp);
    let afs = DirFs(assets);
    let fs = LayeredFs(vec![&wfs, &afs]);
    let jobs = scene_jobs(&fs);
    let (ok, failures) = run(&fs, &jobs);
    report("reference wallpaper", ok, &failures);
    assert!(
        failures.is_empty(),
        "{} reference passes failed",
        failures.len()
    );
}

fn walk(dir: &Path, root: &Path, ext: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, root, ext, out);
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

#[test]
#[ignore = "needs LUMEN_WE_ASSETS (docs/TESTING.md)"]
fn assets_shaders() {
    let Some(assets) = env_dir("LUMEN_WE_ASSETS") else {
        println!("LUMEN_WE_ASSETS is missing; skipped");
        return;
    };
    let fs = DirFs(assets.clone());
    // 1. Every vertex/fragment pair with its default combos.
    let mut verts = Vec::new();
    walk(
        &assets.join("shaders"),
        &assets.join("shaders"),
        "vert",
        &mut verts,
    );
    verts.sort();
    let mut jobs = Vec::new();
    let mut variations = Vec::new();
    for v in verts {
        let name = v.trim_end_matches(".vert").to_string();
        // `HLSL/` holds Direct3D sources of the editor, not the GLSL dialect.
        if name.starts_with("HLSL/")
            || !assets
                .join("shaders")
                .join(format!("{name}.frag"))
                .is_file()
        {
            continue;
        }
        // Single-combo variations: every value of each [COMBO] declared by the pair.
        let mut combos: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
        for ext in ["vert", "frag"] {
            let text =
                std::fs::read_to_string(assets.join("shaders").join(format!("{name}.{ext}")))
                    .unwrap_or_default();
            for line in text.lines() {
                let Some(p) = line.find("// [COMBO] ") else {
                    continue;
                };
                let Ok(Value::Object(o)) = serde_json::from_str::<Value>(&line[p + 11..]) else {
                    continue;
                };
                let Some(c) = o.get("combo").and_then(Value::as_str) else {
                    continue;
                };
                let e = combos.entry(c.to_string()).or_default();
                match o.get("options") {
                    Some(Value::Object(opts)) => e.extend(opts.values().filter_map(Value::as_i64)),
                    _ => {
                        e.insert(1);
                    }
                }
            }
            for line in text
                .lines()
                .filter(|l| l.contains("uniform sampler2D") && l.contains("\"combo\""))
            {
                if let Some(Ok(Value::Object(o))) = line
                    .find("//")
                    .map(|p| serde_json::from_str::<Value>(&line[p + 2..]))
                    && let Some(c) = o.get("combo").and_then(Value::as_str)
                {
                    combos.entry(c.to_string()).or_default().insert(1);
                }
            }
        }
        for (c, values) in combos {
            for v in values {
                variations.push(Job {
                    label: format!("{c}={v}"),
                    shader: name.clone(),
                    combos: PassCombos {
                        material: BTreeMap::from([(c.clone(), v)]),
                        ..Default::default()
                    },
                });
            }
        }
        jobs.push(Job {
            label: "pair".into(),
            shader: name,
            combos: PassCombos::default(),
        });
    }
    let (ok1, f1) = run(&fs, &jobs);
    report("assets shader pairs (default combos)", ok1, &f1);
    let (ok3, f3) = run(&fs, &variations);
    report("assets shader pairs (single-combo variations)", ok3, &f3);
    // 2. Every material pass with its combos.
    let mut mats = Vec::new();
    walk(&assets.join("materials"), &assets, "json", &mut mats);
    mats.sort();
    let mut jobs = Vec::new();
    for m in &mats {
        material_jobs(&fs, m, None, &BTreeMap::new(), "material", &mut jobs);
    }
    let (ok2, f2) = run(&fs, &jobs);
    report("assets material passes", ok2, &f2);
}

/// The only `sampler3D` of the assets: `ccsimple.frag` reads a colour LUT (volume texture) in
/// slot 1 when `LUT` is set. Its plain `#if` combos are not `[COMBO]`-annotated, so the
/// variations above never enable them.
#[test]
#[ignore = "needs LUMEN_WE_ASSETS (docs/TESTING.md)"]
fn assets_volume_lut_shader() {
    let Some(assets) = env_dir("LUMEN_WE_ASSETS") else {
        println!("LUMEN_WE_ASSETS is missing; skipped");
        return;
    };
    let fs = DirFs(assets);
    let compiler = ShaderCompiler::new(CompileOptions::default(), None);
    for combos in [
        &[("LUT", 1)][..],
        &[("LUT", 1), ("HDR", 1)],
        &[("LUT", 1), ("COL", 1)],
    ] {
        let combos = PassCombos {
            material: combos.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
            ..Default::default()
        };
        let s = compiler
            .load_pass(
                &fs,
                &PassRequest {
                    shader: "ccsimple",
                    combos: &combos,
                },
            )
            .unwrap_or_else(|e| panic!("ccsimple {:?}: {e}", combos.material));
        backends(&s, "ccsimple").unwrap();
        let m = &s.program.module;
        let volume = m.global_variables.iter().any(|(_, g)| {
            g.binding
                .as_ref()
                .is_some_and(|b| b.group == 1 && b.binding == 2)
                && matches!(
                    m.types[g.ty].inner,
                    naga::TypeInner::Image {
                        dim: naga::ImageDimension::D3,
                        ..
                    }
                )
        });
        assert!(volume, "slot 1 is a texture_3d");
        assert_eq!(
            s.reflection().texture("g_Texture1").map(|t| t.unit),
            Some(1)
        );
    }
}

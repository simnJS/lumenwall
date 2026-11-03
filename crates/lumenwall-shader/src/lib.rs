//! Translation of wallpaper shaders to GPU-ready modules.
//!
//! Pipeline (all at load time, nothing per frame):
//! 1. Wallpaper Engine textual preprocessing ([`we`]): includes, `#require`,
//!    combos, annotations, `gl_FragColor`, final source assembly (spec 03 §4–§9).
//! 2. Tolerant GLSL front-end ([`glsl`]): C preprocessor, parser, typer with the
//!    R1–R7 relaxations, linker (R8), strict Vulkan GLSL 4.50 emission.
//! 3. naga GLSL front-end per stage, merge into one module with the entry
//!    points `vs_main` / `fs_main` ([`merge`]), validation ([`backend`]). The
//!    module follows the binding contract of lumenwall-render and goes to wgpu
//!    through `ShaderSource::Naga` (`lumenwall_render::ShaderDesc`).
//! 4. Reflection ([`reflect`]) and on-disk cache ([`cache`]).
//!
//! Binding contract: `@group(0) @binding(0)` pass uniforms (WE names),
//! `@group(0) @binding(1)` renderer frame block (optional), `@group(1)
//! @binding(2N)` / `(2N + 1)` texture / filtering sampler of slot N. The vertex
//! position stays in the GLSL clip convention (the renderer corrects it).

pub mod backend;
pub mod cache;
pub mod fs;
pub mod glsl;
pub mod merge;
pub mod reflect;
pub mod we;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;

pub use backend::CompiledProgram;
pub use fs::{DirFs, LayeredFs, MemFs, ShaderFs};
pub use glsl::Stage;
pub use reflect::*;
pub use we::annotations::{ConstantValue, SamplerParam, UniformParam, UniformValue, UnitParams};
pub use we::combos::{Combos, PassCombos};
pub use we::material::{TextureCandidate, TextureSources, TextureStacks};

#[derive(Debug, Clone, thiserror::Error)]
pub enum ShaderError {
    #[error("shader file not found: shaders/{0}")]
    NotFound(String),
    #[error("preprocessing: {0}")]
    Preprocess(String),
    #[error("translation: {0}")]
    Frontend(String),
    #[error("naga {stage}: {message}")]
    Backend {
        stage: &'static str,
        message: String,
    },
}

impl From<glsl::FrontendError> for ShaderError {
    fn from(e: glsl::FrontendError) -> Self {
        ShaderError::Frontend(e.to_string())
    }
}

/// Options that change the generated code (part of the cache key).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CompileOptions {
    /// Translate `ddy(x)` as `dFdy(-(x))` like the OpenGL reference. Off by
    /// default: wgpu's framebuffer y axis already points down (spec 03 §16).
    pub negate_ddy: bool,
    /// Read `g_Time`, `g_Daytime`, pointer, texel sizes and audio spectra from
    /// the renderer's frame block (`@group(0) @binding(1)`), updated once per
    /// frame for all passes, instead of per-pass copies. On by default.
    pub frame_globals: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions {
            negate_ddy: false,
            frame_globals: true,
        }
    }
}

/// One pass to load: effective shader name and its combos.
#[derive(Clone, Debug)]
pub struct PassRequest<'a> {
    /// Shader name without extension (`effects/pulse`, `genericimage2`…).
    pub shader: &'a str,
    pub combos: &'a PassCombos,
}

/// Result of the textual stage (cheap); compiling it may hit the caches.
#[derive(Clone, Debug)]
pub struct PreparedPass {
    pub shader: String,
    /// Final sources (spec 03 §9) fed to the GLSL front-end.
    pub vertex_source: String,
    pub fragment_source: String,
    pub vertex_params: UnitParams,
    pub fragment_params: UnitParams,
    /// Combos discovered in each unit (§5.2, §5.3).
    pub vertex_combos: Combos,
    pub fragment_combos: Combos,
    pub warnings: Vec<String>,
    key: blake3::Hash,
}

impl PreparedPass {
    /// Cache key: blake3 of the final sources, the options and the versions.
    pub fn key(&self) -> &blake3::Hash {
        &self.key
    }
}

/// A loaded pass: the shared program plus the per-pass shader metadata.
#[derive(Clone, Debug)]
pub struct PassShader {
    pub program: Arc<CompiledProgram>,
    /// Identity of the program: equal for equal final sources and options
    /// (use it as the renderer's `ShaderId` for pipeline deduplication).
    pub key: [u8; 32],
    pub vertex_params: UnitParams,
    pub fragment_params: UnitParams,
    pub vertex_combos: Combos,
    pub fragment_combos: Combos,
    pub warnings: Vec<String>,
}

impl PassShader {
    pub fn reflection(&self) -> &ProgramReflection {
        &self.program.reflection
    }

    /// Uniform bound to a `constantshadervalues` key (spec 03 §7.3), with its
    /// location in the block. `None` if no annotation uses the key or the
    /// uniform was eliminated.
    pub fn constant(&self, key: &str) -> Option<(&UniformParam, &UniformMember)> {
        let p = we::material::find_constant(
            &self.vertex_params.uniforms,
            &self.fragment_params.uniforms,
            key,
        )?;
        let m = self.reflection().uniform(&p.uniform)?;
        Some((p, m))
    }

    /// Texture candidate stacks of the pass (spec 03 §8.2).
    pub fn texture_stacks(&self, sources: &TextureSources) -> TextureStacks {
        we::material::texture_stacks(
            &self.vertex_params.samplers,
            &self.fragment_params.samplers,
            sources,
        )
    }

    /// Initial content of the uniform block: zeros plus the defaults of the
    /// annotated uniforms that have a material key (spec 03 §12.1, step 1).
    pub fn initial_uniforms(&self) -> Vec<u8> {
        let Some(block) = &self.reflection().uniform_block else {
            return Vec::new();
        };
        let mut buf = vec![0u8; block.size as usize];
        let mut done: Vec<&str> = Vec::new();
        for p in self
            .vertex_params
            .uniforms
            .iter()
            .chain(&self.fragment_params.uniforms)
        {
            if p.material_key.is_none() || done.contains(&p.uniform.as_str()) {
                continue;
            }
            done.push(&p.uniform);
            if let Some(m) = self.reflection().uniform(&p.uniform) {
                p.default.write(m, &mut buf);
            }
        }
        buf
    }
}

type Memo = Mutex<HashMap<[u8; 32], Arc<OnceLock<Result<Arc<CompiledProgram>, ShaderError>>>>>;

/// Compiles passes, deduplicating identical programs in memory and on disk.
/// `Sync`: share one instance and load passes from several threads.
pub struct ShaderCompiler {
    options: CompileOptions,
    cache: Option<cache::DiskCache>,
    memo: Memo,
}

impl ShaderCompiler {
    /// `cache_dir`: directory of the on-disk cache (created if needed), or
    /// `None` to only cache in memory.
    pub fn new(options: CompileOptions, cache_dir: Option<PathBuf>) -> Self {
        let cache = cache_dir.and_then(|d| match cache::DiskCache::new(&d) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("shader cache disabled ({}): {e}", d.display());
                None
            }
        });
        ShaderCompiler {
            options,
            cache,
            memo: Mutex::new(HashMap::new()),
        }
    }

    pub fn options(&self) -> &CompileOptions {
        &self.options
    }

    /// Drops the in-memory program table (call once a scene is loaded; the
    /// passes keep their own `Arc`s, the disk cache is untouched).
    pub fn clear_memory_cache(&self) {
        self.memo.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Textual stage of spec 03 §1 steps 1–5.
    pub fn prepare(
        &self,
        fs: &dyn ShaderFs,
        req: &PassRequest,
    ) -> Result<PreparedPass, ShaderError> {
        let read = |ext: &str| {
            let rel = fs::with_extension(req.shader, ext);
            fs::read_shader_file(fs, &rel).ok_or(ShaderError::NotFound(rel))
        };
        let vs_text = read("vert")?;
        let fs_text = read("frag")?;
        let vs = we::preprocess_unit(fs, &vs_text)?;
        let fsu = we::preprocess_unit(fs, &fs_text)?;
        let mut warnings: Vec<String> = Vec::new();
        warnings.extend(vs.warnings.iter().map(|w| format!("vertex: {w}")));
        warnings.extend(fsu.warnings.iter().map(|w| format!("fragment: {w}")));
        let params = |items: &[we::ScanItem], warnings: &mut Vec<String>| {
            let raw: Vec<&we::RawAnnotation> = items
                .iter()
                .filter_map(|i| match i {
                    we::ScanItem::Annotation(a) => Some(a),
                    _ => None,
                })
                .collect();
            we::annotations::interpret(&raw, warnings)
        };
        let vertex_params = params(&vs.items, &mut warnings);
        let fragment_params = params(&fsu.items, &mut warnings);
        let vertex_combos = we::combos::discover(
            &vs.items,
            &vertex_params.samplers,
            req.combos,
            &mut warnings,
        );
        let fragment_combos = we::combos::discover(
            &fsu.items,
            &fragment_params.samplers,
            req.combos,
            &mut warnings,
        );
        let vs_body = we::assemble::widen_vertex_varyings(&vs.body, &fsu.body);
        let fs_body = we::assemble::adapt_texcoord(&fsu.body);
        let vdefs = we::combos::defines(req.combos, &vertex_combos, &fragment_combos);
        let fdefs = we::combos::defines(req.combos, &fragment_combos, &vertex_combos);
        let vertex_source =
            we::assemble::assemble(Stage::Vertex, &vdefs, &vs_body, self.options.negate_ddy);
        let fragment_source =
            we::assemble::assemble(Stage::Fragment, &fdefs, &fs_body, self.options.negate_ddy);
        let key = self.key(&vertex_source, &fragment_source);
        Ok(PreparedPass {
            shader: req.shader.to_string(),
            vertex_source,
            fragment_source,
            vertex_params,
            fragment_params,
            vertex_combos,
            fragment_combos,
            warnings,
            key,
        })
    }

    fn key(&self, vs: &str, fs: &str) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        h.update(env!("CARGO_PKG_VERSION").as_bytes());
        h.update(&cache::FORMAT_VERSION.to_le_bytes());
        h.update(b"naga30");
        h.update(&[
            self.options.negate_ddy as u8,
            self.options.frame_globals as u8,
        ]);
        h.update(&(vs.len() as u64).to_le_bytes());
        h.update(vs.as_bytes());
        h.update(&(fs.len() as u64).to_le_bytes());
        h.update(fs.as_bytes());
        h.finalize()
    }

    /// Compiles (or fetches) the program of a prepared pass.
    pub fn compile(&self, p: PreparedPass) -> Result<PassShader, ShaderError> {
        let program = self.program(&p.key, &p.vertex_source, &p.fragment_source)?;
        Ok(PassShader {
            program,
            key: *p.key.as_bytes(),
            vertex_params: p.vertex_params,
            fragment_params: p.fragment_params,
            vertex_combos: p.vertex_combos,
            fragment_combos: p.fragment_combos,
            warnings: p.warnings,
        })
    }

    pub fn load_pass(
        &self,
        fs: &dyn ShaderFs,
        req: &PassRequest,
    ) -> Result<PassShader, ShaderError> {
        let p = self.prepare(fs, req)?;
        self.compile(p)
    }

    /// Loads many passes in parallel (rayon). Results are in request order.
    pub fn load_passes(
        &self,
        fs: &dyn ShaderFs,
        reqs: &[PassRequest],
    ) -> Vec<Result<PassShader, ShaderError>> {
        reqs.par_iter().map(|r| self.load_pass(fs, r)).collect()
    }

    /// Compiles already assembled sources (spec 03 §9 format).
    pub fn compile_sources(
        &self,
        vertex: &str,
        fragment: &str,
    ) -> Result<Arc<CompiledProgram>, ShaderError> {
        let key = self.key(vertex, fragment);
        self.program(&key, vertex, fragment)
    }

    fn program(
        &self,
        key: &blake3::Hash,
        vs: &str,
        fs: &str,
    ) -> Result<Arc<CompiledProgram>, ShaderError> {
        let cell = {
            let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
            memo.entry(*key.as_bytes()).or_default().clone()
        };
        cell.get_or_init(|| {
            if let Some(c) = &self.cache
                && let Some(p) = c.load(key)
            {
                return Ok(Arc::new(p));
            }
            let t = glsl::translate(vs, fs, self.options.frame_globals)?;
            for w in &t.warnings {
                tracing::debug!("shader: {w}");
            }
            let p = backend::compile_translated(&t)?;
            if let Some(c) = &self.cache
                && let Err(e) = c.store(key, &p)
            {
                tracing::warn!("cannot write shader cache entry: {e}");
            }
            Ok(Arc::new(p))
        })
        .clone()
    }
}

/// Front-end only: the strict GLSL 4.50 emitted for a pair of assembled
/// sources (debugging aid).
pub fn translate_to_glsl(
    vertex: &str,
    fragment: &str,
    options: &CompileOptions,
) -> Result<(String, String), ShaderError> {
    let t = glsl::translate(vertex, fragment, options.frame_globals)?;
    Ok((t.vertex, t.fragment))
}

//! Real Wallpaper Engine shaders through `lumenwall-shader` (one naga module per pass, lumenwall-render
//! binding contract). A shader that fails to translate is replaced by its stand-in and logged.

use std::collections::{BTreeSet, HashMap};

use lumenwall_assets::Vfs;
use lumenwall_render::shader::ShaderDesc;
use lumenwall_render::{SceneBuilder, ShaderId};
use lumenwall_shader::{
    CompileOptions, PassCombos, PassRequest, ScalarKind, ShaderCompiler, UniformType, UniformValue,
};

use super::{ConstantParam, FallbackShaders, PassKind, PassShader, ShaderProvider, ShaderRequest};
use crate::assets::ShaderSource;

/// `lumenwall-shader` provider with an on-disk cache and per-shader fallback.
pub struct WeShaders {
    compiler: ShaderCompiler,
    /// Program identity (`PassShader::key`) → compiled renderer shader.
    ids: HashMap<[u8; 32], ShaderId>,
    fallback: FallbackShaders,
    translated: usize,
}

impl Default for WeShaders {
    fn default() -> Self {
        Self::new()
    }
}

impl WeShaders {
    pub fn new() -> Self {
        let cache = crate::cache_dir().map(|d| d.join("shaders"));
        Self {
            compiler: ShaderCompiler::new(CompileOptions::default(), cache),
            ids: HashMap::new(),
            fallback: FallbackShaders::new(),
            translated: 0,
        }
    }

    /// Distinct WE programs translated so far.
    pub fn translated(&self) -> usize {
        self.translated
    }

    fn translate(
        &mut self,
        b: &mut SceneBuilder<'_>,
        vfs: &Vfs,
        req: &ShaderRequest<'_>,
    ) -> Result<PassShader, String> {
        let to_map =
            |c: &[(String, i32)]| c.iter().map(|(k, v)| (k.clone(), i64::from(*v))).collect();
        let combos = PassCombos {
            overrides: to_map(req.override_combos),
            material: to_map(req.material_combos),
            used_slots: req.used_slots.iter().copied().collect::<BTreeSet<u32>>(),
        };
        let pass = self
            .compiler
            .load_pass(
                &ShaderSource(vfs),
                &PassRequest {
                    shader: req.name,
                    combos: &combos,
                },
            )
            .map_err(|e| e.to_string())?;
        for w in &pass.warnings {
            tracing::debug!("shader '{}': {w}", req.name);
        }
        let id = match self.ids.get(&pass.key) {
            Some(id) => *id,
            None => {
                let desc = ShaderDesc {
                    label: req.name.to_owned(),
                    module: pass.program.module.clone(),
                    vertex_entry: lumenwall_shader::VERTEX_ENTRY.to_owned(),
                    fragment_entry: lumenwall_shader::FRAGMENT_ENTRY.to_owned(),
                    upstream_clip_space: true,
                };
                let id = b.add_shader(desc).map_err(|e| e.to_string())?;
                self.ids.insert(pass.key, id);
                self.translated += 1;
                id
            }
        };
        // Material constants (spec 03 §7.3): first vertex annotation of a key, else fragment.
        let mut constants: Vec<ConstantParam> = Vec::new();
        for p in pass
            .vertex_params
            .uniforms
            .iter()
            .chain(&pass.fragment_params.uniforms)
        {
            let Some(key) = &p.material_key else { continue };
            if constants.iter().any(|c| &c.key == key) {
                continue;
            }
            let Some(member) = pass.reflection().uniform(&p.uniform) else {
                continue;
            };
            let components = match member.ty {
                UniformType::Scalar(ScalarKind::Float) => 1,
                UniformType::Vector(ScalarKind::Float, n) => n.min(4),
                UniformType::Scalar(_) => 0,
                _ => continue,
            };
            let default = match p.default {
                UniformValue::Float(f) => [f, 0.0, 0.0, 0.0],
                UniformValue::Int(i) => [i as f32, 0.0, 0.0, 0.0],
                UniformValue::Vec2([x, y]) => [x, y, 0.0, 0.0],
                UniformValue::Vec3([x, y, z]) => [x, y, z, 0.0],
                UniformValue::Vec4(v) => v,
            };
            constants.push(ConstantParam {
                key: key.clone(),
                uniform: p.uniform.clone(),
                components,
                default,
            });
        }
        // Sampler defaults, vertex then fragment (lowest priority of spec 04 §12.3).
        let texture_defaults = pass
            .vertex_params
            .samplers
            .iter()
            .chain(&pass.fragment_params.samplers)
            .filter_map(|s| Some((s.slot?, s.default_texture.clone()?)))
            .collect();
        Ok(PassShader {
            id,
            constants,
            texture_defaults,
            stand_in: false,
        })
    }
}

impl ShaderProvider for WeShaders {
    fn pass_shader(
        &mut self,
        b: &mut SceneBuilder<'_>,
        vfs: &Vfs,
        req: &ShaderRequest<'_>,
    ) -> Result<PassShader, String> {
        if req.kind == PassKind::Copy {
            return self.fallback.stand_in(b, req, "engine copy");
        }
        match self.translate(b, vfs, req) {
            Ok(s) => Ok(s),
            Err(e) => self.fallback.stand_in(b, req, &e),
        }
    }

    fn replaced(&self) -> usize {
        self.fallback.replaced()
    }

    fn detach(&mut self) -> Option<Box<dyn ShaderProvider>> {
        Some(Box::new(std::mem::take(self)))
    }
}

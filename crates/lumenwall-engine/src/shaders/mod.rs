//! Shaders of the passes: a [`ShaderProvider`] turns a Wallpaper Engine shader name plus its
//! combos into a compiled [`ShaderId`] following the lumenwall-render binding contract.
//!
//! - [`FallbackShaders`]: WGSL stand-ins (image without effects, plain sprites). Used when
//!   `lumenwall-shader` is not built in, and for every shader it fails to translate.
//! - `WeShaders` (feature `we-shaders`): the real shaders through `lumenwall-shader`, falling back
//!   per shader.

use std::collections::{HashMap, HashSet};

use lumenwall_assets::Vfs;
use lumenwall_render::shader::ShaderDesc;
use lumenwall_render::texture::TexelFormat;
use lumenwall_render::{SceneBuilder, ShaderId};

#[cfg(feature = "we-shaders")]
mod we;
#[cfg(feature = "we-shaders")]
pub use we::WeShaders;

/// What a pass draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassKind {
    /// First (base) material pass of an image.
    ImageBase,
    /// Any later pass of an image chain (effects, tint, colour blend).
    ImageEffect,
    /// `copy` command pass (position already in clip space).
    Copy,
    /// Sprite particles (17-float vertices).
    Sprite { trail: bool },
    /// Rope particles (26-float vertices).
    Rope,
    /// Pass of a 3D model or sprite (spec 09): Wallpaper Engine clip convention, compiled
    /// without the clip fix. Never replaced by a stand-in: a failure leaves the mesh undrawn.
    Model,
}

/// A shader request.
#[derive(Clone, Debug)]
pub struct ShaderRequest<'a> {
    pub kind: PassKind,
    /// Wallpaper Engine shader name without extension (`genericimage2`, `effects/pulse`).
    pub name: &'a str,
    /// Combos of the scene override plus engine-imposed ones (particles), upper-case names.
    pub override_combos: &'a [(String, i32)],
    /// Combos of the material pass, `TEX0FORMAT` forcing applied, upper-case names.
    pub material_combos: &'a [(String, i32)],
    /// Texture slots set by the material and the override (`used` slots of spec 03 §5.3).
    pub used_slots: &'a [u32],
    /// Format of the texture bound to slot 0 when known (main texture, particle texture).
    pub slot0_format: Option<TexelFormat>,
}

/// A uniform fed from a material constant (`constantshadervalues`, spec 03 §7.3).
#[derive(Clone, Debug, PartialEq)]
pub struct ConstantParam {
    /// Material key (`material` alias of the annotation).
    pub key: String,
    /// Uniform name in the pass block.
    pub uniform: String,
    /// Number of float components (1 to 4), or 0 for an integer.
    pub components: u8,
    /// Annotation default.
    pub default: [f32; 4],
}

/// A compiled pass shader and its metadata.
#[derive(Clone, Debug)]
pub struct PassShader {
    pub id: ShaderId,
    /// Material constants this shader reads.
    pub constants: Vec<ConstantParam>,
    /// Sampler defaults `(slot, texture)` from the shader annotations (lowest priority, §12.3).
    pub texture_defaults: Vec<(u32, String)>,
    /// True when the WE shader was replaced by a stand-in.
    pub stand_in: bool,
}

/// Source of pass shaders.
pub trait ShaderProvider {
    /// Compiles (or reuses) the shader of a pass.
    fn pass_shader(
        &mut self,
        b: &mut SceneBuilder<'_>,
        vfs: &Vfs,
        req: &ShaderRequest<'_>,
    ) -> Result<PassShader, String>;

    /// Number of distinct WE shaders replaced by stand-ins so far.
    fn replaced(&self) -> usize;
}

const FALLBACK_WGSL: &str = include_str!("fallback.wgsl");

/// WGSL stand-ins following the lumenwall-render binding contract.
#[derive(Default)]
pub struct FallbackShaders {
    compiled: HashMap<(PassKind, u8), ShaderId>,
    logged: HashSet<String>,
}

impl FallbackShaders {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stand-in for `req`, logging the replaced WE shader once.
    pub fn stand_in(
        &mut self,
        b: &mut SceneBuilder<'_>,
        req: &ShaderRequest<'_>,
        why: &str,
    ) -> Result<PassShader, String> {
        if req.kind == PassKind::Model {
            return Err(format!("shader '{}': {why}", req.name));
        }
        if req.kind != PassKind::Copy && self.logged.insert(req.name.to_owned()) {
            tracing::warn!(
                "shader '{}' replaced by a Lumenwall stand-in ({why}): {}",
                req.name,
                match req.kind {
                    PassKind::ImageBase => "image drawn without its shader",
                    PassKind::ImageEffect => "effect pass copies its input",
                    PassKind::Copy => "copy",
                    PassKind::Sprite { .. } | PassKind::Rope => "plain particle sprites",
                    PassKind::Model => "not drawn",
                }
            );
        }
        let fmt = match req.slot0_format {
            Some(TexelFormat::Rg8) => 1u8,
            Some(TexelFormat::R8) => 2,
            _ => 0,
        };
        let key = (req.kind, fmt);
        if let Some(id) = self.compiled.get(&key) {
            return Ok(PassShader {
                id: *id,
                constants: Vec::new(),
                texture_defaults: Vec::new(),
                stand_in: true,
            });
        }
        let (vs, fs, trail) = match req.kind {
            PassKind::ImageBase => ("vs_image", "fs_image", false),
            PassKind::ImageEffect => ("vs_image", "fs_passthrough", false),
            PassKind::Copy => ("vs_copy", "fs_passthrough", false),
            PassKind::Sprite { trail } => ("vs_sprite", "fs_particle", trail),
            PassKind::Rope => ("vs_rope", "fs_particle", false),
            PassKind::Model => return Err("no stand-in for 3D passes".into()),
        };
        let source = FALLBACK_WGSL
            .replace("__TEXFMT__", &fmt.to_string())
            .replace("__TRAIL__", if trail { "true" } else { "false" });
        let label = format!(
            "lumenwall.fallback.{vs}.{fs}.{fmt}{}",
            if trail { ".trail" } else { "" }
        );
        let desc = ShaderDesc::from_wgsl(&label, &source, vs, fs).map_err(|e| e.to_string())?;
        let id = b.add_shader(desc).map_err(|e| e.to_string())?;
        self.compiled.insert(key, id);
        Ok(PassShader {
            id,
            constants: Vec::new(),
            texture_defaults: Vec::new(),
            stand_in: true,
        })
    }
}

impl ShaderProvider for FallbackShaders {
    fn pass_shader(
        &mut self,
        b: &mut SceneBuilder<'_>,
        _vfs: &Vfs,
        req: &ShaderRequest<'_>,
    ) -> Result<PassShader, String> {
        let why = if req.kind == PassKind::Copy {
            "engine copy"
        } else {
            "lumenwall-shader not built in"
        };
        self.stand_in(b, req, why)
    }

    fn replaced(&self) -> usize {
        self.logged.len()
    }
}

/// The provider of this build: real WE shaders with feature `we-shaders`, stand-ins otherwise.
pub fn default_provider() -> Box<dyn ShaderProvider> {
    #[cfg(feature = "we-shaders")]
    {
        Box::new(WeShaders::new())
    }
    #[cfg(not(feature = "we-shaders"))]
    {
        Box::new(FallbackShaders::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenwall_render::shader::{VertexAttr, apply_clip_space_fix, reflect, validate};

    /// Every stand-in variant parses, validates after the clip fix and follows the contract.
    #[test]
    fn stand_ins_follow_the_binding_contract() {
        let kinds = [
            ("vs_image", "fs_image", false),
            ("vs_image", "fs_passthrough", false),
            ("vs_copy", "fs_passthrough", false),
            ("vs_sprite", "fs_particle", false),
            ("vs_sprite", "fs_particle", true),
            ("vs_rope", "fs_particle", false),
        ];
        for fmt in 0..3 {
            for (vs, fs, trail) in kinds {
                let src = FALLBACK_WGSL
                    .replace("__TEXFMT__", &fmt.to_string())
                    .replace("__TRAIL__", if trail { "true" } else { "false" });
                let mut desc = ShaderDesc::from_wgsl("t", &src, vs, fs).unwrap();
                let iface = reflect(&desc.module, vs, "t").unwrap();
                assert!(iface.uses_slot(0), "{vs}");
                assert!(iface.field("g_ModelViewProjectionMatrix").is_some());
                apply_clip_space_fix(&mut desc.module, vs).unwrap();
                validate(&desc.module, "t").unwrap();
                if vs == "vs_sprite" {
                    let names: Vec<&str> = iface
                        .vertex_input_names
                        .iter()
                        .map(|(_, n)| n.as_str())
                        .collect();
                    assert_eq!(
                        names,
                        [
                            "a_Position",
                            "a_TexCoordVec4",
                            "a_Color",
                            "a_TexCoordVec4C1"
                        ]
                    );
                    assert!(matches!(
                        iface.vertex_inputs[1].attr,
                        VertexAttr::Unfed { .. }
                    ));
                }
            }
        }
    }
}

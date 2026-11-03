//! Render passes of an image and value precedence (spec 02 §12): override matching, synthetic
//! compatibility passes, texture slots (6 levels), combos and constants.

use crate::intern::Sym;
use crate::project::Project;
use crate::scene::{
    BindSource, Blending, Command, EffectPass, MaterialId, MaterialPass, ObjectIndex, PassOverride,
    TextureMap,
};
use crate::setting::SettingId;
use crate::value::Value;

/// Which override applies to a render pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverrideRef {
    /// No override.
    None,
    /// `overrides[index]` of `effects[effect]` of the image.
    Effect {
        /// Index in [`crate::ImageObject::effects`].
        effect: u32,
        /// Index in [`crate::ImageEffect::overrides`].
        index: u32,
    },
    /// [`ImagePasses::synthetic`]`[i]`.
    Synthetic(u32),
}

/// What a render pass draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassKind {
    /// One pass of a material.
    Material {
        /// Material.
        material: MaterialId,
        /// Pass index in the material.
        pass: u32,
    },
    /// Copy pass: trivial shader sampling slot 0 (`source`) into `target`, normal blending, no
    /// culling, no depth (§12.2).
    Copy {
        /// Slot 0.
        source: BindSource,
        /// Render target.
        target: Sym,
    },
}

/// One render pass of an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassDesc {
    /// What is drawn.
    pub kind: PassKind,
    /// Blending after the first/last swap of §12.6.
    pub blending: Blending,
    /// Override.
    pub over: OverrideRef,
    /// `(image effect index, effect pass index)` for effect passes: binds and target come from
    /// that effect pass.
    pub effect: Option<(u32, u32)>,
}

/// Override created by the implementation (§12.6), with fixed values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyntheticOverride {
    /// Combos, upper-case names.
    pub combos: Vec<(Sym, i32)>,
    /// Constants by parameter alias.
    pub constants: Vec<(Sym, Value)>,
}

/// Render passes of an image, in order: model material passes, passes of each visible effect,
/// then compatibility passes (§12.2, §12.6).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImagePasses {
    /// Passes.
    pub passes: Vec<PassDesc>,
    /// Overrides referenced by [`OverrideRef::Synthetic`].
    pub synthetic: Vec<SyntheticOverride>,
    /// False for a `passthrough` image without a visible effect: nothing is drawn.
    pub draws: bool,
}

/// A texture slot after resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureRef {
    /// `materials/<name>.tex`.
    File(Sym),
    /// Render target (`_rt_*` or `_alias_*`).
    RenderTarget(Sym),
    /// Output of the previous pass of the chain (or the expected input).
    Previous,
    /// The object's own composite buffer "A".
    OwnComposite,
}

/// Format of an image's main texture, for `TEX0FORMAT` (§12.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MainTextureFormat {
    /// RG88: `TEX0FORMAT` forced to 8.
    Rg88,
    /// R8: `TEX0FORMAT` forced to 9.
    R8,
    /// Anything else (no forcing).
    #[default]
    Other,
}

/// Where a shader constant takes its value (§12.5).
#[derive(Clone, Debug, PartialEq)]
pub enum ConstantSource {
    /// Shader annotation default.
    Default,
    /// Live setting: read the view matching the uniform type every frame.
    Setting(SettingId),
    /// Fixed value from a synthetic override.
    Fixed(Value),
}

enum Over<'a> {
    None,
    Effect(&'a PassOverride),
    Synthetic(&'a SyntheticOverride),
}

struct Parts<'a> {
    material: Option<&'a MaterialPass>,
    over: Over<'a>,
    effect_pass: Option<&'a EffectPass>,
}

fn is_render_target(name: &str) -> bool {
    name.starts_with("_rt_") || name.starts_with("_alias_")
}

impl Project {
    /// Builds the render passes of an image with the current setting values (§12.2, §12.6).
    /// Effect visibility is read now: call again when a bound property changes to toggle
    /// effects live.
    pub fn image_passes(&self, obj: ObjectIndex) -> Option<ImagePasses> {
        let scene = self.scene.as_ref()?;
        let image = scene.objects.get(obj.index())?.as_image()?;
        let model = scene.model(image.model?);
        let material = scene.material(model.material);
        let mut out = ImagePasses::default();
        for (i, p) in material.passes.iter().enumerate() {
            out.passes.push(PassDesc {
                kind: PassKind::Material {
                    material: model.material,
                    pass: i as u32,
                },
                blending: p.blending,
                over: OverrideRef::None,
                effect: None,
            });
        }
        let mut any_visible = false;
        for (ei, ie) in image.effects.iter().enumerate() {
            if !self.settings.value(ie.visible).boolean() {
                continue;
            }
            any_visible = true;
            let effect = scene.effect(ie.effect);
            let mut cursor = 0usize;
            for (pi, ep) in effect.passes.iter().enumerate() {
                let tag = Some((ei as u32, pi as u32));
                match ep.material {
                    None => match (ep.command, ep.source, ep.target) {
                        (Some(Command::Copy), Some(source), Some(target)) => {
                            out.passes.push(PassDesc {
                                kind: PassKind::Copy { source, target },
                                blending: Blending::Normal,
                                over: OverrideRef::None,
                                effect: tag,
                            })
                        }
                        _ => tracing::warn!(
                            "effect `{}` pass {pi}: unsupported command pass skipped",
                            self.strings.resolve(effect.file)
                        ),
                    },
                    Some(mid) => {
                        let over = if cursor < ie.overrides.len() {
                            OverrideRef::Effect {
                                effect: ei as u32,
                                index: cursor as u32,
                            }
                        } else {
                            OverrideRef::None
                        };
                        for (mpi, mp) in scene.material(mid).passes.iter().enumerate() {
                            out.passes.push(PassDesc {
                                kind: PassKind::Material {
                                    material: mid,
                                    pass: mpi as u32,
                                },
                                blending: mp.blending,
                                over,
                                effect: tag,
                            });
                        }
                        if cursor < ie.overrides.len() {
                            cursor += 1;
                        }
                    }
                }
            }
        }

        let names = &scene.names;
        // parity: magenta tint heuristic of the reference (Q28).
        let mut tint = None;
        'search: for ie in &image.effects {
            if !self.settings.value(ie.visible).boolean() {
                continue;
            }
            for ov in &ie.overrides {
                if !ov
                    .combos
                    .iter()
                    .any(|c| c.key == names.composite && c.value == 2)
                {
                    continue;
                }
                if let Some(c) = ov.constants.iter().find(|c| c.name == names.compositecolor) {
                    let v = self.settings.value(c.setting).vec3();
                    if v.x > 0.55 && v.y < 0.25 && v.z > 0.45 {
                        tint = Some(v);
                        break 'search;
                    }
                }
            }
        }
        if let Some(color) = tint
            && let Some(Some(mid)) = scene.util_tint
            && let Some(mp) = scene.material(mid).passes.first()
        {
            out.passes.push(PassDesc {
                kind: PassKind::Material {
                    material: mid,
                    pass: 0,
                },
                blending: mp.blending,
                over: OverrideRef::Synthetic(out.synthetic.len() as u32),
                effect: None,
            });
            out.synthetic.push(SyntheticOverride {
                combos: vec![(names.blendmode, 30)],
                constants: vec![
                    (names.alpha, Value::from_float(1.0)),
                    (names.color, Value::from_vec3(color)),
                ],
            });
        }
        let blend_mode = self.settings.value(image.color_blend_mode).int();
        if blend_mode > 0
            && let Some(Some(mid)) = scene.util_passthrough
            && let Some(mp) = scene.material(mid).passes.first()
        {
            out.passes.push(PassDesc {
                kind: PassKind::Material {
                    material: mid,
                    pass: 0,
                },
                blending: mp.blending,
                over: OverrideRef::Synthetic(out.synthetic.len() as u32),
                effect: None,
            });
            out.synthetic.push(SyntheticOverride {
                combos: vec![(names.blendmode, blend_mode)],
                constants: vec![],
            });
        }
        if out.passes.len() > 1 {
            let first = out.passes[0].blending;
            out.passes[0].blending = Blending::Normal;
            if let Some(last) = out.passes.last_mut() {
                last.blending = first;
            }
        }
        out.draws = !(model.passthrough && !any_visible);
        Some(out)
    }

    fn parts<'a>(
        &'a self,
        obj: ObjectIndex,
        passes: &'a ImagePasses,
        pass: usize,
    ) -> Option<Parts<'a>> {
        let scene = self.scene.as_ref()?;
        let image = scene.objects.get(obj.index())?.as_image()?;
        let desc = passes.passes.get(pass)?;
        let material = match desc.kind {
            PassKind::Material { material, pass } => {
                scene.material(material).passes.get(pass as usize)
            }
            PassKind::Copy { .. } => None,
        };
        let over = match desc.over {
            OverrideRef::None => Over::None,
            OverrideRef::Effect { effect, index } => image
                .effects
                .get(effect as usize)
                .and_then(|e| e.overrides.get(index as usize))
                .map_or(Over::None, Over::Effect),
            OverrideRef::Synthetic(i) => passes
                .synthetic
                .get(i as usize)
                .map_or(Over::None, Over::Synthetic),
        };
        let effect_pass = desc.effect.and_then(|(e, p)| {
            let ie = image.effects.get(e as usize)?;
            scene.effect(ie.effect).passes.get(p as usize)
        });
        Some(Parts {
            material,
            over,
            effect_pass,
        })
    }

    /// Texture of each slot of a pass, from lowest to highest priority (§12.3): shader defaults
    /// (vertex then fragment, as given), material `textures`, material `usertextures`, override
    /// `textures`, override `usertextures`, effect-pass `bind`. `texture_exists` is asked about
    /// texture names (not render targets); a missing texture keeps the previous definition.
    pub fn pass_textures(
        &mut self,
        obj: ObjectIndex,
        passes: &ImagePasses,
        pass: usize,
        shader_defaults: &[(u32, &str)],
        texture_exists: &mut dyn FnMut(&str) -> bool,
    ) -> Vec<(u32, TextureRef)> {
        let defaults: Vec<(u32, Sym)> = shader_defaults
            .iter()
            .map(|(s, n)| (*s, self.strings.intern(n)))
            .collect();
        // Every definition in increasing priority; the last one that resolves wins.
        let mut layers: Vec<(u32, BindSource)> = defaults
            .iter()
            .map(|&(s, n)| (s, BindSource::Named(n)))
            .collect();
        if let Some(parts) = self.parts(obj, passes, pass) {
            let mut push_map = |m: &TextureMap| {
                layers.extend(m.entries().iter().map(|&(s, n)| (s, BindSource::Named(n))))
            };
            if let Some(mp) = parts.material {
                push_map(&mp.textures);
                push_map(&mp.user_textures);
            }
            if let Over::Effect(o) = parts.over {
                push_map(&o.textures);
                push_map(&o.user_textures);
            }
            if let Some(ep) = parts.effect_pass {
                layers.extend(
                    ep.binds
                        .iter()
                        .filter_map(|b| u32::try_from(b.index).ok().map(|i| (i, b.source))),
                );
            }
            if let Some(PassKind::Copy { source, .. }) = passes.passes.get(pass).map(|p| p.kind) {
                layers.push((0, source));
            }
        }
        let mut slots: Vec<(u32, TextureRef)> = Vec::new();
        for (slot, source) in layers {
            let r = match source {
                BindSource::Previous => TextureRef::Previous,
                BindSource::OwnComposite => TextureRef::OwnComposite,
                BindSource::Named(n) => {
                    let s = self.strings.resolve(n);
                    if is_render_target(s) {
                        TextureRef::RenderTarget(n)
                    } else if texture_exists(s) {
                        TextureRef::File(n)
                    } else {
                        tracing::warn!(
                            "texture `{s}` not found, slot {slot} keeps its previous texture"
                        );
                        continue;
                    }
                }
            };
            match slots.binary_search_by_key(&slot, |e| e.0) {
                Ok(i) => slots[i].1 = r,
                Err(i) => slots.insert(i, (slot, r)),
            }
        }
        slots
    }

    /// Textures handed to the shader compiler (§12.3 note): material `textures`, overlaid by
    /// material `usertextures`, overlaid by override `textures`.
    pub fn pass_compile_textures(
        &self,
        obj: ObjectIndex,
        passes: &ImagePasses,
        pass: usize,
    ) -> TextureMap {
        let mut out = TextureMap::default();
        let Some(parts) = self.parts(obj, passes, pass) else {
            return out;
        };
        if let Some(mp) = parts.material {
            out.overlay(&mp.textures);
            out.overlay(&mp.user_textures);
        }
        if let Over::Effect(o) = parts.over {
            out.overlay(&o.textures);
        }
        out
    }

    /// Combos of a pass (§12.4), upper-case names in byte order: override first, then material (with
    /// `TEX0FORMAT` forced for RG88/R8 main textures), then shader defaults.
    pub fn pass_combos(
        &mut self,
        obj: ObjectIndex,
        passes: &ImagePasses,
        pass: usize,
        main_format: MainTextureFormat,
        shader_defaults: &[(&str, i32)],
    ) -> Vec<(Sym, i32)> {
        let defaults: Vec<(Sym, i32)> = shader_defaults
            .iter()
            .map(|(n, v)| (self.strings.intern(&n.to_ascii_uppercase()), *v))
            .collect();
        let mut out: Vec<(Sym, i32)> = Vec::new();
        let mut add = |name: Sym, v: i32| {
            if let Err(i) = out.binary_search_by_key(&name, |e| e.0) {
                out.insert(i, (name, v));
            }
        };
        let tex0 = self.scene.as_ref().map(|s| s.names.tex0format);
        if let Some(parts) = self.parts(obj, passes, pass) {
            match parts.over {
                Over::Effect(o) => o.combos.iter().for_each(|c| add(c.key, c.value)),
                Over::Synthetic(s) => s.combos.iter().for_each(|&(k, v)| add(k, v)),
                Over::None => {}
            }
            if let Some(mp) = parts.material {
                let forced = match main_format {
                    MainTextureFormat::Rg88 => Some(8),
                    MainTextureFormat::R8 => Some(9),
                    MainTextureFormat::Other => None,
                };
                if let (Some(v), Some(t)) = (forced, tex0) {
                    add(t, v);
                }
                mp.combos.iter().for_each(|c| add(c.key, c.value));
            }
        }
        defaults.into_iter().for_each(|(k, v)| add(k, v));
        out.sort_by(|a, b| self.strings.resolve(a.0).cmp(self.strings.resolve(b.0)));
        out
    }

    /// Source of each shader parameter (§12.5), in the order of `params` (parameter aliases):
    /// annotation default, replaced by the material constant, replaced by the override constant.
    pub fn pass_constants(
        &self,
        obj: ObjectIndex,
        passes: &ImagePasses,
        pass: usize,
        params: &[&str],
    ) -> Vec<ConstantSource> {
        let parts = self.parts(obj, passes, pass);
        params
            .iter()
            .map(|alias| {
                let mut src = ConstantSource::Default;
                let (Some(parts), Some(sym)) = (parts.as_ref(), self.strings.get(alias)) else {
                    return src;
                };
                if let Some(c) = parts
                    .material
                    .and_then(|m| m.constants.iter().find(|c| c.name == sym))
                {
                    src = ConstantSource::Setting(c.setting);
                }
                match parts.over {
                    Over::Effect(o) => {
                        if let Some(c) = o.constants.iter().find(|c| c.name == sym) {
                            src = ConstantSource::Setting(c.setting);
                        }
                    }
                    Over::Synthetic(s) => {
                        if let Some((_, v)) = s.constants.iter().find(|(n, _)| *n == sym) {
                            src = ConstantSource::Fixed(v.clone());
                        }
                    }
                    Over::None => {}
                }
                src
            })
            .collect()
    }

    /// Main texture of an image (§12.1): lowest slot of the first model pass's `textures`.
    pub fn main_texture(&self, obj: ObjectIndex) -> Option<TextureRef> {
        let scene = self.scene.as_ref()?;
        let image = scene.objects.get(obj.index())?.as_image()?;
        let material = scene.material(scene.model(image.model?).material);
        let &(_, name) = material.passes.first()?.textures.entries().first()?;
        Some(if is_render_target(self.strings.resolve(name)) {
            TextureRef::RenderTarget(name)
        } else {
            TextureRef::File(name)
        })
    }

    /// Render target written by an effect pass (`target`), if any.
    pub fn pass_target(&self, obj: ObjectIndex, passes: &ImagePasses, pass: usize) -> Option<Sym> {
        match passes.passes.get(pass)?.kind {
            PassKind::Copy { target, .. } => Some(target),
            PassKind::Material { .. } => self.parts(obj, passes, pass)?.effect_pass?.target,
        }
    }
}

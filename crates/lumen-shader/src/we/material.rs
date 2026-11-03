//! Per-pass texture candidate stacks (spec 03 §8.2) and binding of
//! `constantshadervalues` to annotated uniforms (§7.3).

use std::collections::BTreeMap;

use super::annotations::{SamplerParam, UniformParam};

/// One candidate of a slot stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextureCandidate {
    /// Texture or render target name, resolved by the renderer (§8.3).
    Named(String),
    /// `previous` bind: the effect input or pass input at draw time (§8.4).
    Previous,
}

/// Texture sources of a pass, indexed by slot (`None` = skipped entry).
#[derive(Clone, Debug, Default)]
pub struct TextureSources {
    pub material_textures: Vec<Option<String>>,
    pub material_usertextures: Vec<Option<String>>,
    pub override_textures: Vec<Option<String>>,
    pub override_usertextures: Vec<Option<String>>,
    /// Effect pass `bind` entries: (slot, name).
    pub binds: Vec<(u32, String)>,
}

/// Slot → stack, bottom first (the last element is the top, §8.2).
pub type TextureStacks = BTreeMap<u32, Vec<TextureCandidate>>;

/// Builds the stacks. Unresolvable names are the caller's to drop (§8.2: a
/// missing texture is not stacked, a missing `bind` target is an error).
pub fn texture_stacks(vertex: &[SamplerParam], fragment: &[SamplerParam], src: &TextureSources) -> TextureStacks {
    let mut stacks = TextureStacks::new();
    // 1. Vertex defaults replace the stack.
    for s in vertex {
        if let (Some(slot), Some(d)) = (s.slot, &s.default_texture) {
            stacks.insert(slot, vec![TextureCandidate::Named(d.clone())]);
        }
    }
    // 2. Fragment defaults.
    for s in fragment {
        if let (Some(slot), Some(d)) = (s.slot, &s.default_texture) {
            stacks.entry(slot).or_default().push(TextureCandidate::Named(d.clone()));
        }
    }
    // 3–6. Material and override maps.
    for list in [&src.material_textures, &src.material_usertextures, &src.override_textures, &src.override_usertextures] {
        for (slot, name) in list.iter().enumerate() {
            if let Some(n) = name.as_ref().filter(|n| !n.is_empty()) {
                stacks.entry(slot as u32).or_default().push(TextureCandidate::Named(n.clone()));
            }
        }
    }
    // 7. Binds.
    for (slot, name) in &src.binds {
        let c = if name == "previous" { TextureCandidate::Previous } else { TextureCandidate::Named(name.clone()) };
        stacks.entry(*slot).or_default().push(c);
    }
    stacks
}

/// §7.3: the uniform bound to a constant key — the first vertex annotation with
/// that material key, else the first fragment one.
pub fn find_constant<'a>(vertex: &'a [UniformParam], fragment: &'a [UniformParam], key: &str) -> Option<&'a UniformParam> {
    vertex
        .iter()
        .find(|p| p.material_key.as_deref() == Some(key))
        .or_else(|| fragment.iter().find(|p| p.material_key.as_deref() == Some(key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(name: &str, slot: u32, default: &str) -> SamplerParam {
        SamplerParam {
            uniform: name.into(),
            slot: Some(slot),
            default_texture: Some(default.into()),
            combo: None,
            require: None,
            require_any: false,
            raw_default: None,
            comparison: false,
        }
    }

    #[test]
    fn stack_order() {
        let src = TextureSources {
            material_textures: vec![None, Some("mat1".into())],
            override_textures: vec![None, Some("ovr1".into()), Some(String::new())],
            binds: vec![(0, "previous".into()), (2, "_rt_x".into())],
            ..Default::default()
        };
        let s = texture_stacks(&[sp("g_Texture1", 1, "vdef")], &[sp("g_Texture1", 1, "fdef")], &src);
        let n = |s: &str| TextureCandidate::Named(s.into());
        assert_eq!(s[&1], vec![n("vdef"), n("fdef"), n("mat1"), n("ovr1")]);
        assert_eq!(s[&0], vec![TextureCandidate::Previous]);
        assert_eq!(s[&2], vec![n("_rt_x")]);
    }
}

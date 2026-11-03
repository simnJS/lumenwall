//! Scene bloom (spec 04 §17) with the Wallpaper Engine asset materials.
//!
//! `lumenwall-scene` builds the synthetic bloom image (id −1): a `genericimage2` base pass, then the
//! four materials of [`lumenwall_scene::BLOOM_MATERIALS`], the first three with the override
//! constants `bloomstrength` / `bloomthreshold`. Its shaders are translated like any image's
//! and drawn through `lumenwall-render`'s fixed five-pass chain ([`add_bloom`]). Every material
//! constant is written with the usual precedence (annotation default < material < override,
//! spec 04 §13.1): `g_BloomStrength` and `g_BloomThreshold` get the scene values, and
//! `g_BloomTint` keeps its annotation default.
//!
//! If one of the five shaders cannot be translated, the whole chain uses lumenwall-render's WGSL
//! stand-ins (a per-shader passthrough would add the scene to itself).

use lumenwall_assets::Vfs;
use lumenwall_render::bloom::{BloomParams, BloomShaders, add_bloom};
use lumenwall_render::targets::{BloomTargets, PingPong, SceneTargets};
use lumenwall_render::{PassId, SceneBuilder};
use lumenwall_scene::{
    BloomInfo, ConstantSource, MainTextureFormat, PassKind as ScenePassKind, Project, SceneBuild,
    Value,
};

use crate::scene::{split_combos, value_floats};
use crate::shaders::{PassKind, PassShader, ShaderProvider, ShaderRequest};

/// Material keys of the bloom constants (annotations of `downsample_quarter_bloom.frag`).
const STRENGTH_KEY: &str = "bloomstrength";
const THRESHOLD_KEY: &str = "bloomthreshold";

/// Translated shaders of the five bloom passes, `None` when one is missing or a stand-in.
fn we_shaders(
    b: &mut SceneBuilder<'_>,
    project: &Project,
    build: &SceneBuild,
    vfs: &Vfs,
    shaders: &mut dyn ShaderProvider,
    info: BloomInfo,
) -> Result<Option<Vec<PassShader>>, String> {
    let idx = info.object;
    let Some(passes) = build.image_passes.get(idx.index()).and_then(Option::as_ref) else {
        return Ok(None);
    };
    let Some(scene) = &project.scene else {
        return Ok(None);
    };
    if passes.passes.len() != 5 {
        tracing::warn!("bloom: {} passes instead of 5", passes.passes.len());
        return Ok(None);
    }
    let mut out = Vec::with_capacity(5);
    for (k, desc) in passes.passes.iter().enumerate() {
        let ScenePassKind::Material { material, pass } = desc.kind else {
            return Ok(None);
        };
        let Some(mp) = scene.material(material).passes.get(pass as usize) else {
            return Ok(None);
        };
        let name = project.strings.resolve(mp.shader);
        let (material_combos, override_combos) =
            split_combos(project, idx, passes, k, MainTextureFormat::Other);
        let used_slots: Vec<u32> = project
            .pass_compile_textures(idx, passes, k)
            .entries()
            .iter()
            .map(|e| e.0)
            .collect();
        let shader = shaders.pass_shader(
            b,
            vfs,
            &ShaderRequest {
                kind: if k == 0 {
                    PassKind::ImageBase
                } else {
                    PassKind::ImageEffect
                },
                name,
                override_combos: &override_combos,
                material_combos: &material_combos,
                used_slots: &used_slots,
                slot0_format: None,
            },
        )?;
        if shader.stand_in {
            return Ok(None);
        }
        out.push(shader);
    }
    Ok(Some(out))
}

/// Writes the material constants of each bloom pass (spec 04 §13.1).
fn write_constants(
    b: &mut SceneBuilder<'_>,
    project: &Project,
    build: &SceneBuild,
    info: BloomInfo,
    shaders: &[PassShader],
    ids: &[PassId; 5],
) {
    let idx = info.object;
    let Some(passes) = build.image_passes.get(idx.index()).and_then(Option::as_ref) else {
        return;
    };
    for (k, (shader, &id)) in shaders.iter().zip(ids).enumerate() {
        let aliases: Vec<&str> = shader.constants.iter().map(|c| c.key.as_str()).collect();
        let sources = project.pass_constants(idx, passes, k, &aliases);
        for (param, source) in shader.constants.iter().zip(sources) {
            let Some(slot) = b.uniform(id, &param.uniform) else {
                continue;
            };
            let value = match source {
                ConstantSource::Default => Value::from_vec4(param.default.into()),
                ConstantSource::Setting(s) => project.settings.value(s).clone(),
                ConstantSource::Fixed(v) => v,
            };
            if param.components == 0 {
                b.shadow_mut().write_ints(&slot, &[value.int()]);
            } else {
                b.shadow_mut()
                    .write_floats(&slot, &value_floats(&value, param.components));
            }
        }
    }
}

/// Uniform of the pass-1 constant with this material key, or the WE name.
fn uniform_of(shader: &PassShader, key: &str, default: &str) -> String {
    shader
        .constants
        .iter()
        .find(|c| c.key == key)
        .map_or_else(|| default.to_owned(), |c| c.uniform.clone())
}

/// Adds the five bloom passes (render order: last object).
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_scene_bloom(
    b: &mut SceneBuilder<'_>,
    project: &Project,
    build: &SceneBuild,
    vfs: &Vfs,
    shaders: &mut dyn ShaderProvider,
    scene: &SceneTargets,
    targets: &BloomTargets,
    composites: PingPong,
    info: BloomInfo,
) -> Result<(), String> {
    let params = BloomParams {
        strength: info.strength,
        threshold: info.threshold,
    };
    let Some(we) = we_shaders(b, project, build, vfs, shaders, info)? else {
        tracing::warn!("bloom: WE shaders unavailable, using Lumenwall's approximation");
        let internal = BloomShaders::internal(b).map_err(|e| e.to_string())?;
        add_bloom(b, &internal, scene, targets, composites, params).map_err(|e| e.to_string())?;
        return Ok(());
    };
    let set = BloomShaders {
        copy: we[0].id,
        quarter: we[1].id,
        eighth_blur_v: we[2].id,
        blur_h: we[3].id,
        combine: we[4].id,
        strength_uniform: uniform_of(&we[1], STRENGTH_KEY, "g_BloomStrength"),
        threshold_uniform: uniform_of(&we[1], THRESHOLD_KEY, "g_BloomThreshold"),
    };
    let ids = add_bloom(b, &set, scene, targets, composites, params).map_err(|e| e.to_string())?;
    write_constants(b, project, build, info, &we, &ids);
    Ok(())
}

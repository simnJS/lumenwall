//! Scene bloom (spec 04 §17): five passes through the generic graph.
//!
//! 0. base pass: `_rt_FullFrameBuffer` → A (copy geometry, `M_copie`, `normal`);
//! 1. quarter downsample with threshold → `_rt_4FrameBuffer`;
//! 2. eighth downsample + vertical blur → `_rt_8FrameBuffer`;
//! 3. horizontal blur → `_rt_Bloom`;
//! 4. combine A (slot 0) + `_rt_Bloom` (slot 1) → `_rt_FullFrameBuffer`,
//!    `normal`, RGB-only writes.
//!
//! The shaders are the WE asset materials translated by `lumen-shader`
//! ([`BloomShaders`]); [`BloomShaders::internal`] provides WGSL stand-ins with
//! the same interface for tests and for installs without WE assets.

use glam::Vec2;

use crate::blend::BlendMode;
use crate::chain::{
    ChainBuffer, ChainOptions, ChainPassInput, GeometryCase, MatrixKind, plan_image_chain,
};
use crate::error::Result;
use crate::graph::{Geometry, Op, PassDesc, PassId, SceneBuilder, ShaderId, SlotBinding};
use crate::math::{copy_matrix, copy_quad, unit_quad};
use crate::shader::ShaderDesc;
use crate::targets::{BloomTargets, PingPong, SceneTargets};

/// Reserved image id of the synthetic bloom image (spec 04 §17).
pub const BLOOM_IMAGE_ID: i64 = -1;

const PASSES_WGSL: &str = include_str!("shaders/passes.wgsl");

/// Shaders of the five bloom passes and the names of their constants.
#[derive(Clone, Debug)]
pub struct BloomShaders {
    /// `genericimage2`.
    pub copy: ShaderId,
    /// `materials/util/downsample_quarter_bloom.json`.
    pub quarter: ShaderId,
    /// `materials/util/downsample_eighth_blur_v.json`.
    pub eighth_blur_v: ShaderId,
    /// `materials/util/blur_h_bloom.json`.
    pub blur_h: ShaderId,
    /// `materials/util/combine.json`.
    pub combine: ShaderId,
    /// Uniform fed with `general.bloomstrength` (passes 1 to 3).
    pub strength_uniform: String,
    /// Uniform fed with `general.bloomthreshold` (passes 1 to 3).
    pub threshold_uniform: String,
}

impl BloomShaders {
    /// WGSL stand-ins (not pixel-exact with Wallpaper Engine).
    pub fn internal(builder: &mut SceneBuilder<'_>) -> Result<Self> {
        let mut add = |label: &str, fs: &str| -> Result<ShaderId> {
            builder.add_shader(ShaderDesc::from_wgsl(label, PASSES_WGSL, "vs_generic", fs)?)
        };
        Ok(Self {
            copy: add("lumen.bloom.copy", "fs_copy")?,
            quarter: add("lumen.bloom.quarter", "fs_bloom_quarter")?,
            eighth_blur_v: add("lumen.bloom.eighth_v", "fs_bloom_eighth_v")?,
            blur_h: add("lumen.bloom.blur_h", "fs_bloom_h")?,
            combine: add("lumen.bloom.combine", "fs_bloom_combine")?,
            strength_uniform: "g_BloomStrength".into(),
            threshold_uniform: "g_BloomThreshold".into(),
        })
    }
}

/// `general.bloomstrength` / `general.bloomthreshold`, read once.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BloomParams {
    pub strength: f32,
    pub threshold: f32,
}

/// Add the bloom passes and their draw operations (call last: the bloom image
/// is the last object of the render order).
///
/// `composites` = A/B of image id −1, `targets` = the late bloom targets.
pub fn add_bloom(
    builder: &mut SceneBuilder<'_>,
    shaders: &BloomShaders,
    scene: &SceneTargets,
    bloom: &BloomTargets,
    composites: PingPong,
    params: BloomParams,
) -> Result<[PassId; 5]> {
    let (w, h) = builder.scene_size();
    // The chain logic decides destinations, geometry, blend transfer and mask.
    let plan = plan_image_chain(
        &[
            ChainPassInput {
                target: None,
                blend: BlendMode::Normal,
            },
            ChainPassInput {
                target: Some(bloom.quarter),
                blend: BlendMode::Normal,
            },
            ChainPassInput {
                target: Some(bloom.eighth),
                blend: BlendMode::Normal,
            },
            ChainPassInput {
                target: Some(bloom.bloom),
                blend: BlendMode::Normal,
            },
            ChainPassInput {
                target: Some(scene.full),
                blend: BlendMode::Normal,
            },
        ],
        ChainOptions::default(),
    );
    debug_assert_eq!(plan[0].destination, ChainBuffer::A);
    let copy_geom = builder.add_vertices(&copy_quad(Vec2::new(w, h), Vec2::ONE));
    let unit_geom = builder.add_vertices(&unit_quad());

    let shader_of = [
        shaders.copy,
        shaders.quarter,
        shaders.eighth_blur_v,
        shaders.blur_h,
        shaders.combine,
    ];
    let slot0 = [
        scene.full,
        scene.full,
        bloom.quarter,
        bloom.eighth,
        composites.a,
    ];
    let labels = [
        "bloom.base",
        "bloom.quarter",
        "bloom.eighth_v",
        "bloom.blur_h",
        "bloom.combine",
    ];
    let mut ids = Vec::with_capacity(5);
    for (k, p) in plan.iter().enumerate() {
        let target = match p.destination {
            ChainBuffer::A => composites.a,
            ChainBuffer::B => composites.b,
            ChainBuffer::Target(t) => t,
            ChainBuffer::Scene | ChainBuffer::Main => scene.full,
        };
        let geometry = match p.geometry {
            GeometryCase::Copy => copy_geom,
            _ => unit_geom,
        };
        let mut desc = PassDesc::new(
            labels[k],
            shader_of[k],
            target,
            Geometry::Vertices(geometry),
        )
        .with_slot(0, SlotBinding::target(slot0[k]));
        if k == 4 {
            desc = desc.with_slot(1, SlotBinding::target(bloom.bloom));
        }
        desc.blend = p.blend;
        desc.write_mask = p.write_mask;
        let id = builder.add_pass(desc)?;
        if let Some(slot) = builder.uniform(id, "g_ModelViewProjectionMatrix") {
            let m = match p.geometry.matrix() {
                MatrixKind::Copy => copy_matrix(Vec2::new(w, h)),
                _ => glam::Mat4::IDENTITY,
            };
            builder.shadow_mut().write_mat4(&slot, &m);
        }
        if (1..=3).contains(&k) {
            if let Some(slot) = builder.uniform(id, &shaders.strength_uniform) {
                builder.shadow_mut().write_floats(&slot, &[params.strength]);
            }
            if let Some(slot) = builder.uniform(id, &shaders.threshold_uniform) {
                builder
                    .shadow_mut()
                    .write_floats(&slot, &[params.threshold]);
            }
        }
        builder.push(Op::Draw(id));
        ids.push(id);
    }
    Ok([ids[0], ids[1], ids[2], ids[3], ids[4]])
}

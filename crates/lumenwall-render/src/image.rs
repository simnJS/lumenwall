//! Image objects on top of the generic graph (spec 04 §8 to §11): chain
//! planning, geometry, texture coordinates, matrices and the per-frame update.
//!
//! The caller (integration layer) resolves names, shaders and textures, then
//! describes the image with render-ready structs: [`ImageSpec`] and one
//! [`ImagePass`] per pass (material passes, effect passes, optional tint and
//! `colorBlendMode` passes, in order).

use glam::{Mat4, Vec2, Vec3};

use crate::blend::{BlendMode, CullMode};
use crate::chain::{
    ChainBuffer, ChainOptions, ChainPassInput, GeometryCase, MatrixKind, PlannedPass,
    plan_image_chain,
};
use crate::dynbuf::{UniformSlot, VertexRange};
use crate::error::Result;
use crate::graph::{
    Geometry, Op, PassDesc, PassId, RenderScene, SceneBuilder, ShaderId, SlotBinding, TextureRef,
};
use crate::math::{
    Alignment, NodeTransform, SkinVertex, Vertex, copy_matrix, copy_quad, copy_uv_max, image_rect,
    rect_quad, screen_matrix, unit_quad,
};
use crate::shader::MAX_SLOTS;
use crate::targets::{PingPong, RtId};
use crate::texture::SamplerKey;

/// Source of a texture slot (after name resolution, spec 04 §12.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotSource {
    /// A resolved texture or render target (highest-priority ready candidate).
    Texture(TextureRef),
    /// The `previous` bind candidate.
    Previous,
}

/// One pass of an image chain.
#[derive(Clone, Debug)]
pub struct ImagePass {
    pub label: String,
    pub shader: ShaderId,
    /// The material's own blending (before the transfer of spec 04 §9.8).
    pub blend: BlendMode,
    pub cull: CullMode,
    /// Resolved `target` of an effect pass.
    pub target: Option<RtId>,
    /// Slot candidates; slot 0 without source = the pass input.
    pub slots: [Option<SlotSource>; MAX_SLOTS],
    /// Frames of an animated main texture for slot 0 (see `RenderScene::set_variant`).
    pub slot0_variants: Vec<TextureRef>,
}

impl ImagePass {
    #[must_use]
    pub fn new(label: impl Into<String>, shader: ShaderId) -> Self {
        Self {
            label: label.into(),
            shader,
            blend: BlendMode::Normal,
            cull: CullMode::None,
            target: None,
            slots: [None; MAX_SLOTS],
            slot0_variants: Vec::new(),
        }
    }
}

/// Image-level description.
#[derive(Clone, Debug)]
pub struct ImageSpec {
    pub label: String,
    /// `S`, frozen after construction (spec 04 §8.2).
    pub size: Vec2,
    pub alignment: Alignment,
    /// Main texture (slot 0 input of the first pass).
    pub main: TextureRef,
    /// `(storage, content)` sizes of the main texture, for `uMax/vMax`.
    pub main_sizes: ((u32, u32), (u32, u32)),
    pub animated: bool,
    pub passthrough: bool,
    pub fullscreen: bool,
    /// Composite pair A/B (created with `RenderTargetPool::create_composites`).
    pub composites: PingPong,
    /// The scene render target.
    pub scene_target: RtId,
    /// See [`ChainOptions::final_to_scene`].
    pub final_to_scene: bool,
}

#[derive(Clone, Debug)]
struct DynamicPass {
    mvp: Option<UniformSlot>,
    mvp_inverse: Option<UniformSlot>,
    effect_mvp: Option<UniformSlot>,
}

/// Per-frame inputs of an image.
#[derive(Clone, Copy, Debug)]
pub struct ImageFrame {
    /// Camera `P · V`.
    pub view_projection: Mat4,
    /// Resolved transform (spec 04 §8.1; origin replaced for `fullscreen`).
    pub transform: NodeTransform,
    /// `T(parallaxe)` translation, zero when parallax is off.
    pub parallax: Vec3,
    /// Projection size `(W, H)`.
    pub scene_size: Vec2,
}

/// Puppet mesh of an image (spec 04 §20): rest positions in local image pixels
/// (`(S.x/2 + x, S.y/2 − y, z)`, y from the top edge), texture coordinates as stored.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PuppetGeometry {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u16>,
    /// Bone indices and weights per vertex (spec 08 §9.8); empty for a static mesh. When
    /// present the mesh is drawn as [`Geometry::Skinned`], so a shader compiled with the
    /// `SKINNING` combo reads them and the `g_Bones` palette.
    pub skin: Vec<SkinVertex>,
}

/// Handle returned by [`add_image`] to update the image every frame.
#[derive(Clone, Debug)]
pub struct ImageHandle {
    pub passes: Vec<PassId>,
    pub plan: Vec<PlannedPass<RtId>>,
    /// Single-pass puppet drawn into the scene: its screen matrix maps local pixels to RC.
    puppet_scene: bool,
    /// Puppet mesh vertices (rewritable for CPU skinning) and the passes drawing it.
    puppet_vertices: Option<VertexRange>,
    puppet_passes: Vec<PassId>,
    size: Vec2,
    alignment: Alignment,
    uv_max: Vec2,
    scene_geometry: Option<(VertexRange, bool)>,
    passthrough_geometry: Option<(VertexRange, bool)>,
    screen_passes: Vec<DynamicPass>,
    passthrough_passes: Vec<DynamicPass>,
}

fn write_matrix(b: &mut SceneBuilder<'_>, pass: PassId, m: Mat4) {
    for (name, value) in [
        ("g_ModelViewProjectionMatrix", m),
        ("g_EffectModelViewProjectionMatrix", m),
        ("g_ModelViewProjectionMatrixInverse", m.inverse()),
    ] {
        if let Some(slot) = b.uniform(pass, name) {
            b.shadow_mut().write_mat4(&slot, &value);
        }
    }
}

/// Build the passes of an image and append their draws to the frame ops.
pub fn add_image(
    b: &mut SceneBuilder<'_>,
    spec: &ImageSpec,
    passes: &[ImagePass],
) -> Result<ImageHandle> {
    add_image_with_puppet(b, spec, passes, None)
}

/// [`add_image`] for an image whose first pass draws a puppet mesh (spec 04 §20.3): indexed
/// mesh with `M_copie`, destination cleared first, first pass translucent; a single-pass chain
/// draws the mesh into the scene with `M_ecran` (positions mapped to RC, spec 04 Q22).
pub fn add_image_with_puppet(
    b: &mut SceneBuilder<'_>,
    spec: &ImageSpec,
    passes: &[ImagePass],
    puppet: Option<&PuppetGeometry>,
) -> Result<ImageHandle> {
    let puppet = puppet.filter(|m| !m.vertices.is_empty() && !m.indices.is_empty());
    let inputs: Vec<ChainPassInput<RtId>> = passes
        .iter()
        .map(|p| ChainPassInput {
            target: p.target,
            blend: p.blend,
        })
        .collect();
    let plan = plan_image_chain(
        &inputs,
        ChainOptions {
            final_to_scene: spec.final_to_scene,
            passthrough: spec.passthrough,
            fullscreen: spec.fullscreen,
            puppet: puppet.is_some(),
        },
    );
    let uv_max = copy_uv_max(
        spec.main_sizes.0,
        spec.main_sizes.1,
        spec.animated,
        spec.passthrough,
    );
    let model = copy_matrix(spec.size);

    let mut copy_geom = None;
    let mut unit_geom = None;
    let mut scene_geom = None;
    let mut passthrough_geom = None;
    let mut mesh_geom = None;
    let mut handle = ImageHandle {
        passes: Vec::with_capacity(plan.len()),
        plan: plan.clone(),
        puppet_scene: false,
        puppet_vertices: None,
        puppet_passes: Vec::new(),
        size: spec.size,
        alignment: spec.alignment,
        uv_max,
        scene_geometry: None,
        passthrough_geometry: None,
        screen_passes: Vec::new(),
        passthrough_passes: Vec::new(),
    };

    let buffer = |c: ChainBuffer<RtId>| -> TextureRef {
        match c {
            ChainBuffer::Main => spec.main,
            ChainBuffer::A => TextureRef::Target(spec.composites.a),
            ChainBuffer::B => TextureRef::Target(spec.composites.b),
            ChainBuffer::Target(t) => TextureRef::Target(t),
            ChainBuffer::Scene => TextureRef::Target(spec.scene_target),
        }
    };

    for (p, q) in passes.iter().zip(plan.iter()) {
        let target = match buffer(q.destination) {
            TextureRef::Target(t) => t,
            _ => spec.scene_target,
        };
        let geometry = match (q.geometry, puppet) {
            (GeometryCase::PuppetLocal | GeometryCase::PuppetScene, Some(mesh)) => {
                handle.puppet_scene |= q.geometry == GeometryCase::PuppetScene;
                *mesh_geom.get_or_insert_with(|| {
                    let vertices = b.add_vertices(&mesh.vertices);
                    let indices = b.add_indices(&mesh.indices);
                    handle.puppet_vertices = Some(vertices);
                    if mesh.skin.len() == mesh.vertices.len() {
                        Geometry::Skinned {
                            vertices,
                            indices,
                            skin: b.add_skin(&mesh.skin),
                        }
                    } else {
                        Geometry::Indexed { vertices, indices }
                    }
                })
            }
            (geometry, _) => Geometry::Vertices(match geometry {
                GeometryCase::Copy | GeometryCase::PuppetLocal => {
                    *copy_geom.get_or_insert_with(|| b.add_vertices(&copy_quad(spec.size, uv_max)))
                }
                GeometryCase::Unit => {
                    *unit_geom.get_or_insert_with(|| b.add_vertices(&unit_quad()))
                }
                GeometryCase::SceneRect
                | GeometryCase::SceneRectCopyUv
                | GeometryCase::PuppetScene => {
                    let copy_uv = q.geometry != GeometryCase::SceneRect;
                    let r =
                        *scene_geom.get_or_insert_with(|| (b.add_vertices(&unit_quad()), copy_uv));
                    handle.scene_geometry = Some(r);
                    r.0
                }
                GeometryCase::Passthrough { fullscreen } => {
                    let r = *passthrough_geom
                        .get_or_insert_with(|| (b.add_vertices(&unit_quad()), fullscreen));
                    handle.passthrough_geometry = Some(r);
                    r.0
                }
            }),
        };

        let mut desc = PassDesc::new(p.label.clone(), p.shader, target, geometry);
        desc.blend = q.blend;
        desc.write_mask = q.write_mask;
        desc.cull = p.cull;
        for (n, src) in p.slots.iter().enumerate() {
            desc.slots[n] = src.map(|s| SlotBinding {
                texture: match s {
                    SlotSource::Texture(t) => t,
                    SlotSource::Previous => buffer(q.previous.unwrap_or(q.input)),
                },
                sampler: None,
            });
        }
        if desc.slots[0].is_none() {
            desc.slots[0] = Some(SlotBinding {
                texture: buffer(q.input),
                sampler: None,
            });
        }
        if q.input == ChainBuffer::Main && p.slots[0].is_none() {
            desc.slot0_variants.clone_from(&p.slot0_variants);
        }
        let draws_mesh = matches!(
            desc.geometry,
            Geometry::Indexed { .. } | Geometry::Skinned { .. }
        );
        let id = b.add_pass(desc)?;
        if draws_mesh {
            handle.puppet_passes.push(id);
        }

        for name in ["g_ModelMatrix", "g_EffectModelMatrix"] {
            if let Some(slot) = b.uniform(id, name) {
                b.shadow_mut().write_mat4(&slot, &model);
            }
        }
        let dynamic = |b: &SceneBuilder<'_>| DynamicPass {
            mvp: b.uniform(id, "g_ModelViewProjectionMatrix"),
            mvp_inverse: b.uniform(id, "g_ModelViewProjectionMatrixInverse"),
            effect_mvp: b.uniform(id, "g_EffectModelViewProjectionMatrix"),
        };
        match (q.geometry.matrix(), q.geometry) {
            (MatrixKind::Copy, GeometryCase::Passthrough { .. }) => {
                handle.passthrough_passes.push(dynamic(b))
            }
            (MatrixKind::Copy, _) => write_matrix(b, id, model),
            (MatrixKind::Identity, _) => write_matrix(b, id, Mat4::IDENTITY),
            (MatrixKind::Screen, _) => handle.screen_passes.push(dynamic(b)),
        }
        if q.clear_before {
            b.push(Op::Clear {
                target,
                color: [0.0; 4],
            });
        }
        b.push(Op::Draw(id));
        handle.passes.push(id);
    }
    Ok(handle)
}

impl ImageHandle {
    /// Changes the anchor of the image rectangle (SceneScript `ILayer.alignment`); applied by
    /// the next [`Self::update`].
    pub fn set_alignment(&mut self, alignment: Alignment) {
        self.alignment = alignment;
    }

    /// Vertices of the puppet mesh in the dynamic region, for CPU skinning through
    /// [`RenderScene::set_vertices`]. `None` when the image draws no mesh.
    #[must_use]
    pub fn puppet_vertices(&self) -> Option<VertexRange> {
        self.puppet_vertices
    }

    /// Passes drawing the puppet mesh (the first pass of the chain).
    #[must_use]
    pub fn puppet_passes(&self) -> &[PassId] {
        &self.puppet_passes
    }

    /// Per-frame update (spec 04 §8.8): rectangle, rotation pivot, `M_ecran`
    /// with parallax, scene geometry. No allocation; unchanged values do not
    /// dirty the scene.
    pub fn update(&self, scene: &mut RenderScene, f: &ImageFrame) {
        let t = f.transform;
        let rect = image_rect(t.origin, t.scale, self.size, self.alignment, f.scene_size);
        let mut screen = screen_matrix(f.view_projection, &rect, t.angle, f.parallax);
        if self.puppet_scene {
            // Mesh in local pixels → the image rectangle in RC (spec 04 Q22).
            let sx = (rect.x1 - rect.x0) / self.size.x.max(f32::MIN_POSITIVE);
            let sy = (rect.y_bottom - rect.y_top) / self.size.y.max(f32::MIN_POSITIVE);
            screen = screen
                * Mat4::from_translation(Vec3::new(rect.x0, rect.y_top, 0.0))
                * Mat4::from_scale(Vec3::new(sx, sy, 1.0));
        }
        if let Some((range, copy_uv)) = self.scene_geometry {
            let uv = if copy_uv { self.uv_max } else { Vec2::ONE };
            scene.set_vertices(range, &rect_quad(&rect, uv));
        }
        if let Some((range, fullscreen)) = self.passthrough_geometry {
            let q = if fullscreen {
                unit_quad()
            } else {
                rect_quad(&rect, Vec2::ONE)
            };
            scene.set_vertices(range, &q);
        }
        if self.screen_passes.is_empty() && self.passthrough_passes.is_empty() {
            return;
        }
        let inverse = screen.inverse();
        let shadow = scene.shadow_mut();
        for d in self.screen_passes.iter().chain(&self.passthrough_passes) {
            if let Some(s) = &d.mvp {
                shadow.write_mat4(s, &screen);
            }
            if let Some(s) = &d.effect_mvp {
                shadow.write_mat4(s, &screen);
            }
            if let Some(s) = &d.mvp_inverse {
                shadow.write_mat4(s, &inverse);
            }
        }
    }
}

/// Sampling of the composites and effect targets of an image: the flags of
/// its main texture interpreted as render-target flags (spec 04 §8.9, §16.1).
#[must_use]
pub fn composite_sampler(main_flags: crate::texture::TexFlags) -> SamplerKey {
    SamplerKey::for_render_target(main_flags)
}

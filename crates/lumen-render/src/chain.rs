//! Pass chain of one image: destinations, inputs, "previous", ping-pong,
//! geometry case, blend transfer and write mask (spec 04 §9.8, §10, §11.6, §20.3).
//!
//! Pure logic, run once at load time (or when the chain must be rebuilt).
//! The caller resolves names (`target`, `bind`) through the RT namespaces and
//! turns the plan into [`crate::graph::PassDesc`]s.

use crate::blend::{BlendMode, WriteMask};

/// Symbolic buffer of an image chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChainBuffer<T> {
    /// The image's main texture (or RT used as main texture).
    Main,
    /// Composite `_rt_imageLayerComposite_<id>_a`.
    A,
    /// Composite `_rt_imageLayerComposite_<id>_b`.
    B,
    /// A named render target (effect `fbos`, or any scene RT).
    Target(T),
    /// The scene render target `_rt_FullFrameBuffer`.
    Scene,
}

/// Geometry, texture coordinates and matrix of a pass (spec 04 §10.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryCase {
    /// First pass, not redirected: `[0, S.x] x [0, S.y]`, copy UVs `[0, uMax] x [0, vMax]`,
    /// MVP = `M_copie`.
    Copy,
    /// First pass of a `passthrough` image: image rectangle in RC (or `[-1, 1]²`
    /// when also `fullscreen`), UVs `[0, 1]²`, MVP = `M_copie` (= `M_ecran`).
    Passthrough { fullscreen: bool },
    /// First pass using a puppet mesh in local pixels, MVP = `M_copie`.
    PuppetLocal,
    /// Pass k ≥ 1 not redirected: `[-1, 1]²`, UVs `[0, 1]²`, identity MVP.
    Unit,
    /// Pass k ≥ 1 redirected to the scene: image rectangle in RC, UVs `[0, 1]²`, MVP = `M_ecran`.
    SceneRect,
    /// Single-pass chain redirected to the scene: image rectangle in RC, copy UVs, MVP = `M_ecran`.
    SceneRectCopyUv,
    /// Single-pass puppet chain redirected to the scene: mesh converted to RC,
    /// MVP = `M_ecran` (deviation from the reference, spec 04 Q22).
    PuppetScene,
}

/// Matrix family fed to `g_ModelViewProjectionMatrix`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixKind {
    /// `M_copie` (`ortho2(0, S.x, 0, S.y)`, or `M_ecran` for passthrough images).
    Copy,
    Identity,
    /// `M_ecran`, recomputed every frame.
    Screen,
}

impl GeometryCase {
    #[must_use]
    pub fn matrix(self) -> MatrixKind {
        match self {
            Self::Copy | Self::Passthrough { .. } | Self::PuppetLocal => MatrixKind::Copy,
            Self::Unit => MatrixKind::Identity,
            Self::SceneRect | Self::SceneRectCopyUv | Self::PuppetScene => MatrixKind::Screen,
        }
    }
}

/// One pass of the chain, as declared (material passes, effect passes, the
/// optional tint and `colorBlendMode` passes, in order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainPassInput<T> {
    /// Resolved `target` of an effect pass. An unresolvable target must be
    /// passed as `None` (the pass is then treated as having no target).
    pub target: Option<T>,
    /// The material's own blending mode.
    pub blend: BlendMode,
}

/// Image-level options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainOptions {
    /// Redirect the last pass without target to the scene.
    ///
    /// The reference only does it for images visible at initialisation;
    /// Lumen passes `true` for every image (spec 04 Q8, deviation), so an
    /// image made visible later by a script or property composes normally.
    /// `false` reproduces the diagnostic "no final pass for solid layers".
    pub final_to_scene: bool,
    pub passthrough: bool,
    pub fullscreen: bool,
    pub puppet: bool,
}

impl Default for ChainOptions {
    fn default() -> Self {
        Self {
            final_to_scene: true,
            passthrough: false,
            fullscreen: false,
            puppet: false,
        }
    }
}

/// Planned pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannedPass<T> {
    pub destination: ChainBuffer<T>,
    /// Texture bound to slot 0 when the slot has no candidate.
    pub input: ChainBuffer<T>,
    /// Meaning of the `previous` bind candidate, if any.
    pub previous: Option<ChainBuffer<T>>,
    pub geometry: GeometryCase,
    /// Blend mode after the transfer of spec 04 §9.8.
    pub blend: BlendMode,
    pub write_mask: WriteMask,
    /// Clear the destination to (0, 0, 0, 0) before drawing (puppet first pass, spec 04 §20.3).
    pub clear_before: bool,
}

/// Plan a chain (spec 04 §10.1). Returns an empty plan for an empty input.
#[must_use]
pub fn plan_image_chain<T: Copy + Eq>(
    passes: &[ChainPassInput<T>],
    opts: ChainOptions,
) -> Vec<PlannedPass<T>> {
    let n = passes.len();
    let mut out = Vec::with_capacity(n);
    let mut write = ChainBuffer::A;
    let mut input = ChainBuffer::Main;
    let mut prev: Option<ChainBuffer<T>> = None;
    let mut seq = false;

    for (k, p) in passes.iter().enumerate() {
        let last = k + 1 == n;
        let first = k == 0;
        let (destination, previous) = if let Some(t) = p.target {
            if !seq {
                prev = Some(input);
                seq = true;
            }
            (ChainBuffer::Target(t), prev)
        } else {
            let redirected = last && opts.final_to_scene;
            let d = if redirected {
                ChainBuffer::Scene
            } else {
                write
            };
            (d, if seq { prev } else { None })
        };

        let to_scene = destination == ChainBuffer::Scene;
        let geometry = match (first, to_scene) {
            (true, true) if opts.puppet => GeometryCase::PuppetScene,
            (true, true) => GeometryCase::SceneRectCopyUv,
            (false, true) => GeometryCase::SceneRect,
            (true, false) if opts.puppet => GeometryCase::PuppetLocal,
            (true, false) if opts.passthrough => GeometryCase::Passthrough {
                fullscreen: opts.fullscreen,
            },
            (true, false) => GeometryCase::Copy,
            (false, false) => GeometryCase::Unit,
        };

        out.push(PlannedPass {
            destination,
            input,
            previous,
            geometry,
            blend: p.blend,
            write_mask: if last { WriteMask::Rgb } else { WriteMask::All },
            clear_before: first && opts.puppet && !to_scene,
        });

        if p.target.is_some() {
            input = destination;
        } else {
            input = write;
            write = if write == ChainBuffer::A {
                ChainBuffer::B
            } else {
                ChainBuffer::A
            };
            seq = false;
            prev = None;
        }
    }

    // Blend transfer (spec 04 §9.8).
    if n > 1 {
        out[n - 1].blend = out[0].blend;
        out[0].blend = BlendMode::Normal;
    }
    if opts.puppet && n > 0 {
        out[0].blend = BlendMode::Translucent;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::ChainBuffer::{A, B, Main, Scene, Target};
    use super::*;

    fn p(target: Option<u32>, blend: BlendMode) -> ChainPassInput<u32> {
        ChainPassInput { target, blend }
    }

    #[test]
    fn simple_image() {
        let plan = plan_image_chain(&[p(None, BlendMode::Translucent)], ChainOptions::default());
        assert_eq!(plan.len(), 1);
        let q = plan[0];
        assert_eq!((q.destination, q.input, q.previous), (Scene, Main, None));
        assert_eq!(q.geometry, GeometryCase::SceneRectCopyUv);
        assert_eq!(q.geometry.matrix(), MatrixKind::Screen);
        assert_eq!(q.blend, BlendMode::Translucent);
        assert_eq!(q.write_mask, WriteMask::Rgb);
    }

    #[test]
    fn image_with_one_effect() {
        let plan = plan_image_chain(
            &[
                p(None, BlendMode::Translucent),
                p(None, BlendMode::Additive),
            ],
            ChainOptions::default(),
        );
        assert_eq!((plan[0].destination, plan[0].input), (A, Main));
        assert_eq!(plan[0].geometry, GeometryCase::Copy);
        assert_eq!(plan[0].blend, BlendMode::Normal);
        assert_eq!(plan[0].write_mask, WriteMask::All);
        assert_eq!((plan[1].destination, plan[1].input), (Scene, A));
        assert_eq!(plan[1].geometry, GeometryCase::SceneRect);
        // The effect's own blend is lost; the model's blend is transferred.
        assert_eq!(plan[1].blend, BlendMode::Translucent);
        assert_eq!(plan[1].write_mask, WriteMask::Rgb);
    }

    #[test]
    fn blur_with_declared_targets() {
        // Spec 04 §10.3, third example: t1 = 1, t2 = 2.
        let plan = plan_image_chain(
            &[
                p(None, BlendMode::Normal),
                p(Some(1), BlendMode::Normal),
                p(Some(2), BlendMode::Normal),
                p(None, BlendMode::Normal),
            ],
            ChainOptions::default(),
        );
        assert_eq!(
            (plan[0].destination, plan[0].input, plan[0].previous),
            (A, Main, None)
        );
        assert_eq!(
            (plan[1].destination, plan[1].input, plan[1].previous),
            (Target(1), A, Some(A))
        );
        assert_eq!(plan[1].geometry, GeometryCase::Unit);
        assert_eq!(
            (plan[2].destination, plan[2].input, plan[2].previous),
            (Target(2), Target(1), Some(A))
        );
        assert_eq!(
            (plan[3].destination, plan[3].input, plan[3].previous),
            (Scene, Target(2), Some(A))
        );
        assert_eq!(plan[3].geometry, GeometryCase::SceneRect);
    }

    #[test]
    fn ping_pong_alternates() {
        let plan = plan_image_chain(&[p(None, BlendMode::Normal); 5], ChainOptions::default());
        let d: Vec<_> = plan.iter().map(|q| (q.input, q.destination)).collect();
        assert_eq!(d, vec![(Main, A), (A, B), (B, A), (A, B), (B, Scene)]);
        assert!(plan.iter().all(|q| q.previous.is_none()));
    }

    #[test]
    fn last_pass_with_target_draws_nothing_in_scene() {
        // Bloom-like: base pass then targets only.
        let plan = plan_image_chain(
            &[
                p(None, BlendMode::Normal),
                p(Some(7), BlendMode::Normal),
                p(Some(8), BlendMode::Normal),
            ],
            ChainOptions::default(),
        );
        assert!(plan.iter().all(|q| q.destination != Scene));
        assert_eq!(
            plan[2].write_mask,
            WriteMask::Rgb,
            "RGB-only applies to the last pass even with a target"
        );
        // A first pass with a target keeps the copy geometry.
        let plan = plan_image_chain(
            &[p(Some(3), BlendMode::Normal), p(None, BlendMode::Normal)],
            ChainOptions::default(),
        );
        assert_eq!(plan[0].geometry, GeometryCase::Copy);
        assert_eq!(plan[0].previous, Some(Main));
        assert_eq!((plan[1].input, plan[1].previous), (Target(3), Some(Main)));
    }

    #[test]
    fn options() {
        let opts = ChainOptions {
            final_to_scene: false,
            ..ChainOptions::default()
        };
        let plan = plan_image_chain(
            &[p(None, BlendMode::Normal), p(None, BlendMode::Normal)],
            opts,
        );
        assert_eq!(plan[1].destination, B);

        let opts = ChainOptions {
            passthrough: true,
            fullscreen: true,
            ..ChainOptions::default()
        };
        let plan = plan_image_chain(
            &[p(None, BlendMode::Normal), p(None, BlendMode::Normal)],
            opts,
        );
        assert_eq!(
            plan[0].geometry,
            GeometryCase::Passthrough { fullscreen: true }
        );
        assert_eq!(plan[0].geometry.matrix(), MatrixKind::Copy);

        let opts = ChainOptions {
            puppet: true,
            ..ChainOptions::default()
        };
        let plan = plan_image_chain(
            &[p(None, BlendMode::Additive), p(None, BlendMode::Normal)],
            opts,
        );
        assert_eq!(plan[0].geometry, GeometryCase::PuppetLocal);
        assert!(plan[0].clear_before);
        assert_eq!(plan[0].blend, BlendMode::Translucent);
        assert_eq!(plan[1].blend, BlendMode::Additive);
        let plan = plan_image_chain(&[p(None, BlendMode::Normal)], opts);
        assert_eq!(plan[0].geometry, GeometryCase::PuppetScene);
        assert!(!plan[0].clear_before);
        assert!(plan_image_chain::<u32>(&[], ChainOptions::default()).is_empty());
    }
}

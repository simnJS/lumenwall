//! Pure planning of the direct Vulkan path: which path a scene records through, and the
//! image transitions of a frame. No GPU access; unit tested.

use ash::vk;

/// How a scene graph records its frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Recording {
    /// Straight into a Vulkan command buffer when the device is Vulkan (and the scene's passes
    /// could all be created through wgpu-hal), through wgpu otherwise.
    #[default]
    Auto,
    /// Always through wgpu: render passes validated and tracked by wgpu-core.
    Wgpu,
}

impl Recording {
    /// `LUMENWALL_DIRECT_VULKAN` (measurements and comparisons): `0`, `false` or `off` records
    /// every scene through wgpu; anything else (or nothing) leaves the choice automatic.
    #[must_use]
    pub fn from_env_value(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("0" | "false" | "off" | "no") => Self::Wgpu,
            _ => Self::Auto,
        }
    }

    /// [`Self::from_env_value`] of the process environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var("LUMENWALL_DIRECT_VULKAN").ok().as_deref())
    }
}

/// Why a scene records through wgpu instead of Vulkan directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WgpuReason {
    /// The device is not Vulkan (Direct3D 12 on Windows).
    NotVulkan,
    /// [`Recording::Wgpu`] (switch or `LUMENWALL_DIRECT_VULKAN=0`).
    Requested,
    /// A pipeline, layout or bind group could not be created through wgpu-hal, or the frame
    /// uses an image two ways in one step: the scene went back to wgpu.
    Unsupported,
}

/// The path a new scene takes, before any object is created.
pub fn choose(recording: Recording, backend: wgpu::Backend) -> Result<(), WgpuReason> {
    if recording == Recording::Wgpu {
        return Err(WgpuReason::Requested);
    }
    if backend != wgpu::Backend::Vulkan {
        return Err(WgpuReason::NotVulkan);
    }
    Ok(())
}

/// How one step of a frame (a render pass or a copy) uses an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Use {
    /// Sampled by the draws of a render pass (`SHADER_READ_ONLY_OPTIMAL`); the resting use of
    /// render targets between frames, which wgpu-core also tracks (`TextureUses::RESOURCE`).
    Sampled,
    /// Colour attachment.
    Color,
    /// Depth attachment; the resting use of depth buffers.
    Depth,
    /// Source of a copy.
    CopySrc,
    /// Destination of a copy.
    CopyDst,
}

impl Use {
    pub(crate) fn layout(self) -> vk::ImageLayout {
        match self {
            Self::Sampled => vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            Self::Color => vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            // What wgpu-hal derives from `TextureUses::DEPTH_STENCIL_WRITE`.
            Self::Depth => vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
            Self::CopySrc => vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            Self::CopyDst => vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        }
    }

    pub(crate) fn stages(self) -> vk::PipelineStageFlags {
        match self {
            Self::Sampled => {
                vk::PipelineStageFlags::VERTEX_SHADER | vk::PipelineStageFlags::FRAGMENT_SHADER
            }
            Self::Color => vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            Self::Depth => {
                vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                    | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS
            }
            Self::CopySrc | Self::CopyDst => vk::PipelineStageFlags::TRANSFER,
        }
    }

    pub(crate) fn access(self) -> vk::AccessFlags {
        match self {
            Self::Sampled => vk::AccessFlags::SHADER_READ,
            Self::Color => {
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
            }
            Self::Depth => {
                vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
            }
            Self::CopySrc => vk::AccessFlags::TRANSFER_READ,
            Self::CopyDst => vk::AccessFlags::TRANSFER_WRITE,
        }
    }

    /// The accesses a later step must wait for (writes only: reads need no availability).
    pub(crate) fn writes(self) -> vk::AccessFlags {
        match self {
            Self::Sampled | Self::CopySrc => vk::AccessFlags::empty(),
            Self::Color => vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            Self::Depth => vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            Self::CopyDst => vk::AccessFlags::TRANSFER_WRITE,
        }
    }

    fn is_write(self) -> bool {
        !self.writes().is_empty()
    }
}

/// One image transition (index in the frame's image table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Transition {
    pub image: u32,
    pub from: Use,
    pub to: Use,
}

/// Transitions of a frame: before each step, and back to the resting uses after the last.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BarrierPlan {
    pub before: Vec<Vec<Transition>>,
    pub after: Vec<Transition>,
}

/// Plans the image transitions of a frame. Every image starts the frame in its resting use
/// (`rest[image]`, what wgpu-core believes) and ends it there. A step that needs an image in
/// another use, or that follows a write to it, gets a transition first (also between two
/// render passes on the same attachment: a write after a write). `Err(image)` when one step
/// uses an image two ways (sampled while it is the attachment): the scene goes back to wgpu,
/// which reports the conflict as it does today.
pub(crate) fn plan_barriers(steps: &[Vec<(u32, Use)>], rest: &[Use]) -> Result<BarrierPlan, u32> {
    let mut state = rest.to_vec();
    let mut plan = BarrierPlan {
        before: Vec::with_capacity(steps.len()),
        after: Vec::new(),
    };
    let mut needs: Vec<(u32, Use)> = Vec::new();
    for step in steps {
        needs.clear();
        for &(image, need) in step {
            match needs.iter().find(|(i, _)| *i == image) {
                Some((_, u)) if *u != need => return Err(image),
                Some(_) => {}
                None => needs.push((image, need)),
            }
        }
        let mut before = Vec::new();
        for &(image, need) in &needs {
            let cur = state[image as usize];
            if cur != need || cur.is_write() || need.is_write() {
                before.push(Transition {
                    image,
                    from: cur,
                    to: need,
                });
            }
            state[image as usize] = need;
        }
        plan.before.push(before);
    }
    for (image, (&cur, &r)) in state.iter().zip(rest).enumerate() {
        if cur != r {
            plan.after.push(Transition {
                image: image as u32,
                from: cur,
                to: r,
            });
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Use = Use::Sampled;

    #[test]
    fn path_decision() {
        assert_eq!(choose(Recording::Auto, wgpu::Backend::Vulkan), Ok(()));
        assert_eq!(
            choose(Recording::Auto, wgpu::Backend::Dx12),
            Err(WgpuReason::NotVulkan)
        );
        assert_eq!(
            choose(Recording::Wgpu, wgpu::Backend::Vulkan),
            Err(WgpuReason::Requested)
        );
        assert_eq!(Recording::from_env_value(None), Recording::Auto);
        assert_eq!(Recording::from_env_value(Some("1")), Recording::Auto);
        assert_eq!(Recording::from_env_value(Some(" 0 ")), Recording::Wgpu);
        assert_eq!(Recording::from_env_value(Some("off")), Recording::Wgpu);
    }

    #[test]
    fn frame_starts_and_ends_at_rest() {
        // Image 0 rendered then sampled while image 1 is rendered; image 2 (depth) rendered.
        let steps = vec![
            vec![(0, Use::Color), (2, Use::Depth)],
            vec![(1, Use::Color), (0, Use::Sampled)],
        ];
        let rest = [T, T, Use::Depth];
        let plan = plan_barriers(&steps, &rest).unwrap();
        assert_eq!(
            plan.before[0],
            vec![
                Transition {
                    image: 0,
                    from: T,
                    to: Use::Color
                },
                // Depth stays in its layout but follows the last frame's writes.
                Transition {
                    image: 2,
                    from: Use::Depth,
                    to: Use::Depth
                },
            ]
        );
        assert_eq!(
            plan.before[1],
            vec![
                Transition {
                    image: 1,
                    from: T,
                    to: Use::Color
                },
                Transition {
                    image: 0,
                    from: Use::Color,
                    to: T
                },
            ]
        );
        // Image 1 goes back to its resting use; 0 already is, 2 never left it.
        assert_eq!(
            plan.after,
            vec![Transition {
                image: 1,
                from: Use::Color,
                to: T
            }]
        );
    }

    #[test]
    fn reads_need_no_barrier_but_writes_do() {
        let steps = vec![
            vec![(1, Use::Color), (0, T)],
            vec![(2, Use::Color), (0, T)],
            // Second render pass on the same attachment: write after write.
            vec![(2, Use::Color)],
        ];
        let plan = plan_barriers(&steps, &[T, T, T]).unwrap();
        assert!(plan.before[0].iter().all(|t| t.image != 0));
        assert!(plan.before[1].iter().all(|t| t.image != 0));
        assert_eq!(
            plan.before[2],
            vec![Transition {
                image: 2,
                from: Use::Color,
                to: Use::Color
            }]
        );
    }

    #[test]
    fn copies_and_shared_images() {
        // A hazard copy of 0 into the scratch 1, then a pass into 0 sampling the scratch. Two
        // targets sharing one texture are one image: the planner only sees images.
        let steps = vec![
            vec![(0, Use::CopySrc), (1, Use::CopyDst)],
            vec![(0, Use::Color), (1, T)],
            vec![(0, Use::Color)],
        ];
        let plan = plan_barriers(&steps, &[T, T]).unwrap();
        assert_eq!(plan.before[0].len(), 2);
        assert_eq!(
            plan.before[1],
            vec![
                Transition {
                    image: 0,
                    from: Use::CopySrc,
                    to: Use::Color
                },
                Transition {
                    image: 1,
                    from: Use::CopyDst,
                    to: T
                },
            ]
        );
        assert_eq!(plan.after.len(), 1);
        assert_eq!(plan.after[0].to, T);
    }

    #[test]
    fn an_image_used_two_ways_in_one_step_is_refused() {
        let steps = vec![vec![(0, Use::Color), (0, T)]];
        assert_eq!(plan_barriers(&steps, &[T]), Err(0));
        // The same use twice is fine (two draws sampling the same target).
        let steps = vec![vec![(1, Use::Color), (0, T), (0, T)]];
        assert!(plan_barriers(&steps, &[T, T]).is_ok());
    }

    #[test]
    fn layouts_match_wgpu_hal() {
        assert_eq!(
            Use::Sampled.layout(),
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        );
        assert_eq!(
            Use::Depth.layout(),
            vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL
        );
        assert!(Use::Sampled.writes().is_empty());
        assert!(Use::CopySrc.writes().is_empty());
        assert!(!Use::Color.writes().is_empty());
    }
}

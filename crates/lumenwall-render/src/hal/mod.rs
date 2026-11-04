//! Direct Vulkan recording of a scene graph through wgpu-hal.
//!
//! wgpu 30 validates, tracks and encodes every render pass in `CommandEncoder::finish`, about
//! 8.5 µs per render pass on the reference scenes whatever the number of draws. On a Vulkan
//! device a scene records its frame instead into a Vulkan command buffer of its own: its
//! pipelines, layouts, bind groups and samplers are created through wgpu-hal from the same
//! descriptors as the wgpu path (same SPIR-V, same states, so the same pixels), and the frame
//! is a precomputed list of barriers, copies, render passes and draws replayed every frame.
//!
//! wgpu-core does not see that command buffer. What keeps its view of the resources true:
//!
//! * **Resting states.** Between frames every render target is in `RESOURCE`
//!   (`SHADER_READ_ONLY_OPTIMAL`), every depth buffer in `DEPTH_STENCIL_WRITE` and every
//!   texture in `RESOURCE`, the states wgpu-core believes they are in. A frame starts and ends
//!   in them, with a full memory barrier at both ends: the compositor (wgpu) samples
//!   `_rt_FullFrameBuffer` and readbacks copy targets without knowing about the frame.
//! * **Adoption.** A texture the frame uses for the first time (build, resize, unshare, hazard
//!   scratch, layers added later) is cleared once by wgpu if it is a render target (what
//!   wgpu-core's lazy initialisation would have done) and transitioned to its resting state
//!   with `transition_resources`, so wgpu-core's state and initialisation tracking stay true.
//! * **Uploads stay on wgpu**: the staging belt copies (uniforms, particle streams) and
//!   `Queue::write_*` run in a command buffer submitted before the scene's, and
//!   [`crate::graph::RenderScene::mark_dirty`] after an external texture write (text) puts the
//!   scene's textures back to `RESOURCE` in the next frame's upload command buffer.
//! * **Lifetimes.** The command buffer holds no wgpu-core reference: the graph keeps clones of
//!   every wgpu object a recorded frame uses, and waits for the GPU before destroying any of
//!   its own objects (rebuilds after a resize, scene drop).
//!
//! Anything this path cannot create (a wgpu-hal error) or an image used two ways in one step
//! sends the scene back to the wgpu path ([`WgpuReason::Unsupported`]); Direct3D 12 always
//! records through wgpu.

mod plan;
#[cfg(test)]
mod tests;

pub use plan::{Recording, WgpuReason, choose};

/// Injected failures (tests of the fallback to wgpu).
#[cfg(test)]
pub(crate) mod faults {
    use std::sync::atomic::{AtomicBool, Ordering};

    static PIPELINES: AtomicBool = AtomicBool::new(false);
    static PROGRAMS: AtomicBool = AtomicBool::new(false);

    /// wgpu-hal pipeline creation fails while set.
    pub(crate) fn fail_pipelines(on: bool) {
        PIPELINES.store(on, Ordering::SeqCst);
    }

    /// Frame planning fails while set.
    pub(crate) fn fail_programs(on: bool) {
        PROGRAMS.store(on, Ordering::SeqCst);
    }

    pub(super) fn pipelines() -> bool {
        PIPELINES.load(Ordering::SeqCst)
    }

    pub(super) fn programs() -> bool {
        PROGRAMS.load(Ordering::SeqCst)
    }
}

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicI64, Ordering};

use ash::vk;
use wgpu::hal::{self, CommandEncoder as _, Device as _, api::Vulkan, vulkan as hv};

use crate::context::GpuContext;
use crate::texture::{AddressMode, SamplerKey};
use plan::{Transition, Use, plan_barriers};

/// wgpu-hal objects created by every direct graph and not yet destroyed (tests: a dropped
/// scene leaves nothing behind).
static LIVE_OBJECTS: AtomicI64 = AtomicI64::new(0);

/// wgpu-hal and Vulkan objects alive in every direct graph of the process.
#[must_use]
pub fn live_objects() -> i64 {
    LIVE_OBJECTS.load(Ordering::Relaxed)
}

fn created(n: i64) {
    LIVE_OBJECTS.fetch_add(n, Ordering::Relaxed);
}

/// Error of the direct path; the scene goes back to wgpu.
#[derive(Clone, Debug)]
pub(crate) struct Unsupported(pub String);

type Res<T> = std::result::Result<T, Unsupported>;

fn fail(what: &str, e: impl std::fmt::Debug) -> Unsupported {
    Unsupported(format!("{what}: {e:?}"))
}

/// One vertex buffer of a pipeline (the same description feeds both paths).
#[derive(Clone, Debug)]
pub(crate) struct VertexBuffer {
    pub stride: u64,
    pub step: wgpu::VertexStepMode,
    pub attributes: Vec<wgpu::VertexAttribute>,
}

/// A render pipeline as both paths create it.
pub(crate) struct PipelineSpec<'a> {
    pub shader: usize,
    pub vertex_entry: &'a str,
    pub fragment_entry: &'a str,
    /// `(slots, volume slots)` of the texture bind group.
    pub layout: (u8, u8),
    pub buffers: &'a [VertexBuffer],
    pub primitive: wgpu::PrimitiveState,
    pub depth_stencil: Option<wgpu::DepthStencilState>,
    pub target: wgpu::ColorTargetState,
}

/// One texture slot of a pass bind group: bindings `2 * slot` (view) and `2 * slot + 1`.
pub(crate) struct SlotView<'a> {
    pub slot: u32,
    pub view: &'a wgpu::TextureView,
    pub sampler: SamplerKey,
    pub volume: bool,
}

/// How a draw is issued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrawCall {
    Vertices { first: u32, count: u32 },
    Indexed { first: u32, count: u32, base: i32 },
    /// Index count of a vertex stream, read every frame.
    Stream(u32),
}

/// One draw of a render step.
pub(crate) struct DrawStep<'a> {
    pub pass: u32,
    pub pipeline: usize,
    pub uniform_offset: u32,
    pub vertex: Vec<(&'a wgpu::Buffer, u64)>,
    pub index: Option<(&'a wgpu::Buffer, wgpu::IndexFormat)>,
    pub call: DrawCall,
}

/// One step of a frame, in order (the scene's segments).
pub(crate) enum Step<'a> {
    Copy {
        src: &'a wgpu::Texture,
        dst: &'a wgpu::Texture,
        size: (u32, u32),
    },
    Render {
        target: &'a wgpu::Texture,
        clear: Option<wgpu::Color>,
        /// Depth texture, its view, cleared to 1 by this render pass.
        depth: Option<(&'a wgpu::Texture, &'a wgpu::TextureView, bool)>,
        /// Render targets the draws sample (every slot-0 variant included).
        sampled: Vec<&'a wgpu::Texture>,
        draws: Vec<DrawStep<'a>>,
    },
}

/// What changes between frames without a rebuild.
pub(crate) trait FrameState {
    /// Selected slot-0 variant of a pass.
    fn variant(&self, pass: u32) -> usize;
    /// Indices to draw from a vertex stream this frame (0 = nothing).
    fn stream_indices(&self, stream: u32) -> u32;
}

/// A wgpu object a recorded frame uses.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Kept {
    Texture(wgpu::Texture),
    Buffer(wgpu::Buffer),
}

/// Plain description of an image barrier (built into a `vk::ImageMemoryBarrier` when recorded,
/// so that the graph holds no raw pointer).
#[derive(Clone, Copy, Debug)]
struct ImageBarrier {
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    from: Use,
    to: Use,
}

impl ImageBarrier {
    fn vk(&self) -> vk::ImageMemoryBarrier<'static> {
        vk::ImageMemoryBarrier::default()
            .src_access_mask(self.from.writes())
            .dst_access_mask(self.to.access())
            .old_layout(self.from.layout())
            .new_layout(self.to.layout())
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: self.aspect,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
    }
}

/// A draw with every handle resolved.
struct Draw {
    pass: u32,
    pipeline: u32,
    uniform_offset: u32,
    buffers: Vec<vk::Buffer>,
    offsets: Vec<vk::DeviceSize>,
    index: Option<(vk::Buffer, vk::IndexType)>,
    call: DrawCall,
}

enum Cmd {
    /// Image barriers `first..first + count`; `global` adds a full memory barrier between
    /// every stage (start and end of the frame).
    Barriers {
        first: u32,
        count: u32,
        src: vk::PipelineStageFlags,
        dst: vk::PipelineStageFlags,
        global: bool,
    },
    Copy {
        src: vk::Image,
        dst: vk::Image,
        size: (u32, u32),
        segment: u32,
    },
    Begin(Box<Begin>),
    Draw(u32),
    End {
        hal: bool,
        segment: u32,
    },
}

struct Begin {
    render_pass: vk::RenderPass,
    framebuffer: vk::Framebuffer,
    size: (u32, u32),
    clear: Option<wgpu::Color>,
    /// Depth attachment, cleared to 1 by this pass?
    depth: Option<bool>,
    /// Attachments, for wgpu-hal's `begin_render_pass` (the first pass of the frame).
    color: wgpu::Texture,
    depth_texture: Option<wgpu::Texture>,
    /// Begun through wgpu-hal: it resets the encoder's bind point to graphics, whatever an
    /// earlier user of the pooled encoder left there.
    hal: bool,
    segment: u32,
}

#[derive(Default)]
struct Program {
    cmds: Vec<Cmd>,
    barriers: Vec<ImageBarrier>,
    draws: Vec<Draw>,
    segments: u32,
    render_passes: u32,
    /// The wgpu objects it records (images, views, buffers).
    keep: Vec<Kept>,
    /// Its render targets and depth buffers.
    images: Vec<wgpu::Texture>,
}

/// A texture bind group layout and its pipeline layout.
struct Layout {
    key: (u8, u8),
    group: hv::BindGroupLayout,
    pipeline: hv::PipelineLayout,
}

/// Counters of a direct graph (debug logs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectStats {
    /// wgpu-hal and Vulkan objects alive in the graph (pipelines, layouts, bind groups,
    /// samplers, views, render passes, framebuffers…).
    pub objects: i64,
    pub pipelines: usize,
    pub render_passes: u32,
    pub draws: usize,
    pub barriers: usize,
}

/// The wgpu-hal side of a scene recorded directly in Vulkan.
pub(crate) struct DirectGraph {
    device: wgpu::Device,
    raw: ash::Device,
    clamp_to_border: bool,
    anisotropy: bool,
    samplers: HashMap<(SamplerKey, bool), hv::Sampler>,
    /// `None` only while dropped.
    uniform_layout: Option<hv::BindGroupLayout>,
    layouts: Vec<Layout>,
    /// By shader id.
    modules: Vec<Option<hv::ShaderModule>>,
    /// `(pipeline, layout index)`.
    pipelines: Vec<(hv::RenderPipeline, usize)>,
    uniform_group: Option<hv::BindGroup>,
    uniform_buffer: Option<wgpu::Buffer>,
    /// Texture bind groups of each pass, one per slot-0 variant.
    pass_groups: Vec<Vec<hv::BindGroup>>,
    /// Textures each pass's bind groups sample.
    pass_textures: Vec<Vec<wgpu::Texture>>,
    /// wgpu-hal views of the textures the graph uses (the default view, as wgpu creates it):
    /// wgpu-core allows one `as_hal` guard at a time, a bind group needs several views.
    views: HashMap<wgpu::Texture, hv::TextureView>,
    /// Replaced bind groups: destroyed once the GPU is idle.
    dead_groups: Vec<hv::BindGroup>,
    render_passes: HashMap<(bool, Option<bool>), vk::RenderPass>,
    framebuffers: HashMap<(vk::RenderPass, vk::ImageView, vk::ImageView, u32, u32), vk::Framebuffer>,
    program: Program,
    /// Every wgpu object something recorded since the last [`Self::idle`] may use (the
    /// recorded frames hold no wgpu-core reference).
    keep: HashSet<Kept>,
    /// Textures already in their resting states for wgpu-core.
    adopted: HashSet<wgpu::Texture>,
    /// wgpu-hal and Vulkan objects this graph created and has not destroyed.
    objects: i64,
}

/// Runs `f` with the wgpu-hal device of `device`.
fn with_hal<R>(device: &wgpu::Device, f: impl FnOnce(&hv::Device) -> R) -> Option<R> {
    // SAFETY: the wgpu-hal device is only used during `f`, while `device` keeps the wgpu-core
    // device alive; nothing here destroys it or its raw handle.
    let dev = unsafe { device.as_hal::<Vulkan>() }?;
    Some(f(&dev))
}

/// Bind group layout entries of the texture slots (both paths).
pub(crate) fn texture_layout_entries(slots: u8, volumes: u8) -> Vec<wgpu::BindGroupLayoutEntry> {
    let mut entries = Vec::new();
    for n in 0..crate::shader::MAX_SLOTS as u32 {
        if slots & (1 << n) == 0 {
            continue;
        }
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 2 * n,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: if volumes & (1 << n) == 0 {
                    wgpu::TextureViewDimension::D2
                } else {
                    wgpu::TextureViewDimension::D3
                },
                multisampled: false,
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 2 * n + 1,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
    }
    entries
}

/// Uniform bind group layout entries: pass blocks (dynamic offset) and frame globals.
pub(crate) fn uniform_layout_entries() -> [wgpu::BindGroupLayoutEntry; 2] {
    let entry = |binding, dynamic| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: None,
        },
        count: None,
    };
    [entry(0, true), entry(1, false)]
}

const COLOR_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
const DEPTH_FORMAT: vk::Format = vk::Format::D32_SFLOAT;
/// Image barriers recorded per `vkCmdPipelineBarrier` call (stack buffer).
const BARRIER_BATCH: usize = 32;

impl DirectGraph {
    /// A direct graph on `ctx`'s device, or `None` when it is not Vulkan.
    pub(crate) fn new(ctx: &GpuContext) -> Res<Self> {
        let device = ctx.device().clone();
        let entries = uniform_layout_entries();
        let (raw, uniform_layout) = with_hal(&device, |dev| {
            // SAFETY: a valid descriptor; the layout is destroyed by `Drop` with this device.
            let layout = unsafe {
                dev.create_bind_group_layout(&hal::BindGroupLayoutDescriptor {
                    label: None,
                    flags: hal::BindGroupLayoutFlags::empty(),
                    entries: &entries,
                })
            };
            (dev.raw_device().clone(), layout)
        })
        .ok_or_else(|| Unsupported("not a Vulkan device".into()))?;
        let uniform_layout = uniform_layout.map_err(|e| fail("uniform layout", e))?;
        created(1);
        let anisotropy = ctx
            .adapter()
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::ANISOTROPIC_FILTERING);
        Ok(Self {
            device,
            raw,
            clamp_to_border: ctx.caps().clamp_to_border,
            anisotropy,
            samplers: HashMap::new(),
            uniform_layout: Some(uniform_layout),
            layouts: Vec::new(),
            modules: Vec::new(),
            pipelines: Vec::new(),
            uniform_group: None,
            uniform_buffer: None,
            pass_groups: Vec::new(),
            pass_textures: Vec::new(),
            views: HashMap::new(),
            dead_groups: Vec::new(),
            render_passes: HashMap::new(),
            framebuffers: HashMap::new(),
            program: Program::default(),
            keep: HashSet::new(),
            adopted: HashSet::new(),
            objects: 1,
        })
    }

    /// Counts objects created (`n > 0`) or destroyed.
    fn note(&mut self, n: i64) {
        self.objects += n;
        created(n);
    }

    fn uniform_layout(&self) -> Res<&hv::BindGroupLayout> {
        self.uniform_layout
            .as_ref()
            .ok_or_else(|| Unsupported("graph being dropped".into()))
    }

    pub(crate) fn stats(&self) -> DirectStats {
        DirectStats {
            objects: self.objects,
            pipelines: self.pipelines.len(),
            render_passes: self.program.render_passes,
            draws: self.program.draws.len(),
            barriers: self.program.barriers.len(),
        }
    }

    /// Shader module of shader `id` from its final naga module (clip fix applied) and its
    /// validation info, as wgpu-core would hand it to wgpu-hal.
    pub(crate) fn add_module(
        &mut self,
        id: usize,
        module: naga::Module,
        info: naga::valid::ModuleInfo,
    ) -> Res<()> {
        let created_module = with_hal(&self.device, |dev| {
            // SAFETY: a validated naga module; wgpu's `create_shader_module` uses the same
            // descriptor (`ShaderRuntimeChecks::checked()`, no label in release). Destroyed by
            // `Drop`.
            unsafe {
                dev.create_shader_module(
                    &hal::ShaderModuleDescriptor {
                        label: None,
                        runtime_checks: wgpu::ShaderRuntimeChecks::checked(),
                    },
                    hal::ShaderInput::Naga(hal::NagaShader {
                        module: Cow::Owned(module),
                        info,
                        debug_source: None,
                    }),
                )
            }
        })
        .ok_or_else(|| Unsupported("not a Vulkan device".into()))?
        .map_err(|e| fail("shader module", e))?;
        self.note(1);
        if self.modules.len() <= id {
            self.modules.resize_with(id + 1, || None);
        }
        if let Some(old) = self.modules[id].replace(created_module) {
            self.destroy_module(old);
        }
        Ok(())
    }

    fn destroy_module(&mut self, m: hv::ShaderModule) {
        with_hal(&self.device, |dev| {
            // SAFETY: no pipeline is being created from it; pipelines do not reference modules.
            unsafe { dev.destroy_shader_module(m) };
        });
        self.note(-1);
    }

    /// Index of the texture layout `key`, created on first use.
    fn layout(&mut self, key: (u8, u8)) -> Res<usize> {
        if let Some(i) = self.layouts.iter().position(|l| l.key == key) {
            return Ok(i);
        }
        let entries = texture_layout_entries(key.0, key.1);
        let uniform = self.uniform_layout()?;
        let made = with_hal(&self.device, |dev| {
            // SAFETY: valid descriptors (the entries the wgpu path uses); both objects are
            // destroyed by `Drop`.
            unsafe {
                let group = dev.create_bind_group_layout(&hal::BindGroupLayoutDescriptor {
                    label: None,
                    flags: hal::BindGroupLayoutFlags::empty(),
                    entries: &entries,
                })?;
                match dev.create_pipeline_layout(&hal::PipelineLayoutDescriptor {
                    label: None,
                    flags: hal::PipelineLayoutFlags::empty(),
                    bind_group_layouts: &[Some(uniform), Some(&group)],
                    immediate_size: 0,
                }) {
                    Ok(pipeline) => Ok((group, pipeline)),
                    Err(e) => {
                        dev.destroy_bind_group_layout(group);
                        Err(e)
                    }
                }
            }
        })
        .ok_or_else(|| Unsupported("not a Vulkan device".into()))?
        .map_err(|e| fail("pipeline layout", e))?;
        self.note(2);
        self.layouts.push(Layout {
            key,
            group: made.0,
            pipeline: made.1,
        });
        Ok(self.layouts.len() - 1)
    }

    /// Creates a render pipeline; returns its index.
    pub(crate) fn add_pipeline(&mut self, spec: &PipelineSpec<'_>) -> Res<usize> {
        #[cfg(test)]
        if faults::pipelines() {
            return Err(Unsupported("injected pipeline failure".into()));
        }
        let layout = self.layout(spec.layout)?;
        let module = self
            .modules
            .get(spec.shader)
            .and_then(Option::as_ref)
            .ok_or_else(|| Unsupported("shader without a wgpu-hal module".into()))?;
        let buffers: Vec<Option<hal::VertexBufferLayout<'_>>> = spec
            .buffers
            .iter()
            .map(|b| {
                Some(hal::VertexBufferLayout {
                    array_stride: b.stride,
                    step_mode: b.step,
                    attributes: &b.attributes,
                })
            })
            .collect();
        let constants = naga::back::PipelineConstants::default();
        let stage = |entry_point| hal::ProgrammableStage {
            module,
            entry_point,
            constants: &constants,
            // `PipelineCompilationOptions::default()` of the wgpu path.
            zero_initialize_workgroup_memory: true,
        };
        let targets = [Some(spec.target.clone())];
        let desc = hal::RenderPipelineDescriptor {
            label: None,
            layout: &self.layouts[layout].pipeline,
            vertex_processor: hal::VertexProcessor::Standard {
                vertex_buffers: &buffers,
                vertex_stage: stage(spec.vertex_entry),
            },
            primitive: spec.primitive,
            depth_stencil: spec.depth_stencil.clone(),
            multisample: wgpu::MultisampleState::default(),
            fragment_stage: Some(stage(spec.fragment_entry)),
            color_targets: &targets,
            multiview_mask: None,
            cache: None::<&hv::PipelineCache>,
        };
        let pipeline = with_hal(&self.device, |dev| {
            // SAFETY: the descriptor mirrors what wgpu-core hands to wgpu-hal for the same
            // wgpu descriptor; the layout and module belong to this device. Destroyed by `Drop`.
            unsafe { dev.create_render_pipeline(&desc) }
        })
        .ok_or_else(|| Unsupported("not a Vulkan device".into()))?
        .map_err(|e| fail("render pipeline", e))?;
        self.note(1);
        self.pipelines.push((pipeline, layout));
        Ok(self.pipelines.len() - 1)
    }

    /// The shared sampler of `key` (the descriptor of `GpuContext::sampler`/`volume_sampler`,
    /// as wgpu-core converts it).
    fn sampler(&mut self, key: SamplerKey, volume: bool) -> Res<()> {
        if self.samplers.contains_key(&(key, volume)) {
            return Ok(());
        }
        let address = match key.address {
            AddressMode::Clamp => wgpu::AddressMode::ClampToEdge,
            AddressMode::Repeat => wgpu::AddressMode::Repeat,
            AddressMode::Border if self.clamp_to_border => wgpu::AddressMode::ClampToBorder,
            AddressMode::Border => wgpu::AddressMode::ClampToEdge,
        };
        let (filter, mip) = if key.nearest {
            (wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Nearest)
        } else {
            (wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Linear)
        };
        let anisotropy = if key.nearest || volume || !self.anisotropy {
            1
        } else {
            8
        };
        let desc = hal::SamplerDescriptor {
            label: None,
            address_modes: [address; 3],
            mag_filter: filter,
            min_filter: filter,
            mipmap_filter: mip,
            lod_clamp: 0.0..if key.mipmaps { 32.0 } else { 0.0 },
            compare: None,
            anisotropy_clamp: anisotropy,
            border_color: (address == wgpu::AddressMode::ClampToBorder)
                .then_some(wgpu::SamplerBorderColor::TransparentBlack),
        };
        let sampler = with_hal(&self.device, |dev| {
            // SAFETY: a valid descriptor (the wgpu path creates the same); destroyed by `Drop`.
            unsafe { dev.create_sampler(&desc) }
        })
        .ok_or_else(|| Unsupported("not a Vulkan device".into()))?
        .map_err(|e| fail("sampler", e))?;
        self.note(1);
        self.samplers.insert((key, volume), sampler);
        Ok(())
    }

    /// (Re)creates the uniform bind group over the dynamic buffer.
    pub(crate) fn set_uniform_group(
        &mut self,
        buffer: &wgpu::Buffer,
        binding_size: u32,
        globals_size: u32,
    ) -> Res<()> {
        let layout = self.uniform_layout()?;
        // SAFETY: the wgpu-hal buffer is only used to create the bind group, while `buffer`
        // lives; the group keeps `buffer` alive through `keep`.
        let group = unsafe {
            let hb = buffer
                .as_hal::<Vulkan>()
                .ok_or_else(|| Unsupported("buffer without a Vulkan handle".into()))?;
            let buffers = [
                hal::BufferBinding::new_unchecked(
                    &*hb,
                    0,
                    NonZeroU64::new(u64::from(binding_size)),
                ),
                hal::BufferBinding::new_unchecked(
                    &*hb,
                    0,
                    NonZeroU64::new(u64::from(globals_size)),
                ),
            ];
            let entries = [
                hal::BindGroupEntry {
                    binding: 0,
                    resource_index: 0,
                    count: 1,
                },
                hal::BindGroupEntry {
                    binding: 1,
                    resource_index: 1,
                    count: 1,
                },
            ];
            with_hal(&self.device, |dev| {
                // SAFETY: entries match the layout; the buffer is a uniform buffer of this
                // device holding both ranges. Destroyed by `Drop` or after the next idle wait.
                dev.create_bind_group(&hal::BindGroupDescriptor {
                    label: None,
                    layout,
                    buffers: &buffers,
                    samplers: &[],
                    textures: &[],
                    entries: &entries,
                    acceleration_structures: &[],
                    external_textures: &[],
                })
            })
        }
        .ok_or_else(|| Unsupported("not a Vulkan device".into()))?
        .map_err(|e| fail("uniform bind group", e))?;
        self.note(1);
        self.keep.insert(Kept::Buffer(buffer.clone()));
        self.uniform_buffer = Some(buffer.clone());
        if let Some(old) = self.uniform_group.replace(group) {
            self.dead_groups.push(old);
        }
        Ok(())
    }

    /// The wgpu-hal view of `texture` (its whole default view), created on first use.
    fn view(&mut self, texture: &wgpu::Texture) -> Res<&hv::TextureView> {
        if !self.views.contains_key(texture) {
            let depth = texture.format().is_depth_stencil_format();
            let usage = texture.usage();
            let mut uses = wgpu::TextureUses::empty();
            if usage.contains(wgpu::TextureUsages::COPY_SRC) {
                uses |= wgpu::TextureUses::COPY_SRC;
            }
            if usage.contains(wgpu::TextureUsages::COPY_DST) {
                uses |= wgpu::TextureUses::COPY_DST;
            }
            if usage.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
                uses |= wgpu::TextureUses::RESOURCE;
            }
            if usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
                uses |= if depth {
                    wgpu::TextureUses::DEPTH_STENCIL_READ | wgpu::TextureUses::DEPTH_STENCIL_WRITE
                } else {
                    wgpu::TextureUses::COLOR_TARGET
                };
            }
            let desc = hal::TextureViewDescriptor {
                label: None,
                format: texture.format(),
                dimension: match texture.dimension() {
                    wgpu::TextureDimension::D3 => wgpu::TextureViewDimension::D3,
                    _ => wgpu::TextureViewDimension::D2,
                },
                usage: uses,
                range: wgpu::ImageSubresourceRange {
                    aspect: wgpu::TextureAspect::All,
                    base_mip_level: 0,
                    mip_level_count: Some(texture.mip_level_count()),
                    base_array_layer: 0,
                    array_layer_count: Some(1),
                },
            };
            // SAFETY: the wgpu-hal texture is only used to create the view, while `texture`
            // lives; the map keeps `texture` alive as long as the view, destroyed after a GPU
            // wait (`idle`, `Drop`).
            let view = unsafe {
                let t = texture
                    .as_hal::<Vulkan>()
                    .ok_or_else(|| Unsupported("texture without a Vulkan handle".into()))?;
                with_hal(&self.device, |dev| dev.create_texture_view(&*t, &desc))
            }
            .ok_or_else(|| Unsupported("not a Vulkan device".into()))?
            .map_err(|e| fail("texture view", e))?;
            self.note(1);
            self.views.insert(texture.clone(), view);
        }
        Ok(&self.views[texture])
    }

    /// (Re)creates the texture bind groups of pass `pass`, one per slot-0 variant.
    pub(crate) fn set_pass_groups(
        &mut self,
        pass: usize,
        layout: (u8, u8),
        variants: &[Vec<SlotView<'_>>],
    ) -> Res<()> {
        let li = self.layout(layout)?;
        for v in variants {
            for s in v {
                self.sampler(s.sampler, s.volume)?;
                self.view(s.view.texture())?;
            }
        }
        let mut groups = Vec::with_capacity(variants.len());
        let mut textures = Vec::new();
        for v in variants {
            let bindings: Vec<hal::TextureBinding<'_, hv::TextureView>> = v
                .iter()
                .map(|s| hal::TextureBinding {
                    view: &self.views[s.view.texture()],
                    usage: wgpu::TextureUses::RESOURCE,
                })
                .collect();
            let samplers: Vec<&hv::Sampler> = v
                .iter()
                .map(|s| &self.samplers[&(s.sampler, s.volume)])
                .collect();
            let mut entries = Vec::with_capacity(v.len() * 2);
            for (k, s) in v.iter().enumerate() {
                entries.push(hal::BindGroupEntry {
                    binding: 2 * s.slot,
                    resource_index: k as u32,
                    count: 1,
                });
                entries.push(hal::BindGroupEntry {
                    binding: 2 * s.slot + 1,
                    resource_index: k as u32,
                    count: 1,
                });
            }
            let layout = &self.layouts[li].group;
            let group = with_hal(&self.device, |dev| {
                // SAFETY: entries are sorted by binding and match the layout (the wgpu path
                // binds the same textures and samplers); destroyed by `Drop` or after the next
                // idle wait.
                unsafe {
                    dev.create_bind_group(&hal::BindGroupDescriptor {
                        label: None,
                        layout,
                        buffers: &[],
                        samplers: &samplers,
                        textures: &bindings,
                        entries: &entries,
                        acceleration_structures: &[],
                        external_textures: &[],
                    })
                }
            })
            .ok_or_else(|| Unsupported("not a Vulkan device".into()))?
            .map_err(|e| fail("texture bind group", e))?;
            groups.push(group);
            for s in v {
                textures.push(s.view.texture().clone());
            }
        }
        self.note(groups.len() as i64);
        if self.pass_groups.len() <= pass {
            self.pass_groups.resize_with(pass + 1, Vec::new);
            self.pass_textures.resize_with(pass + 1, Vec::new);
        }
        let old = std::mem::replace(&mut self.pass_groups[pass], groups);
        self.dead_groups.extend(old);
        self.pass_textures[pass] = textures;
        Ok(())
    }

    /// Waits for the GPU, then destroys the replaced bind groups, the cached framebuffers and
    /// the views of textures the scene no longer owns (`owned`), and releases the wgpu objects
    /// only earlier frames used. Call before anything recorded frames use may be destroyed
    /// (rebuilds after resizes); the program must be rebuilt afterwards.
    pub(crate) fn idle(&mut self, owned: &dyn Fn(&wgpu::Texture) -> bool) {
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let groups = std::mem::take(&mut self.dead_groups);
        let framebuffers: Vec<vk::Framebuffer> =
            self.framebuffers.drain().map(|(_, f)| f).collect();
        let gone: Vec<wgpu::Texture> = self.views.keys().filter(|t| !owned(t)).cloned().collect();
        let views: Vec<hv::TextureView> =
            gone.iter().filter_map(|t| self.views.remove(t)).collect();
        let n = (groups.len() + framebuffers.len() + views.len()) as i64;
        with_hal(&self.device, |dev| {
            // SAFETY: the GPU is idle; no program records these groups, and the bind groups
            // still alive only use views of textures the scene owns (the rebuild that follows
            // recreates them over the scene's current textures).
            unsafe {
                for g in groups {
                    dev.destroy_bind_group(g);
                }
                for v in views {
                    dev.destroy_texture_view(v);
                }
            }
        });
        for f in framebuffers {
            // SAFETY: the GPU is idle; the next program creates the framebuffers it needs.
            unsafe { self.raw.destroy_framebuffer(f, None) };
        }
        self.note(-n);
        // The program's framebuffers are gone: it must be rebuilt before the next frame.
        self.program = Program::default();
        self.keep.clear();
        self.keep
            .extend(self.uniform_buffer.iter().cloned().map(Kept::Buffer));
        self.adopted.retain(|t| owned(t));
    }

    /// After a rebuild that followed [`Self::idle`] with no frame recorded in between: destroys
    /// the bind groups it replaced and keeps only the wgpu objects the new program uses.
    pub(crate) fn compact(&mut self) {
        let groups = std::mem::take(&mut self.dead_groups);
        let n = groups.len() as i64;
        with_hal(&self.device, |dev| {
            for g in groups {
                // SAFETY: replaced after the last GPU wait, never recorded since.
                unsafe { dev.destroy_bind_group(g) };
            }
        });
        self.note(-n);
        self.keep.clear();
        self.keep.extend(self.program.keep.iter().cloned());
        self.keep
            .extend(self.uniform_buffer.iter().cloned().map(Kept::Buffer));
    }

    /// Whether a program is ready to record.
    pub(crate) fn has_program(&self) -> bool {
        !self.program.cmds.is_empty()
    }

    /// Render pass with a colour attachment (cleared or loaded) and an optional depth
    /// attachment (cleared or loaded), compatible with wgpu-hal's pipelines (same attachments,
    /// layouts and single subpass as its `make_render_pass`).
    fn render_pass(&mut self, clear: bool, depth: Option<bool>) -> Res<vk::RenderPass> {
        if let Some(rp) = self.render_passes.get(&(clear, depth)) {
            return Ok(*rp);
        }
        let load = |clear| {
            if clear {
                vk::AttachmentLoadOp::CLEAR
            } else {
                vk::AttachmentLoadOp::LOAD
            }
        };
        let color_layout = Use::Color.layout();
        let depth_layout = Use::Depth.layout();
        let mut attachments = vec![
            vk::AttachmentDescription::default()
                .format(COLOR_FORMAT)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(load(clear))
                .store_op(vk::AttachmentStoreOp::STORE)
                .initial_layout(color_layout)
                .final_layout(color_layout),
        ];
        if let Some(c) = depth {
            attachments.push(
                vk::AttachmentDescription::default()
                    .format(DEPTH_FORMAT)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(load(c))
                    .store_op(vk::AttachmentStoreOp::STORE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(depth_layout)
                    .final_layout(depth_layout),
            );
        }
        let color_refs = [vk::AttachmentReference {
            attachment: 0,
            layout: color_layout,
        }];
        let resolve_refs = [vk::AttachmentReference {
            attachment: vk::ATTACHMENT_UNUSED,
            layout: vk::ImageLayout::UNDEFINED,
        }];
        let depth_ref = vk::AttachmentReference {
            attachment: 1,
            layout: depth_layout,
        };
        let mut subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_refs)
            .resolve_attachments(&resolve_refs);
        if depth.is_some() {
            subpass = subpass.depth_stencil_attachment(&depth_ref);
        }
        let subpasses = [subpass];
        let info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses);
        // SAFETY: a valid create info whose arrays outlive the call; destroyed by `Drop`.
        let rp = unsafe { self.raw.create_render_pass(&info, None) }
            .map_err(|e| fail("render pass", e))?;
        self.note(1);
        self.render_passes.insert((clear, depth), rp);
        Ok(rp)
    }

    fn framebuffer(
        &mut self,
        render_pass: vk::RenderPass,
        color: vk::ImageView,
        depth: vk::ImageView,
        size: (u32, u32),
    ) -> Res<vk::Framebuffer> {
        let key = (render_pass, color, depth, size.0, size.1);
        if let Some(f) = self.framebuffers.get(&key) {
            return Ok(*f);
        }
        let views = [color, depth];
        let n = if depth == vk::ImageView::null() { 1 } else { 2 };
        let info = vk::FramebufferCreateInfo::default()
            .render_pass(render_pass)
            .attachments(&views[..n])
            .width(size.0)
            .height(size.1)
            .layers(1);
        // SAFETY: the views are attachments of `size` of this device, kept alive by `keep`
        // while the framebuffer is cached; destroyed by `idle` or `Drop`.
        let f = unsafe { self.raw.create_framebuffer(&info, None) }
            .map_err(|e| fail("framebuffer", e))?;
        self.note(1);
        self.framebuffers.insert(key, f);
        Ok(f)
    }

    /// Replaces the recorded frame with `steps` (the scene's segments, in order), and adopts
    /// the textures it uses for the first time.
    pub(crate) fn build_program(&mut self, ctx: &GpuContext, steps: &[Step<'_>]) -> Res<()> {
        #[cfg(test)]
        if faults::programs() {
            return Err(Unsupported("injected planning failure".into()));
        }
        // Image table: every render target and depth buffer a step touches.
        let mut images: Vec<(wgpu::Texture, vk::Image, Use)> = Vec::new();
        let mut index: HashMap<wgpu::Texture, u32> = HashMap::new();
        let mut image = |t: &wgpu::Texture, rest: Use| -> Res<u32> {
            if let Some(i) = index.get(t) {
                return Ok(*i);
            }
            // SAFETY: only the raw handle is read; `keep` holds the texture while the program
            // records it.
            let raw = unsafe {
                t.as_hal::<Vulkan>()
                    .map(|h| h.raw_handle())
                    .ok_or_else(|| Unsupported("texture without a Vulkan handle".into()))?
            };
            let i = images.len() as u32;
            images.push((t.clone(), raw, rest));
            index.insert(t.clone(), i);
            Ok(i)
        };
        let mut uses: Vec<Vec<(u32, Use)>> = Vec::with_capacity(steps.len());
        for step in steps {
            let mut u = Vec::new();
            match step {
                Step::Copy { src, dst, .. } => {
                    u.push((image(src, Use::Sampled)?, Use::CopySrc));
                    u.push((image(dst, Use::Sampled)?, Use::CopyDst));
                }
                Step::Render {
                    target,
                    depth,
                    sampled,
                    ..
                } => {
                    u.push((image(target, Use::Sampled)?, Use::Color));
                    if let Some((d, _, _)) = depth {
                        u.push((image(d, Use::Depth)?, Use::Depth));
                    }
                    for s in sampled {
                        u.push((image(s, Use::Sampled)?, Use::Sampled));
                    }
                }
            }
            uses.push(u);
        }
        let rest: Vec<Use> = images.iter().map(|(_, _, r)| *r).collect();
        let plan = plan_barriers(&uses, &rest).map_err(|i| {
            Unsupported(format!(
                "image {:?} used two ways in one step",
                images[i as usize].0
            ))
        })?;

        let mut keep: HashSet<Kept> = HashSet::new();
        for (t, _, _) in &images {
            keep.insert(Kept::Texture(t.clone()));
        }
        let mut program = Program::default();
        let barriers_of = |program: &mut Program, ts: &[Transition]| -> (u32, u32) {
            let first = program.barriers.len() as u32;
            for t in ts {
                let (_, raw, rest) = &images[t.image as usize];
                program.barriers.push(ImageBarrier {
                    image: *raw,
                    aspect: if *rest == Use::Depth {
                        vk::ImageAspectFlags::DEPTH
                    } else {
                        vk::ImageAspectFlags::COLOR
                    },
                    from: t.from,
                    to: t.to,
                });
            }
            (first, ts.len() as u32)
        };
        for (i, step) in steps.iter().enumerate() {
            let before = &plan.before[i];
            if i == 0 {
                let (first, count) = barriers_of(&mut program, before);
                program.cmds.push(Cmd::Barriers {
                    first,
                    count,
                    src: vk::PipelineStageFlags::ALL_COMMANDS,
                    dst: vk::PipelineStageFlags::ALL_COMMANDS,
                    global: true,
                });
            } else if !before.is_empty() {
                let src = before
                    .iter()
                    .fold(vk::PipelineStageFlags::empty(), |s, t| s | t.from.stages());
                let dst = before
                    .iter()
                    .fold(vk::PipelineStageFlags::empty(), |s, t| s | t.to.stages());
                let (first, count) = barriers_of(&mut program, before);
                program.cmds.push(Cmd::Barriers {
                    first,
                    count,
                    src,
                    dst,
                    global: false,
                });
            }
            let segment = i as u32;
            match step {
                Step::Copy { src, dst, size } => {
                    program.cmds.push(Cmd::Copy {
                        src: images[index[*src] as usize].1,
                        dst: images[index[*dst] as usize].1,
                        size: *size,
                        segment,
                    });
                }
                Step::Render {
                    target,
                    clear,
                    depth,
                    draws,
                    ..
                } => {
                    let size = (target.width(), target.height());
                    let rp = self.render_pass(clear.is_some(), depth.map(|d| d.2))?;
                    // SAFETY: only the raw handles of the graph's own views are read; they live
                    // until the next idle wait, which also destroys the framebuffers.
                    let cv = unsafe { self.view(target)?.raw_handle() };
                    let dv = match depth {
                        // SAFETY: as above.
                        Some((d, _, _)) => unsafe { self.view(d)?.raw_handle() },
                        None => vk::ImageView::null(),
                    };
                    let framebuffer = self.framebuffer(rp, cv, dv, size)?;
                    let hal = program.render_passes == 0;
                    program.render_passes += 1;
                    program.cmds.push(Cmd::Begin(Box::new(Begin {
                        render_pass: rp,
                        framebuffer,
                        size,
                        clear: *clear,
                        depth: depth.map(|d| d.2),
                        color: (*target).clone(),
                        depth_texture: depth.map(|d| d.0.clone()),
                        hal,
                        segment,
                    })));
                    for d in draws {
                        let mut buffers = Vec::with_capacity(d.vertex.len());
                        let mut offsets = Vec::with_capacity(d.vertex.len());
                        for (b, off) in &d.vertex {
                            buffers.push(raw_buffer(b)?);
                            offsets.push(*off);
                            keep.insert(Kept::Buffer((*b).clone()));
                        }
                        let index = match d.index {
                            Some((b, f)) => {
                                keep.insert(Kept::Buffer(b.clone()));
                                Some((
                                    raw_buffer(b)?,
                                    match f {
                                        wgpu::IndexFormat::Uint16 => vk::IndexType::UINT16,
                                        wgpu::IndexFormat::Uint32 => vk::IndexType::UINT32,
                                    },
                                ))
                            }
                            None => None,
                        };
                        if self
                            .pass_groups
                            .get(d.pass as usize)
                            .is_none_or(Vec::is_empty)
                        {
                            return Err(Unsupported(format!("pass {} has no bind group", d.pass)));
                        }
                        program.cmds.push(Cmd::Draw(program.draws.len() as u32));
                        program.draws.push(Draw {
                            pass: d.pass,
                            pipeline: d.pipeline as u32,
                            uniform_offset: d.uniform_offset,
                            buffers,
                            offsets,
                            index,
                            call: d.call,
                        });
                    }
                    program.cmds.push(Cmd::End { hal, segment });
                }
            }
        }
        // Back to the resting states, everything visible to whatever wgpu does next.
        let (first, count) = barriers_of(&mut program, &plan.after);
        program.cmds.push(Cmd::Barriers {
            first,
            count,
            src: vk::PipelineStageFlags::ALL_COMMANDS,
            dst: vk::PipelineStageFlags::ALL_COMMANDS,
            global: true,
        });
        program.segments = steps.len() as u32;
        if self.uniform_group.is_none() {
            return Err(Unsupported("no uniform bind group".into()));
        }

        // Textures used for the first time: the program's images, and what its draws sample.
        let mut fresh: Vec<(wgpu::Texture, Use)> = Vec::new();
        let note = |t: &wgpu::Texture, rest: Use, fresh: &mut Vec<(wgpu::Texture, Use)>| {
            if !self.adopted.contains(t) && !fresh.iter().any(|(f, _)| f == t) {
                fresh.push((t.clone(), rest));
            }
        };
        for (t, _, rest) in &images {
            note(t, *rest, &mut fresh);
        }
        for d in &program.draws {
            for t in &self.pass_textures[d.pass as usize] {
                note(t, Use::Sampled, &mut fresh);
            }
        }
        if !fresh.is_empty() {
            adopt(ctx, &fresh);
            self.adopted.extend(fresh.into_iter().map(|(t, _)| t));
        }

        // Objects of the previous program stay in `keep` until the GPU is idle.
        program.images = images.into_iter().map(|(t, _, _)| t).collect();
        program.keep = keep.into_iter().collect();
        self.keep.extend(program.keep.iter().cloned());
        self.program = program;
        Ok(())
    }

    /// Records the frame into a new command buffer (submit it after the frame's uploads and
    /// before anything that samples the scene's targets). With `queries`, timestamps `2i` and
    /// `2i + 1` around step `i`.
    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        state: &dyn FrameState,
        queries: Option<vk::QueryPool>,
    ) -> wgpu::CommandBuffer {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        // SAFETY: the closure records into the encoder's open command buffer and never ends it,
        // the wgpu encoder is untouched meanwhile, and every handle recorded belongs to this
        // device and outlives the GPU's use of the command buffer (`keep`, `idle`): the
        // contract of `record_into`.
        let recorded = unsafe {
            enc.as_hal_mut::<Vulkan, _, _>(|e| e.map(|e| self.record_into(e, state, queries)))
        };
        if recorded.is_none() {
            tracing::error!(target: "lumenwall_render::direct", "no Vulkan command encoder: frame not recorded");
        }
        enc.finish()
    }

    /// # Safety
    ///
    /// `e` is open on this graph's device; the program's handles are alive.
    unsafe fn record_into(
        &self,
        e: &mut hv::CommandEncoder,
        state: &dyn FrameState,
        queries: Option<vk::QueryPool>,
    ) {
        let raw = &self.raw;
        // SAFETY: `e` is in the recording state (contract).
        let cb = unsafe { e.raw_handle() };
        let Some(uniform) = &self.uniform_group else {
            return;
        };
        if let Some(q) = queries {
            // SAFETY: outside any render pass, on a pool of `2 * segments` queries.
            unsafe { raw.cmd_reset_query_pool(cb, q, 0, 2 * self.program.segments) };
        }
        let stamp = |index: u32| {
            if let Some(q) = queries {
                // SAFETY: a reset query of the pool, written once per frame.
                unsafe {
                    raw.cmd_write_timestamp(cb, vk::PipelineStageFlags::BOTTOM_OF_PIPE, q, index)
                };
            }
        };
        let mut pipeline = u32::MAX;
        for cmd in &self.program.cmds {
            match cmd {
                Cmd::Barriers {
                    first,
                    count,
                    src,
                    dst,
                    global,
                } => {
                    let memory = [vk::MemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                        .dst_access_mask(
                            vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                        )];
                    let memory: &[vk::MemoryBarrier<'_>] = if *global { &memory } else { &[] };
                    let all = &self.program.barriers
                        [*first as usize..(*first + *count) as usize];
                    let mut buf = [vk::ImageMemoryBarrier::default(); BARRIER_BATCH];
                    let mut chunks = all.chunks(BARRIER_BATCH).peekable();
                    if chunks.peek().is_none() {
                        // SAFETY: a memory barrier outside any render pass.
                        unsafe {
                            raw.cmd_pipeline_barrier(
                                cb,
                                *src,
                                *dst,
                                vk::DependencyFlags::empty(),
                                memory,
                                &[],
                                &[],
                            )
                        };
                    }
                    for chunk in chunks {
                        for (b, out) in chunk.iter().zip(buf.iter_mut()) {
                            *out = b.vk();
                        }
                        // SAFETY: outside any render pass; the images are alive (`keep`) and
                        // in the `old_layout` the plan derived from the resting states.
                        unsafe {
                            raw.cmd_pipeline_barrier(
                                cb,
                                *src,
                                *dst,
                                vk::DependencyFlags::empty(),
                                memory,
                                &[],
                                &buf[..chunk.len()],
                            )
                        };
                    }
                }
                Cmd::Copy {
                    src,
                    dst,
                    size,
                    segment,
                } => {
                    let layers = vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    };
                    let region = vk::ImageCopy {
                        src_subresource: layers,
                        src_offset: vk::Offset3D::default(),
                        dst_subresource: layers,
                        dst_offset: vk::Offset3D::default(),
                        extent: vk::Extent3D {
                            width: size.0,
                            height: size.1,
                            depth: 1,
                        },
                    };
                    stamp(2 * segment);
                    // SAFETY: both images are in the transfer layouts the previous barrier set;
                    // the extent fits both (the smaller of the two sizes).
                    unsafe {
                        raw.cmd_copy_image(
                            cb,
                            *src,
                            Use::CopySrc.layout(),
                            *dst,
                            Use::CopyDst.layout(),
                            &[region],
                        )
                    };
                    stamp(2 * segment + 1);
                }
                Cmd::Begin(b) => {
                    stamp(2 * b.segment);
                    // SAFETY: the attachments are in the layouts of the render pass.
                    unsafe { self.begin(e, cb, b) };
                    pipeline = u32::MAX;
                }
                Cmd::Draw(i) => {
                    let d = &self.program.draws[*i as usize];
                    let indices = match d.call {
                        DrawCall::Stream(s) => {
                            let n = state.stream_indices(s);
                            if n == 0 {
                                continue;
                            }
                            n
                        }
                        _ => 0,
                    };
                    let groups = &self.pass_groups[d.pass as usize];
                    let group = &groups[state.variant(d.pass).min(groups.len() - 1)];
                    let (p, layout) = &self.pipelines[d.pipeline as usize];
                    let layout = &self.layouts[*layout].pipeline;
                    // SAFETY: inside a render pass compatible with the pipeline; the groups
                    // match its layout, the buffers are alive (`keep`) and hold the ranges the
                    // wgpu path draws.
                    unsafe {
                        if pipeline != d.pipeline {
                            e.set_render_pipeline(p);
                            pipeline = d.pipeline;
                        }
                        e.set_bind_group(layout, 0, uniform, &[d.uniform_offset]);
                        e.set_bind_group(layout, 1, group, &[]);
                        raw.cmd_bind_vertex_buffers(cb, 0, &d.buffers, &d.offsets);
                        if let Some((b, t)) = d.index {
                            raw.cmd_bind_index_buffer(cb, b, 0, t);
                        }
                        match d.call {
                            DrawCall::Vertices { first, count } => {
                                raw.cmd_draw(cb, count, 1, first, 0);
                            }
                            DrawCall::Indexed { first, count, base } => {
                                raw.cmd_draw_indexed(cb, count, 1, first, base, 0);
                            }
                            DrawCall::Stream(_) => raw.cmd_draw_indexed(cb, indices, 1, 0, 0, 0),
                        }
                    }
                }
                Cmd::End { hal, segment } => {
                    // SAFETY: ends the render pass begun by the matching `Begin`.
                    unsafe {
                        if *hal {
                            e.end_render_pass();
                        } else {
                            raw.cmd_end_render_pass(cb);
                        }
                    }
                    stamp(2 * segment + 1);
                }
            }
        }
    }

    /// # Safety
    ///
    /// Outside any render pass of `e`, whose raw command buffer is `cb`; the attachments are in
    /// the layouts of `b.render_pass`.
    unsafe fn begin(&self, e: &mut hv::CommandEncoder, cb: vk::CommandBuffer, b: &Begin) {
        let (w, h) = b.size;
        let clear = b.clear.unwrap_or(wgpu::Color::TRANSPARENT);
        let load = |c: bool| {
            if c {
                hal::AttachmentOps::LOAD_CLEAR | hal::AttachmentOps::STORE
            } else {
                hal::AttachmentOps::LOAD | hal::AttachmentOps::STORE
            }
        };
        if b.hal {
            let dv = b.depth_texture.as_ref().and_then(|t| self.views.get(t));
            if let Some(cv) = self.views.get(&b.color) {
                let color = [Some(hal::ColorAttachment {
                    target: hal::Attachment {
                        view: cv,
                        usage: wgpu::TextureUses::COLOR_TARGET,
                    },
                    depth_slice: None,
                    resolve_target: None,
                    ops: load(b.clear.is_some()),
                    clear_value: clear,
                })];
                let desc = hal::RenderPassDescriptor::<hv::QuerySet, hv::TextureView> {
                    label: None,
                    extent: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    sample_count: 1,
                    color_attachments: &color,
                    depth_stencil_attachment: match (dv, b.depth) {
                        (Some(dv), Some(c)) => Some(hal::DepthStencilAttachment {
                            target: hal::Attachment {
                                view: dv,
                                usage: wgpu::TextureUses::DEPTH_STENCIL_WRITE,
                            },
                            depth_ops: load(c),
                            stencil_ops: hal::AttachmentOps::LOAD | hal::AttachmentOps::STORE,
                            clear_value: (1.0, 0),
                        }),
                        _ => None,
                    },
                    multiview_mask: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                };
                // SAFETY: valid attachments of this device in the descriptor's layouts.
                if unsafe { e.begin_render_pass(&desc) }.is_ok() {
                    return;
                }
            }
            tracing::error!(target: "lumenwall_render::direct", "wgpu-hal render pass not begun; recording it directly");
        }
        let color = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [
                    clear.r as f32,
                    clear.g as f32,
                    clear.b as f32,
                    clear.a as f32,
                ],
            },
        };
        let depth = vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: 1.0,
                stencil: 0,
            },
        };
        let clears = [color, depth];
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: w,
                height: h,
            },
        };
        // wgpu-hal's viewport: y flipped through a negative height.
        let viewport = vk::Viewport {
            x: 0.0,
            y: h as f32,
            width: w as f32,
            height: -(h as f32),
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let info = vk::RenderPassBeginInfo::default()
            .render_pass(b.render_pass)
            .framebuffer(b.framebuffer)
            .render_area(area)
            .clear_values(&clears[..if b.depth.is_some() { 2 } else { 1 }]);
        // SAFETY: outside any render pass (contract); the framebuffer matches the render pass.
        unsafe {
            self.raw.cmd_set_viewport(cb, 0, &[viewport]);
            self.raw.cmd_set_scissor(cb, 0, &[area]);
            self.raw
                .cmd_begin_render_pass(cb, &info, vk::SubpassContents::INLINE);
        }
    }

    /// GPU milliseconds of each step of one frame (tools): records the frame with timestamps,
    /// submits it after `uploads`, waits for the GPU.
    pub(crate) fn profile(
        &self,
        ctx: &GpuContext,
        state: &dyn FrameState,
        uploads: wgpu::CommandBuffer,
    ) -> Option<Vec<f64>> {
        let count = 2 * self.program.segments;
        if count == 0 {
            return None;
        }
        let info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::TIMESTAMP)
            .query_count(count);
        // SAFETY: a valid create info; destroyed below once the GPU is idle.
        let pool = unsafe { self.raw.create_query_pool(&info, None) }.ok()?;
        let frame = self.record(ctx.device(), state, Some(pool));
        ctx.queue().submit([uploads, frame]);
        ctx.wait_idle();
        let mut ticks = vec![0u64; count as usize];
        // SAFETY: every query was written by the finished frame; 64-bit results.
        let read = unsafe {
            self.raw.get_query_pool_results(
                pool,
                0,
                &mut ticks,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            )
        };
        // SAFETY: the GPU is idle; the pool is not used any more.
        unsafe { self.raw.destroy_query_pool(pool, None) };
        read.ok()?;
        let period = f64::from(ctx.queue().get_timestamp_period()) / 1e6;
        Some(
            ticks
                .chunks(2)
                .map(|t| t[1].saturating_sub(t[0]) as f64 * period)
                .collect(),
        )
    }
}

fn raw_buffer(b: &wgpu::Buffer) -> Res<vk::Buffer> {
    // SAFETY: only the raw handle is read; `keep` holds the buffer while it is recorded.
    unsafe {
        b.as_hal::<Vulkan>()
            .map(|h| h.raw_handle())
            .ok_or_else(|| Unsupported("buffer without a Vulkan handle".into()))
    }
}

/// Puts textures the direct frame uses for the first time in their resting states for
/// wgpu-core: render targets are cleared to transparent black first (their initialisation,
/// which wgpu-core would otherwise do lazily, then over a frame it never saw), depth buffers
/// go to `DEPTH_STENCIL_WRITE`, everything else to `RESOURCE`.
fn adopt(ctx: &GpuContext, textures: &[(wgpu::Texture, Use)]) {
    let mut enc = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lumenwall.direct.adopt"),
        });
    for (t, rest) in textures {
        if *rest == Use::Sampled && t.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
            let view = t.create_view(&wgpu::TextureViewDescriptor::default());
            let _ = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
    }
    enc.transition_resources(
        std::iter::empty(),
        textures.iter().map(|(t, rest)| wgpu::TextureTransition {
            texture: t,
            selector: None,
            state: if *rest == Use::Depth {
                wgpu::TextureUses::DEPTH_STENCIL_WRITE
            } else {
                wgpu::TextureUses::RESOURCE
            },
        }),
    );
    ctx.queue().submit([enc.finish()]);
}

/// Puts `textures` back to `RESOURCE` for wgpu-core (after external writes through
/// `Queue::write_texture`), in `encoder`, before the direct frame samples them.
pub(crate) fn restore_textures<'a>(
    encoder: &mut wgpu::CommandEncoder,
    textures: impl Iterator<Item = &'a wgpu::Texture>,
) {
    encoder.transition_resources(
        std::iter::empty(),
        textures.map(|t| wgpu::TextureTransition {
            texture: t,
            selector: None,
            state: wgpu::TextureUses::RESOURCE,
        }),
    );
}

impl Drop for DirectGraph {
    fn drop(&mut self) {
        self.idle(&|_| false);
        let mut n = 0i64;
        let samplers: Vec<hv::Sampler> = self.samplers.drain().map(|(_, s)| s).collect();
        let modules: Vec<hv::ShaderModule> = self.modules.drain(..).flatten().collect();
        let pipelines: Vec<hv::RenderPipeline> =
            self.pipelines.drain(..).map(|(p, _)| p).collect();
        let groups: Vec<hv::BindGroup> = self
            .pass_groups
            .drain(..)
            .flatten()
            .chain(self.uniform_group.take())
            .collect();
        let layouts: Vec<Layout> = self.layouts.drain(..).collect();
        let views: Vec<hv::TextureView> = self.views.drain().map(|(_, v)| v).collect();
        n += (samplers.len() + modules.len() + pipelines.len() + groups.len() + views.len()) as i64;
        n += 2 * layouts.len() as i64;
        let uniform_layout = self.uniform_layout.take();
        n += i64::from(uniform_layout.is_some());
        with_hal(&self.device, |dev| {
            // SAFETY: the GPU is idle (`idle` above) and nothing records with these objects
            // any more; bind groups go before their layouts, pipelines before theirs.
            unsafe {
                for g in groups {
                    dev.destroy_bind_group(g);
                }
                for p in pipelines {
                    dev.destroy_render_pipeline(p);
                }
                for l in layouts {
                    dev.destroy_pipeline_layout(l.pipeline);
                    dev.destroy_bind_group_layout(l.group);
                }
                for s in samplers {
                    dev.destroy_sampler(s);
                }
                for m in modules {
                    dev.destroy_shader_module(m);
                }
                if let Some(l) = uniform_layout {
                    dev.destroy_bind_group_layout(l);
                }
                for v in views {
                    dev.destroy_texture_view(v);
                }
            }
        });
        for (_, rp) in self.render_passes.drain() {
            // SAFETY: the GPU is idle; framebuffers using it were destroyed by `idle`.
            unsafe { self.raw.destroy_render_pass(rp, None) };
            n += 1;
        }
        created(-n);
    }
}

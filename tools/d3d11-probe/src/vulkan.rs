//! The workload on Vulkan through wgpu, set up the way lumenwall-render does it: Vulkan backend
//! only, the adapter's real limits, 4-8 MiB memory blocks, a Mailbox swap chain with frame
//! latency 1, one render pass per run of draws, `write_buffer` for the constants.

use std::borrow::Cow;
use std::collections::HashMap;
use std::num::NonZeroIsize;
use std::time::Instant;

use anyhow::{Result, anyhow};
use lumenwall_formats::tex::GpuFormat;
use lumenwall_shader::{FRAGMENT_ENTRY, ScalarKind, VERTEX_ENTRY};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};

use crate::common::{self, Attr, cb_size, fill_frame, fill_pass, quad, slot_dims};
use crate::plan::{OUTPUT, Src, TargetFormat, Workload};
use crate::shaders::{BytecodeCache, ResourceKind, resources};
use crate::sys::Window;
use crate::{Backend, LoadTimes};

fn wgpu_format(f: GpuFormat) -> wgpu::TextureFormat {
    use wgpu::TextureFormat as F;
    match f {
        GpuFormat::R8Unorm => F::R8Unorm,
        GpuFormat::Rg8Unorm => F::Rg8Unorm,
        GpuFormat::Rgba8Unorm => F::Rgba8Unorm,
        GpuFormat::Bc1RgbaUnorm => F::Bc1RgbaUnorm,
        GpuFormat::Bc2RgbaUnorm => F::Bc2RgbaUnorm,
        GpuFormat::Bc3RgbaUnorm => F::Bc3RgbaUnorm,
        GpuFormat::Bc7RgbaUnorm => F::Bc7RgbaUnorm,
        GpuFormat::R16Float => F::R16Float,
        GpuFormat::Rg16Float => F::Rg16Float,
        GpuFormat::Rgba16Float => F::Rgba16Float,
        GpuFormat::Rgb10a2Unorm => F::Rgb10a2Unorm,
    }
}

fn target_format(f: TargetFormat) -> wgpu::TextureFormat {
    match f {
        TargetFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
        TargetFormat::R8 => wgpu::TextureFormat::R8Unorm,
        TargetFormat::Rg8 => wgpu::TextureFormat::Rg8Unorm,
    }
}

struct Program {
    module: wgpu::ShaderModule,
    bgl0: wgpu::BindGroupLayout,
    bgl1: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    attrs: Vec<Attr>,
    stride: u32,
    cb_bytes: u32,
    time_offset: Option<usize>,
    /// Texture binding numbers (`2N`) of group 1.
    texture_bindings: Vec<u32>,
    has_frame_block: bool,
    has_pass_block: bool,
    /// Sampler bindings (`2N + 1`) the module declares.
    sampler_bindings: Vec<u32>,
}

struct DrawState {
    program: usize,
    target: usize,
    format: wgpu::TextureFormat,
    vb: wgpu::Buffer,
    cb: wgpu::Buffer,
    data: Vec<u8>,
    bg0: wgpu::BindGroup,
    bg1: wgpu::BindGroup,
}

pub struct Vulkan {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    bc: bool,
    surface_format: wgpu::TextureFormat,
    // After `load`:
    textures: Vec<Vec<wgpu::TextureView>>,
    blank: Option<wgpu::TextureView>,
    targets: Vec<wgpu::TextureView>,
    sampler: Option<wgpu::Sampler>,
    programs: Vec<Program>,
    pipelines: HashMap<(usize, wgpu::TextureFormat), wgpu::RenderPipeline>,
    draws: Vec<DrawState>,
    present: Option<DrawState>,
    frame_buf: Option<wgpu::Buffer>,
    frame_data: Vec<u8>,
}

impl Backend for Vulkan {
    fn create(window: &Window) -> Result<(Self, f64, String)> {
        let t = Instant::now();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let hwnd = NonZeroIsize::new(window.hwnd.0 as isize).ok_or_else(|| anyhow!("null hwnd"))?;
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Windows(WindowsDisplayHandle::new())),
            raw_window_handle: RawWindowHandle::Win32(Win32WindowHandle::new(hwnd)),
        };
        // SAFETY: the window outlives the surface (both live in `run`).
        let surface = unsafe { instance.create_surface_unsafe(target)? };
        let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
            .into_iter()
            .filter(|a| a.is_surface_supported(&surface))
            .min_by_key(|a| match a.get_info().device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                wgpu::DeviceType::VirtualGpu => 2,
                _ => 4,
            })
            .ok_or_else(|| anyhow!("no Vulkan adapter"))?;
        let info = adapter.get_info();
        let bc = adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("probe.device"),
                required_features: if bc {
                    wgpu::Features::TEXTURE_COMPRESSION_BC
                } else {
                    wgpu::Features::empty()
                },
                required_limits: adapter.limits(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Manual {
                    suballocated_device_memory_block_size: (4 << 20)..(8 << 20),
                },
                trace: wgpu::Trace::Off,
            }))?;
        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| anyhow!("no surface format"))?;
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        let mut config = surface
            .get_default_config(&adapter, OUTPUT.0, OUTPUT.1)
            .ok_or_else(|| anyhow!("surface not supported"))?;
        config.usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        config.format = surface_format;
        config.present_mode = present_mode;
        config.desired_maximum_frame_latency = 1;
        config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        config.view_formats = vec![];
        surface.configure(&device, &config);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        Ok((
            Self {
                surface,
                device,
                queue,
                bc,
                surface_format,
                textures: Vec::new(),
                blank: None,
                targets: Vec::new(),
                sampler: None,
                programs: Vec::new(),
                pipelines: HashMap::new(),
                draws: Vec::new(),
                present: None,
                frame_buf: None,
                frame_data: Vec::new(),
            },
            ms,
            format!(
                "{} (Vulkan via wgpu, {:?}, {:?}, BC {bc})",
                info.name, present_mode, surface_format
            ),
        ))
    }

    fn load(&mut self, w: &mut Workload, _cache: &BytecodeCache) -> Result<LoadTimes> {
        let mut times = LoadTimes::default();
        let dev = &self.device;

        // Shader modules (naga IR straight to wgpu) and layouts.
        let t = Instant::now();
        for (i, p) in w.programs.iter().enumerate() {
            let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&w.program_names[i]),
                source: wgpu::ShaderSource::Naga(Cow::Owned(p.program.module.clone())),
            });
            let res = resources(&p.program.module);
            let both = wgpu::ShaderStages::VERTEX_FRAGMENT;
            let mut e0 = Vec::new();
            let mut e1 = Vec::new();
            let mut texture_bindings = Vec::new();
            for (&(g, b), kind) in &res {
                let (ty, list) = match kind {
                    ResourceKind::Uniform(_) => (
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        &mut e0,
                    ),
                    ResourceKind::Texture => {
                        texture_bindings.push(b);
                        (
                            wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            &mut e1,
                        )
                    }
                    ResourceKind::Sampler => (
                        wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        &mut e1,
                    ),
                };
                debug_assert!(g <= 1);
                list.push(wgpu::BindGroupLayoutEntry {
                    binding: b,
                    visibility: both,
                    ty,
                    count: None,
                });
            }
            let bgl0 = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &e0,
            });
            let bgl1 = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &e1,
            });
            let layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&bgl0), Some(&bgl1)],
                immediate_size: 0,
            });
            let cb_bytes = match res.get(&(0, 0)) {
                Some(ResourceKind::Uniform(s)) => cb_size(*s),
                _ => 16,
            };
            let (attrs, stride) = common::attributes(p);
            let time_offset = p
                .program
                .reflection
                .uniform_block
                .as_ref()
                .and_then(|b| b.members.iter().find(|m| m.name == "g_Time"))
                .map(|m| m.offset as usize);
            self.programs.push(Program {
                module,
                bgl0,
                bgl1,
                layout,
                attrs,
                stride,
                cb_bytes,
                time_offset,
                texture_bindings,
                has_frame_block: res.contains_key(&(0, 1)),
                has_pass_block: res.contains_key(&(0, 0)),
                sampler_bindings: res
                    .iter()
                    .filter(|((g, _), k)| *g == 1 && **k == ResourceKind::Sampler)
                    .map(|((_, b), _)| *b)
                    .collect(),
            });
        }
        times.shaders_ms = t.elapsed().as_secs_f64() * 1000.0;

        // Textures: the real files, one `write_texture` per mip.
        let t = Instant::now();
        for tex in &mut w.textures {
            let images = std::mem::take(&mut tex.images);
            let mut views = Vec::new();
            for mips in &images {
                let (bw, bh) = mips.first().map_or((1, 1), |m| (m.0, m.1));
                let format = wgpu_format(tex.format);
                if format.is_bcn() && !self.bc {
                    return Err(anyhow!("texture {}: BC formats unsupported", tex.name));
                }
                let texture = dev.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&tex.name),
                    size: wgpu::Extent3d {
                        width: bw,
                        height: bh,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: mips.len() as u32,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let (blk, _) = format.block_dimensions();
                for (level, (mw, mh, data)) in mips.iter().enumerate() {
                    self.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture,
                            mip_level: level as u32,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(tex.format.bytes_per_row(*mw) as u32),
                            rows_per_image: Some(tex.format.rows_per_image(*mh) as u32),
                        },
                        wgpu::Extent3d {
                            width: mw.div_ceil(blk) * blk,
                            height: mh.div_ceil(blk) * blk,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                views.push(texture.create_view(&wgpu::TextureViewDescriptor::default()));
            }
            self.textures.push(views);
        }
        let blank = dev.create_texture(&wgpu::TextureDescriptor {
            label: Some("blank"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &blank,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0u8; 4],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        self.blank = Some(blank.create_view(&wgpu::TextureViewDescriptor::default()));
        self.queue.submit([]);
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        times.upload_ms = t.elapsed().as_secs_f64() * 1000.0;

        // Render targets.
        let t = Instant::now();
        for rt in &w.targets {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&rt.label),
                size: wgpu::Extent3d {
                    width: rt.width,
                    height: rt.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: target_format(rt.format),
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            self.targets
                .push(tex.create_view(&wgpu::TextureViewDescriptor::default()));
        }
        times.targets_ms = t.elapsed().as_secs_f64() * 1000.0;

        // Pipelines, buffers and bind groups.
        let t = Instant::now();
        self.sampler = Some(self.device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        }));
        let frame_bytes = w
            .programs
            .iter()
            .find_map(|p| match resources(&p.program.module).get(&(0, 1)) {
                Some(ResourceKind::Uniform(s)) => Some(cb_size(*s)),
                _ => None,
            })
            .unwrap_or(16);
        self.frame_data = vec![0; frame_bytes as usize];
        self.frame_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame"),
            size: u64::from(frame_bytes),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        let mut draws = Vec::new();
        for d in &w.draws {
            let format = target_format(w.targets[d.target].format);
            draws.push(self.draw_state(w, d.program, d.target, format, &d.slots, d)?);
        }
        self.draws = draws;
        let pd = crate::plan::Draw {
            program: w.present_program,
            slots: vec![(0, Src::Target(0))],
            target: usize::MAX,
            label: "present".into(),
            coverage: (1.0, 1.0),
        };
        self.present =
            Some(self.draw_state(w, pd.program, 0, self.surface_format, &pd.slots, &pd)?);
        let keys: Vec<(usize, wgpu::TextureFormat)> = self
            .draws
            .iter()
            .chain(self.present.iter())
            .map(|d| (d.program, d.format))
            .collect();
        for key in keys {
            if !self.pipelines.contains_key(&key) {
                let p = self.pipeline(key.0, key.1);
                self.pipelines.insert(key, p);
            }
        }
        self.queue.submit([]);
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        times.pipelines_ms = t.elapsed().as_secs_f64() * 1000.0;
        Ok(times)
    }

    fn frame(&mut self, w: &Workload, time: f32) -> Result<()> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            _ => return Ok(()),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let frame_buf = self
            .frame_buf
            .as_ref()
            .ok_or_else(|| anyhow!("not loaded"))?;
        fill_frame(&mut self.frame_data, time);
        self.queue.write_buffer(frame_buf, 0, &self.frame_data);
        for d in &mut self.draws {
            if let Some(o) = self.programs[d.program].time_offset
                && let Some(dst) = d.data.get_mut(o..o + 4)
            {
                dst.copy_from_slice(&time.to_le_bytes());
            }
            self.queue.write_buffer(&d.cb, 0, &d.data);
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let mut cleared = vec![false; w.targets.len()];
        let mut i = 0;
        while i < self.draws.len() {
            let target = self.draws[i].target;
            let mut j = i;
            while j < self.draws.len() && self.draws[j].target == target {
                j += 1;
            }
            let load = if cleared[target] {
                wgpu::LoadOp::Load
            } else {
                cleared[target] = true;
                wgpu::LoadOp::Clear(if target == 0 {
                    wgpu::Color::BLACK
                } else {
                    wgpu::Color::TRANSPARENT
                })
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets[target],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for d in &self.draws[i..j] {
                Self::record(&mut pass, &self.pipelines, d);
            }
            drop(pass);
            i = j;
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(d) = &self.present {
                Self::record(&mut pass, &self.pipelines, d);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(())
    }
}

impl Vulkan {
    fn record(
        pass: &mut wgpu::RenderPass<'_>,
        pipelines: &HashMap<(usize, wgpu::TextureFormat), wgpu::RenderPipeline>,
        d: &DrawState,
    ) {
        if let Some(p) = pipelines.get(&(d.program, d.format)) {
            pass.set_pipeline(p);
            pass.set_bind_group(0, &d.bg0, &[]);
            pass.set_bind_group(1, &d.bg1, &[]);
            pass.set_vertex_buffer(0, d.vb.slice(..));
            pass.draw(0..4, 0..1);
        }
    }

    fn pipeline(&self, program: usize, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        let p = &self.programs[program];
        let attributes: Vec<wgpu::VertexAttribute> = p
            .attrs
            .iter()
            .map(|a| wgpu::VertexAttribute {
                format: match (a.kind, a.comps) {
                    (ScalarKind::Float, 1) => wgpu::VertexFormat::Float32,
                    (ScalarKind::Float, 2) => wgpu::VertexFormat::Float32x2,
                    (ScalarKind::Float, 3) => wgpu::VertexFormat::Float32x3,
                    (ScalarKind::Float, _) => wgpu::VertexFormat::Float32x4,
                    (_, 1) => wgpu::VertexFormat::Sint32,
                    (_, 2) => wgpu::VertexFormat::Sint32x2,
                    (_, 3) => wgpu::VertexFormat::Sint32x3,
                    (_, _) => wgpu::VertexFormat::Sint32x4,
                },
                offset: u64::from(a.offset),
                shader_location: a.location,
            })
            .collect();
        let buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: u64::from(p.stride.max(4)),
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        })];
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: Some(&p.layout),
                vertex: wgpu::VertexState {
                    module: &p.module,
                    entry_point: Some(VERTEX_ENTRY),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &p.module,
                    entry_point: Some(FRAGMENT_ENTRY),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::SrcAlpha,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
    }

    fn draw_state(
        &self,
        w: &Workload,
        program: usize,
        target: usize,
        format: wgpu::TextureFormat,
        slots: &[(u32, Src)],
        draw: &crate::plan::Draw,
    ) -> Result<DrawState> {
        let p = &self.programs[program];
        let shader = &w.programs[program];
        let vdata = quad(&p.attrs, p.stride.max(4), draw.coverage);
        let vb = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: vdata.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&vb, 0, &vdata);
        let cb = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(p.cb_bytes),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut data = vec![0u8; p.cb_bytes as usize];
        fill_pass(&mut data, shader, 0.0, &|s| slot_dims(w, draw, s));
        let frame_buf = self
            .frame_buf
            .as_ref()
            .ok_or_else(|| anyhow!("no frame buffer"))?;
        let mut e0 = Vec::new();
        if p.has_pass_block {
            e0.push(wgpu::BindGroupEntry {
                binding: 0,
                resource: cb.as_entire_binding(),
            });
        }
        if p.has_frame_block {
            e0.push(wgpu::BindGroupEntry {
                binding: 1,
                resource: frame_buf.as_entire_binding(),
            });
        }
        let bg0 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &p.bgl0,
            entries: &e0,
        });
        let blank = self
            .blank
            .as_ref()
            .ok_or_else(|| anyhow!("no blank texture"))?;
        let sampler = self.sampler.as_ref().ok_or_else(|| anyhow!("no sampler"))?;
        let mut views: Vec<(u32, &wgpu::TextureView)> = Vec::new();
        for &b in &p.texture_bindings {
            let slot = b / 2;
            let view = match slots.iter().find(|(s, _)| *s == slot).map(|(_, s)| *s) {
                Some(Src::Tex(i)) => self.textures[i].first().unwrap_or(blank),
                Some(Src::Target(i)) => &self.targets[i],
                None => blank,
            };
            views.push((b, view));
        }
        let mut e1 = Vec::new();
        for (b, view) in &views {
            e1.push(wgpu::BindGroupEntry {
                binding: *b,
                resource: wgpu::BindingResource::TextureView(view),
            });
            if p.sampler_bindings.contains(&(*b + 1)) {
                e1.push(wgpu::BindGroupEntry {
                    binding: *b + 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                });
            }
        }
        let bg1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &p.bgl1,
            entries: &e1,
        });
        Ok(DrawState {
            program,
            target,
            format,
            vb,
            cb,
            data,
            bg0,
            bg1,
        })
    }
}

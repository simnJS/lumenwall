//! YUV → RGB conversion of decoded video frames (spec 07 §14), on the GPU.
//!
//! A frame arrives as two (Y + interleaved CbCr, e.g. NV12/P010) or three (Y, Cb, Cr, e.g.
//! I420) planes. [`YuvPlanes`] owns plane textures filled by `write_plane` (copy paths);
//! [`YuvConverter::source_from_views`] binds planes owned by someone else (zero-copy
//! paths). [`YuvConverter::encode`] draws one fullscreen triangle into an RGBA8 render
//! target (the wallpaper image the compositor scales to the outputs), applying the stream's
//! matrix (BT.601/709/2020…), range (limited/full), bit depth and chroma siting.
//!
//! 8-bit planes are sampled as UNORM with the hardware bilinear filter; deeper planes are
//! stored as `R16Uint`/`Rg16Uint` and filtered in the shader, so no optional 16-bit
//! normalized format is needed. Everything is created once per stream layout; a frame costs
//! the plane uploads (or nothing, zero-copy) plus one draw.

use crate::context::{GpuContext, MemoryToken};
use crate::targets::{RT_FORMAT, RenderTarget};

/// Y'CbCr → R'G'B' matrix of a stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum YuvMatrix {
    /// ITU-R BT.601 (SD).
    Bt601,
    /// ITU-R BT.709 (HD).
    #[default]
    Bt709,
    /// ITU-R BT.2020 non-constant luminance (UHD).
    Bt2020,
    /// SMPTE 240M.
    Smpte240m,
    /// FCC 73.682.
    Fcc,
}

impl YuvMatrix {
    /// Luma coefficients `(Kr, Kb)`.
    #[must_use]
    pub fn coefficients(self) -> (f64, f64) {
        match self {
            Self::Bt601 => (0.299, 0.114),
            Self::Bt709 => (0.2126, 0.0722),
            Self::Bt2020 => (0.2627, 0.0593),
            Self::Smpte240m => (0.212, 0.087),
            Self::Fcc => (0.30, 0.11),
        }
    }
}

/// Quantization range of a stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum YuvRange {
    /// "TV" / MPEG range: Y in [16, 235], C in [16, 240] (8-bit, scaled for deeper samples).
    #[default]
    Limited,
    /// "PC" / JPEG range: every code value.
    Full,
}

/// Storage of one sample in the plane textures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PlaneDepth {
    /// 8 bits per sample: `R8Unorm` / `Rg8Unorm`.
    #[default]
    U8,
    /// 9 to 16 bits per sample in 16-bit containers: `R16Uint` / `Rg16Uint`.
    U16,
}

/// How the chroma samples are stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChromaPlanes {
    /// One plane with both chroma samples per texel (NV12, P010…).
    #[default]
    Interleaved,
    /// Like `Interleaved` with V before U (NV21).
    InterleavedSwapped,
    /// Two planes, U then V (I420, I444…).
    Planar,
}

/// Geometry of a frame's planes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct YuvLayout {
    /// Luma plane size.
    pub width: u32,
    pub height: u32,
    /// Chroma plane size.
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub planes: ChromaPlanes,
    pub depth: PlaneDepth,
}

impl YuvLayout {
    /// Number of plane textures (2 or 3).
    #[must_use]
    pub fn plane_count(&self) -> usize {
        match self.planes {
            ChromaPlanes::Planar => 3,
            ChromaPlanes::Interleaved | ChromaPlanes::InterleavedSwapped => 2,
        }
    }

    /// Size of plane `i`.
    #[must_use]
    pub fn plane_size(&self, i: usize) -> (u32, u32) {
        if i == 0 {
            (self.width, self.height)
        } else {
            (self.chroma_width, self.chroma_height)
        }
    }

    /// Texture format of plane `i`.
    #[must_use]
    pub fn plane_format(&self, i: usize) -> wgpu::TextureFormat {
        let two = i > 0 && self.plane_count() == 2;
        match (self.depth, two) {
            (PlaneDepth::U8, false) => wgpu::TextureFormat::R8Unorm,
            (PlaneDepth::U8, true) => wgpu::TextureFormat::Rg8Unorm,
            (PlaneDepth::U16, false) => wgpu::TextureFormat::R16Uint,
            (PlaneDepth::U16, true) => wgpu::TextureFormat::Rg16Uint,
        }
    }

    /// Bytes of one texel row of plane `i` (tightly packed).
    #[must_use]
    pub fn plane_row_bytes(&self, i: usize) -> u32 {
        let (w, _) = self.plane_size(i);
        let channels = if i > 0 && self.plane_count() == 2 {
            2
        } else {
            1
        };
        let bytes = match self.depth {
            PlaneDepth::U8 => 1,
            PlaneDepth::U16 => 2,
        };
        w * channels * bytes
    }

    /// GPU bytes of every plane.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        (0..self.plane_count())
            .map(|i| u64::from(self.plane_row_bytes(i)) * u64::from(self.plane_size(i).1))
            .sum()
    }
}

/// Colorimetry of a stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YuvColor {
    pub matrix: YuvMatrix,
    pub range: YuvRange,
    /// Significant bits per sample (8, 10, 12…).
    pub bits: u32,
    /// Low padding bits inside the 16-bit container (P010: 6; LSB-aligned formats: 0).
    pub shift: u32,
    /// Chroma siting: offset of the chroma sampling position, in chroma texels, relative to
    /// a centred chroma sample (MPEG-2 "left" 4:2:0 = `(0.25, 0)`).
    pub chroma_offset: [f32; 2],
}

impl Default for YuvColor {
    fn default() -> Self {
        Self {
            matrix: YuvMatrix::Bt709,
            range: YuvRange::Limited,
            bits: 8,
            shift: 0,
            chroma_offset: [0.0, 0.0],
        }
    }
}

/// Rows of the 3×4 matrix mapping `(sample_Y, sample_Cb, sample_Cr, 1)` to R'G'B', where a
/// sample is what the shader reads: `code/255` for 8-bit UNORM planes, the raw 16-bit
/// container value for `U16` planes. Pure, for testing.
#[must_use]
pub fn conversion_matrix(color: &YuvColor, depth: PlaneDepth) -> [[f32; 4]; 3] {
    let bits = color.bits.clamp(1, 16);
    // sample → code value at the stream's bit depth.
    let to_code = match depth {
        PlaneDepth::U8 => 255.0,
        PlaneDepth::U16 => 1.0 / f64::from(1u32 << color.shift.min(15)),
    };
    let scale = f64::from(1u32 << bits.saturating_sub(8));
    let max = f64::from((1u32 << bits) - 1);
    // Normalized Y' = ay·code + by ∈ [0, 1]; C' = ac·code + bc ∈ [-0.5, 0.5].
    let (ay, by, ac, bc) = match color.range {
        YuvRange::Limited => (
            1.0 / (219.0 * scale),
            -16.0 / 219.0,
            1.0 / (224.0 * scale),
            -128.0 / 224.0,
        ),
        YuvRange::Full => (
            1.0 / max,
            0.0,
            1.0 / max,
            -f64::from(1u32 << (bits - 1)) / max,
        ),
    };
    let (ay, ac) = (ay * to_code, ac * to_code);
    let (kr, kb) = color.matrix.coefficients();
    let kg = 1.0 - kr - kb;
    // R = Y + 2(1−Kr)·Cr ; B = Y + 2(1−Kb)·Cb ; G = Y − (2Kb(1−Kb)/Kg)·Cb − (2Kr(1−Kr)/Kg)·Cr
    let rows = [
        [1.0, 0.0, 2.0 * (1.0 - kr)],
        [
            1.0,
            -2.0 * kb * (1.0 - kb) / kg,
            -2.0 * kr * (1.0 - kr) / kg,
        ],
        [1.0, 2.0 * (1.0 - kb), 0.0],
    ];
    rows.map(|[y, cb, cr]| {
        [
            (y * ay) as f32,
            (cb * ac) as f32,
            (cr * ac) as f32,
            (y * by + (cb + cr) * bc) as f32,
        ]
    })
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    r: [f32; 4],
    g: [f32; 4],
    b: [f32; 4],
    chroma: [f32; 4],
    dst: [f32; 4],
    crop: [f32; 4],
}

const FLOAT_WGSL: &str = include_str!("shaders/yuv_float.wgsl");
const UINT_WGSL: &str = include_str!("shaders/yuv_uint.wgsl");

/// Plane bindings and parameters of one stream, ready for [`YuvConverter::encode`].
pub struct YuvSource {
    depth: PlaneDepth,
    planes: ChromaPlanes,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    written: Option<Params>,
}

impl YuvSource {
    /// Upload the conversion parameters for `color`, a target of `target` pixels and planes
    /// whose visible part is `crop` (fraction of their size, `[1, 1]` = whole planes); only
    /// when they changed.
    pub fn set_params(
        &mut self,
        ctx: &GpuContext,
        color: &YuvColor,
        target: (u32, u32),
        crop: [f32; 2],
    ) {
        let m = conversion_matrix(color, self.depth);
        let (w, h) = (target.0.max(1) as f32, target.1.max(1) as f32);
        let params = Params {
            r: m[0],
            g: m[1],
            b: m[2],
            chroma: [
                color.chroma_offset[0],
                color.chroma_offset[1],
                if self.planes == ChromaPlanes::Planar {
                    1.0
                } else {
                    0.0
                },
                if self.planes == ChromaPlanes::InterleavedSwapped {
                    1.0
                } else {
                    0.0
                },
            ],
            dst: [w, h, 1.0 / w, 1.0 / h],
            crop: [crop[0], crop[1], 0.0, 0.0],
        };
        if self.written != Some(params) {
            ctx.queue()
                .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&params));
            self.written = Some(params);
        }
    }
}

/// Plane textures of one stream layout, filled by the CPU copy paths.
pub struct YuvPlanes {
    layout: YuvLayout,
    textures: Vec<wgpu::Texture>,
    source: YuvSource,
    _memory: MemoryToken,
}

impl YuvPlanes {
    #[must_use]
    pub fn layout(&self) -> &YuvLayout {
        &self.layout
    }

    /// Plane texture `i` (also usable as a copy destination).
    #[must_use]
    pub fn texture(&self, i: usize) -> &wgpu::Texture {
        &self.textures[i]
    }

    /// Upload plane `i` from `data` with `bytes_per_row` bytes between rows (any stride at
    /// least [`YuvLayout::plane_row_bytes`]).
    pub fn write_plane(&self, ctx: &GpuContext, i: usize, data: &[u8], bytes_per_row: u32) {
        let (w, h) = self.layout.plane_size(i);
        ctx.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.textures[i],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    #[must_use]
    pub fn source(&self) -> &YuvSource {
        &self.source
    }

    pub fn source_mut(&mut self) -> &mut YuvSource {
        &mut self.source
    }
}

/// The two conversion pipelines (8-bit sampled, 16-bit loaded), shared by every stream.
pub struct YuvConverter {
    float_layout: wgpu::BindGroupLayout,
    uint_layout: wgpu::BindGroupLayout,
    float_pipeline: wgpu::RenderPipeline,
    uint_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
        },
        count: None,
    }
}

impl YuvConverter {
    #[must_use]
    pub fn new(ctx: &GpuContext) -> Self {
        let device = ctx.device();
        let float = wgpu::TextureSampleType::Float { filterable: true };
        let float_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumen.yuv.float.bgl"),
            entries: &[
                texture_entry(0, float),
                texture_entry(1, float),
                texture_entry(2, float),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                uniform_entry(4),
            ],
        });
        let uint = wgpu::TextureSampleType::Uint;
        let uint_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumen.yuv.uint.bgl"),
            entries: &[
                texture_entry(0, uint),
                texture_entry(1, uint),
                texture_entry(2, uint),
                uniform_entry(4),
            ],
        });
        let pipeline = |label: &str, layout: &wgpu::BindGroupLayout, source: &str| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: RT_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: ctx.pipeline_cache(),
            })
        };
        let float_pipeline = pipeline("lumen.yuv.float", &float_layout, FLOAT_WGSL);
        let uint_pipeline = pipeline("lumen.yuv.uint", &uint_layout, UINT_WGSL);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lumen.yuv.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        ctx.note_resources(11);
        Self {
            float_layout,
            uint_layout,
            float_pipeline,
            uint_pipeline,
            sampler,
        }
    }

    /// Plane textures for `layout` (load time / stream reconfiguration only).
    #[must_use]
    pub fn create_planes(&self, ctx: &GpuContext, layout: &YuvLayout) -> YuvPlanes {
        let textures: Vec<wgpu::Texture> = (0..layout.plane_count())
            .map(|i| {
                let (w, h) = layout.plane_size(i);
                ctx.device().create_texture(&wgpu::TextureDescriptor {
                    label: Some("lumen.yuv.plane"),
                    size: wgpu::Extent3d {
                        width: w.max(1),
                        height: h.max(1),
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: layout.plane_format(i),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            })
            .collect();
        ctx.note_resources(textures.len() as u64);
        let views: Vec<wgpu::TextureView> = textures
            .iter()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
            .collect();
        let third = views.get(2).unwrap_or(&views[1]);
        let source = self.source_from_views(
            ctx,
            layout.depth,
            layout.planes,
            [&views[0], &views[1], third],
        );
        YuvPlanes {
            layout: *layout,
            textures,
            source,
            _memory: ctx.memory().track(layout.bytes()),
        }
    }

    /// Binds plane views owned elsewhere (zero-copy imports). `views[2]` is ignored unless
    /// `planes` is [`ChromaPlanes::Planar`] (pass `views[1]` again). Views must have the
    /// formats of [`YuvLayout::plane_format`] for `depth`.
    #[must_use]
    pub fn source_from_views(
        &self,
        ctx: &GpuContext,
        depth: PlaneDepth,
        planes: ChromaPlanes,
        views: [&wgpu::TextureView; 3],
    ) -> YuvSource {
        let uniform = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumen.yuv.params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(views[0]),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(views[1]),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(views[2]),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: uniform.as_entire_binding(),
            },
        ];
        let layout = match depth {
            PlaneDepth::U8 => {
                entries.push(wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                });
                &self.float_layout
            }
            PlaneDepth::U16 => &self.uint_layout,
        };
        let bind_group = ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lumen.yuv.bg"),
            layout,
            entries: &entries,
        });
        ctx.note_resources(2);
        YuvSource {
            depth,
            planes,
            uniform,
            bind_group,
            written: None,
        }
    }

    /// Record the conversion of `source` into `target` (whole target, no blending).
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        source: &YuvSource,
        target: &RenderTarget,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lumen.yuv"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Every pixel is written; a clear is free on tiled GPUs, a load is not.
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(match source.depth {
            PlaneDepth::U8 => &self.float_pipeline,
            PlaneDepth::U16 => &self.uint_pipeline,
        });
        pass.set_bind_group(0, &source.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &[[f32; 4]; 3], y: f32, cb: f32, cr: f32) -> [f32; 3] {
        let v = [y, cb, cr, 1.0];
        m.map(|row| row.iter().zip(v).map(|(a, b)| a * b).sum::<f32>())
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 2e-3)
    }

    #[test]
    fn bt709_limited_8bit_black_white_red() {
        let c = YuvColor::default();
        let m = conversion_matrix(&c, PlaneDepth::U8);
        let s = |v: f32| v / 255.0;
        assert!(close(apply(&m, s(16.0), s(128.0), s(128.0)), [0.0; 3]));
        assert!(close(apply(&m, s(235.0), s(128.0), s(128.0)), [1.0; 3]));
        // BT.709 limited-range red: Y' = 63, Cb = 102, Cr = 240 (rounded codes).
        let red = apply(&m, s(63.0), s(102.0), s(240.0));
        assert!((red[0] - 1.0).abs() < 0.01 && red[1].abs() < 0.01 && red[2].abs() < 0.01);
    }

    #[test]
    fn bt601_full_range() {
        let c = YuvColor {
            matrix: YuvMatrix::Bt601,
            range: YuvRange::Full,
            ..YuvColor::default()
        };
        let m = conversion_matrix(&c, PlaneDepth::U8);
        let s = |v: f32| v / 255.0;
        assert!(close(apply(&m, 0.0, s(128.0), s(128.0)), [0.0, 0.0, 0.0]));
        assert!(close(apply(&m, 1.0, s(128.0), s(128.0)), [1.0, 1.0, 1.0]));
        // JPEG green: Y = 149.685, Cb = 43.527, Cr = 21.234 → (0, 1, 0).
        assert!(close(
            apply(&m, s(149.685), s(43.527), s(21.234)),
            [0.0, 1.0, 0.0]
        ));
    }

    #[test]
    fn p010_msb_aligned_and_lsb_aligned_10bit_agree() {
        let msb = YuvColor {
            matrix: YuvMatrix::Bt2020,
            bits: 10,
            shift: 6,
            ..YuvColor::default()
        };
        let lsb = YuvColor { shift: 0, ..msb };
        let a = conversion_matrix(&msb, PlaneDepth::U16);
        let b = conversion_matrix(&lsb, PlaneDepth::U16);
        // 10-bit limited black 64, white 940, neutral chroma 512.
        let (k, w, n) = (64.0f32, 940.0f32, 512.0f32);
        assert!(close(apply(&a, k * 64.0, n * 64.0, n * 64.0), [0.0; 3]));
        assert!(close(apply(&a, w * 64.0, n * 64.0, n * 64.0), [1.0; 3]));
        assert!(close(apply(&b, k, n, n), [0.0; 3]));
        assert!(close(apply(&b, w, n, n), [1.0; 3]));
    }

    #[test]
    fn layouts() {
        let nv12 = YuvLayout {
            width: 3840,
            height: 2160,
            chroma_width: 1920,
            chroma_height: 1080,
            planes: ChromaPlanes::Interleaved,
            depth: PlaneDepth::U8,
        };
        assert_eq!(nv12.plane_count(), 2);
        assert_eq!(nv12.plane_format(1), wgpu::TextureFormat::Rg8Unorm);
        assert_eq!(nv12.plane_row_bytes(1), 3840);
        assert_eq!(nv12.bytes(), 3840 * 2160 * 3 / 2);
        let i420p10 = YuvLayout {
            planes: ChromaPlanes::Planar,
            depth: PlaneDepth::U16,
            ..nv12
        };
        assert_eq!(i420p10.plane_count(), 3);
        assert_eq!(i420p10.plane_format(2), wgpu::TextureFormat::R16Uint);
        assert_eq!(i420p10.plane_row_bytes(2), 3840);
    }
}

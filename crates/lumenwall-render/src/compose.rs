//! Final composition of `_rt_FullFrameBuffer` to an output (spec 04 §21).
//!
//! The UV window is a pure function of the output and wallpaper sizes,
//! recomputed only when one of them changes. The GPU part is one fullscreen
//! triangle per output, blending off, alpha forced to 1 (opaque output).

use std::collections::HashMap;

use crate::context::GpuContext;
use crate::math::UvRange;
use crate::targets::RenderTarget;
use crate::texture::{AddressMode, SamplerKey};

/// Output scaling mode (spec 04 §21.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScalingMode {
    /// Stretch the wallpaper over the output.
    Stretch,
    /// Cover the output, cropping the excess.
    Fill,
    /// Fit inside the output, leaving bands.
    Fit,
    /// Reference default: adjust on the width when output and wallpaper have
    /// the same orientation, on the height otherwise.
    #[default]
    Default,
}

/// Wrap mode of `_rt_FullFrameBuffer` for an output (spec 04 §21.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ClampMode {
    /// Clamp to edge (default).
    #[default]
    Clamp,
    /// Transparent black border, hence black bands.
    Border,
    /// Repeat.
    Repeat,
}

impl ClampMode {
    #[must_use]
    pub fn address(self) -> AddressMode {
        match self {
            Self::Clamp => AddressMode::Clamp,
            Self::Border => AddressMode::Border,
            Self::Repeat => AddressMode::Repeat,
        }
    }
}

/// Horizontal adjustment `AH(pw, ph)`.
fn adjust_h(uv: &mut UvRange, vw: f32, vh: f32, pw: f32, ph: f32) {
    // parity: the product is truncated to an integer, the rest stays f32 (spec 04 §21.2).
    let n = ((vh / ph) * pw).trunc();
    uv.u0 = (n / 2.0 - vw / 2.0) / n;
    uv.u1 = (n / 2.0 + vw / 2.0) / n;
}

/// Vertical adjustment `AV(pw, ph)`.
fn adjust_v(uv: &mut UvRange, vw: f32, vh: f32, pw: f32, ph: f32) {
    let n = ((vw / pw) * ph).trunc();
    uv.v0 = (n / 2.0 - vh / 2.0) / n;
    uv.v1 = (n / 2.0 + vh / 2.0) / n;
}

/// UV window for an output of `(vw, vh)` pixels showing a wallpaper of
/// logical size `(pw, ph)` (spec 04 §21.2), truncations reproduced exactly.
#[must_use]
pub fn uv_range(mode: ScalingMode, vw: u32, vh: u32, pw: u32, ph: u32) -> UvRange {
    let mut uv = UvRange::FULL;
    if vw == 0 || vh == 0 || pw == 0 || ph == 0 {
        return uv;
    }
    let (vwf, vhf, pwf, phf) = (vw as f32, vh as f32, pw as f32, ph as f32);
    match mode {
        ScalingMode::Stretch => {}
        ScalingMode::Fill | ScalingMode::Fit => {
            let (sx, sy) = (vwf / pwf, vhf / phf);
            let m = if mode == ScalingMode::Fill {
                sx.max(sy)
            } else {
                sx.min(sy)
            };
            let w = (pwf * m).trunc();
            let h = (phf * m).trunc();
            if w != vwf {
                adjust_h(&mut uv, vwf, vhf, w, h);
            } else if h != vhf {
                adjust_v(&mut uv, vwf, vhf, w, h);
            }
        }
        ScalingMode::Default => {
            if (vh > vw && pw >= ph) || (vw > vh && ph > pw) {
                adjust_h(&mut uv, vwf, vhf, pwf, phf);
            }
            if (vw > vh && pw >= ph) || (vh > vw && ph > pw) {
                adjust_v(&mut uv, vwf, vhf, pwf, phf);
            }
        }
    }
    uv
}

/// A rectangle in desktop coordinates (y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DesktopRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Bounding box of a group of outputs (span mode, spec 04 §21.5).
#[must_use]
pub fn bounding_box(outputs: &[DesktopRect]) -> Option<DesktopRect> {
    let first = outputs.first()?;
    let (mut x0, mut y0) = (first.x, first.y);
    let (mut x1, mut y1) = (first.x + first.width, first.y + first.height);
    for o in &outputs[1..] {
        x0 = x0.min(o.x);
        y0 = y0.min(o.y);
        x1 = x1.max(o.x + o.width);
        y1 = y1.max(o.y + o.height);
    }
    Some(DesktopRect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// UV window of one output inside a span group (spec 04 §21.5). `base` is
/// [`uv_range`] computed with the bounding box size.
#[must_use]
pub fn span_range(base: UvRange, bbox: DesktopRect, output: DesktopRect) -> UvRange {
    let rg = (output.x - bbox.x) / bbox.width;
    let rd = (output.x + output.width - bbox.x) / bbox.width;
    let rh = (output.y - bbox.y) / bbox.height;
    let rb = (output.y + output.height - bbox.y) / bbox.height;
    let du = base.u1 - base.u0;
    let dv = base.v1 - base.v0;
    UvRange {
        u0: base.u0 + rg * du,
        u1: base.u0 + rd * du,
        v0: base.v0 + rh * dv,
        v1: base.v0 + rb * dv,
    }
}

/// Vertical inversion of spec 04 §21.4 (`v → 1 − v`). Lumenwall always renders in
/// the canonical orientation (row 0 at the top of the physical output), so
/// this is only needed by a consumer that reads the output back upside down.
#[must_use]
pub fn flip_vertical(uv: UvRange) -> UvRange {
    UvRange {
        v0: 1.0 - uv.v0,
        v1: 1.0 - uv.v1,
        ..uv
    }
}

/// Per-output composition settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComposeSettings {
    pub scaling: ScalingMode,
    pub clamp: ClampMode,
    /// Apply [`flip_vertical`].
    pub flip_vertical: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct ComposeParams {
    uv: [f32; 4],
    /// x: 1 = transparent border outside `[0, 1]`.
    mode: [f32; 4],
}

const COMPOSE_WGSL: &str = include_str!("shaders/compose.wgsl");

/// Shared composition pipelines (one per surface format).
pub struct Compositor {
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    module: wgpu::ShaderModule,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
}

impl Compositor {
    #[must_use]
    pub fn new(ctx: &GpuContext) -> Self {
        let device = ctx.device();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumenwall.compose.bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lumenwall.compose.layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lumenwall.compose"),
            source: wgpu::ShaderSource::Wgsl(COMPOSE_WGSL.into()),
        });
        ctx.note_resources(4);
        Self {
            layout,
            pipeline_layout,
            module,
            pipelines: HashMap::new(),
        }
    }

    /// Pipeline for a destination format (created once, at output attach time).
    pub fn prepare(&mut self, ctx: &GpuContext, format: wgpu::TextureFormat) {
        if self.pipelines.contains_key(&format) {
            return;
        }
        let pipeline = ctx
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("lumenwall.compose.pipeline"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: ctx.pipeline_cache(),
            });
        ctx.note_resources(1);
        self.pipelines.insert(format, pipeline);
    }

    /// Composition state of one output reading `source`.
    #[must_use]
    pub fn create_output(
        &mut self,
        ctx: &GpuContext,
        format: wgpu::TextureFormat,
        source: &RenderTarget,
        settings: ComposeSettings,
    ) -> OutputComposer {
        self.prepare(ctx, format);
        let uniform = ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumenwall.compose.params"),
            size: std::mem::size_of::<ComposeParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.note_resources(1);
        let bind_group = self.bind_group(ctx, source, &uniform, settings.clamp);
        OutputComposer {
            format,
            uniform,
            bind_group,
            settings,
            sizes: None,
            range: UvRange::default(),
            written: None,
        }
    }

    fn bind_group(
        &self,
        ctx: &GpuContext,
        source: &RenderTarget,
        uniform: &wgpu::Buffer,
        clamp: ClampMode,
    ) -> wgpu::BindGroup {
        // Border is emulated in the shader (identical result, no device feature needed).
        let address = match clamp {
            ClampMode::Repeat => AddressMode::Repeat,
            ClampMode::Clamp | ClampMode::Border => AddressMode::Clamp,
        };
        let sampler = ctx.sampler(SamplerKey {
            address,
            nearest: false,
            mipmaps: false,
        });
        ctx.note_resources(1);
        ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lumenwall.compose.bg"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        })
    }

    /// Rebind an output after its source was reallocated or its clamp mode changed.
    pub fn rebind(
        &self,
        ctx: &GpuContext,
        out: &mut OutputComposer,
        source: &RenderTarget,
        settings: ComposeSettings,
    ) {
        out.bind_group = self.bind_group(ctx, source, &out.uniform, settings.clamp);
        if out.settings != settings {
            out.settings = settings;
            out.sizes = None;
        }
    }

    /// Record the composition into `target` (a surface or headless view).
    ///
    /// `region` = `(x, y, width, height)` of the output inside the target, in
    /// pixels with origin top-left. `clear` clears the whole target first
    /// (the output driver's clear).
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        out: &OutputComposer,
        target: &wgpu::TextureView,
        region: [f32; 4],
        clear: Option<wgpu::Color>,
    ) {
        let Some(pipeline) = self.pipelines.get(&out.format) else {
            return;
        };
        let load = clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lumenwall.compose"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
        pass.set_viewport(region[0], region[1], region[2], region[3], 0.0, 1.0);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &out.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Composition state of one output.
pub struct OutputComposer {
    format: wgpu::TextureFormat,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    settings: ComposeSettings,
    /// `(vw, vh, pw, ph)` the range was computed for.
    sizes: Option<(u32, u32, u32, u32)>,
    range: UvRange,
    written: Option<ComposeParams>,
}

impl OutputComposer {
    /// Recompute the UV window if the output or wallpaper size changed, and
    /// upload it if it differs from the GPU copy. Returns the window, which
    /// also feeds the pointer mapping of the next frame (spec 04 §7.1).
    pub fn update(
        &mut self,
        ctx: &GpuContext,
        output: (u32, u32),
        wallpaper: (u32, u32),
    ) -> UvRange {
        let key = (output.0, output.1, wallpaper.0, wallpaper.1);
        if self.sizes != Some(key) {
            self.sizes = Some(key);
            let r = uv_range(
                self.settings.scaling,
                output.0,
                output.1,
                wallpaper.0,
                wallpaper.1,
            );
            self.range = if self.settings.flip_vertical {
                flip_vertical(r)
            } else {
                r
            };
        }
        self.upload(ctx);
        self.range
    }

    /// Use an explicit window (span mode, spec 04 §21.5).
    pub fn set_range(&mut self, ctx: &GpuContext, range: UvRange) {
        self.sizes = None;
        self.range = if self.settings.flip_vertical {
            flip_vertical(range)
        } else {
            range
        };
        self.upload(ctx);
    }

    fn upload(&mut self, ctx: &GpuContext) {
        let params = ComposeParams {
            uv: [self.range.u0, self.range.u1, self.range.v0, self.range.v1],
            mode: [
                if self.settings.clamp == ClampMode::Border {
                    1.0
                } else {
                    0.0
                },
                0.0,
                0.0,
                0.0,
            ],
        };
        if self.written != Some(params) {
            ctx.queue()
                .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&params));
            self.written = Some(params);
        }
    }

    #[must_use]
    pub fn range(&self) -> UvRange {
        self.range
    }

    #[must_use]
    pub fn settings(&self) -> ComposeSettings {
        self.settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn spec_example_fill_and_fit() {
        let fill = uv_range(ScalingMode::Fill, 1920, 1080, 1080, 1080);
        assert_eq!((fill.u0, fill.u1), (0.0, 1.0));
        assert_eq!((fill.v0, fill.v1), (0.21875, 0.78125));
        let fit = uv_range(ScalingMode::Fit, 1920, 1080, 1080, 1080);
        assert!(close(fit.u0, -0.388_888_9) && close(fit.u1, 1.388_889));
        assert_eq!((fit.v0, fit.v1), (0.0, 1.0));
    }

    #[test]
    fn stretch_and_same_size() {
        assert_eq!(
            uv_range(ScalingMode::Stretch, 1920, 1080, 640, 480),
            UvRange::FULL
        );
        for m in [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Stretch] {
            assert_eq!(uv_range(m, 1920, 1080, 1920, 1080), UvRange::FULL);
        }
        // Default: same aspect ratio, same orientation → AV with N = Vh → identity.
        assert_eq!(
            uv_range(ScalingMode::Default, 1920, 1080, 3840, 2160),
            UvRange::FULL
        );
    }

    #[test]
    fn default_mode_branches() {
        // Landscape output, landscape wallpaper (Vw > Vh, Pw >= Ph) → AV.
        let r = uv_range(ScalingMode::Default, 1920, 1080, 1920, 1200);
        assert_eq!((r.u0, r.u1), (0.0, 1.0));
        // N = trunc(1920/1920*1200) = 1200 → v0 = (600-540)/1200.
        assert_eq!((r.v0, r.v1), (0.05, 0.95));
        // Portrait output, landscape wallpaper → AH.
        let r = uv_range(ScalingMode::Default, 1080, 1920, 1920, 1080);
        // N = trunc(1920/1080*1920) = 3413 → u0 = (1706.5 - 540)/3413.
        assert!(close(r.u0, (3413.0 / 2.0 - 540.0) / 3413.0));
        assert_eq!((r.v0, r.v1), (0.0, 1.0));
        // Square output: no adjustment.
        assert_eq!(
            uv_range(ScalingMode::Default, 1000, 1000, 1920, 1080),
            UvRange::FULL
        );
        // Landscape output, portrait wallpaper (Vw > Vh, Ph > Pw) → AH.
        let r = uv_range(ScalingMode::Default, 1920, 1080, 1080, 1920);
        assert!(r.u0 < 0.0 && r.u1 > 1.0);
    }

    #[test]
    fn truncation_is_reproduced() {
        // 1366x768 output, 1920x1080 wallpaper: m = 0.71145833; 1920·m = 1366.0
        // and 1080·m = 768.375 → trunc 768 = Vh: no adjustment at all.
        let r = uv_range(ScalingMode::Fill, 1366, 768, 1920, 1080);
        assert_eq!(r, UvRange::FULL);
        // Fit 1000x700 output, 1920x1080 wallpaper: 1920·m = 999.99994 truncates
        // to 999 ≠ Vw, so the horizontal branch runs (the rounding effect the
        // spec asks to keep), leaving a sub-pixel band.
        let r = uv_range(ScalingMode::Fit, 1000, 700, 1920, 1080);
        let m = (1000.0f32 / 1920.0).min(700.0 / 1080.0);
        let (w, h) = ((1920.0 * m).trunc(), (1080.0 * m).trunc());
        assert_eq!((w, h), (999.0, 562.0));
        let n = ((700.0f32 / h) * w).trunc();
        assert_eq!(r.u0, (n / 2.0 - 500.0) / n);
        assert_eq!((r.v0, r.v1), (0.0, 1.0));
    }

    #[test]
    fn span_two_outputs() {
        let outs = [
            DesktopRect {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
            DesktopRect {
                x: 1920.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
        ];
        let bbox = bounding_box(&outs).unwrap();
        assert_eq!((bbox.width, bbox.height), (3840.0, 1080.0));
        let base = uv_range(ScalingMode::Fill, 3840, 1080, 3840, 1080);
        let left = span_range(base, bbox, outs[0]);
        let right = span_range(base, bbox, outs[1]);
        assert_eq!((left.u0, left.u1, left.v0, left.v1), (0.0, 0.5, 0.0, 1.0));
        assert_eq!((right.u0, right.u1), (0.5, 1.0));
        assert!(bounding_box(&[]).is_none());
        let f = flip_vertical(UvRange {
            u0: 0.0,
            u1: 1.0,
            v0: 0.25,
            v1: 0.75,
        });
        assert_eq!((f.v0, f.v1), (0.75, 0.25));
    }
}

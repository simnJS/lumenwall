//! Presentation surfaces (one per output): FIFO, opaque, never sRGB.

use crate::context::GpuContext;
use crate::error::{RenderError, Result};

/// Pick `(configured format, view format)` without gamma conversion
/// (spec 04 §24.5). Prefers a UNORM 8-bit format; if the surface only offers
/// sRGB formats, renders through a non-sRGB view of it.
#[must_use]
pub fn choose_surface_format(
    formats: &[wgpu::TextureFormat],
) -> Option<(wgpu::TextureFormat, wgpu::TextureFormat)> {
    use wgpu::TextureFormat as F;
    for wanted in [F::Bgra8Unorm, F::Rgba8Unorm] {
        if formats.contains(&wanted) {
            return Some((wanted, wanted));
        }
    }
    if let Some(f) = formats
        .iter()
        .find(|f| !f.is_srgb() && matches!(f, F::Rgb10a2Unorm))
    {
        return Some((*f, *f));
    }
    for srgb in [F::Bgra8UnormSrgb, F::Rgba8UnormSrgb] {
        if formats.contains(&srgb) {
            return Some((srgb, srgb.remove_srgb_suffix()));
        }
    }
    formats.first().map(|f| (*f, f.remove_srgb_suffix()))
}

/// Frames queued ahead of the display: swap-chain images − 1. One: a wallpaper renders at most
/// at the configured rate (30 fps by default), far below the display's refresh, so the image it
/// presented is released long before the next frame and a third image would only hold memory
/// (one output-sized image: 14 MiB at 2560x1440). Windows only for now: Wayland compositors
/// may hold a buffer until their next frame callback, which was not measured with two images.
/// `LUMENWALL_FRAME_LATENCY` overrides it (measurements).
fn frame_latency() -> u32 {
    std::env::var("LUMENWALL_FRAME_LATENCY")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&n| (1..=3).contains(&n))
        .unwrap_or(if cfg!(windows) { 1 } else { 2 })
}

/// A configured window surface.
pub struct OutputSurface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    view_format: wgpu::TextureFormat,
}

/// An acquired swap-chain image.
pub struct SurfaceFrame {
    texture: wgpu::SurfaceTexture,
    view: wgpu::TextureView,
    suboptimal: bool,
}

impl SurfaceFrame {
    #[must_use]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.texture.texture.width(), self.texture.texture.height())
    }

    /// Present (after the frame's command buffers were submitted).
    pub fn present(self, ctx: &GpuContext) -> bool {
        ctx.queue().present(self.texture);
        self.suboptimal
    }
}

impl OutputSurface {
    /// Configure `surface` (created with `GpuInstance::create_surface`) at `size`.
    pub fn new(
        ctx: &GpuContext,
        surface: wgpu::Surface<'static>,
        size: (u32, u32),
    ) -> Result<Self> {
        if !ctx.adapter().is_surface_supported(&surface) {
            return Err(RenderError::SurfaceUnsupported);
        }
        let caps = surface.get_capabilities(ctx.adapter());
        let (format, view_format) =
            choose_surface_format(&caps.formats).ok_or(RenderError::SurfaceUnsupported)?;
        let mut config = surface
            .get_default_config(ctx.adapter(), size.0.max(1), size.1.max(1))
            .ok_or(RenderError::SurfaceUnsupported)?;
        config.usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        config.format = format;
        config.view_formats = if view_format == format {
            Vec::new()
        } else {
            vec![view_format]
        };
        config.present_mode = wgpu::PresentMode::Fifo;
        // Opaque output (spec 04 §11.6, Q11); the compositor writes alpha = 1 anyway.
        config.alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };
        config.desired_maximum_frame_latency = frame_latency();
        surface.configure(ctx.device(), &config);
        ctx.note_resources(1);
        Ok(Self {
            surface,
            config,
            view_format,
        })
    }

    /// Format of the views handed to the compositor.
    #[must_use]
    pub fn format(&self) -> wgpu::TextureFormat {
        self.view_format
    }

    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Reconfigure for a new size (output mode change). Not per frame.
    pub fn resize(&mut self, ctx: &GpuContext, size: (u32, u32)) {
        let size = (size.0.max(1), size.1.max(1));
        if size == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = size.0;
        self.config.height = size.1;
        self.surface.configure(ctx.device(), &self.config);
    }

    /// Acquire the next image. `Ok(None)`: nothing to draw into this frame
    /// (occluded or timed out) — skip rendering for this output.
    pub fn acquire(&mut self, ctx: &GpuContext) -> Result<Option<SurfaceFrame>> {
        for _ in 0..2 {
            let (texture, suboptimal) = match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t) => (t, false),
                wgpu::CurrentSurfaceTexture::Suboptimal(t) => (t, true),
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    return Ok(None);
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    self.surface.configure(ctx.device(), &self.config);
                    continue;
                }
                wgpu::CurrentSurfaceTexture::Validation => return Err(RenderError::SurfaceLost),
            };
            let view = texture.texture.create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.view_format),
                ..wgpu::TextureViewDescriptor::default()
            });
            return Ok(Some(SurfaceFrame {
                texture,
                view,
                suboptimal,
            }));
        }
        Err(RenderError::SurfaceLost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::TextureFormat as F;

    #[test]
    fn never_srgb() {
        assert_eq!(
            choose_surface_format(&[F::Bgra8UnormSrgb, F::Bgra8Unorm]),
            Some((F::Bgra8Unorm, F::Bgra8Unorm))
        );
        assert_eq!(
            choose_surface_format(&[F::Rgba8UnormSrgb]),
            Some((F::Rgba8UnormSrgb, F::Rgba8Unorm))
        );
        assert_eq!(
            choose_surface_format(&[F::Rgb10a2Unorm, F::Rgba8Unorm]),
            Some((F::Rgba8Unorm, F::Rgba8Unorm))
        );
        assert_eq!(choose_surface_format(&[]), None);
        for (cfg, view) in [
            choose_surface_format(&[F::Bgra8UnormSrgb]).unwrap(),
            choose_surface_format(&[F::Rgba16Float]).unwrap(),
        ] {
            assert!(!view.is_srgb(), "{cfg:?} → {view:?}");
        }
    }
}

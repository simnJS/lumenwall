//! Off-screen output and GPU → CPU readback, for visual parity tests and
//! tools. Readback blocks on the GPU: never use it in the render loop.

use std::path::Path;

use crate::context::GpuContext;
use crate::error::{RenderError, Result};

/// An off-screen colour target standing in for a window surface.
#[derive(Debug)]
pub struct HeadlessOutput {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl HeadlessOutput {
    /// Format of the off-screen output (non-sRGB, like the surfaces).
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    #[must_use]
    pub fn new(ctx: &GpuContext, width: u32, height: u32) -> Self {
        let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("lumenwall.headless"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        ctx.note_resources(2);
        Self {
            texture,
            view,
            width: width.max(1),
            height: height.max(1),
        }
    }

    #[must_use]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    #[must_use]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Read the output back as tightly packed RGBA8 rows (row 0 = top).
    pub fn read_rgba8(&self, ctx: &GpuContext) -> Result<Vec<u8>> {
        read_texture_rgba8(ctx, &self.texture)
    }

    /// Read back and write a PNG.
    pub fn write_png(&self, ctx: &GpuContext, path: &Path) -> Result<()> {
        let data = self.read_rgba8(ctx)?;
        write_png(path, self.width, self.height, &data)
    }
}

/// Copy mip 0 of an RGBA8/BGRA8 texture to the CPU (blocking).
pub fn read_texture_rgba8(ctx: &GpuContext, texture: &wgpu::Texture) -> Result<Vec<u8>> {
    let (w, h) = (texture.width(), texture.height());
    let bgra = match texture.format() {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
        f => return Err(RenderError::Readback(format!("unsupported format {f:?}"))),
    };
    let row = w * 4;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = ctx.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("lumenwall.readback"),
        size: u64::from(padded) * u64::from(h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lumenwall.readback"),
        });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    if texture
        .usage()
        .contains(wgpu::TextureUsages::TEXTURE_BINDING)
    {
        // Back to the sampled state: a scene recorded directly in Vulkan expects its targets
        // there between frames (crate::hal), and the wgpu path does not mind.
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture,
                selector: None,
                state: wgpu::TextureUses::RESOURCE,
            }),
        );
    }
    ctx.queue().submit(std::iter::once(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    ctx.wait_idle();
    rx.recv()
        .map_err(|e| RenderError::Readback(e.to_string()))?
        .map_err(|e| RenderError::Readback(e.to_string()))?;
    let mut out = Vec::with_capacity((row * h) as usize);
    {
        let view = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        for y in 0..h as usize {
            let start = y * padded as usize;
            out.extend_from_slice(&view[start..start + row as usize]);
        }
    }
    buffer.unmap();
    if bgra {
        for px in out.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }
    Ok(out)
}

/// Write tightly packed RGBA8 rows to a PNG file.
pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let file = std::fs::File::create(path).map_err(|source| RenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(())
}

//! Loaded textures: render-ready upload description, GPU upload, sampling
//! flags (spec 04 §16.1, §16.2, §24.7).

use crate::context::{GpuContext, MemoryToken};
use crate::error::{RenderError, Result};

/// Texel format of an upload. BC data is uploaded as-is (no CPU decode).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexelFormat {
    /// RGBA8888 and decoded embedded images.
    Rgba8,
    /// RG88 → shader reads (r, g, 0, 1).
    Rg8,
    /// R8 → shader reads (r, 0, 0, 1).
    R8,
    /// DXT1 (with 1-bit alpha).
    Bc1,
    /// DXT3.
    Bc2,
    /// DXT5.
    Bc3,
}

impl TexelFormat {
    /// wgpu format. Always UNORM, never sRGB (spec 04 §16.2, §24.5).
    #[must_use]
    pub fn wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
            Self::Rg8 => wgpu::TextureFormat::Rg8Unorm,
            Self::R8 => wgpu::TextureFormat::R8Unorm,
            Self::Bc1 => wgpu::TextureFormat::Bc1RgbaUnorm,
            Self::Bc2 => wgpu::TextureFormat::Bc2RgbaUnorm,
            Self::Bc3 => wgpu::TextureFormat::Bc3RgbaUnorm,
        }
    }

    #[must_use]
    pub fn is_compressed(self) -> bool {
        matches!(self, Self::Bc1 | Self::Bc2 | Self::Bc3)
    }

    /// `(block width/height, bytes per block)`; uncompressed formats use 1×1 blocks.
    #[must_use]
    pub fn block(self) -> (u32, u32) {
        match self {
            Self::Rgba8 => (1, 4),
            Self::Rg8 => (1, 2),
            Self::R8 => (1, 1),
            Self::Bc1 => (4, 8),
            Self::Bc2 | Self::Bc3 => (4, 16),
        }
    }

    /// `(bytes per row, rows)` of one mip level.
    #[must_use]
    pub fn level_layout(self, width: u32, height: u32) -> (u32, u32) {
        let (b, bytes) = self.block();
        (width.div_ceil(b) * bytes, height.div_ceil(b))
    }

    /// Byte size of one mip level.
    #[must_use]
    pub fn level_bytes(self, width: u32, height: u32) -> u64 {
        let (row, rows) = self.level_layout(width, height);
        u64::from(row) * u64::from(rows)
    }
}

/// Flags of a `.tex` file (spec 04 §16.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TexFlags(pub u32);

impl TexFlags {
    pub const NO_INTERPOLATION: u32 = 1;
    pub const CLAMP_UVS: u32 = 2;
    pub const IS_ANIMATED: u32 = 4;
    pub const CLAMP_UVS_BORDER: u32 = 8;
    pub const VIDEO: u32 = 32;

    #[must_use]
    pub fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

/// Wrap mode of a sampler.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AddressMode {
    #[default]
    Repeat,
    Clamp,
    /// Clamp to a transparent black border.
    Border,
}

/// Sampler identity; samplers are shared through `GpuContext::sampler`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SamplerKey {
    pub address: AddressMode,
    /// Nearest filtering (flag 1), otherwise linear / trilinear.
    pub nearest: bool,
    /// Sample the mip chain.
    pub mipmaps: bool,
}

impl SamplerKey {
    /// Sampler of a loaded texture: bit 2 → clamp, else repeat; bit 8 is not
    /// honoured for loaded textures (spec 04 §16.1).
    #[must_use]
    pub fn for_texture(flags: TexFlags, mip_levels: u32) -> Self {
        Self {
            address: if flags.has(TexFlags::CLAMP_UVS) {
                AddressMode::Clamp
            } else {
                AddressMode::Repeat
            },
            nearest: flags.has(TexFlags::NO_INTERPOLATION),
            mipmaps: mip_levels > 1,
        }
    }

    /// Sampler of a render target: bit 2 → clamp, else bit 8 → border, else
    /// repeat; no mipmaps (spec 04 §16.1).
    #[must_use]
    pub fn for_render_target(flags: TexFlags) -> Self {
        let address = if flags.has(TexFlags::CLAMP_UVS) {
            AddressMode::Clamp
        } else if flags.has(TexFlags::CLAMP_UVS_BORDER) {
            AddressMode::Border
        } else {
            AddressMode::Repeat
        };
        Self {
            address,
            nearest: flags.has(TexFlags::NO_INTERPOLATION),
            mipmaps: false,
        }
    }

    /// Clamp-to-edge, linear, no mipmaps (scene RTs of spec 04 §15).
    pub const CLAMP_LINEAR: Self = Self {
        address: AddressMode::Clamp,
        nearest: false,
        mipmaps: false,
    };
}

/// One mip level of an upload; `data` is tightly packed (no row padding).
#[derive(Clone, Copy, Debug)]
pub struct MipLevel<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

/// Render-ready texture: what `lumen-formats` produces for one image of a
/// `.tex` file (or a decoded PNG/JPEG). Borrowed: the CPU copy can be dropped
/// right after `upload_texture` returns.
#[derive(Clone, Copy, Debug)]
pub struct TextureUpload<'a> {
    pub label: &'a str,
    pub format: TexelFormat,
    /// Level 0 first. Level `i` must be `max(1, size0 >> i)`.
    pub levels: &'a [MipLevel<'a>],
    /// Declared content size ("real" size); `None` = level 0 size.
    pub content_size: Option<(u32, u32)>,
    pub flags: TexFlags,
}

/// One mip level of a volume upload: `depth` slices of `width × height`, tightly packed
/// (x fastest, then y, then z).
#[derive(Clone, Copy, Debug)]
pub struct VolumeLevel<'a> {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub data: &'a [u8],
}

/// Render-ready volume (3D) texture: a `.tex` with the `VOLUME` flag (spec 01 A.1), e.g. a
/// 32×32×32 colour LUT. Uncompressed formats only (`lumen-formats` decodes BC volumes).
#[derive(Clone, Copy, Debug)]
pub struct VolumeUpload<'a> {
    pub label: &'a str,
    pub format: TexelFormat,
    /// Level 0 first. Level `i` must be `max(1, size0 >> i)` on every axis.
    pub levels: &'a [VolumeLevel<'a>],
    pub flags: TexFlags,
}

/// A texture resident on the GPU.
#[derive(Debug)]
pub struct GpuTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: TexelFormat,
    storage: (u32, u32),
    content: (u32, u32),
    /// Slices of a volume texture; 1 for 2D textures.
    depth: u32,
    volume: bool,
    mip_levels: u32,
    sampler: SamplerKey,
    _memory: MemoryToken,
}

impl GpuTexture {
    #[must_use]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    #[must_use]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    #[must_use]
    pub fn format(&self) -> TexelFormat {
        self.format
    }

    /// Allocated size (level 0).
    #[must_use]
    pub fn storage_size(&self) -> (u32, u32) {
        self.storage
    }

    /// Declared content size.
    #[must_use]
    pub fn content_size(&self) -> (u32, u32) {
        self.content
    }

    #[must_use]
    pub fn mip_levels(&self) -> u32 {
        self.mip_levels
    }

    /// True for a volume (3D) texture, bound only to `texture_3d` slots.
    #[must_use]
    pub fn is_volume(&self) -> bool {
        self.volume
    }

    /// Level-0 depth (1 for 2D textures).
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.depth
    }

    #[must_use]
    pub fn sampler_key(&self) -> SamplerKey {
        self.sampler
    }

    /// `g_TextureNResolution` value: `(storage w, storage h, content w, content h)`.
    #[must_use]
    pub fn resolution(&self) -> [f32; 4] {
        [
            self.storage.0 as f32,
            self.storage.1 as f32,
            self.content.0 as f32,
            self.content.1 as f32,
        ]
    }

    /// Replace level 0 in place (video frames, thumbnails of the same size).
    /// No allocation on the GPU side. Ignored for volume textures.
    pub fn write_level0(&self, ctx: &GpuContext, data: &[u8]) {
        if self.volume {
            tracing::warn!(target: "lumen_render", "write_level0 on a volume texture ignored");
            return;
        }
        let (row, rows) = self.format.level_layout(self.storage.0, self.storage.1);
        if data.len() < (row as usize) * (rows as usize) {
            tracing::warn!(target: "lumen_render", "write_level0: short buffer ignored");
            return;
        }
        write_level(
            ctx,
            &self.texture,
            self.format,
            0,
            self.storage.0,
            self.storage.1,
            data,
        );
    }
}

fn write_level(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    format: TexelFormat,
    mip: u32,
    w: u32,
    h: u32,
    data: &[u8],
) {
    write_slices(ctx, texture, format, mip, (w, h, 1), data);
}

/// Write `depth` slices of one level (1 for 2D textures).
fn write_slices(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    format: TexelFormat,
    mip: u32,
    (w, h, depth): (u32, u32, u32),
    data: &[u8],
) {
    let (row, rows) = format.level_layout(w, h);
    // Physical size of a compressed level is rounded up to whole blocks.
    let (b, _) = format.block();
    ctx.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: mip,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row),
            rows_per_image: Some(rows),
        },
        wgpu::Extent3d {
            width: w.div_ceil(b) * b,
            height: h.div_ceil(b) * b,
            depth_or_array_layers: depth,
        },
    );
}

/// Check an upload description without touching the GPU.
pub fn validate_upload(up: &TextureUpload<'_>, bc_supported: bool) -> Result<()> {
    let bad = |reason: String| RenderError::InvalidTexture {
        label: up.label.to_owned(),
        reason,
    };
    let Some(l0) = up.levels.first() else {
        return Err(bad("no mip level".into()));
    };
    if l0.width == 0 || l0.height == 0 {
        return Err(bad("zero-sized texture".into()));
    }
    if up.format.is_compressed() {
        if !bc_supported {
            return Err(RenderError::BcUnsupported);
        }
        if l0.width % 4 != 0 || l0.height % 4 != 0 {
            return Err(bad(format!(
                "BC texture size {}x{} is not a multiple of 4; decode to RGBA instead",
                l0.width, l0.height
            )));
        }
    }
    let max_levels = 32 - l0.width.max(l0.height).leading_zeros();
    if up.levels.len() as u32 > max_levels {
        return Err(bad(format!(
            "{} mip levels for {}x{}",
            up.levels.len(),
            l0.width,
            l0.height
        )));
    }
    for (i, l) in up.levels.iter().enumerate() {
        let (ew, eh) = ((l0.width >> i).max(1), (l0.height >> i).max(1));
        if (l.width, l.height) != (ew, eh) {
            return Err(bad(format!(
                "level {i} is {}x{}, expected {ew}x{eh}",
                l.width, l.height
            )));
        }
        let need = up.format.level_bytes(l.width, l.height);
        if (l.data.len() as u64) < need {
            return Err(bad(format!(
                "level {i}: {} bytes, expected {need}",
                l.data.len()
            )));
        }
    }
    Ok(())
}

/// Upload a texture with all its mip levels.
pub fn upload_texture(ctx: &GpuContext, up: &TextureUpload<'_>) -> Result<GpuTexture> {
    validate_upload(up, ctx.supports_bc())?;
    let l0 = up.levels[0];
    let mip_levels = up.levels.len() as u32;
    let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
        label: Some(up.label),
        size: wgpu::Extent3d {
            width: l0.width,
            height: l0.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: mip_levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: up.format.wgpu(),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut bytes = 0;
    for (i, l) in up.levels.iter().enumerate() {
        write_level(
            ctx, &texture, up.format, i as u32, l.width, l.height, l.data,
        );
        bytes += up.format.level_bytes(l.width, l.height);
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    ctx.note_resources(2);
    Ok(GpuTexture {
        texture,
        view,
        format: up.format,
        storage: (l0.width, l0.height),
        content: up.content_size.unwrap_or((l0.width, l0.height)),
        depth: 1,
        volume: false,
        mip_levels,
        sampler: SamplerKey::for_texture(up.flags, mip_levels),
        _memory: ctx.memory().track(bytes),
    })
}

/// Check a volume upload description without touching the GPU.
pub fn validate_volume_upload(up: &VolumeUpload<'_>) -> Result<()> {
    let bad = |reason: String| RenderError::InvalidTexture {
        label: up.label.to_owned(),
        reason,
    };
    let Some(l0) = up.levels.first() else {
        return Err(bad("no mip level".into()));
    };
    if l0.width == 0 || l0.height == 0 || l0.depth == 0 {
        return Err(bad("zero-sized volume".into()));
    }
    if up.format.is_compressed() {
        return Err(bad(
            "compressed volume textures must be decoded to RGBA".into()
        ));
    }
    let max_levels = 32 - l0.width.max(l0.height).max(l0.depth).leading_zeros();
    if up.levels.len() as u32 > max_levels {
        return Err(bad(format!("{} mip levels", up.levels.len())));
    }
    for (i, l) in up.levels.iter().enumerate() {
        let e = (
            (l0.width >> i).max(1),
            (l0.height >> i).max(1),
            (l0.depth >> i).max(1),
        );
        if (l.width, l.height, l.depth) != e {
            return Err(bad(format!(
                "level {i} is {}x{}x{}, expected {}x{}x{}",
                l.width, l.height, l.depth, e.0, e.1, e.2
            )));
        }
        let need = up.format.level_bytes(l.width, l.height) * u64::from(l.depth);
        if (l.data.len() as u64) < need {
            return Err(bad(format!(
                "level {i}: {} bytes, expected {need}",
                l.data.len()
            )));
        }
    }
    Ok(())
}

/// Upload a volume (3D) texture with all its mip levels.
pub fn upload_volume(ctx: &GpuContext, up: &VolumeUpload<'_>) -> Result<GpuTexture> {
    validate_volume_upload(up)?;
    let l0 = up.levels[0];
    let mip_levels = up.levels.len() as u32;
    let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
        label: Some(up.label),
        size: wgpu::Extent3d {
            width: l0.width,
            height: l0.height,
            depth_or_array_layers: l0.depth,
        },
        mip_level_count: mip_levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: up.format.wgpu(),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut bytes = 0;
    for (i, l) in up.levels.iter().enumerate() {
        write_slices(
            ctx,
            &texture,
            up.format,
            i as u32,
            (l.width, l.height, l.depth),
            l.data,
        );
        bytes += up.format.level_bytes(l.width, l.height) * u64::from(l.depth);
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    ctx.note_resources(2);
    Ok(GpuTexture {
        texture,
        view,
        format: up.format,
        storage: (l0.width, l0.height),
        content: (l0.width, l0.height),
        depth: l0.depth,
        volume: true,
        mip_levels,
        sampler: SamplerKey::for_texture(up.flags, mip_levels),
        _memory: ctx.memory().track(bytes),
    })
}

/// 1×1 texture `(0, 0, 0, 1)` bound to unused slots (spec 04 §24.7).
pub fn default_texture(ctx: &GpuContext) -> GpuTexture {
    let data = [0u8, 0, 0, 255];
    let level = [MipLevel {
        width: 1,
        height: 1,
        data: &data,
    }];
    upload_texture(
        ctx,
        &TextureUpload {
            label: "lumen.default-texture",
            format: TexelFormat::Rgba8,
            levels: &level,
            content_size: None,
            flags: TexFlags(TexFlags::CLAMP_UVS),
        },
    )
    .expect("1x1 RGBA8 upload is always valid")
}

/// 1×1×1 volume `(0, 0, 0, 1)` bound to `texture_3d` slots without a volume texture.
pub fn default_volume(ctx: &GpuContext) -> GpuTexture {
    let data = [0u8, 0, 0, 255];
    let level = [VolumeLevel {
        width: 1,
        height: 1,
        depth: 1,
        data: &data,
    }];
    upload_volume(
        ctx,
        &VolumeUpload {
            label: "lumen.default-volume",
            format: TexelFormat::Rgba8,
            levels: &level,
            flags: TexFlags(TexFlags::CLAMP_UVS),
        },
    )
    .expect("1x1x1 RGBA8 volume upload is always valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        assert_eq!(TexelFormat::Rgba8.level_layout(5, 3), (20, 3));
        assert_eq!(TexelFormat::R8.level_layout(5, 3), (5, 3));
        assert_eq!(TexelFormat::Bc1.level_layout(8, 8), (16, 2));
        assert_eq!(TexelFormat::Bc3.level_layout(2, 2), (16, 1));
        assert_eq!(TexelFormat::Bc2.level_bytes(256, 128), 64 * 32 * 16);
        assert!(TexelFormat::Bc1.is_compressed() && !TexelFormat::Rg8.is_compressed());
        assert!(!TexelFormat::Rgba8.wgpu().is_srgb());
    }

    #[test]
    fn sampler_flags() {
        let k = SamplerKey::for_texture(TexFlags(TexFlags::CLAMP_UVS_BORDER), 3);
        assert_eq!(
            k.address,
            AddressMode::Repeat,
            "bit 8 ignored for loaded textures"
        );
        assert!(k.mipmaps && !k.nearest);
        let k = SamplerKey::for_texture(
            TexFlags(TexFlags::CLAMP_UVS | TexFlags::NO_INTERPOLATION),
            1,
        );
        assert_eq!(k.address, AddressMode::Clamp);
        assert!(k.nearest && !k.mipmaps);
        assert_eq!(
            SamplerKey::for_render_target(TexFlags(TexFlags::CLAMP_UVS_BORDER)).address,
            AddressMode::Border
        );
        assert_eq!(
            SamplerKey::for_render_target(TexFlags(
                TexFlags::CLAMP_UVS | TexFlags::CLAMP_UVS_BORDER
            ))
            .address,
            AddressMode::Clamp
        );
        assert_eq!(
            SamplerKey::for_render_target(TexFlags(0)).address,
            AddressMode::Repeat
        );
    }

    #[test]
    fn upload_validation() {
        let d = vec![0u8; 64 * 64 * 4];
        let levels = [
            MipLevel {
                width: 4,
                height: 4,
                data: &d,
            },
            MipLevel {
                width: 2,
                height: 2,
                data: &d,
            },
            MipLevel {
                width: 1,
                height: 1,
                data: &d,
            },
        ];
        let mut up = TextureUpload {
            label: "t",
            format: TexelFormat::Rgba8,
            levels: &levels,
            content_size: None,
            flags: TexFlags::default(),
        };
        assert!(validate_upload(&up, false).is_ok());
        up.format = TexelFormat::Bc1;
        assert!(matches!(
            validate_upload(&up, false),
            Err(RenderError::BcUnsupported)
        ));
        assert!(validate_upload(&up, true).is_ok());
        let odd = [MipLevel {
            width: 6,
            height: 4,
            data: &d,
        }];
        up.levels = &odd;
        assert!(validate_upload(&up, true).is_err());
        let wrong = [levels[0], levels[2]];
        up.format = TexelFormat::Rgba8;
        up.levels = &wrong;
        assert!(validate_upload(&up, true).is_err());
        let short = [MipLevel {
            width: 64,
            height: 64,
            data: &d[..100],
        }];
        up.levels = &short;
        assert!(validate_upload(&up, true).is_err());
    }

    #[test]
    fn volume_upload_validation() {
        let d = vec![0u8; 32 * 32 * 32 * 4];
        let lvl = |w: u32, h: u32, depth: u32| VolumeLevel {
            width: w,
            height: h,
            depth,
            data: &d,
        };
        let levels = [lvl(32, 32, 32), lvl(16, 16, 16)];
        let mut up = VolumeUpload {
            label: "lut",
            format: TexelFormat::Rgba8,
            levels: &levels,
            flags: TexFlags::default(),
        };
        assert!(validate_volume_upload(&up).is_ok());
        let wrong = [lvl(32, 32, 32), lvl(16, 16, 32)];
        up.levels = &wrong;
        assert!(validate_volume_upload(&up).is_err(), "depth halves too");
        let short = [VolumeLevel {
            width: 32,
            height: 32,
            depth: 32,
            data: &d[..32 * 32 * 4],
        }];
        up.levels = &short;
        assert!(validate_volume_upload(&up).is_err());
        up.levels = &levels[..1];
        up.format = TexelFormat::Bc1;
        assert!(
            validate_volume_upload(&up).is_err(),
            "BC volumes are decoded first"
        );
    }
}

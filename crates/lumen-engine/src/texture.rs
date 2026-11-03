//! `.tex` files → GPU textures (spec 01 §2.10, spec 04 §16).
//!
//! BC1/BC2/BC3 data is uploaded as is when the device supports it and decoded on the CPU
//! otherwise. Every CPU copy (LZ4 output, decoded PNG, BC fallback) is dropped as soon as the
//! upload returns; archive entries are borrowed from the `.pkg` mapping and never copied.

use std::collections::HashMap;
use std::rc::Rc;

use lumen_assets::Vfs;
use lumen_formats::tex::{
    Animation, GpuContent, GpuFormat, Spritesheet, TextureDimension, UploadOptions,
};
use lumen_render::texture::{
    MipLevel, TexFlags, TexelFormat, TextureUpload, VolumeLevel, VolumeUpload,
};
use lumen_render::{SceneBuilder, TextureId};
use lumen_scene::MainTextureFormat;

/// Frames of an animated texture (flag 4), for the slot-0 variants of the passes using it.
#[derive(Clone, Debug)]
pub struct TextureAnimation {
    pub animation: Animation,
    /// Total duration `T`, computed once.
    pub total: f64,
    /// Texture size of each image (§2.11), for the frame uniforms.
    pub sizes: Vec<[u32; 2]>,
}

/// A texture resident on the GPU, with what the scene builder needs to know about it.
#[derive(Clone, Debug)]
pub struct LoadedTexture {
    /// One GPU texture per image of the file (several for an animated texture).
    pub ids: Vec<TextureId>,
    /// Level-0 size of image 0 (storage).
    pub storage: (u32, u32),
    /// Declared ("real") size; one frame for an animated texture.
    pub real: (u32, u32),
    /// Flags of the file (sampling, animation).
    pub flags: TexFlags,
    /// GPU format actually uploaded.
    pub format: TexelFormat,
    /// `TEX0FORMAT` forcing of the declared format (spec 04 §13.2).
    pub main_format: MainTextureFormat,
    pub animation: Option<TextureAnimation>,
    /// Sprite sheet values (particles).
    pub spritesheet: Spritesheet,
    /// The raw `.tex-json` companion, if any (particles read `spritesheetsequences`).
    pub tex_json: Option<serde_json::Value>,
}

impl LoadedTexture {
    /// First image (non-animated binding).
    pub fn id(&self) -> TextureId {
        self.ids[0]
    }

    pub fn is_animated(&self) -> bool {
        self.animation.is_some() && self.ids.len() > 1
    }
}

/// Per-scene texture cache, keyed by name (spec 04 Q16: per wallpaper, never shared).
#[derive(Default)]
pub struct TextureCache {
    map: HashMap<String, Option<Rc<LoadedTexture>>>,
    /// CPU bytes uploaded (statistics).
    pub uploaded_bytes: u64,
}

impl TextureCache {
    /// Loads `materials/<name>.tex` once. Failures are logged once and cached as `None`.
    pub fn get(
        &mut self,
        b: &mut SceneBuilder<'_>,
        vfs: &Vfs,
        name: &str,
    ) -> Option<Rc<LoadedTexture>> {
        if let Some(t) = self.map.get(name) {
            return t.clone();
        }
        let loaded = match load_texture(b, vfs, name, &mut self.uploaded_bytes) {
            Ok(t) => Some(Rc::new(t)),
            Err(e) => {
                tracing::warn!("texture '{name}': {e}");
                None
            }
        };
        self.map.insert(name.to_owned(), loaded.clone());
        loaded
    }

    /// Whether a texture exists in the VFS (no upload).
    pub fn exists(&self, vfs: &Vfs, name: &str) -> bool {
        self.map.get(name).is_some_and(Option::is_some)
            || vfs.exists(&lumen_assets::path::texture_path(name))
    }

    /// Number of textures uploaded.
    pub fn len(&self) -> usize {
        self.map.values().filter(|v| v.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn texel_format(f: GpuFormat) -> Result<TexelFormat, String> {
    Ok(match f {
        GpuFormat::R8Unorm => TexelFormat::R8,
        GpuFormat::Rg8Unorm => TexelFormat::Rg8,
        GpuFormat::Rgba8Unorm => TexelFormat::Rgba8,
        GpuFormat::Bc1RgbaUnorm => TexelFormat::Bc1,
        GpuFormat::Bc2RgbaUnorm => TexelFormat::Bc2,
        GpuFormat::Bc3RgbaUnorm => TexelFormat::Bc3,
        other => {
            return Err(format!(
                "GPU format {other:?} is not supported by the renderer"
            ));
        }
    })
}

fn load_texture(
    b: &mut SceneBuilder<'_>,
    vfs: &Vfs,
    name: &str,
    uploaded: &mut u64,
) -> Result<LoadedTexture, String> {
    let files = vfs.read_texture(name).map_err(|e| e.to_string())?;
    let tex_json = files
        .tex_json
        .as_ref()
        .and_then(|j| serde_json::from_slice::<serde_json::Value>(j).ok());
    let tex = files.parse().map_err(|e| e.to_string())?;
    let options = UploadOptions::default();
    let mut prepared = tex.into_upload(&options).map_err(|e| e.to_string())?;
    if prepared.requires_cpu_decode(b.context().supports_bc()) {
        prepared
            .decode_compressed(options.max_level_bytes)
            .map_err(|e| e.to_string())?;
    }
    let main_format = match prepared.tex0_format_combo {
        Some(8) => MainTextureFormat::Rg88,
        Some(9) => MainTextureFormat::R8,
        _ => MainTextureFormat::Other,
    };
    let flags = TexFlags(prepared.flags.bits());
    let real = (prepared.resolution[2].max(1), prepared.resolution[3].max(1));
    let GpuContent::Images {
        format,
        dimension,
        images,
    } = &prepared.content
    else {
        return Err("video textures are not supported yet (lumen-video is not integrated)".into());
    };
    let format = texel_format(*format)?;
    let mut ids = Vec::with_capacity(images.len());
    let mut levels = Vec::new();
    let mut volume_levels = Vec::new();
    for (i, image) in images.iter().enumerate() {
        if *dimension == TextureDimension::D3 {
            // Volume texture (flag 0x40, e.g. a 32³ colour LUT): bound to `sampler3D` slots.
            volume_levels.clear();
            volume_levels.extend(image.mips.iter().map(|m| VolumeLevel {
                width: m.width,
                height: m.height,
                depth: m.depth,
                data: &m.data,
            }));
            *uploaded += image.mips.iter().map(|m| m.data.len() as u64).sum::<u64>();
            let id = b
                .add_volume_texture(&VolumeUpload {
                    label: name,
                    format,
                    levels: &volume_levels,
                    flags,
                })
                .map_err(|e| e.to_string())?;
            ids.push(id);
            continue;
        }
        levels.clear();
        levels.extend(image.mips.iter().map(|m| MipLevel {
            width: m.width,
            height: m.height,
            data: &m.data,
        }));
        *uploaded += image.mips.iter().map(|m| m.data.len() as u64).sum::<u64>();
        let label = if images.len() > 1 {
            format!("{name}#{i}")
        } else {
            name.to_owned()
        };
        let id = b
            .add_texture(&TextureUpload {
                label: &label,
                format,
                levels: &levels,
                content_size: Some(real),
                flags,
            })
            .map_err(|e| e.to_string())?;
        ids.push(id);
    }
    if ids.is_empty() {
        return Err("texture without image".into());
    }
    let storage = images
        .first()
        .map_or((1, 1), |img| (img.width(), img.height()));
    let animation = prepared
        .animation
        .clone()
        .filter(|_| flags.has(TexFlags::IS_ANIMATED))
        .map(|animation| TextureAnimation {
            total: prepared.animation_duration,
            sizes: prepared.texture_sizes.clone(),
            animation,
        });
    Ok(LoadedTexture {
        ids,
        storage,
        real,
        flags,
        format,
        main_format,
        animation,
        spritesheet: prepared.spritesheet,
        tex_json,
    })
    // `prepared` (decoded pixels, LZ4 output) is dropped here: no CPU copy is kept.
}

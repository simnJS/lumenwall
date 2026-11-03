//! Named render targets (`_rt_*`), scoped namespaces, scaled sizes, ping-pong
//! pairs and shared scratch copies (spec 04 §4.3, §8.9, §9.5, §12.1, §15, §24.6).
//!
//! Everything is allocated at load time. Render targets are RGBA8 UNORM,
//! never sRGB, without mipmaps; wgpu zero-initialises them, which matches the
//! clear to (0, 0, 0, 0) at creation.

use std::collections::HashMap;

use crate::context::{GpuContext, MemoryToken};
use crate::texture::SamplerKey;

/// Format of every render target (spec 04 §15).
pub const RT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Handle of a render target in a [`RenderTargetPool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RtId(pub(crate) u32);

/// Handle of a name scope (pass/effect, image, scene).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScopeId(u32);

/// A GPU render target.
#[derive(Debug)]
pub struct RenderTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    sampler: SamplerKey,
    label: String,
    _memory: MemoryToken,
}

impl RenderTarget {
    #[must_use]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    #[must_use]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[must_use]
    pub fn sampler_key(&self) -> SamplerKey {
        self.sampler
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// `g_TextureNResolution` of a render target: `(w, h, w, h)`.
    #[must_use]
    pub fn resolution(&self) -> [f32; 4] {
        let (w, h) = (self.width as f32, self.height as f32);
        [w, h, w, h]
    }
}

/// `tronc(S / scale)` for effect `fbos` (spec 04 §9.5). Clamped to 1 pixel:
/// a zero-sized texture is invalid on every API (crash fix, not a deviation).
#[must_use]
pub fn scaled_size(base: (f32, f32), scale: f32) -> (u32, u32) {
    let s = if scale > 0.0 { scale } else { 1.0 };
    let t = |v: f32| crate::math::trunc_u32(v / s).max(1);
    (t(base.0), t(base.1))
}

/// Integer division of a size (`_rt_4FrameBuffer` = `W div 4`…), clamped to 1.
#[must_use]
pub fn divided_size(base: (u32, u32), divisor: u32) -> (u32, u32) {
    let d = divisor.max(1);
    ((base.0 / d).max(1), (base.1 / d).max(1))
}

/// Names of the composite pair of image `id` (spec 04 §8.9).
#[must_use]
pub fn composite_names(id: i64) -> (String, String) {
    (
        format!("_rt_imageLayerComposite_{id}_a"),
        format!("_rt_imageLayerComposite_{id}_b"),
    )
}

/// Two targets alternated by an image chain (A and B of spec 04 §10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PingPong {
    pub a: RtId,
    pub b: RtId,
}

impl PingPong {
    #[must_use]
    pub fn other(&self, id: RtId) -> RtId {
        if id == self.a { self.b } else { self.a }
    }
}

#[derive(Debug, Default)]
struct Scope {
    parent: Option<ScopeId>,
    names: HashMap<String, RtId>,
}

/// Pool of render targets with scoped names.
#[derive(Debug)]
pub struct RenderTargetPool {
    targets: Vec<RenderTarget>,
    scopes: Vec<Scope>,
    scratch: HashMap<(u32, u32), RtId>,
}

impl Default for RenderTargetPool {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderTargetPool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            targets: Vec::new(),
            scopes: vec![Scope::default()],
            scratch: HashMap::new(),
        }
    }

    /// The scene scope (outermost).
    #[must_use]
    pub fn scene_scope(&self) -> ScopeId {
        ScopeId(0)
    }

    /// A nested scope (image inside the scene, effect instance inside the image).
    pub fn new_scope(&mut self, parent: ScopeId) -> ScopeId {
        self.scopes.push(Scope {
            parent: Some(parent),
            names: HashMap::new(),
        });
        ScopeId(self.scopes.len() as u32 - 1)
    }

    fn allocate(
        ctx: &GpuContext,
        label: &str,
        width: u32,
        height: u32,
        sampler: SamplerKey,
    ) -> RenderTarget {
        let (width, height) = (width.max(1), height.max(1));
        let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: RT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        ctx.note_resources(2);
        RenderTarget {
            texture,
            view,
            width,
            height,
            sampler,
            label: label.to_owned(),
            _memory: ctx.memory().track(u64::from(width) * u64::from(height) * 4),
        }
    }

    /// Create a target and register `name` in `scope` (a later registration of
    /// the same name in the same scope shadows the earlier one).
    pub fn create(
        &mut self,
        ctx: &GpuContext,
        scope: ScopeId,
        name: &str,
        size: (u32, u32),
        sampler: SamplerKey,
    ) -> RtId {
        let id = self.create_anonymous(ctx, name, size, sampler);
        self.alias(scope, name, id);
        id
    }

    /// Create a target without a name.
    pub fn create_anonymous(
        &mut self,
        ctx: &GpuContext,
        label: &str,
        size: (u32, u32),
        sampler: SamplerKey,
    ) -> RtId {
        self.targets
            .push(Self::allocate(ctx, label, size.0, size.1, sampler));
        RtId(self.targets.len() as u32 - 1)
    }

    /// Register another name for an existing target (`_rt_MipMappedFrameBuffer`,
    /// `_alias_lightCookie`).
    pub fn alias(&mut self, scope: ScopeId, name: &str, id: RtId) {
        if let Some(s) = self.scopes.get_mut(scope.0 as usize) {
            s.names.insert(name.to_owned(), id);
        }
    }

    /// Resolve a name from the innermost scope outward (spec 04 §12.1).
    #[must_use]
    pub fn resolve(&self, scope: ScopeId, name: &str) -> Option<RtId> {
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let sc = self.scopes.get(s.0 as usize)?;
            if let Some(id) = sc.names.get(name) {
                return Some(*id);
            }
            cur = sc.parent;
        }
        None
    }

    /// Shared scratch target of a given size, used to copy a destination
    /// before a pass that samples it (spec 04 §24.6). Passes run in order on
    /// the GPU, so one scratch per size serves every hazard of the frame.
    pub fn scratch(&mut self, ctx: &GpuContext, size: (u32, u32)) -> RtId {
        if let Some(id) = self.scratch.get(&size) {
            return *id;
        }
        let id = self.create_anonymous(ctx, "lumen.scratch", size, SamplerKey::CLAMP_LINEAR);
        self.scratch.insert(size, id);
        id
    }

    /// Change the sampling of a target (e.g. the output clamp mode applied to
    /// `_rt_FullFrameBuffer`, spec 04 §21.3). Bind groups must be rebuilt.
    pub fn set_sampler(&mut self, id: RtId, sampler: SamplerKey) {
        if let Some(t) = self.targets.get_mut(id.0 as usize)
            && t.sampler != sampler
        {
            t.sampler = sampler;
        }
    }

    /// Reallocate a target at a new size (video / web wallpapers, output
    /// resize). Event-driven, never per frame. Bind groups must be rebuilt.
    pub fn resize(&mut self, ctx: &GpuContext, id: RtId, size: (u32, u32)) {
        let Some(t) = self.targets.get(id.0 as usize) else {
            return;
        };
        if t.size() == (size.0.max(1), size.1.max(1)) {
            return;
        }
        let (label, sampler) = (t.label.clone(), t.sampler);
        self.targets[id.0 as usize] = Self::allocate(ctx, &label, size.0, size.1, sampler);
    }

    #[must_use]
    pub fn get(&self, id: RtId) -> &RenderTarget {
        &self.targets[id.0 as usize]
    }

    #[must_use]
    pub fn try_get(&self, id: RtId) -> Option<&RenderTarget> {
        self.targets.get(id.0 as usize)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

/// Standard scene targets of spec 04 §4.3.
#[derive(Clone, Copy, Debug)]
pub struct SceneTargets {
    /// `_rt_FullFrameBuffer` (+ alias `_rt_MipMappedFrameBuffer`).
    pub full: RtId,
    /// `_rt_shadowAtlas` (+ alias `_alias_lightCookie`), never filled.
    pub shadow_atlas: RtId,
}

/// Late scene targets of spec 04 §4.3 (created after the objects).
#[derive(Clone, Copy, Debug)]
pub struct BloomTargets {
    /// `_rt_4FrameBuffer`.
    pub quarter: RtId,
    /// `_rt_8FrameBuffer`.
    pub eighth: RtId,
    /// `_rt_Bloom`.
    pub bloom: RtId,
}

impl RenderTargetPool {
    /// Create `_rt_FullFrameBuffer` (`tronc(W) x tronc(H)`) and
    /// `_rt_shadowAtlas` with their aliases (call before creating objects).
    ///
    /// Deviation: the shadow atlas is never filled (it always samples as
    /// transparent black), so it is allocated 1×1 instead of W×H; only its
    /// `g_TextureNResolution` differs. Saves 4·W·H bytes of VRAM.
    pub fn create_scene_targets(
        &mut self,
        ctx: &GpuContext,
        size: (u32, u32),
        full_sampler: SamplerKey,
    ) -> SceneTargets {
        let root = self.scene_scope();
        let full = self.create(ctx, root, "_rt_FullFrameBuffer", size, full_sampler);
        self.alias(root, "_rt_MipMappedFrameBuffer", full);
        let shadow_atlas = self.create(
            ctx,
            root,
            "_rt_shadowAtlas",
            (1, 1),
            SamplerKey::CLAMP_LINEAR,
        );
        self.alias(root, "_alias_lightCookie", shadow_atlas);
        SceneTargets { full, shadow_atlas }
    }

    /// Create `_rt_4FrameBuffer`, `_rt_8FrameBuffer`, `_rt_Bloom` (call after
    /// creating objects, so that objects cannot resolve them).
    pub fn create_bloom_targets(&mut self, ctx: &GpuContext, size: (u32, u32)) -> BloomTargets {
        let root = self.scene_scope();
        let q = divided_size(size, 4);
        let e = divided_size(size, 8);
        BloomTargets {
            quarter: self.create(ctx, root, "_rt_4FrameBuffer", q, SamplerKey::CLAMP_LINEAR),
            eighth: self.create(ctx, root, "_rt_8FrameBuffer", e, SamplerKey::CLAMP_LINEAR),
            bloom: self.create(ctx, root, "_rt_Bloom", e, SamplerKey::CLAMP_LINEAR),
        }
    }

    /// Composites of the synthetic bloom image (id −1, spec 04 §17). Its chain
    /// never writes B, and no object can resolve the name (bloom is created
    /// after every object), so B is allocated 1×1.
    pub fn create_bloom_composites(&mut self, ctx: &GpuContext, size: (u32, u32)) -> PingPong {
        let (na, nb) = composite_names(crate::bloom::BLOOM_IMAGE_ID);
        let root = self.scene_scope();
        PingPong {
            a: self.create(ctx, root, &na, size, SamplerKey::CLAMP_LINEAR),
            b: self.create(ctx, root, &nb, (1, 1), SamplerKey::CLAMP_LINEAR),
        }
    }

    /// Create the composite pair of image `id` in the scene scope (spec 04 §8.9).
    pub fn create_composites(
        &mut self,
        ctx: &GpuContext,
        id: i64,
        size: (u32, u32),
        sampler: SamplerKey,
    ) -> PingPong {
        let (na, nb) = composite_names(id);
        let root = self.scene_scope();
        PingPong {
            a: self.create(ctx, root, &na, size, sampler),
            b: self.create(ctx, root, &nb, size, sampler),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(scaled_size((3440.0, 1440.0), 2.0), (1720, 720));
        assert_eq!(scaled_size((3440.0, 1440.0), 4.0), (860, 360));
        assert_eq!(scaled_size((101.7, 33.0), 2.0), (50, 16));
        assert_eq!(scaled_size((1.0, 1.0), 8.0), (1, 1));
        assert_eq!(scaled_size((100.0, 50.0), 0.0), (100, 50));
        assert_eq!(divided_size((1921, 1081), 4), (480, 270));
        assert_eq!(divided_size((1921, 1081), 8), (240, 135));
        assert_eq!(divided_size((3, 3), 8), (1, 1));
        assert_eq!(
            composite_names(-1),
            (
                "_rt_imageLayerComposite_-1_a".into(),
                "_rt_imageLayerComposite_-1_b".into()
            )
        );
        let pp = PingPong {
            a: RtId(3),
            b: RtId(4),
        };
        assert_eq!(pp.other(RtId(3)), RtId(4));
        assert_eq!(pp.other(RtId(4)), RtId(3));
    }

    #[test]
    fn scoped_resolution_without_gpu() {
        // Names only: exercise the scope logic with fake ids.
        let mut pool = RenderTargetPool::new();
        let root = pool.scene_scope();
        let image = pool.new_scope(root);
        let fx1 = pool.new_scope(image);
        let fx2 = pool.new_scope(image);
        pool.alias(root, "_rt_FullFrameBuffer", RtId(0));
        pool.alias(fx1, "_rt_HalfCompoBuffer1", RtId(5));
        pool.alias(fx2, "_rt_HalfCompoBuffer1", RtId(6));
        assert_eq!(pool.resolve(fx1, "_rt_FullFrameBuffer"), Some(RtId(0)));
        assert_eq!(pool.resolve(fx1, "_rt_HalfCompoBuffer1"), Some(RtId(5)));
        assert_eq!(
            pool.resolve(fx2, "_rt_HalfCompoBuffer1"),
            Some(RtId(6)),
            "same name, no conflict"
        );
        assert_eq!(
            pool.resolve(image, "_rt_HalfCompoBuffer1"),
            None,
            "effect RTs are private"
        );
        assert_eq!(pool.resolve(root, "_rt_HalfCompoBuffer2"), None);
    }
}

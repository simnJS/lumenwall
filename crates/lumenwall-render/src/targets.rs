//! Named render targets (`_rt_*`), scoped namespaces, scaled sizes, ping-pong
//! pairs and shared scratch copies (spec 04 §4.3, §8.9, §9.5, §12.1, §15, §24.6).
//!
//! Everything is allocated at load time. Render targets are RGBA8 UNORM,
//! never sRGB, without mipmaps; wgpu zero-initialises them, which matches the
//! clear to (0, 0, 0, 0) at creation.
//!
//! A scene builder's pool is *deferred*: targets get their GPU memory when the scene is built
//! ([`RenderTargetPool::materialize`]), so that transient targets whose frame lifetimes never
//! overlap can share one texture (render-graph aliasing, decided by `graph`).

use std::collections::{HashMap, HashSet};

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
    /// `None` until a deferred pool is materialized.
    gpu: Option<(wgpu::Texture, wgpu::TextureView)>,
    width: u32,
    height: u32,
    sampler: SamplerKey,
    label: String,
    /// `None` while not allocated, or when the texture belongs to another target.
    _memory: Option<MemoryToken>,
    /// The target whose texture this one shares.
    shares: Option<RtId>,
}

impl RenderTarget {
    fn gpu(&self) -> &(wgpu::Texture, wgpu::TextureView) {
        self.gpu
            .as_ref()
            .expect("render target used before its scene was built")
    }

    #[must_use]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.gpu().0
    }

    #[must_use]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.gpu().1
    }

    /// The target whose GPU memory this one shares (render-graph aliasing), if any.
    #[must_use]
    pub fn shares(&self) -> Option<RtId> {
        self.shares
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

fn owner_size(targets: &[RenderTarget], id: RtId) -> (u32, u32) {
    targets
        .get(id.0 as usize)
        .map_or((0, 0), |t| (t.width, t.height))
}

/// How one frame operation touches a target (render-graph aliasing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Sampled, copied from, or kept (a pass that blends into it or writes part of its
    /// channels also reads what was there).
    Read,
    /// Cleared.
    Clear,
    /// Drawn into without reading it: blending off, every channel written.
    Overwrite,
}

/// Result of [`plan_aliasing`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AliasPlan {
    /// Target → the target whose texture it uses.
    pub shares: HashMap<RtId, RtId>,
    /// Targets whose texture is shared and whose first access in the frame is an
    /// [`Access::Overwrite`]: the draw must clear first, since the texture holds another
    /// target's pixels (those the draw does not cover would otherwise leak).
    pub clear_first: HashSet<RtId>,
}

/// Render-graph aliasing (pure): `accesses` lists, in frame order, `(operation index, target,
/// access)` (reads of an operation before its write); `size_of` gives the size of a transient
/// target and `None` for the others. A transient target whose first access in the frame is a
/// clear or an overwrite never shows anything from a previous frame: its content lives from
/// that access to its last one. Targets of the same size whose lifetimes do not overlap share
/// one texture (greedy interval colouring in order of first access).
pub fn plan_aliasing(
    accesses: &[(usize, RtId, Access)],
    size_of: impl Fn(RtId) -> Option<(u32, u32)>,
) -> AliasPlan {
    // First access, its kind, last access.
    let mut life: HashMap<RtId, (usize, Access, usize)> = HashMap::new();
    let mut order = Vec::new();
    for &(i, t, a) in accesses {
        if size_of(t).is_none() {
            continue;
        }
        match life.get_mut(&t) {
            Some(l) => l.2 = i,
            None => {
                life.insert(t, (i, a, i));
                order.push(t);
            }
        }
    }
    let mut plan = AliasPlan::default();
    // Physical textures: owner, size, last operation using it, members.
    let mut groups: Vec<(RtId, (u32, u32), usize, Vec<RtId>)> = Vec::new();
    for t in order {
        let (first, kind, last) = life[&t];
        if kind == Access::Read {
            continue;
        }
        let Some(size) = size_of(t) else { continue };
        match groups.iter_mut().find(|g| g.1 == size && g.2 < first) {
            Some(g) => {
                plan.shares.insert(t, g.0);
                g.2 = last;
                g.3.push(t);
            }
            None => groups.push((t, size, last, vec![t])),
        }
    }
    for (_, _, _, members) in groups.iter().filter(|g| g.3.len() > 1) {
        for t in members {
            if life[t].1 == Access::Overwrite {
                plan.clear_first.insert(*t);
            }
        }
    }
    plan
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
    /// Targets wait for [`Self::materialize`] before getting GPU memory.
    deferred: bool,
    /// Targets that may share memory with others ([`Self::mark_transient`]).
    transient: HashSet<RtId>,
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
            deferred: false,
            transient: HashSet::new(),
        }
    }

    /// A pool whose targets get GPU memory only at [`Self::materialize`] (scene builders).
    #[must_use]
    pub fn deferred() -> Self {
        Self {
            deferred: true,
            ..Self::new()
        }
    }

    /// Whether the targets are still waiting for [`Self::materialize`].
    #[must_use]
    pub fn is_deferred(&self) -> bool {
        self.deferred
    }

    /// Allows `id` to share its GPU memory with other transient targets of the same size whose
    /// lifetimes within a frame never overlap with its own (image composites, effect `fbos`,
    /// bloom buffers). The scene graph decides, when it is built, which ones really can: a
    /// target read before it is written in a frame (feedback, accumulation) keeps its own.
    pub fn mark_transient(&mut self, id: RtId) {
        if id.0 < self.targets.len() as u32 {
            self.transient.insert(id);
        }
    }

    /// [`Self::mark_transient`] for every target so far but `except` (targets something reads
    /// outside the frame operations, such as the scene target the outputs compose).
    pub fn mark_transient_all(&mut self, except: &[RtId]) {
        for i in 0..self.targets.len() as u32 {
            if !except.contains(&RtId(i)) {
                self.transient.insert(RtId(i));
            }
        }
    }

    #[must_use]
    pub fn is_transient(&self, id: RtId) -> bool {
        self.transient.contains(&id)
    }

    /// Gives every target its GPU memory: `shares` maps a target to the one whose texture it
    /// uses (that one must not share itself). The pool is no longer deferred afterwards: later
    /// targets are allocated when created.
    pub fn materialize(&mut self, ctx: &GpuContext, shares: &HashMap<RtId, RtId>) {
        for i in 0..self.targets.len() {
            let id = RtId(i as u32);
            if self.targets[i].gpu.is_some() || shares.contains_key(&id) {
                continue;
            }
            Self::allocate_gpu(ctx, &mut self.targets[i]);
        }
        for (&id, &owner) in shares {
            let Some(owner_gpu) = self
                .targets
                .get(owner.0 as usize)
                .and_then(|t| t.gpu.clone())
            else {
                continue;
            };
            let size = owner_size(&self.targets, owner);
            let t = &mut self.targets[id.0 as usize];
            if t.gpu.is_none() && (t.width, t.height) == size {
                t.gpu = Some(owner_gpu);
                t.shares = Some(owner);
            }
        }
        // A share that could not be honoured (sizes changed): its own memory.
        for t in &mut self.targets {
            if t.gpu.is_none() {
                Self::allocate_gpu(ctx, t);
            }
        }
        self.deferred = false;
    }

    /// Gives every target that shares another's texture a texture of its own (the frame
    /// order changed after the scene was built, so lifetimes no longer hold).
    pub fn unshare(&mut self, ctx: &GpuContext) {
        for t in &mut self.targets {
            if t.shares.take().is_some() {
                Self::allocate_gpu(ctx, t);
            }
        }
    }

    fn allocate_gpu(ctx: &GpuContext, t: &mut RenderTarget) {
        let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
            label: Some(&t.label),
            size: wgpu::Extent3d {
                width: t.width,
                height: t.height,
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
        t.gpu = Some((texture, view));
        t._memory = Some(
            ctx.memory()
                .track(u64::from(t.width) * u64::from(t.height) * 4),
        );
        t.shares = None;
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
        &self,
        ctx: &GpuContext,
        label: &str,
        width: u32,
        height: u32,
        sampler: SamplerKey,
    ) -> RenderTarget {
        let mut t = RenderTarget {
            gpu: None,
            width: width.max(1),
            height: height.max(1),
            sampler,
            label: label.to_owned(),
            _memory: None,
            shares: None,
        };
        if !self.deferred {
            Self::allocate_gpu(ctx, &mut t);
        }
        t
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
        let t = self.allocate(ctx, label, size.0, size.1, sampler);
        self.targets.push(t);
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
        let id = self.create_anonymous(ctx, "lumenwall.scratch", size, SamplerKey::CLAMP_LINEAR);
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
        // A resized target never shares memory any more (its size is its own).
        self.targets[id.0 as usize] = self.allocate(ctx, &label, size.0, size.1, sampler);
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

    /// Every target (statistics: labels and sizes).
    pub fn iter(&self) -> impl Iterator<Item = &RenderTarget> {
        self.targets.iter()
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
        self.create_composites_sized(ctx, id, (size, size), sampler)
    }

    /// [`Self::create_composites`] with a size for A and one for B: a composite the chain never
    /// writes and no material names holds only zeros and can be 1×1 (lumenwall-engine decides).
    pub fn create_composites_sized(
        &mut self,
        ctx: &GpuContext,
        id: i64,
        (size_a, size_b): ((u32, u32), (u32, u32)),
        sampler: SamplerKey,
    ) -> PingPong {
        let (na, nb) = composite_names(id);
        let root = self.scene_scope();
        PingPong {
            a: self.create(ctx, root, &na, size_a, sampler),
            b: self.create(ctx, root, &nb, size_b, sampler),
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

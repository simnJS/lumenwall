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
//!
//! Calls that may allocate take a [`Gpu`] (any `&GpuContext` converts). Without a wgpu context (a
//! scene on another backend, which allocates its own memory from the plan) the pool only plans:
//! names, sizes, flexible sizes and shares; [`RenderTarget::texture`] and [`RenderTarget::view`]
//! are then unavailable.

use std::collections::{HashMap, HashSet};

use crate::backend::Gpu;
use crate::context::{GpuContext, MemoryToken};
use crate::texture::SamplerKey;

/// Format of every render target (spec 04 §15).
pub const RT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Format of the render targets of a scene with HDR targets.
pub const RT_FORMAT_HDR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Bytes per texel of a render target format.
#[must_use]
pub fn texel_bytes(format: wgpu::TextureFormat) -> u64 {
    format.block_copy_size(None).map_or(4, u64::from)
}

/// Handle of a render target in a [`RenderTargetPool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RtId(pub(crate) u32);

impl RtId {
    /// Position in the pool (creation order, from 0).
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

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
    /// Size the shaders see (`g_TextureNResolution`): the size the target stands for when it is
    /// rendered smaller, at the displayed resolution ([`RenderTargetPool::create_scaled`]).
    nominal: (u32, u32),
    sampler: SamplerKey,
    label: String,
    /// `None` while not allocated, or when the texture belongs to another target.
    _memory: Option<MemoryToken>,
    /// The target whose texture this one shares.
    shares: Option<RtId>,
    /// Rendered below its nominal size: its actual size may grow to share a texture.
    flexible: bool,
    /// Texel format (the pool's when it was created).
    format: wgpu::TextureFormat,
    /// Samples per pixel of the drawing surface (1 = none). Above 1 the target keeps a
    /// multisampled companion texture that its render passes draw into and resolve to `gpu`.
    samples: u32,
    msaa: Option<(wgpu::Texture, wgpu::TextureView)>,
}

impl RenderTarget {
    fn gpu(&self) -> &(wgpu::Texture, wgpu::TextureView) {
        self.gpu
            .as_ref()
            .expect("render target used before its scene was built")
    }

    /// The wgpu texture (pools on wgpu only).
    ///
    /// # Panics
    ///
    /// Before the scene is built, or in a pool that plans for another backend.
    #[must_use]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.gpu().0
    }

    /// Samples per pixel of the drawing surface.
    #[must_use]
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// The multisampled view render passes draw into (resolving to [`Self::view`]), if any.
    #[must_use]
    pub fn msaa_view(&self) -> Option<&wgpu::TextureView> {
        self.msaa.as_ref().map(|m| &m.1)
    }

    #[must_use]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.gpu().1
    }

    /// Rendered below its nominal size, so its actual size may grow to share a texture.
    #[must_use]
    pub fn is_flexible(&self) -> bool {
        self.flexible
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

    /// Size the shaders see: [`Self::size`] unless the target is rendered below the size it
    /// stands for ([`RenderTargetPool::create_scaled`]).
    #[must_use]
    pub fn nominal_size(&self) -> (u32, u32) {
        self.nominal
    }

    /// `g_TextureNResolution` of a render target: `(w, h, w, h)` of its nominal size. A target
    /// rendered at the displayed resolution reports the size it stands for, so that texel
    /// offsets computed from it (blurs, sharpening, pixel grids) cover the same distance in
    /// texture coordinates as at full size: only the sampling density changes.
    #[must_use]
    pub fn resolution(&self) -> [f32; 4] {
        let (w, h) = (self.nominal.0 as f32, self.nominal.1 as f32);
        [w, h, w, h]
    }
}

/// Size of a target rendered at `scale` (≤ 1) of `nominal`: each axis rounded up, at least 1
/// and at most the nominal size.
#[must_use]
pub fn scaled_actual(nominal: (u32, u32), scale: f32) -> (u32, u32) {
    if !(scale > 0.0 && scale < 1.0) {
        return (nominal.0.max(1), nominal.1.max(1));
    }
    let f = |n: u32| ((n as f32 * scale).ceil() as u32).clamp(1, n.max(1));
    (f(nominal.0), f(nominal.1))
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
    /// New size of flexible targets that share a larger texture (every member of a group
    /// takes the group's size).
    pub sizes: HashMap<RtId, (u32, u32)>,
}

/// Size of a transient target for [`plan_aliasing`]: its pixels, and whether it may be
/// rendered larger (a target rendered below its nominal size, whose shaders see the nominal
/// size anyway: rendering it a little larger changes nothing but the sampling density).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AliasSize {
    pub size: (u32, u32),
    pub flexible: bool,
}

/// How much larger (in pixels) a flexible target may be rendered to share a texture.
pub const ALIAS_AREA_SLACK: f64 = 1.5;

fn area(s: (u32, u32)) -> f64 {
    f64::from(s.0) * f64::from(s.1)
}

/// Render-graph aliasing (pure): `accesses` lists, in frame order, `(operation index, target,
/// access)` (reads of an operation before its write); `size_of` gives the size of a transient
/// target and `None` for the others. A transient target whose first access in the frame is a
/// clear or an overwrite never shows anything from a previous frame: its content lives from
/// that access to its last one. Targets whose lifetimes do not overlap share one texture
/// (greedy interval colouring in order of first access) when they have the same size, or,
/// for flexible targets, when the group's texture (the largest width and height of its
/// members) is at most `slack` times the pixels of each member.
pub fn plan_aliasing(
    accesses: &[(usize, RtId, Access)],
    size_of: impl Fn(RtId) -> Option<AliasSize>,
    slack: f64,
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
    struct Group {
        owner: RtId,
        size: (u32, u32),
        flexible: bool,
        /// Pixels of the smallest member.
        smallest: f64,
        last: usize,
        members: Vec<RtId>,
    }
    let mut plan = AliasPlan::default();
    let mut groups: Vec<Group> = Vec::new();
    for t in order {
        let (first, kind, last) = life[&t];
        if kind == Access::Read {
            continue;
        }
        let Some(s) = size_of(t) else { continue };
        // The group that wastes the fewest pixels.
        let fit = |g: &Group| -> Option<f64> {
            if g.last >= first {
                return None;
            }
            if g.size == s.size {
                return Some(0.0);
            }
            if !(g.flexible && s.flexible) {
                return None;
            }
            let merged = (g.size.0.max(s.size.0), g.size.1.max(s.size.1));
            let smallest = g.smallest.min(area(s.size));
            (area(merged) <= slack * smallest).then(|| area(merged) - area(g.size))
        };
        let best = groups
            .iter()
            .enumerate()
            .filter_map(|(i, g)| fit(g).map(|w| (i, w)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        match best {
            Some(i) => {
                let g = &mut groups[i];
                plan.shares.insert(t, g.owner);
                g.size = (g.size.0.max(s.size.0), g.size.1.max(s.size.1));
                g.flexible &= s.flexible;
                g.smallest = g.smallest.min(area(s.size));
                g.last = last;
                g.members.push(t);
            }
            None => groups.push(Group {
                owner: t,
                size: s.size,
                flexible: s.flexible,
                smallest: area(s.size),
                last,
                members: vec![t],
            }),
        }
    }
    for g in groups.iter().filter(|g| g.members.len() > 1) {
        for t in &g.members {
            if life[t].1 == Access::Overwrite {
                plan.clear_first.insert(*t);
            }
            if size_of(*t).is_some_and(|s| s.size != g.size) {
                plan.sizes.insert(*t, g.size);
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
    /// Texel format of every target created from now on ([`RT_FORMAT`] unless HDR).
    format: wgpu::TextureFormat,
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
            format: RT_FORMAT,
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

    /// Gives `id` a multisampled drawing surface of `samples` per pixel. Before materialization.
    pub fn set_samples(&mut self, id: RtId, samples: u32) {
        self.targets[id.0 as usize].samples = samples.max(1);
    }

    /// Format of the targets created from now on: call before creating any. 16-bit float
    /// targets keep values above 1 (HDR scenes).
    pub fn set_format(&mut self, format: wgpu::TextureFormat) {
        self.format = format;
    }

    /// Texel format of the targets.
    #[must_use]
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
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

    /// Whether any target was marked transient (the owner allows the frame to be restructured).
    #[must_use]
    pub fn has_transient(&self) -> bool {
        !self.transient.is_empty()
    }

    #[must_use]
    pub fn is_transient(&self, id: RtId) -> bool {
        self.transient.contains(&id)
    }

    /// Gives every target its GPU memory: `shares` maps a target to the one whose texture it
    /// uses (that one must not share itself). The pool is no longer deferred afterwards: later
    /// targets are allocated when created.
    pub fn materialize<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        shares: &HashMap<RtId, RtId>,
        sizes: &HashMap<RtId, (u32, u32)>,
    ) {
        let gpu: Gpu<'g> = gpu.into();
        let Some(ctx) = gpu.wgpu() else {
            self.plan_without_memory(shares, sizes);
            return;
        };
        for (&id, &(w, h)) in sizes {
            if let Some(t) = self.targets.get_mut(id.0 as usize)
                && t.gpu.is_none()
                && t.flexible
            {
                (t.width, t.height) = (w.max(1), h.max(1));
            }
        }
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

    /// [`Self::materialize`] for a backend that holds the memory itself (no wgpu context): the
    /// sizes of flexible targets and the shares are recorded ([`RenderTarget::shares`],
    /// [`RenderTarget::size`]), nothing is allocated.
    fn plan_without_memory(
        &mut self,
        shares: &HashMap<RtId, RtId>,
        sizes: &HashMap<RtId, (u32, u32)>,
    ) {
        if self.deferred {
            for (&id, &(w, h)) in sizes {
                if let Some(t) = self.targets.get_mut(id.0 as usize)
                    && t.flexible
                {
                    (t.width, t.height) = (w.max(1), h.max(1));
                }
            }
            for (&id, &owner) in shares {
                // The owner keeps memory of its own (it shares nothing itself).
                if owner == id
                    || owner.0 as usize >= self.targets.len()
                    || shares.contains_key(&owner)
                {
                    continue;
                }
                let size = owner_size(&self.targets, owner);
                if let Some(t) = self.targets.get_mut(id.0 as usize)
                    && (t.width, t.height) == size
                {
                    t.shares = Some(owner);
                }
            }
        }
        self.deferred = false;
    }

    /// Gives every target that shares another's texture a texture of its own (the frame
    /// order changed after the scene was built, so lifetimes no longer hold).
    pub fn unshare<'g>(&mut self, gpu: impl Into<Gpu<'g>>) {
        let gpu: Gpu<'g> = gpu.into();
        for t in &mut self.targets {
            if t.shares.take().is_some()
                && let Some(ctx) = gpu.wgpu()
            {
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
            format: t.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        ctx.note_resources(2);
        t.gpu = Some((texture, view));
        t.msaa = (t.samples > 1).then(|| {
            let ms = ctx.device().create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("{} (msaa)", t.label)),
                size: wgpu::Extent3d {
                    width: t.width,
                    height: t.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: t.samples,
                dimension: wgpu::TextureDimension::D2,
                format: t.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = ms.create_view(&wgpu::TextureViewDescriptor::default());
            ctx.note_resources(2);
            (ms, view)
        });
        t._memory = Some(ctx.memory().track(
            u64::from(t.width)
                * u64::from(t.height)
                * texel_bytes(t.format)
                * u64::from(1 + if t.samples > 1 { t.samples } else { 0 }),
        ));
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
        gpu: Gpu<'_>,
        label: &str,
        (width, height): (u32, u32),
        nominal: (u32, u32),
        sampler: SamplerKey,
    ) -> RenderTarget {
        let mut t = RenderTarget {
            gpu: None,
            width: width.max(1),
            height: height.max(1),
            nominal: (nominal.0.max(1), nominal.1.max(1)),
            sampler,
            label: label.to_owned(),
            _memory: None,
            shares: None,
            flexible: false,
            format: self.format,
            samples: 1,
            msaa: None,
        };
        if !self.deferred
            && let Some(ctx) = gpu.wgpu()
        {
            Self::allocate_gpu(ctx, &mut t);
        }
        t
    }

    /// Create a target and register `name` in `scope` (a later registration of
    /// the same name in the same scope shadows the earlier one).
    pub fn create<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        scope: ScopeId,
        name: &str,
        size: (u32, u32),
        sampler: SamplerKey,
    ) -> RtId {
        let gpu: Gpu<'g> = gpu.into();
        let id = self.create_anonymous(gpu, name, size, sampler);
        self.alias(scope, name, id);
        id
    }

    /// Create a target without a name.
    pub fn create_anonymous<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        label: &str,
        size: (u32, u32),
        sampler: SamplerKey,
    ) -> RtId {
        let gpu: Gpu<'g> = gpu.into();
        let t = self.allocate(gpu, label, size, size, sampler);
        self.targets.push(t);
        RtId(self.targets.len() as u32 - 1)
    }

    /// [`Self::create`] for a target rendered at `scale` (≤ 1) of `nominal`, the size it stands
    /// for: allocated at [`scaled_actual`], reporting `nominal` to the shaders
    /// ([`RenderTarget::resolution`]).
    pub fn create_scaled<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        scope: ScopeId,
        name: &str,
        nominal: (u32, u32),
        scale: f32,
        sampler: SamplerKey,
    ) -> RtId {
        let gpu: Gpu<'g> = gpu.into();
        let mut t = self.allocate(gpu, name, scaled_actual(nominal, scale), nominal, sampler);
        t.flexible = self.deferred && scale < 1.0 && t.size() != t.nominal;
        self.targets.push(t);
        let id = RtId(self.targets.len() as u32 - 1);
        self.alias(scope, name, id);
        id
    }

    /// An unnamed target of `actual` pixels standing for `nominal` (`lumenwall.blank`: a
    /// never-written layer source is 1×1 whatever the size it reports).
    pub fn create_anonymous_at<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        label: &str,
        actual: (u32, u32),
        nominal: (u32, u32),
        sampler: SamplerKey,
    ) -> RtId {
        let gpu: Gpu<'g> = gpu.into();
        let t = self.allocate(gpu, label, actual, nominal, sampler);
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
    pub fn scratch<'g>(&mut self, gpu: impl Into<Gpu<'g>>, size: (u32, u32)) -> RtId {
        let gpu: Gpu<'g> = gpu.into();
        if let Some(id) = self.scratch.get(&size) {
            return *id;
        }
        let id = self.create_anonymous(gpu, "lumenwall.scratch", size, SamplerKey::CLAMP_LINEAR);
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
    pub fn resize<'g>(&mut self, gpu: impl Into<Gpu<'g>>, id: RtId, size: (u32, u32)) {
        let gpu: Gpu<'g> = gpu.into();
        let Some(t) = self.targets.get(id.0 as usize) else {
            return;
        };
        let size = (size.0.max(1), size.1.max(1));
        if t.size() == size && t.nominal == size {
            return;
        }
        let (label, sampler) = (t.label.clone(), t.sampler);
        // A resized target never shares memory any more (its size is its own).
        self.targets[id.0 as usize] = self.allocate(gpu, &label, size, size, sampler);
    }

    /// [`Self::resize`] for a target rendered at `scale` of `nominal` ([`Self::create_scaled`]).
    pub fn resize_scaled<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        id: RtId,
        nominal: (u32, u32),
        scale: f32,
    ) {
        let gpu: Gpu<'g> = gpu.into();
        let Some(t) = self.targets.get(id.0 as usize) else {
            return;
        };
        let actual = scaled_actual(nominal, scale);
        if t.size() == actual && t.nominal == nominal {
            return;
        }
        let (label, sampler) = (t.label.clone(), t.sampler);
        self.targets[id.0 as usize] = self.allocate(gpu, &label, actual, nominal, sampler);
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
    pub fn create_scene_targets<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        size: (u32, u32),
        full_sampler: SamplerKey,
    ) -> SceneTargets {
        let gpu: Gpu<'g> = gpu.into();
        self.create_scene_targets_at(gpu, size, size, full_sampler)
    }

    /// [`Self::create_scene_targets`] with `_rt_FullFrameBuffer` rendered at `actual` pixels while
    /// standing for `nominal` (the scene at its displayed size, [`RenderTarget::resolution`]).
    pub fn create_scene_targets_at<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        nominal: (u32, u32),
        actual: (u32, u32),
        full_sampler: SamplerKey,
    ) -> SceneTargets {
        let gpu: Gpu<'g> = gpu.into();
        let root = self.scene_scope();
        let full =
            self.create_anonymous_at(gpu, "_rt_FullFrameBuffer", actual, nominal, full_sampler);
        self.alias(root, "_rt_FullFrameBuffer", full);
        self.alias(root, "_rt_MipMappedFrameBuffer", full);
        let shadow_atlas = self.create(
            gpu,
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
    pub fn create_bloom_targets<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        size: (u32, u32),
    ) -> BloomTargets {
        let gpu: Gpu<'g> = gpu.into();
        self.create_bloom_targets_scaled(gpu, size, 1.0)
    }

    /// [`Self::create_bloom_targets`] rendered at `scale` of their nominal sizes.
    pub fn create_bloom_targets_scaled<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        size: (u32, u32),
        scale: f32,
    ) -> BloomTargets {
        let gpu: Gpu<'g> = gpu.into();
        let root = self.scene_scope();
        let q = divided_size(size, 4);
        let e = divided_size(size, 8);
        let c = SamplerKey::CLAMP_LINEAR;
        BloomTargets {
            quarter: self.create_scaled(gpu, root, "_rt_4FrameBuffer", q, scale, c),
            eighth: self.create_scaled(gpu, root, "_rt_8FrameBuffer", e, scale, c),
            bloom: self.create_scaled(gpu, root, "_rt_Bloom", e, scale, c),
        }
    }

    /// Composites of the synthetic bloom image (id −1, spec 04 §17). Its chain
    /// never writes B, and no object can resolve the name (bloom is created
    /// after every object), so B is allocated 1×1.
    pub fn create_bloom_composites<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        size: (u32, u32),
    ) -> PingPong {
        let gpu: Gpu<'g> = gpu.into();
        self.create_bloom_composites_scaled(gpu, size, 1.0)
    }

    /// [`Self::create_bloom_composites`] with A rendered at `scale` of `size`.
    pub fn create_bloom_composites_scaled<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        size: (u32, u32),
        scale: f32,
    ) -> PingPong {
        let gpu: Gpu<'g> = gpu.into();
        let (na, nb) = composite_names(crate::bloom::BLOOM_IMAGE_ID);
        let root = self.scene_scope();
        PingPong {
            a: self.create_scaled(gpu, root, &na, size, scale, SamplerKey::CLAMP_LINEAR),
            b: self.create(gpu, root, &nb, (1, 1), SamplerKey::CLAMP_LINEAR),
        }
    }

    /// Create the composite pair of image `id` in the scene scope (spec 04 §8.9).
    pub fn create_composites<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        id: i64,
        size: (u32, u32),
        sampler: SamplerKey,
    ) -> PingPong {
        let gpu: Gpu<'g> = gpu.into();
        self.create_composites_sized(gpu, id, (size, size), sampler)
    }

    /// [`Self::create_composites`] with a size for A and one for B: a composite the chain never
    /// writes and no material names holds only zeros and can be 1×1 (lumenwall-engine decides).
    pub fn create_composites_sized<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        id: i64,
        (size_a, size_b): ((u32, u32), (u32, u32)),
        sampler: SamplerKey,
    ) -> PingPong {
        let gpu: Gpu<'g> = gpu.into();
        self.create_composites_scaled(gpu, id, (size_a, size_b), 1.0, sampler)
    }

    /// [`Self::create_composites_sized`] rendered at `scale` of their nominal sizes.
    pub fn create_composites_scaled<'g>(
        &mut self,
        gpu: impl Into<Gpu<'g>>,
        id: i64,
        (size_a, size_b): ((u32, u32), (u32, u32)),
        scale: f32,
        sampler: SamplerKey,
    ) -> PingPong {
        let gpu: Gpu<'g> = gpu.into();
        let (na, nb) = composite_names(id);
        let root = self.scene_scope();
        PingPong {
            a: self.create_scaled(gpu, root, &na, size_a, scale, sampler),
            b: self.create_scaled(gpu, root, &nb, size_b, scale, sampler),
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

    fn fixed(w: u32, h: u32) -> Option<AliasSize> {
        Some(AliasSize {
            size: (w, h),
            flexible: false,
        })
    }

    #[test]
    fn aliasing_shares_targets_whose_lifetimes_do_not_overlap() {
        use Access::{Clear, Overwrite, Read};
        let (a, b, c, d, e) = (RtId(0), RtId(1), RtId(2), RtId(3), RtId(4));
        // a: ops 0..2, b: ops 3..4 (same size, after a: shares a), c: overlaps b (own),
        // d: read before written (feedback: own memory), e: other size (own).
        let acc = [
            (0, a, Overwrite),
            (1, a, Read),
            (2, a, Read),
            (3, b, Clear),
            (3, c, Overwrite),
            (4, b, Read),
            (4, c, Read),
            (5, d, Read),
            (6, d, Overwrite),
            (7, e, Overwrite),
        ];
        let size = |t: RtId| if t == e { fixed(8, 8) } else { fixed(4, 4) };
        let plan = plan_aliasing(&acc, size, 1.0);
        assert_eq!(plan.shares.get(&b), Some(&a));
        assert_eq!(plan.shares.get(&c), None, "c lives while b does");
        assert_eq!(plan.shares.get(&d), None, "d keeps last frame's pixels");
        assert_eq!(plan.shares.get(&e), None);
        // a is overwritten first and its texture is shared: cleared first. b starts with a
        // clear of its own.
        assert!(plan.clear_first.contains(&a) && !plan.clear_first.contains(&b));
        assert!(plan.sizes.is_empty());
        // Not transient at all: nothing shared.
        assert_eq!(plan_aliasing(&acc, |_| None, 1.0), AliasPlan::default());
    }

    #[test]
    fn flexible_targets_share_a_slightly_larger_texture() {
        use Access::{Overwrite, Read};
        let (a, b, c) = (RtId(0), RtId(1), RtId(2));
        let acc = [
            (0, a, Overwrite),
            (1, a, Read),
            (2, b, Overwrite),
            (3, b, Read),
            (4, c, Overwrite),
            (5, c, Read),
        ];
        let flex = |w, h| {
            Some(AliasSize {
                size: (w, h),
                flexible: true,
            })
        };
        // b is 10 % wider than a: one 110x100 texture; c is 4× a: too large to merge.
        let size = |t: RtId| match t.0 {
            0 => flex(100, 100),
            1 => flex(110, 95),
            _ => flex(200, 200),
        };
        let plan = plan_aliasing(&acc, size, ALIAS_AREA_SLACK);
        assert_eq!(plan.shares.get(&b), Some(&a));
        assert_eq!(plan.shares.get(&c), None);
        assert_eq!(plan.sizes.get(&a), Some(&(110, 100)));
        assert_eq!(plan.sizes.get(&b), Some(&(110, 100)));
        // Without slack, or when one of them is not flexible, only equal sizes share.
        assert!(plan_aliasing(&acc, size, 1.0).shares.is_empty());
        let rigid = |t: RtId| if t == b { fixed(110, 95) } else { size(t) };
        assert!(
            plan_aliasing(&acc, rigid, ALIAS_AREA_SLACK)
                .shares
                .is_empty()
        );
    }

    #[test]
    fn scaled_targets_report_their_nominal_size() {
        assert_eq!(scaled_actual((3840, 2160), 2.0 / 3.0), (2560, 1440));
        assert_eq!(scaled_actual((3440, 1440), 1.0), (3440, 1440));
        assert_eq!(scaled_actual((1, 1), 0.3), (1, 1));
        assert_eq!(scaled_actual((101, 33), 0.5), (51, 17));
        assert_eq!(scaled_actual((0, 5), 0.5), (1, 3));
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

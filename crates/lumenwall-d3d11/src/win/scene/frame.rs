//! The frame: uploads from the borrowed CPU state, then the ops recorded on the immediate context
//! with every binding tracked (no object created, no allocation), optionally with a timestamp pair
//! around each segment (`Scene::profile_frame`).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CLEAR_DEPTH, D3D11_QUERY, D3D11_QUERY_DATA_TIMESTAMP_DISJOINT,
    D3D11_QUERY_DESC, D3D11_QUERY_TIMESTAMP, D3D11_QUERY_TIMESTAMP_DISJOINT, ID3D11Buffer,
    ID3D11DepthStencilView, ID3D11Query, ID3D11SamplerState, ID3D11ShaderResourceView,
};
use windows::core::Interface;

use super::{
    DEFAULTS_OFFSET, DEFAULTS_SLOT, Draw, FrameStats, GEOMETRY_SLOT, Pass, SKIN_SLOT, Scene,
    SlotBind, Srv,
};
use crate::error::{Error, Result, out};
use crate::hlsl::MAX_SLOTS;
use crate::layout::ConstantRegion;
use crate::plan::{FrameInput, IMAGE_VERTEX_STRIDE, OpPlan, SKIN_STRIDE};
use crate::win::device::{Device, get_data};

const SLOTS: usize = MAX_SLOTS as usize;
/// How long a profiling frame waits for its timestamps.
const PROFILE_TIMEOUT: Duration = Duration::from_secs(5);

/// GPU time of one segment of a profiled frame ([`Scene::profile_frame`]).
#[derive(Clone, Debug, PartialEq)]
pub struct SegmentTime {
    /// Label of the target drawn into (of the destination, for a copy).
    pub target: String,
    pub size: (u32, u32),
    /// Draws of the segment (0 for a copy).
    pub passes: usize,
    /// Label of its first pass (`"copy"` for a copy).
    pub first_pass: String,
    pub ms: f64,
}

/// The frame input must match the scene (sizes of the shadow and the vertex region, one entry
/// per pass and stream).
fn check_frame(scene: &Scene, frame: &FrameInput<'_>) -> Result<()> {
    let bad = |m: String| Err(Error::InvalidData(format!("frame input: {m}")));
    if frame.shadow.len() != scene.constants_len as usize {
        return bad(format!(
            "shadow of {} bytes, the scene's is {}",
            frame.shadow.len(),
            scene.constants_len
        ));
    }
    if frame.vertices.len() != scene.vertex_bytes as usize {
        return bad(format!(
            "vertex region of {} bytes, the scene's is {}",
            frame.vertices.len(),
            scene.vertex_bytes
        ));
    }
    if frame.variants.len() < scene.passes.len() || frame.enabled.len() < scene.passes.len() {
        return bad(format!(
            "{} variants and {} enabled flags for {} passes",
            frame.variants.len(),
            frame.enabled.len(),
            scene.passes.len()
        ));
    }
    if frame.streams.len() < scene.streams.len() {
        return bad(format!(
            "{} streams, the scene has {}",
            frame.streams.len(),
            scene.streams.len()
        ));
    }
    Ok(())
}

/// The frame's writes: the shadow (one `Map` at most), the vertex region when dirty, the streams
/// given vertices.
fn upload(
    scene: &mut Scene,
    device: &Device,
    frame: &FrameInput<'_>,
    stats: &mut FrameStats,
) -> Result<()> {
    if scene.constants_len > 0 {
        let whole = ConstantRegion {
            offset: 0,
            size: scene.constants_len,
        };
        scene.arena.write(whole, 0, frame.shadow)?;
    }
    if scene.arena.upload(device)? {
        stats.constant_maps = 1;
    }
    if let Some(vb) = &scene.vertices {
        let dirty = frame
            .vertices_dirty
            .as_ref()
            .is_some_and(|r| r.start < r.end);
        if (dirty || scene.vertices_stale) && !frame.vertices.is_empty() {
            // `WRITE_DISCARD` drops the previous content: the region is written whole (a few
            // KiB: 20 bytes per vertex).
            vb.write(device, frame.vertices)?;
            stats.vertex_maps = 1;
            scene.vertices_stale = false;
        }
    }
    for (s, f) in scene.streams.iter_mut().zip(frame.streams) {
        if let Some(v) = f.vertices
            && !v.is_empty()
        {
            s.buffer.write(device, v)?;
            stats.stream_maps += 1;
        }
        s.index_count = f.index_count.min(s.max_indices);
    }
    Ok(())
}

pub(super) fn render(scene: &mut Scene, device: &Device, frame: &FrameInput<'_>) -> Result<()> {
    check_frame(scene, frame)?;
    let mut stats = FrameStats::default();
    upload(scene, device, frame, &mut stats)?;
    let mut cleared = std::mem::take(&mut scene.depth_cleared);
    cleared.clear();
    cleared.resize(scene.targets.len(), false);
    Recorder::new(scene, device, frame, &mut cleared, &mut stats, None).run();
    scene.depth_cleared = cleared;
    scene.last = stats;
    device.check_lost()
}

pub(super) fn profile(
    scene: &mut Scene,
    device: &Device,
    frame: &FrameInput<'_>,
) -> Result<Option<Vec<SegmentTime>>> {
    check_frame(scene, frame)?;
    let mut stats = FrameStats::default();
    upload(scene, device, frame, &mut stats)?;
    let mut cleared = std::mem::take(&mut scene.depth_cleared);
    cleared.clear();
    cleared.resize(scene.targets.len(), false);
    let mut prof = Profile {
        queries: std::mem::take(&mut scene.profile_queries),
        next: 0,
        segments: Vec::new(),
        created: 0,
        error: None,
    };
    let disjoint = prof.query(device, D3D11_QUERY_TIMESTAMP_DISJOINT);
    if let Some(q) = disjoint.and_then(|i| prof.queries.get(i)) {
        // SAFETY: a query of this device, begun once and ended below.
        unsafe { device.context().Begin(q) };
    }
    Recorder::new(
        scene,
        device,
        frame,
        &mut cleared,
        &mut stats,
        Some(&mut prof),
    )
    .run();
    if let Some(q) = disjoint.and_then(|i| prof.queries.get(i)) {
        // SAFETY: as above.
        unsafe { device.context().End(q) };
    }
    let Profile {
        queries,
        segments,
        created,
        error,
        ..
    } = prof;
    scene.depth_cleared = cleared;
    scene.last = stats;
    scene.created += created;
    let result = match (error, disjoint) {
        (Some(e), _) => Err(e),
        (None, None) => Ok(None),
        (None, Some(d)) => read_profile(scene, device, &queries, d, &segments),
    };
    scene.profile_queries = queries;
    device.check_lost()?;
    result
}

/// Waits for the timestamps of a profiled frame and turns them into segment times.
fn read_profile(
    scene: &Scene,
    device: &Device,
    queries: &[ID3D11Query],
    disjoint: usize,
    segments: &[Segment],
) -> Result<Option<Vec<SegmentTime>>> {
    device.flush();
    let ctx = device.context();
    let deadline = Instant::now() + PROFILE_TIMEOUT;
    let wait =
        |q: &ID3D11Query, read: &mut dyn FnMut(&ID3D11Query) -> Result<bool>| -> Result<bool> {
            loop {
                if read(q)? {
                    return Ok(true);
                }
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
    let Some(dq) = queries.get(disjoint) else {
        return Ok(None);
    };
    let mut dj = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
    let ready = wait(dq, &mut |q| {
        get_data(ctx, q, &mut dj, 0).map_err(|e| device.error("GetData", &e))
    })?;
    if !ready || dj.Disjoint.as_bool() || dj.Frequency == 0 {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(segments.len());
    for s in segments {
        let mut ticks = [0u64; 2];
        for (t, i) in ticks.iter_mut().zip([s.begin, s.end]) {
            let Some(q) = queries.get(i) else {
                return Ok(None);
            };
            if !wait(q, &mut |q| {
                get_data(ctx, q, t, 0).map_err(|e| device.error("GetData", &e))
            })? {
                return Ok(None);
            }
        }
        let ms = ticks[1].saturating_sub(ticks[0]) as f64 * 1000.0 / dj.Frequency as f64;
        let target = scene.targets.get(s.target);
        out.push(SegmentTime {
            target: target.map_or_else(String::new, |t| t.plan.label.clone()),
            size: target.map_or((0, 0), |t| t.plan.size),
            passes: s.draws,
            first_pass: match (s.copy, s.first_pass.and_then(|p| scene.passes.get(p))) {
                (true, _) => "copy".to_owned(),
                (false, Some(p)) => p.label.clone(),
                (false, None) => String::new(),
            },
            ms,
        });
    }
    Ok(Some(out))
}

/// One segment of a profiled frame and the indices of its timestamp queries.
struct Segment {
    target: usize,
    copy: bool,
    draws: usize,
    first_pass: Option<usize>,
    begin: usize,
    end: usize,
}

/// Timestamp queries of a profiled frame (created the first time, reused afterwards).
struct Profile {
    queries: Vec<ID3D11Query>,
    next: usize,
    segments: Vec<Segment>,
    created: u64,
    error: Option<Error>,
}

impl Profile {
    /// The next query of the frame (index in `queries`), created when the frame needs more than
    /// any profiled frame before. Index 0 is the disjoint query.
    fn query(&mut self, device: &Device, kind: D3D11_QUERY) -> Option<usize> {
        if self.error.is_some() {
            return None;
        }
        let i = self.next;
        if i >= self.queries.len() {
            let mut q = None;
            // SAFETY: valid descriptor, live out-parameter.
            let r = unsafe {
                device.raw().CreateQuery(
                    &D3D11_QUERY_DESC {
                        Query: kind,
                        MiscFlags: 0,
                    },
                    Some(&mut q),
                )
            }
            .map_err(|e| device.error("CreateQuery", &e))
            .and_then(|()| out(q, "CreateQuery"));
            match r {
                Ok(q) => {
                    self.queries.push(q);
                    self.created += 1;
                }
                Err(e) => {
                    self.error = Some(e);
                    return None;
                }
            }
        }
        self.next += 1;
        Some(i)
    }

    /// Writes a timestamp; its query index.
    fn stamp(&mut self, device: &Device) -> usize {
        let Some(i) = self.query(device, D3D11_QUERY_TIMESTAMP) else {
            return usize::MAX;
        };
        if let Some(q) = self.queries.get(i) {
            // SAFETY: a timestamp query of this device (ended only).
            unsafe { device.context().End(q) };
        }
        i
    }
}

/// Identity of a bound shader resource: the texture behind a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewKey {
    Texture(usize),
    /// A render target's texture (index in `Scene::textures_rt`).
    Target(usize),
    Default2D,
    Default3D,
}

const NO_VIEWS: [Option<ID3D11ShaderResourceView>; SLOTS] = [const { None }; SLOTS];

/// Records the ops of one frame, setting only what changed since the previous draw.
struct Recorder<'a, 'f> {
    scene: &'a Scene,
    device: &'a Device,
    frame: &'a FrameInput<'f>,
    cleared: &'a mut [bool],
    stats: &'a mut FrameStats,
    profile: Option<&'a mut Profile>,
    /// Target of the open segment.
    target: Option<usize>,
    program: Option<usize>,
    layout: Option<usize>,
    /// Raw pointers of the bound states (0: none).
    states: [usize; 3],
    /// `(buffer, stride, offset)` of the geometry and skin slots.
    vertex_buffers: [Option<(usize, u32, u32)>; 2],
    /// `(buffer, 32-bit, offset)`.
    index_buffer: Option<(usize, bool, u32)>,
    b0: Option<ConstantRegion>,
    views: [Option<ViewKey>; SLOTS],
    samplers: [Option<usize>; SLOTS],
}

impl<'a, 'f> Recorder<'a, 'f> {
    fn new(
        scene: &'a Scene,
        device: &'a Device,
        frame: &'a FrameInput<'f>,
        cleared: &'a mut [bool],
        stats: &'a mut FrameStats,
        profile: Option<&'a mut Profile>,
    ) -> Self {
        Self {
            scene,
            device,
            frame,
            cleared,
            stats,
            profile,
            target: None,
            program: None,
            layout: None,
            states: [0; 3],
            vertex_buffers: [None; 2],
            index_buffer: None,
            b0: None,
            views: [None; SLOTS],
            samplers: [None; SLOTS],
        }
    }

    fn run(mut self) {
        self.begin_frame();
        let scene = self.scene;
        for op in &scene.ops {
            match *op {
                OpPlan::Clear { target, color } => self.open_segment(target, Some(color)),
                OpPlan::Copy { src, dst } => self.copy(src, dst),
                OpPlan::Draw(p) => {
                    if !self.frame.enabled.get(p).copied().unwrap_or(false) {
                        continue;
                    }
                    let Some(pass) = scene.passes.get(p) else {
                        continue;
                    };
                    if let Some(scratch) = pass.hazard {
                        self.copy(pass.target, scratch);
                    }
                    if pass.clear_first {
                        // Shared texture: start from transparent black, as a fresh target.
                        self.open_segment(pass.target, Some([0.0; 4]));
                    } else if self.target != Some(pass.target) {
                        self.open_segment(pass.target, None);
                    }
                    self.draw(p, pass);
                }
            }
        }
        self.end_frame();
    }

    /// Bindings no op changes: nothing is assumed of the context (other users, `Trim`'s
    /// `ClearState`).
    fn begin_frame(&mut self) {
        let (scene, device) = (self.scene, self.device);
        let ctx = device.context();
        // SAFETY: plain state calls on the immediate context with null views and a constant.
        unsafe {
            ctx.VSSetShaderResources(0, Some(&NO_VIEWS));
            ctx.PSSetShaderResources(0, Some(&NO_VIEWS));
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        }
        scene
            .static_vertices
            .bind_vertices(device, DEFAULTS_SLOT, 0, DEFAULTS_OFFSET);
        if let Some(w) = scene.frame_block {
            scene.arena.bind(device, 1, w);
        }
    }

    /// Unbinds the render target and the views, so that whatever runs next (the composition,
    /// another scene) can sample or draw into any target of this one.
    fn end_frame(&mut self) {
        self.close_segment();
        let ctx = self.device.context();
        // SAFETY: plain state calls on the immediate context with null views.
        unsafe {
            ctx.OMSetRenderTargets(None, None::<&ID3D11DepthStencilView>);
            if self.views.iter().any(Option::is_some) {
                ctx.VSSetShaderResources(0, Some(&NO_VIEWS));
                ctx.PSSetShaderResources(0, Some(&NO_VIEWS));
            }
        }
    }

    /// Binds `target` (after unbinding the views of its texture), clears it if asked and clears
    /// its depth at its first segment of the frame.
    fn open_segment(&mut self, target: usize, clear: Option<[f32; 4]>) {
        self.close_segment();
        let (scene, device) = (self.scene, self.device);
        let Some(t) = scene.targets.get(target) else {
            return;
        };
        let Some(rt) = scene.textures_rt.get(t.physical) else {
            return;
        };
        if t.samples == 1 {
            // Draws go to the sampled texture itself (with MSAA they go to the multisampled one).
            self.unbind_views(ViewKey::Target(t.physical));
        }
        rt.bind(device);
        let (w, h) = rt.size();
        let ctx = device.context();
        // SAFETY: views of this device; the rectangle is a plain value.
        unsafe {
            ctx.RSSetScissorRects(Some(&[RECT {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            }]));
            if let Some(c) = clear {
                ctx.ClearRenderTargetView(rt.draw_view(), &c);
            }
            if let Some(d) = rt.depth()
                && let Some(done) = self.cleared.get_mut(target)
                && !*done
            {
                ctx.ClearDepthStencilView(d.view(), D3D11_CLEAR_DEPTH.0, 1.0, 0);
                *done = true;
            }
        }
        if clear.is_some() {
            self.stats.clears += 1;
        }
        self.stats.segments += 1;
        self.target = Some(target);
        if let Some(p) = self.profile.as_deref_mut() {
            let begin = p.stamp(device);
            p.segments.push(Segment {
                target,
                copy: false,
                draws: 0,
                first_pass: None,
                begin,
                end: usize::MAX,
            });
        }
    }

    /// Ends the open segment: a multisampled target is resolved into its sampled texture.
    fn close_segment(&mut self) {
        let Some(target) = self.target.take() else {
            return;
        };
        let (scene, device) = (self.scene, self.device);
        if let Some(t) = scene.targets.get(target)
            && t.samples > 1
            && let Some(rt) = scene.textures_rt.get(t.physical)
        {
            rt.resolve(device);
            self.stats.resolves += 1;
        }
        if let Some(p) = self.profile.as_deref_mut() {
            let end = p.stamp(device);
            if let Some(s) = p.segments.last_mut() {
                s.end = end;
            }
        }
    }

    /// Copies the (resolved) texture of `src` into the texture of `dst`, at most their common
    /// size.
    fn copy(&mut self, src: usize, dst: usize) {
        self.close_segment();
        let (scene, device) = (self.scene, self.device);
        let (Some(s), Some(d)) = (scene.targets.get(src), scene.targets.get(dst)) else {
            return;
        };
        if s.physical == d.physical {
            return;
        }
        let (Some(from), Some(to)) = (
            scene.textures_rt.get(s.physical),
            scene.textures_rt.get(d.physical),
        ) else {
            return;
        };
        let begin = self.profile.as_deref_mut().map(|p| p.stamp(device));
        let ctx = device.context();
        let ((sw, sh), (dw, dh)) = (from.size(), to.size());
        // SAFETY: two distinct single-sample textures of this device with the same format
        // (checked at load time); the box lies inside both.
        unsafe {
            if (sw, sh) == (dw, dh) {
                ctx.CopyResource(to.texture(), from.texture());
            } else {
                let region = D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: sw.min(dw),
                    bottom: sh.min(dh),
                    back: 1,
                };
                ctx.CopySubresourceRegion(
                    to.texture(),
                    0,
                    0,
                    0,
                    0,
                    from.texture(),
                    0,
                    Some(&raw const region),
                );
            }
        }
        self.stats.copies += 1;
        self.stats.segments += 1;
        if let Some(p) = self.profile.as_deref_mut() {
            let end = p.stamp(device);
            p.segments.push(Segment {
                target: dst,
                copy: true,
                draws: 0,
                first_pass: None,
                begin: begin.unwrap_or(usize::MAX),
                end,
            });
        }
    }

    /// Unbinds every slot that samples `key` (both stages).
    fn unbind_views(&mut self, key: ViewKey) {
        let ctx = self.device.context();
        for (n, bound) in self.views.iter_mut().enumerate() {
            if *bound == Some(key) {
                let none = [None::<ID3D11ShaderResourceView>];
                // SAFETY: one null view at a valid slot.
                unsafe {
                    ctx.VSSetShaderResources(n as u32, Some(&none));
                    ctx.PSSetShaderResources(n as u32, Some(&none));
                }
                *bound = None;
            }
        }
    }

    fn view_key(&self, srv: Srv) -> Option<ViewKey> {
        Some(match srv {
            Srv::Texture(i) => ViewKey::Texture(i),
            Srv::Target(t) => ViewKey::Target(self.scene.targets.get(t)?.physical),
            Srv::Default2D => ViewKey::Default2D,
            Srv::Default3D => ViewKey::Default3D,
        })
    }

    fn view(&self, key: ViewKey) -> Option<&'a ID3D11ShaderResourceView> {
        let scene = self.scene;
        match key {
            ViewKey::Texture(i) => scene.textures.get(i).map(|t| t.srv()),
            ViewKey::Target(p) => scene.textures_rt.get(p).map(|t| t.srv()),
            ViewKey::Default2D => Some(scene.default_2d.srv()),
            ViewKey::Default3D => scene.default_3d.as_ref().map(|t| t.srv()),
        }
    }

    fn draw(&mut self, index: usize, pass: &'a Pass) {
        let (scene, device) = (self.scene, self.device);
        let Some(program) = scene.programs.get(pass.program) else {
            return;
        };
        if self.program != Some(pass.program) {
            program.bind(device);
            self.program = Some(pass.program);
        }
        let ctx = device.context();
        if self.layout != Some(pass.layout) {
            let layout = scene.layouts.get(pass.layout).and_then(Option::as_ref);
            // SAFETY: an input layout of this device made for this program (or none).
            unsafe { ctx.IASetInputLayout(layout) };
            self.layout = Some(pass.layout);
        }
        let states = [
            pass.blend.as_raw() as usize,
            pass.raster.as_raw() as usize,
            pass.depth.as_raw() as usize,
        ];
        // SAFETY: state objects of this device.
        unsafe {
            if self.states[0] != states[0] {
                ctx.OMSetBlendState(&pass.blend, None, 0xFFFF_FFFF);
            }
            if self.states[1] != states[1] {
                ctx.RSSetState(&pass.raster);
            }
            if self.states[2] != states[2] {
                ctx.OMSetDepthStencilState(&pass.depth, 0);
            }
        }
        self.states = states;
        if let Some(w) = pass.uniform
            && self.b0 != Some(w)
        {
            scene.arena.bind(device, 0, w);
            self.b0 = Some(w);
        }
        let variant = usize::from(self.frame.variants.get(index).copied().unwrap_or(0));
        if let Some(table) = pass
            .tables
            .get(variant.min(pass.tables.len().saturating_sub(1)))
        {
            self.bind_slots(pass.slots, table);
        }
        if self.geometry(pass.draw) {
            self.stats.draws += 1;
            if let Some(p) = self.profile.as_deref_mut()
                && let Some(s) = p.segments.last_mut()
            {
                s.draws += 1;
                s.first_pass.get_or_insert(index);
            }
        }
    }

    /// Binds the views and samplers of the slots the program declares (vertex and pixel stages,
    /// as the wgpu path's bind group layouts) where they changed.
    fn bind_slots(&mut self, declared: u8, table: &[SlotBind; SLOTS]) {
        let ctx = self.device.context();
        for (n, slot) in table.iter().enumerate() {
            if declared & (1 << n) == 0 {
                continue;
            }
            if let Some(key) = slot.srv.and_then(|s| self.view_key(s))
                && self.views[n] != Some(key)
                && let Some(view) = self.view(key)
            {
                let views = [Some(view.clone())];
                // SAFETY: one view of this device at a valid slot.
                unsafe {
                    ctx.VSSetShaderResources(n as u32, Some(&views));
                    ctx.PSSetShaderResources(n as u32, Some(&views));
                }
                self.views[n] = Some(key);
            }
            if let Some(s) = slot.sampler
                && self.samplers[n] != Some(s)
                && let Some(sampler) = self.scene.samplers.get(s)
            {
                let samplers: [Option<ID3D11SamplerState>; 1] = [Some(sampler.clone())];
                // SAFETY: one sampler of this device at a valid slot.
                unsafe {
                    ctx.VSSetSamplers(n as u32, Some(&samplers));
                    ctx.PSSetSamplers(n as u32, Some(&samplers));
                }
                self.samplers[n] = Some(s);
            }
        }
    }

    fn vertex_buffer(&mut self, slot: u32, buffer: &ID3D11Buffer, stride: u32, offset: u32) {
        let key = Some((buffer.as_raw() as usize, stride, offset));
        let Some(bound) = self.vertex_buffers.get_mut(slot as usize) else {
            return;
        };
        if *bound != key {
            let buffers = [Some(buffer.clone())];
            // SAFETY: one buffer, one stride and one offset, live for the call.
            unsafe {
                self.device.context().IASetVertexBuffers(
                    slot,
                    1,
                    Some(buffers.as_ptr()),
                    Some(&raw const stride),
                    Some(&raw const offset),
                );
            }
            *bound = key;
        }
    }

    /// Binds index buffer `buffer` of the scene; `false` when there is none.
    fn index_buffer(&mut self, buffer: usize, wide: bool, offset: u32) -> bool {
        let Some(b) = self.scene.index_buffers.get(buffer) else {
            return false;
        };
        let key = Some((b.buffer().as_raw() as usize, wide, offset));
        if self.index_buffer != key {
            b.bind_indices(self.device, wide, offset);
            self.index_buffer = key;
        }
        true
    }

    /// Binds the geometry and draws; `false` when nothing was drawn.
    fn geometry(&mut self, draw: Draw) -> bool {
        let scene = self.scene;
        let ctx = self.device.context();
        match draw {
            Draw::Vertices { first, count } => {
                let Some(vb) = scene.vertices.as_ref().filter(|_| count > 0) else {
                    return false;
                };
                self.vertex_buffer(GEOMETRY_SLOT, vb.buffer(), IMAGE_VERTEX_STRIDE, 0);
                // SAFETY: the vertices lie in the bound region (checked at load time).
                unsafe { ctx.Draw(count, first) };
            }
            Draw::Indexed {
                first_vertex,
                first_index,
                count,
            } => {
                let Some(vb) = scene.vertices.as_ref().filter(|_| count > 0) else {
                    return false;
                };
                let Some(ib) = scene.main_index else {
                    return false;
                };
                self.vertex_buffer(GEOMETRY_SLOT, vb.buffer(), IMAGE_VERTEX_STRIDE, 0);
                if !self.index_buffer(ib, false, scene.puppet_index_offset) {
                    return false;
                }
                // SAFETY: the indices lie in the bound buffer (checked at load time); vertices
                // read past the region read zeros.
                unsafe { ctx.DrawIndexed(count, first_index, first_vertex as i32) };
            }
            Draw::Skinned {
                vertex_offset,
                skin_offset,
                first_index,
                count,
            } => {
                let Some(vb) = scene.vertices.as_ref().filter(|_| count > 0) else {
                    return false;
                };
                let Some(ib) = scene.main_index else {
                    return false;
                };
                self.vertex_buffer(
                    GEOMETRY_SLOT,
                    vb.buffer(),
                    IMAGE_VERTEX_STRIDE,
                    vertex_offset,
                );
                self.vertex_buffer(
                    SKIN_SLOT,
                    scene.static_vertices.buffer(),
                    SKIN_STRIDE,
                    skin_offset,
                );
                if !self.index_buffer(ib, false, scene.puppet_index_offset) {
                    return false;
                }
                // SAFETY: as for indexed geometry; the region and the records are bound at the
                // mesh's first vertex and record.
                unsafe { ctx.DrawIndexed(count, first_index, 0) };
            }
            Draw::Stream(s) => {
                let Some(stream) = scene.streams.get(s).filter(|s| s.index_count > 0) else {
                    return false;
                };
                self.vertex_buffer(GEOMETRY_SLOT, stream.buffer.buffer(), stream.plan.stride, 0);
                if !self.index_buffer(stream.index_buffer, true, stream.index_offset) {
                    return false;
                }
                // SAFETY: at most the stream's own indices, each below its vertex capacity.
                unsafe { ctx.DrawIndexed(stream.index_count, 0, 0) };
            }
            Draw::Mesh(m) => {
                let Some(mesh) = scene.meshes.get(m).filter(|m| m.index_count > 0) else {
                    return false;
                };
                let Some(ib) = scene.main_index else {
                    return false;
                };
                self.vertex_buffer(
                    GEOMETRY_SLOT,
                    scene.static_vertices.buffer(),
                    mesh.layout.stride,
                    mesh.vertex_offset,
                );
                if !self.index_buffer(ib, false, mesh.index_offset) {
                    return false;
                }
                // SAFETY: the mesh's indices, each below its vertex count (checked when added).
                unsafe { ctx.DrawIndexed(mesh.index_count, 0, 0) };
            }
        }
        true
    }
}

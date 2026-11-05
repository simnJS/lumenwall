//! Load time: targets, buffers, states, slot tables and input layouts of a plan, and the changes
//! after load (`extend`, `set_ops`).

use std::collections::HashMap;
use std::time::Duration;

use super::{
    DEFAULTS_SLOT, Draw, GEOMETRY_SLOT, LayoutKind, Pass, SKIN_SLOT, STATIC_ALIGN, Scene,
    SceneBuilder, ScenePlanExtension, SlotBind, Srv, Stream, Target, default_texture,
};
use crate::error::{Error, Result};
use crate::hlsl::{InputKind, MAX_SLOTS, VertexInput};
use crate::layout::{
    CONSTANT_WINDOW_ALIGN, ConstantLayout, Dimension, LinearArena, MAX_CONSTANT_WINDOW,
    VertexFormat,
};
use crate::plan::{
    AttributePlan, GeometryPlan, IMAGE_VERTEX_ATTRIBUTES, IMAGE_VERTEX_STRIDE, OpPlan, PassPlan,
    SKIN_ATTRIBUTES, SKIN_STRIDE, ScenePlan, StreamPlan, TargetPlan, TextureSource,
};
use crate::state::{DepthKey, RasterKey, SamplerKey};
use crate::win::buffer::{ConstantArena, DynamicBuffer, StaticBuffer, StaticBufferKind};
use crate::win::compile::{VertexElement, create_input_layout};
use crate::win::device::Device;
use crate::win::state::StateCache;
use crate::win::target::{RenderTarget, TargetDesc};

/// How long `build` waits for the uploads before trimming.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) fn build(b: SceneBuilder<'_>, plan: &ScenePlan) -> Result<Scene> {
    let device = b.device;
    let (textures, programs, meshes) = b.counts();
    plan.validate(textures, programs, meshes)?;
    let SceneBuilder {
        uploads,
        mut static_vertices,
        mut static_indices,
        meshes,
        ..
    } = b;
    let mut created = uploads.created;

    let mut targets = Vec::with_capacity(plan.targets.len());
    let mut textures_rt = Vec::new();
    created += add_targets(device, &mut targets, &mut textures_rt, &plan.targets)?;

    // Static vertex data: defaults and meshes (added by the builder), then the skin records.
    let skin_offset = static_vertices.push(&plan.skins, STATIC_ALIGN);
    let static_vb = StaticBuffer::new(device, StaticBufferKind::Vertex, static_vertices.bytes())?;
    created += 1;
    // Static index data: meshes, then puppet indices, then the streams' 32-bit indices.
    let puppet: Vec<u8> = plan.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    let puppet_index_offset = static_indices.push(&puppet, 4);
    let stream_offsets: Vec<u32> = plan
        .streams
        .iter()
        .map(|s| static_indices.push(&wide_indices(&s.indices), 4))
        .collect();
    let mut index_buffers = Vec::new();
    if !static_indices.is_empty() {
        index_buffers.push(StaticBuffer::new(
            device,
            StaticBufferKind::Index,
            static_indices.bytes(),
        )?);
        created += 1;
    }
    let arena = ConstantArena::new(device, &constant_layout(plan.constants_len)?)?;
    created += 1;
    let vertices = if plan.vertex_bytes > 0 {
        created += 1;
        Some(DynamicBuffer::new(device, plan.vertex_bytes, false)?)
    } else {
        None
    };
    let main_index = (!index_buffers.is_empty()).then_some(0);
    let mut streams = Vec::with_capacity(plan.streams.len());
    for (s, &offset) in plan.streams.iter().zip(&stream_offsets) {
        streams.push(new_stream(
            device,
            s,
            main_index.unwrap_or(usize::MAX),
            offset,
        )?);
        created += 1;
    }
    let default_2d = default_texture(device, Dimension::D2)?;
    created += 2;

    let n_targets = targets.len();
    let mut scene = Scene {
        textures: uploads.textures,
        programs: uploads.programs,
        meshes,
        targets,
        textures_rt,
        default_2d,
        default_3d: None,
        arena,
        constants_len: plan.constants_len,
        frame_block: plan.frame_block,
        vertex_bytes: plan.vertex_bytes,
        vertices,
        vertices_stale: true,
        static_vertices: static_vb,
        skin_offset,
        main_index,
        index_buffers,
        puppet_index_offset,
        streams,
        states: StateCache::new(),
        samplers: Vec::new(),
        sampler_index: HashMap::new(),
        layouts: Vec::new(),
        layout_index: HashMap::new(),
        passes: Vec::with_capacity(plan.passes.len()),
        ops: Vec::new(),
        depth_cleared: vec![false; n_targets],
        profile_queries: Vec::new(),
        created,
        cache_hits: uploads.cache_hits,
        cache_compiled: uploads.cache_compiled,
        last: super::FrameStats::default(),
    };
    for p in &plan.passes {
        let pass = compile_pass(&mut scene, device, p)?;
        scene.passes.push(pass);
    }
    check_ops(&scene, &plan.ops)?;
    scene.ops.clone_from(&plan.ops);
    // The uploads are done before the driver is asked to give back its staging memory
    // (tools/d3d11-probe: shared memory 2.9–3.2 → 1.2–2.0 MiB, loading peak 13–16 → 0.8 MiB).
    if !device.wait_idle(IDLE_TIMEOUT)? {
        tracing::warn!("scene build: GPU not idle after {IDLE_TIMEOUT:?}; trimming anyway");
    }
    device.trim()?;
    Ok(scene)
}

/// A constant layout of `len` bytes (one arena: windows of at most 64 KiB are bound in it, the
/// buffer itself may be larger with Direct3D 11.1).
fn constant_layout(len: u32) -> Result<ConstantLayout> {
    let mut layout = ConstantLayout::new();
    let mut left = len.next_multiple_of(CONSTANT_WINDOW_ALIGN);
    while left > 0 {
        let n = left.min(MAX_CONSTANT_WINDOW);
        layout.alloc(n)?;
        left -= n;
    }
    Ok(layout)
}

fn wide_indices(indices: &[u32]) -> Vec<u8> {
    indices.iter().flat_map(|i| i.to_le_bytes()).collect()
}

fn new_stream(
    device: &Device,
    plan: &StreamPlan,
    index_buffer: usize,
    index_offset: u32,
) -> Result<Stream> {
    let bytes = plan.max_vertices.checked_mul(plan.stride).ok_or_else(|| {
        Error::InvalidData(format!(
            "stream of {} vertices of {} bytes",
            plan.max_vertices, plan.stride
        ))
    })?;
    Ok(Stream {
        plan: StreamPlan {
            // Uploaded: no CPU copy kept.
            indices: Vec::new(),
            ..plan.clone()
        },
        buffer: DynamicBuffer::new(device, bytes, false)?,
        index_buffer,
        index_offset,
        max_indices: plan.indices.len() as u32,
        index_count: 0,
    })
}

/// The highest sample count at most `requested` that the device supports for `format`.
fn supported_samples(device: &Device, plan: &TargetPlan) -> u32 {
    let mut s = plan.samples.max(1);
    while s > 1 {
        if device.msaa_quality_levels(plan.format.dxgi(), s) > 0 {
            return s;
        }
        s /= 2;
    }
    1
}

/// Direct3D objects of a render target (texture, SRV, RTV; MSAA texture and RTV; depth texture
/// and DSV).
fn target_objects(desc: TargetDesc) -> u64 {
    3 + if desc.samples > 1 { 2 } else { 0 } + if desc.depth { 2 } else { 0 }
}

/// Appends `new` (indices continue those of `targets`): owners get a texture, sharers draw
/// into their owner's. Returns the objects created.
fn add_targets(
    device: &Device,
    targets: &mut Vec<Target>,
    textures_rt: &mut Vec<RenderTarget>,
    new: &[TargetPlan],
) -> Result<u64> {
    let base = targets.len();
    let mut created = 0;
    for plan in new {
        let physical = if plan.shares.is_none() {
            let samples = supported_samples(device, plan);
            if samples != plan.samples.max(1) {
                tracing::warn!(
                    "{}: {}× MSAA unsupported for {:?}, using {samples}×",
                    plan.label,
                    plan.samples,
                    plan.format
                );
            }
            let desc = TargetDesc {
                size: plan.size,
                format: plan.format,
                samples,
                depth: plan.depth,
            };
            textures_rt.push(RenderTarget::new(device, desc)?);
            created += target_objects(desc);
            Some(textures_rt.len() - 1)
        } else {
            None
        };
        targets.push(Target {
            plan: plan.clone(),
            physical: physical.unwrap_or(usize::MAX),
            samples: 1,
        });
    }
    // Sharers, once every owner has its texture (an owner may come after its sharers).
    for i in base..targets.len() {
        let owner = targets[i].plan.shares.unwrap_or(i);
        let physical = targets
            .get(owner)
            .map(|t| t.physical)
            .filter(|&p| p < textures_rt.len())
            .ok_or_else(|| {
                Error::InvalidData(format!("target {i} shares {owner}, which has no texture"))
            })?;
        targets[i].physical = physical;
        targets[i].samples = textures_rt[physical].desc().samples;
    }
    Ok(created)
}

/// A copy (op or hazard) must land in a single-sampled texture of the same format: a copy into a
/// multisampled target would leave its multisampled texture stale (on wgpu such a target stays
/// single-sampled).
fn check_copy(scene: &Scene, src: usize, dst: usize, what: &str) -> Result<()> {
    let (Some(s), Some(d)) = (scene.targets.get(src), scene.targets.get(dst)) else {
        return Err(Error::InvalidData(format!("{what}: target {src} or {dst}")));
    };
    if d.samples > 1 {
        return Err(Error::InvalidData(format!(
            "{what}: copy into the multisampled target `{}`",
            d.plan.label
        )));
    }
    if s.plan.format != d.plan.format {
        return Err(Error::InvalidData(format!(
            "{what}: copy from {:?} (`{}`) into {:?} (`{}`)",
            s.plan.format, s.plan.label, d.plan.format, d.plan.label
        )));
    }
    Ok(())
}

fn check_ops(scene: &Scene, ops: &[OpPlan]) -> Result<()> {
    let n = scene.targets.len();
    for (k, op) in ops.iter().enumerate() {
        let ok = match *op {
            OpPlan::Clear { target, .. } => target < n,
            OpPlan::Copy { src, dst } => {
                if src < n && dst < n && src != dst {
                    check_copy(scene, src, dst, &format!("op {k}"))?;
                    true
                } else {
                    false
                }
            }
            OpPlan::Draw(p) => p < scene.passes.len(),
        };
        if !ok {
            return Err(Error::InvalidData(format!("scene op {k}: {op:?}")));
        }
    }
    Ok(())
}

/// Format of an attribute fed with `components` values of `kind`.
fn input_format(kind: InputKind, components: u32) -> VertexFormat {
    match (kind, components) {
        (InputKind::Float, 1) => VertexFormat::Float32,
        (InputKind::Float, 2) => VertexFormat::Float32x2,
        (InputKind::Float, 3) => VertexFormat::Float32x3,
        (InputKind::Float, _) => VertexFormat::Float32x4,
        (InputKind::Sint, 1) => VertexFormat::Sint32,
        (InputKind::Sint, 2) => VertexFormat::Sint32x2,
        (InputKind::Sint, 3) => VertexFormat::Sint32x3,
        (InputKind::Sint, _) => VertexFormat::Sint32x4,
        (InputKind::Uint, 1) => VertexFormat::Uint32,
        (InputKind::Uint, 2) => VertexFormat::Uint32x2,
        (InputKind::Uint, 3) => VertexFormat::Uint32x3,
        (InputKind::Uint, _) => VertexFormat::Uint32x4,
    }
}

/// `(offset, format)` of a skinned input read from the skin records, as `lumenwall-render`
/// feeds them: integer `a_BlendIndices`, float `a_BlendWeights` / `a_Normal` / `a_Tangent4`.
/// An input of the other kind is not fed (it reads the defaults).
fn skin_input(input: &VertexInput) -> Option<(u32, VertexFormat)> {
    let &(_, offset) = SKIN_ATTRIBUTES.iter().find(|(n, _)| *n == input.name)?;
    let integer = input.kind != InputKind::Float;
    if integer != (offset == 0) {
        return None;
    }
    Some((offset, input_format(input.kind, input.components)))
}

/// What feeds a layout: the image vertex region (and the skin records), or a stream or mesh
/// layout.
enum Attributes<'a> {
    Image { skinned: bool },
    Custom(&'a [AttributePlan]),
}

/// Elements of the inputs the geometry feeds, matched by name.
fn fed_elements(inputs: &[VertexInput], attributes: &Attributes<'_>) -> Vec<VertexElement> {
    let mut fed = Vec::with_capacity(inputs.len());
    for input in inputs {
        match attributes {
            Attributes::Image { skinned } => {
                if *skinned && let Some((offset, format)) = skin_input(input) {
                    fed.push(VertexElement {
                        location: input.location,
                        format,
                        offset,
                        slot: SKIN_SLOT,
                    });
                    continue;
                }
                if let Some(&(_, format, offset)) =
                    IMAGE_VERTEX_ATTRIBUTES.iter().find(|a| a.0 == input.name)
                {
                    fed.push(VertexElement {
                        location: input.location,
                        format,
                        offset,
                        slot: GEOMETRY_SLOT,
                    });
                }
            }
            Attributes::Custom(list) => {
                if let Some(a) = list.iter().find(|a| a.name == input.name) {
                    fed.push(VertexElement {
                        location: input.location,
                        format: a.format,
                        offset: a.offset,
                        slot: GEOMETRY_SLOT,
                    });
                }
            }
        }
    }
    fed
}

/// The input layout of `program` for `kind` (one per program and geometry layout).
fn layout_for(
    scene: &mut Scene,
    device: &Device,
    program: usize,
    kind: LayoutKind,
) -> Result<usize> {
    if let Some(&i) = scene.layout_index.get(&(program, kind)) {
        return Ok(i);
    }
    let shader = scene
        .programs
        .get(program)
        .ok_or_else(|| Error::InvalidData(format!("program {program}")))?;
    let attributes = match kind {
        LayoutKind::Image => Attributes::Image { skinned: false },
        LayoutKind::Skinned => Attributes::Image { skinned: true },
        LayoutKind::Stream(s) => Attributes::Custom(
            scene
                .streams
                .get(s)
                .map(|s| &s.plan.attributes[..])
                .unwrap_or_default(),
        ),
        LayoutKind::Mesh(m) => Attributes::Custom(
            scene
                .meshes
                .get(m)
                .map(|m| &m.layout.attributes[..])
                .unwrap_or_default(),
        ),
    };
    let fed = fed_elements(shader.inputs(), &attributes);
    let elements = shader.complete_elements(&fed, DEFAULTS_SLOT);
    let layout = create_input_layout(device, shader, &elements)?;
    if layout.is_some() {
        scene.created += 1;
    }
    scene.layouts.push(layout);
    let i = scene.layouts.len() - 1;
    scene.layout_index.insert((program, kind), i);
    Ok(i)
}

fn sampler_for(scene: &mut Scene, device: &Device, key: SamplerKey) -> Result<usize> {
    if let Some(&i) = scene.sampler_index.get(&key) {
        return Ok(i);
    }
    let s = scene.states.sampler(device, key)?;
    scene.created += 1;
    scene.samplers.push(s);
    let i = scene.samplers.len() - 1;
    scene.sampler_index.insert(key, i);
    Ok(i)
}

/// The source of slot `n` in variant `v`.
fn source_of(p: &PassPlan, n: usize, v: usize) -> TextureSource {
    if n == 0
        && let Some(&s) = p.slot0_variants.get(v)
    {
        return s;
    }
    p.slots
        .get(n)
        .copied()
        .flatten()
        .map_or(TextureSource::Default2D, |s| s.source)
}

/// Compiles one pass: states, window checks, slot tables, hazard, input layout and geometry.
fn compile_pass(scene: &mut Scene, device: &Device, p: &PassPlan) -> Result<Pass> {
    let err = |m: String| Error::InvalidData(format!("pass `{}`: {m}", p.label));
    let res = scene
        .programs
        .get(p.program)
        .ok_or_else(|| err(format!("program {}", p.program)))?
        .resources()
        .clone();
    let target = scene
        .targets
        .get(p.target)
        .ok_or_else(|| err(format!("target {}", p.target)))?;
    let (has_depth, samples, physical) = (target.plan.depth, target.samples, target.physical);

    // The windows the program reads must hold its blocks.
    if let Some(size) = res.uniform_block
        && !p.uniform.is_some_and(|w| w.size >= size)
    {
        return Err(err(format!(
            "its program reads a {size}-byte block (b0), the pass window is {:?}",
            p.uniform
        )));
    }
    if let Some(size) = res.frame_block
        && !scene.frame_block.is_some_and(|w| w.size >= size)
    {
        return Err(err(format!(
            "its program reads a {size}-byte frame block (b1), the scene's is {:?}",
            scene.frame_block
        )));
    }

    // States. Depth clip and multisampling follow the target.
    let before = scene.states.counts();
    let blend = scene.states.blend(device, p.blend)?;
    let raster = scene.states.raster(
        device,
        RasterKey {
            depth_clip: has_depth,
            multisample: samples > 1,
            ..p.raster
        },
    )?;
    let depth = scene.states.depth(
        device,
        if has_depth {
            p.depth
        } else {
            DepthKey::default()
        },
    )?;
    let after = scene.states.counts();
    scene.created += ((after.1 - before.1) + (after.2 - before.2) + (after.3 - before.3)) as u64;

    // Slots. Only the slots the program declares are bound; a pass sampling its own target
    // reads the hazard scratch instead (wgpu path: only for the slots the shader samples).
    let variants = p.slot0_variants.len().max(1);
    let sampled = |n: usize| res.textures & (1 << n) != 0;
    let sampled_targets = || {
        (0..MAX_SLOTS as usize)
            .filter(|&n| sampled(n))
            .flat_map(|n| (0..if n == 0 { variants } else { 1 }).map(move |v| (n, v)))
            .filter_map(|(n, v)| match source_of(p, n, v) {
                TextureSource::Target(t) if res.volumes & (1 << n) == 0 => Some(t),
                _ => None,
            })
    };
    for t in sampled_targets() {
        if t != p.target && scene.targets.get(t).is_some_and(|t| t.physical == physical) {
            return Err(err(format!(
                "samples target {t}, which shares the texture it draws into"
            )));
        }
    }
    let hazard = if sampled_targets().any(|t| t == p.target) {
        let scratch = p
            .hazard_scratch
            .ok_or_else(|| err("samples its own target without a hazard scratch".into()))?;
        if scene
            .targets
            .get(scratch)
            .is_some_and(|s| s.physical == physical)
        {
            return Err(err("its hazard scratch shares its target's texture".into()));
        }
        check_copy(scene, p.target, scratch, &format!("pass `{}`", p.label))?;
        Some(scratch)
    } else {
        None
    };
    if res.volumes != 0 {
        scene.ensure_default_3d(device)?;
    }
    let mut tables = Vec::with_capacity(variants);
    for v in 0..variants {
        let mut table = [SlotBind::default(); MAX_SLOTS as usize];
        for (n, slot) in table.iter_mut().enumerate() {
            let volume = res.volumes & (1 << n) != 0;
            if sampled(n) {
                let default = if volume {
                    Srv::Default3D
                } else {
                    Srv::Default2D
                };
                slot.srv = Some(match source_of(p, n, v) {
                    // A slot only takes a texture of its dimension; anything else reads the
                    // default of the right one (as on the wgpu path).
                    TextureSource::Texture(i) => match scene.textures.get(i) {
                        Some(t) if (t.dimension() == Dimension::D3) == volume => Srv::Texture(i),
                        Some(_) => default,
                        None => return Err(err(format!("texture {i}"))),
                    },
                    TextureSource::Target(_) if volume => default,
                    TextureSource::Target(t) if t == p.target => match hazard {
                        Some(s) => Srv::Target(s),
                        None => Srv::Target(t),
                    },
                    TextureSource::Target(t) => Srv::Target(t),
                    TextureSource::Default2D | TextureSource::Default3D => default,
                });
            }
            if res.samplers & (1 << n) != 0 {
                let key = p
                    .slots
                    .get(n)
                    .copied()
                    .flatten()
                    .map_or(SamplerKey::CLAMP_LINEAR, |s| s.sampler);
                slot.sampler = Some(sampler_for(scene, device, key)?);
            }
        }
        tables.push(table);
    }

    // Geometry and its input layout.
    let (kind, draw) = match p.geometry {
        GeometryPlan::Vertices { first, count } => {
            (LayoutKind::Image, Draw::Vertices { first, count })
        }
        GeometryPlan::Indexed {
            first_vertex,
            first_index,
            index_count,
        } => (
            LayoutKind::Image,
            Draw::Indexed {
                first_vertex,
                first_index,
                count: index_count,
            },
        ),
        GeometryPlan::Skinned {
            first_vertex,
            first_index,
            index_count,
            first_skin,
        } => {
            let offsets = first_vertex.checked_mul(IMAGE_VERTEX_STRIDE).zip(
                first_skin
                    .checked_mul(SKIN_STRIDE)
                    .and_then(|o| o.checked_add(scene.skin_offset)),
            );
            let (vertex_offset, skin_offset) =
                offsets.ok_or_else(|| err(format!("geometry {:?}", p.geometry)))?;
            (
                LayoutKind::Skinned,
                Draw::Skinned {
                    vertex_offset,
                    skin_offset,
                    first_index,
                    count: index_count,
                },
            )
        }
        GeometryPlan::Stream(s) => {
            let plan = &scene
                .streams
                .get(s)
                .ok_or_else(|| err(format!("stream {s}")))?
                .plan;
            let first = scene
                .streams
                .iter()
                .position(|o| o.plan.stride == plan.stride && o.plan.attributes == plan.attributes)
                .unwrap_or(s);
            (LayoutKind::Stream(first), Draw::Stream(s))
        }
        GeometryPlan::Mesh(m) => {
            let layout = &scene
                .meshes
                .get(m)
                .ok_or_else(|| err(format!("mesh {m}")))?
                .layout;
            let first = scene
                .meshes
                .iter()
                .position(|o| o.layout == *layout)
                .unwrap_or(m);
            (LayoutKind::Mesh(first), Draw::Mesh(m))
        }
    };
    let layout = layout_for(scene, device, p.program, kind)?;
    Ok(Pass {
        label: p.label.clone(),
        program: p.program,
        target: p.target,
        blend,
        raster,
        depth,
        layout,
        uniform: p.uniform,
        slots: res.textures | res.samplers,
        tables,
        draw,
        hazard,
        clear_first: p.clear_first,
    })
}

pub(super) fn extend(scene: &mut Scene, device: &Device, ext: ScenePlanExtension) -> Result<()> {
    if let Some(p) = ext.passes.iter().find(|p| {
        matches!(
            p.geometry,
            GeometryPlan::Indexed { .. } | GeometryPlan::Skinned { .. }
        )
    }) {
        return Err(Error::Unsupported(format!(
            "pass `{}`: indexed or skinned geometry added after load",
            p.label
        )));
    }
    if ext.constants_len < scene.constants_len || ext.vertex_bytes < scene.vertex_bytes {
        return Err(Error::InvalidData(format!(
            "extension shrinks the shadow ({} → {}) or the vertex region ({} → {})",
            scene.constants_len, ext.constants_len, scene.vertex_bytes, ext.vertex_bytes
        )));
    }
    // The whole scene as a plan with the new passes only: every index and window is checked by
    // the contract's own validation.
    let check = ScenePlan {
        targets: scene
            .targets
            .iter()
            .map(|t| t.plan.clone())
            .chain(ext.targets.iter().cloned())
            .collect(),
        passes: ext.passes.clone(),
        ops: Vec::new(),
        constants_len: ext.constants_len,
        frame_block: scene.frame_block,
        vertex_bytes: ext.vertex_bytes,
        indices: Vec::new(),
        skins: Vec::new(),
        streams: scene
            .streams
            .iter()
            .map(|s| s.plan.clone())
            .chain(ext.streams.iter().cloned())
            .collect(),
    };
    check.validate(
        scene.textures.len(),
        scene.programs.len(),
        scene.meshes.len(),
    )?;

    let (old_targets, old_rt, old_streams, old_indices) = (
        scene.targets.len(),
        scene.textures_rt.len(),
        scene.streams.len(),
        scene.index_buffers.len(),
    );
    let (old_len, old_vertex_bytes) = (scene.constants_len, scene.vertex_bytes);
    let r = grow(scene, device, &ext);
    if r.is_err() {
        // Back to the scene as it was (larger buffers are kept: they hold the old sizes too).
        scene.targets.truncate(old_targets);
        scene.textures_rt.truncate(old_rt);
        scene.streams.truncate(old_streams);
        scene.index_buffers.truncate(old_indices);
        scene.constants_len = old_len;
        scene.vertex_bytes = old_vertex_bytes;
        scene.depth_cleared.resize(old_targets, false);
        // Layouts made for a removed stream must not serve a later stream of that index.
        scene
            .layout_index
            .retain(|&(_, k), _| !matches!(k, LayoutKind::Stream(s) if s >= old_streams));
    }
    r
}

/// The creations of [`extend`]; on error the caller restores the scene.
fn grow(scene: &mut Scene, device: &Device, ext: &ScenePlanExtension) -> Result<()> {
    scene.created += add_targets(
        device,
        &mut scene.targets,
        &mut scene.textures_rt,
        &ext.targets,
    )?;
    scene.depth_cleared.resize(scene.targets.len(), false);
    if !ext.streams.is_empty() {
        let mut indices = LinearArena::new();
        let offsets: Vec<u32> = ext
            .streams
            .iter()
            .map(|s| indices.push(&wide_indices(&s.indices), 4))
            .collect();
        let buffer = scene.index_buffers.len();
        if !indices.is_empty() {
            scene.index_buffers.push(StaticBuffer::new(
                device,
                StaticBufferKind::Index,
                indices.bytes(),
            )?);
            scene.created += 1;
        }
        for (s, &offset) in ext.streams.iter().zip(&offsets) {
            scene.streams.push(new_stream(device, s, buffer, offset)?);
            scene.created += 1;
        }
    }
    if ext.constants_len as usize > scene.arena.len() {
        scene.arena = ConstantArena::new(device, &constant_layout(ext.constants_len)?)?;
        scene.created += 1;
    }
    scene.constants_len = ext.constants_len;
    if ext.vertex_bytes > scene.vertices.as_ref().map_or(0, DynamicBuffer::capacity) {
        scene.vertices = Some(DynamicBuffer::new(device, ext.vertex_bytes, false)?);
        scene.created += 1;
        scene.vertices_stale = true;
    }
    scene.vertex_bytes = ext.vertex_bytes;
    let first = scene.passes.len();
    let mut compiled = Vec::with_capacity(ext.passes.len());
    for p in &ext.passes {
        match compile_pass(scene, device, p) {
            Ok(pass) => compiled.push(pass),
            Err(e) => {
                scene.passes.truncate(first);
                return Err(e);
            }
        }
    }
    scene.passes.extend(compiled);
    Ok(())
}

pub(super) fn set_ops(scene: &mut Scene, device: &Device, ops: Vec<OpPlan>) -> Result<()> {
    check_ops(scene, &ops)?;
    if scene.targets.iter().any(|t| t.plan.shares.is_some()) {
        unshare(scene, device)?;
    }
    scene.ops = ops;
    Ok(())
}

/// Gives every target that shares a texture its own (lifetimes planned on another order may
/// overlap now), as `lumenwall-render` does when a scene with aliased targets is reordered.
fn unshare(scene: &mut Scene, device: &Device) -> Result<()> {
    for i in 0..scene.targets.len() {
        if scene.targets[i].plan.shares.is_none() {
            continue;
        }
        let desc = scene
            .textures_rt
            .get(scene.targets[i].physical)
            .map(RenderTarget::desc)
            .ok_or_else(|| Error::InvalidData(format!("target {i} has no texture")))?;
        scene.textures_rt.push(RenderTarget::new(device, desc)?);
        scene.created += target_objects(desc);
        let t = &mut scene.targets[i];
        t.physical = scene.textures_rt.len() - 1;
        t.plan.shares = None;
    }
    for p in &mut scene.passes {
        p.clear_first = false;
    }
    Ok(())
}

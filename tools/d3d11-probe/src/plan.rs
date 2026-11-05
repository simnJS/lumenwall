//! The workload: what one frame of a scene draws, as plain data both backends execute.
//!
//! It is derived on the CPU from the real wallpaper, the way `lumenwall-engine`'s `prepare_image`
//! does it (image sizes, composites A/B, effect `fbos`, texture slots, targets), without the
//! renderer: textures are the wallpaper's real `.tex` files, shaders the real translated programs.
//! It is not pixel exact (no puppets, particles, text, 3D models, bloom, scripts), it has the same
//! kinds of resources and roughly the same GPU work.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use lumenwall_assets::{Vfs, WallpaperVfsOptions};
use lumenwall_formats::tex::{GpuContent, GpuFormat, UploadOptions};
use lumenwall_scene::{
    BuildOptions, ImagePasses, MainTextureFormat, ObjectIndex, ObjectKind, PassKind, Project,
    TextureRef, load_project,
};
use lumenwall_shader::{CompileOptions, PassCombos, PassRequest, PassShader, ShaderCompiler};

/// Output window of the measurements (Nora's method: 2560×1440, off screen).
pub const OUTPUT: (u32, u32) = (2560, 1440);

/// A `.tex` of the wallpaper, decoded: every image of the file is a GPU texture.
pub struct TexAsset {
    pub name: String,
    pub format: GpuFormat,
    /// `images[i][level] = (width, height, bytes)`; emptied once uploaded.
    pub images: Vec<Vec<(u32, u32, Vec<u8>)>>,
    pub width: u32,
    pub height: u32,
    pub mips: u32,
}

impl TexAsset {
    pub fn bytes(&self) -> u64 {
        self.images
            .iter()
            .flatten()
            .map(|(_, _, d)| d.len() as u64)
            .sum()
    }
}

/// Where a texture slot reads from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    Tex(usize),
    Target(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetFormat {
    Rgba8,
    R8,
    Rg8,
}

pub struct Target {
    pub label: String,
    pub width: u32,
    pub height: u32,
    pub format: TargetFormat,
}

pub struct Draw {
    pub program: usize,
    pub slots: Vec<(u32, Src)>,
    /// Index in [`Workload::targets`].
    pub target: usize,
    pub label: String,
    /// Part of the target the quad covers, per axis (images smaller than the scene).
    pub coverage: (f32, f32),
}

pub struct Workload {
    pub wallpaper: String,
    pub textures: Vec<TexAsset>,
    pub targets: Vec<Target>,
    pub programs: Vec<PassShader>,
    pub program_names: Vec<String>,
    pub draws: Vec<Draw>,
    /// Program of the final copy to the swap chain (`commands/copy`).
    pub present_program: usize,
    pub load_ms: f64,
    /// Time in `lumenwall-shader` (preprocess, GLSL front-end, naga), summed over the passes.
    pub translate_ms: f64,
    /// `(shader, error)` of every pass whose shader did not translate.
    pub shader_errors: Vec<(String, String)>,
    /// Textures that did not load (the slots that name them stay empty).
    pub texture_errors: Vec<String>,
}

impl Workload {
    /// Runs of consecutive draws with the same target (one render pass each).
    pub fn render_passes(&self) -> usize {
        let mut n = 0;
        let mut last = usize::MAX;
        for d in &self.draws {
            if d.target != last {
                n += 1;
                last = d.target;
            }
        }
        n
    }

    pub fn texture_bytes(&self) -> u64 {
        self.textures.iter().map(TexAsset::bytes).sum()
    }

    pub fn target_bytes(&self) -> u64 {
        self.targets
            .iter()
            .map(|t| {
                let bpp = match t.format {
                    TargetFormat::Rgba8 => 4,
                    TargetFormat::Rg8 => 2,
                    TargetFormat::R8 => 1,
                };
                u64::from(t.width) * u64::from(t.height) * bpp
            })
            .sum()
    }
}

pub struct VfsAssets<'a>(pub &'a Vfs);

impl lumenwall_scene::AssetSource for VfsAssets<'_> {
    fn read(&self, path: &str) -> Result<Cow<'_, [u8]>, lumenwall_scene::AssetError> {
        self.0.read(path).map_err(|e| {
            if e.is_not_found() {
                lumenwall_scene::AssetError::NotFound
            } else {
                lumenwall_scene::AssetError::Io(std::io::Error::other(e.to_string()))
            }
        })
    }

    fn exists(&self, path: &str) -> bool {
        self.0.exists(path)
    }
}

pub struct VfsShaders<'a>(pub &'a Vfs);

impl lumenwall_shader::ShaderFs for VfsShaders<'_> {
    fn read(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        self.0.read(path).ok()
    }

    fn identity(&self) -> Option<u64> {
        Some(self.0.generation())
    }
}

pub fn open_vfs(wallpaper: &Path, assets: &Path) -> Result<Vfs> {
    let options = WallpaperVfsOptions {
        mount_current_dir: false,
        ..WallpaperVfsOptions::default()
    };
    Vfs::for_wallpaper(wallpaper, assets, &options)
        .map_err(|e| anyhow!("cannot open {}: {e}", wallpaper.display()))
}

fn trunc(v: f32) -> u32 {
    (v as u32).max(1)
}

fn scaled(size: (f32, f32), divisor: f32, scale: f32) -> (u32, u32) {
    (
        trunc(size.0 / divisor * scale),
        trunc(size.1 / divisor * scale),
    )
}

fn target_format(name: &str) -> TargetFormat {
    match name.to_ascii_lowercase().as_str() {
        "r8" => TargetFormat::R8,
        "rg88" => TargetFormat::Rg8,
        _ => TargetFormat::Rgba8,
    }
}

fn load_texture(vfs: &Vfs, name: &str) -> Result<TexAsset> {
    let files = vfs
        .read_texture(name)
        .map_err(|e| anyhow!("texture {name}: {e}"))?;
    let tex = files.parse().map_err(|e| anyhow!("texture {name}: {e}"))?;
    let options = UploadOptions::default();
    let mut prepared = tex
        .into_upload(&options)
        .map_err(|e| anyhow!("texture {name}: {e}"))?;
    if prepared.requires_cpu_decode(true) {
        prepared
            .decode_compressed(options.max_level_bytes)
            .map_err(|e| anyhow!("texture {name}: {e}"))?;
    }
    let GpuContent::Images {
        format,
        dimension,
        images,
    } = &prepared.content
    else {
        return Err(anyhow!("texture {name}: video texture"));
    };
    if !matches!(dimension, lumenwall_formats::tex::TextureDimension::D2) {
        return Err(anyhow!("texture {name}: volume texture"));
    }
    let images: Vec<Vec<(u32, u32, Vec<u8>)>> = images
        .iter()
        .map(|img| {
            img.mips
                .iter()
                .map(|m| (m.width, m.height, m.data.to_vec()))
                .collect()
        })
        .collect();
    let (width, height) = images
        .first()
        .and_then(|i| i.first())
        .map_or((1, 1), |m| (m.0, m.1));
    let mips = images.first().map_or(1, |i| i.len() as u32);
    Ok(TexAsset {
        name: name.to_owned(),
        format: *format,
        images,
        width,
        height,
        mips,
    })
}

/// Per-image state while the chain is laid out.
struct ImageScope {
    named: HashMap<String, usize>,
}

pub struct PlanOptions {
    pub shader_cache: Option<std::path::PathBuf>,
}

/// Builds the workload of a wallpaper folder. Textures and shaders that fail to load are skipped
/// and counted in `skipped` (printed by the caller).
pub fn build(
    wallpaper: &Path,
    assets: &Path,
    options: &PlanOptions,
    log: &mut dyn FnMut(String),
) -> Result<Workload> {
    let t0 = std::time::Instant::now();
    let vfs = open_vfs(wallpaper, assets)?;
    let src = VfsAssets(&vfs);
    let mut project: Project = load_project(&src, Default::default())
        .map_err(|e| anyhow!("project.json of {}: {e}", wallpaper.display()))?;
    let build = project
        .build(&src, &BuildOptions::new(OUTPUT.0, OUTPUT.1))
        .map_err(|e| anyhow!("scene build: {e}"))?;
    let scene_size = (build.size.x, build.size.y);
    // The scene target is rendered at the scene's size, never above the output's
    // (`render_scale` of lumenwall-engine: displayed pixels per scene pixel, at most 1).
    let chain_scale = (OUTPUT.0 as f32 / scene_size.0)
        .max(OUTPUT.1 as f32 / scene_size.1)
        .min(1.0);
    let rendered = scaled(scene_size, 1.0, chain_scale);

    let compiler = ShaderCompiler::new(CompileOptions::default(), options.shader_cache.clone());
    let shader_fs = VfsShaders(&vfs);

    let mut w = Workload {
        wallpaper: wallpaper
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        textures: Vec::new(),
        targets: vec![Target {
            label: "_rt_FullFrameBuffer".into(),
            width: rendered.0,
            height: rendered.1,
            format: TargetFormat::Rgba8,
        }],
        programs: Vec::new(),
        program_names: Vec::new(),
        draws: Vec::new(),
        present_program: 0,
        load_ms: 0.0,
        translate_ms: 0.0,
        shader_errors: Vec::new(),
        texture_errors: Vec::new(),
    };
    let mut tex_index: HashMap<String, Option<usize>> = HashMap::new();
    let mut program_index: HashMap<[u8; 32], usize> = HashMap::new();
    let mut globals: HashMap<String, usize> = HashMap::new();
    globals.insert("_rt_FullFrameBuffer".into(), 0);
    for (name, div) in [("_rt_4FrameBuffer", 4.0), ("_rt_8FrameBuffer", 8.0)] {
        w.targets.push(Target {
            label: name.into(),
            width: trunc(rendered.0 as f32 / div),
            height: trunc(rendered.1 as f32 / div),
            format: TargetFormat::Rgba8,
        });
        globals.insert(name.into(), w.targets.len() - 1);
    }
    for rt in &build.render_targets {
        let name = project.strings.resolve(rt.name).to_owned();
        w.targets.push(Target {
            label: name.clone(),
            width: rt.width.max(1),
            height: rt.height.max(1),
            format: TargetFormat::Rgba8,
        });
        globals.insert(name, w.targets.len() - 1);
    }
    let mut skipped_textures = 0usize;
    let mut skipped_shaders = 0usize;

    let mut texture = |w: &mut Workload, name: &str, skipped: &mut usize| -> Option<usize> {
        if let Some(i) = tex_index.get(name) {
            return *i;
        }
        let r = match load_texture(&vfs, name) {
            Ok(t) => {
                w.textures.push(t);
                Some(w.textures.len() - 1)
            }
            Err(e) => {
                w.texture_errors.push(e.to_string());
                *skipped += 1;
                None
            }
        };
        tex_index.insert(name.to_owned(), r);
        r
    };

    let order: Vec<ObjectIndex> = build.render_order.clone();
    for idx in order {
        let Some(passes) = build.image_passes.get(idx.index()).cloned().flatten() else {
            continue;
        };
        if !passes.draws || passes.passes.is_empty() {
            continue;
        }
        let Some(scene) = project.scene.as_ref() else {
            continue;
        };
        let obj = scene.object(idx);
        let ObjectKind::Image(_) = &obj.kind else {
            continue;
        };
        let Some(img) = obj.as_layer() else { continue };
        let id = i64::from(obj.id);
        let Some(model_id) = img.model else { continue };
        let model = scene.model(model_id);
        let (fullscreen, solid) = (model.fullscreen, model.solid_layer);
        let model_size = match (model.width, model.height) {
            (Some(a), Some(b)) if a > 0 && b > 0 => Some((a as f32, b as f32)),
            _ => None,
        };
        let effects: Vec<_> = img.effects.iter().map(|e| e.effect).collect();
        let mut size = (img.size.x, img.size.y);

        // Main texture.
        let main_ref = project.main_texture(idx);
        let mut main_src: Option<Src> = None;
        let mut main_name: Option<String> = None;
        let mut main_format = MainTextureFormat::Other;
        let mut main_dims: Option<(u32, u32)> = None;
        if let Some(TextureRef::File(sym)) = main_ref {
            let name = project.strings.resolve(sym).to_owned();
            if let Some(i) = texture(&mut w, &name, &mut skipped_textures) {
                main_src = Some(Src::Tex(i));
                main_dims = Some((w.textures[i].width, w.textures[i].height));
                main_name = Some(name);
                main_format = match w.textures[i].format {
                    GpuFormat::Rg8Unorm => MainTextureFormat::Rg88,
                    GpuFormat::R8Unorm => MainTextureFormat::R8,
                    _ => MainTextureFormat::Other,
                };
            }
        } else if solid && size == (0.0, 0.0) {
            size = scene_size;
        }
        if size.0 == 0.0 || size.1 == 0.0 {
            size = model_size.unwrap_or(main_dims.map_or((0.0, 0.0), |d| (d.0 as f32, d.1 as f32)));
        }
        if fullscreen {
            size = scene_size;
        }
        if size.0 <= 0.0 || size.1 <= 0.0 {
            continue;
        }
        let world_scale = project.world_transform(idx, &build).scale;
        let magnify = world_scale.x.abs().max(world_scale.y.abs()).max(1.0);
        let image_scale = (chain_scale * magnify).min(1.0);

        // Composites A and B at the image size (the engine makes unused ones 1×1; every chain
        // here uses both when it has an intermediate pass).
        let (name_a, name_b) = (
            format!("_rt_imageLayerComposite_{id}_a"),
            format!("_rt_imageLayerComposite_{id}_b"),
        );
        let uses_chain = passes.passes.len() > 1;
        let comp_size = if uses_chain {
            scaled(size, 1.0, image_scale)
        } else {
            (1, 1)
        };
        let mut image = ImageScope {
            named: HashMap::new(),
        };
        for n in [&name_a, &name_b] {
            w.targets.push(Target {
                label: n.clone(),
                width: comp_size.0,
                height: comp_size.1,
                format: TargetFormat::Rgba8,
            });
            image.named.insert(n.clone(), w.targets.len() - 1);
        }
        let (comp_a, comp_b) = (image.named[&name_a], image.named[&name_b]);
        let mut effect_fbos: HashMap<u32, HashMap<String, usize>> = HashMap::new();

        let mut prev: Option<Src> = main_src;
        let mut ping = comp_a;
        let n_passes = passes.passes.len();
        for k in 0..n_passes {
            let desc = passes.passes[k];
            let scene = project.scene.as_ref().unwrap();
            // Effect fbos, created on first use.
            if let Some((ei, _)) = desc.effect
                && !effect_fbos.contains_key(&ei)
            {
                let mut map = HashMap::new();
                if let Some(eid) = effects.get(ei as usize) {
                    for fbo in &scene.effect(*eid).fbos {
                        let name = project.strings.resolve(fbo.name).to_owned();
                        let div = if fbo.scale > 0.0 { fbo.scale } else { 1.0 };
                        let (tw, th) = scaled(size, div, image_scale);
                        w.targets.push(Target {
                            label: format!("{name} (object {id})"),
                            width: tw,
                            height: th,
                            format: target_format(project.strings.resolve(fbo.format)),
                        });
                        map.insert(name, w.targets.len() - 1);
                    }
                }
                effect_fbos.insert(ei, map);
            }
            let (shader_name, is_copy) = match desc.kind {
                PassKind::Material { material, pass } => {
                    let Some(mp) = scene.material(material).passes.get(pass as usize) else {
                        continue;
                    };
                    (project.strings.resolve(mp.shader).to_owned(), false)
                }
                PassKind::Copy { .. } => ("commands/copy".to_owned(), true),
            };
            let (material_combos, override_combos) =
                split_combos(&project, idx, &passes, k, main_format);
            let used_slots = project
                .pass_compile_textures(idx, &passes, k)
                .entries()
                .iter()
                .map(|e| e.0)
                .collect();
            let combos = PassCombos {
                overrides: override_combos
                    .iter()
                    .map(|(k, v)| (k.clone(), i64::from(*v)))
                    .collect(),
                material: material_combos
                    .iter()
                    .map(|(k, v)| (k.clone(), i64::from(*v)))
                    .collect(),
                used_slots,
            };
            let t_shader = std::time::Instant::now();
            let loaded = compiler.load_pass(
                &shader_fs,
                &PassRequest {
                    shader: &shader_name,
                    combos: &combos,
                },
            );
            w.translate_ms += t_shader.elapsed().as_secs_f64() * 1000.0;
            let shader = match loaded {
                Ok(s) => s,
                Err(e) => {
                    w.shader_errors.push((shader_name.clone(), e.to_string()));
                    log(format!("skipped shader {shader_name}: {e}"));
                    skipped_shaders += 1;
                    continue;
                }
            };
            let program = *program_index.entry(shader.key).or_insert_with(|| {
                w.programs.push(shader.clone());
                w.program_names.push(shader_name.clone());
                w.programs.len() - 1
            });
            // Texture slots (spec 02 §12.3).
            let defaults: Vec<(u32, &str)> = shader
                .vertex_params
                .samplers
                .iter()
                .chain(&shader.fragment_params.samplers)
                .filter_map(|s| Some((s.slot?, s.default_texture.as_deref()?)))
                .collect();
            let slots_ref = project.pass_textures(idx, &passes, k, &defaults, &mut |name| {
                vfs.read_texture(name).is_ok()
            });
            let target_name = project
                .pass_target(idx, &passes, k)
                .map(|s| project.strings.resolve(s).to_owned());
            let resolve = |w: &mut Workload,
                           name: &str,
                           fbos: Option<&HashMap<String, usize>>,
                           image: &ImageScope,
                           globals: &mut HashMap<String, usize>|
             -> usize {
                if let Some(i) = fbos.and_then(|m| m.get(name)) {
                    return *i;
                }
                if let Some(i) = image.named.get(name) {
                    return *i;
                }
                if let Some(i) = globals.get(name) {
                    return *i;
                }
                // Unknown scene-wide target: one at the scene's size (`_rt_shadowAtlas` is 1x1 in
                // lumenwall-engine: never filled).
                let (tw, th) = if name == "_rt_shadowAtlas" {
                    (1, 1)
                } else {
                    rendered
                };
                w.targets.push(Target {
                    label: name.to_owned(),
                    width: tw,
                    height: th,
                    format: TargetFormat::Rgba8,
                });
                globals.insert(name.to_owned(), w.targets.len() - 1);
                w.targets.len() - 1
            };
            let fbos = desc.effect.and_then(|(ei, _)| effect_fbos.get(&ei));
            let mut slots: Vec<(u32, Src)> = Vec::new();
            for (slot, r) in slots_ref {
                let src = match r {
                    TextureRef::File(sym) => {
                        let name = project.strings.resolve(sym).to_owned();
                        if k == 0 && slot == 0 && main_name.as_deref() == Some(name.as_str()) {
                            main_src
                        } else {
                            texture(&mut w, &name, &mut skipped_textures).map(Src::Tex)
                        }
                    }
                    TextureRef::RenderTarget(sym) => {
                        let name = project.strings.resolve(sym).to_owned();
                        Some(Src::Target(resolve(
                            &mut w,
                            &name,
                            fbos,
                            &image,
                            &mut globals,
                        )))
                    }
                    TextureRef::Previous => prev,
                    TextureRef::OwnComposite => Some(Src::Target(comp_a)),
                };
                if let Some(s) = src {
                    slots.push((slot, s));
                }
            }
            if k == 0
                && !slots.iter().any(|(s, _)| *s == 0)
                && let Some(m) = main_src
            {
                slots.push((0, m));
            }
            // Target: declared by the effect pass, else the chain's ping-pong buffer; the last
            // pass of the image lands on the scene target.
            let target = match target_name {
                Some(n) => resolve(&mut w, &n, fbos, &image, &mut globals),
                None if k + 1 == n_passes => 0,
                None => {
                    let t = ping;
                    ping = if ping == comp_a { comp_b } else { comp_a };
                    t
                }
            };
            let target = if k + 1 == n_passes && is_copy {
                0
            } else {
                target
            };
            prev = Some(Src::Target(target));
            w.draws.push(Draw {
                program,
                slots,
                target,
                label: format!("object {id} pass {k} ({shader_name})"),
                coverage: if target == 0 {
                    (
                        (size.0 * world_scale.x.abs() / scene_size.0).min(1.0),
                        (size.1 * world_scale.y.abs() / scene_size.1).min(1.0),
                    )
                } else {
                    (1.0, 1.0)
                },
            });
        }
    }

    // Fewer render passes, as lumenwall-render does (graph.rs, "chains first, scene draws
    // together"): the draws that fill intermediate targets keep their order and run first, then
    // every draw to the scene target in object order.
    let (mut chains, scene_draws): (Vec<Draw>, Vec<Draw>) = std::mem::take(&mut w.draws)
        .into_iter()
        .partition(|d| d.target != 0);
    chains.extend(scene_draws);
    w.draws = chains;
    // A texture is never read by the draw that writes it (the engine copies it first): the slot
    // is left to the backend's blank texture.
    for d in &mut w.draws {
        let t = d.target;
        d.slots.retain(|(_, s)| *s != Src::Target(t));
    }

    // The copy of the scene target to the swap chain.
    let copy = compiler
        .load_pass(
            &shader_fs,
            &PassRequest {
                shader: "commands/copy",
                combos: &PassCombos::default(),
            },
        )
        .map_err(|e| anyhow!("commands/copy: {e}"))?;
    w.present_program = *program_index.entry(copy.key).or_insert_with(|| {
        w.programs.push(copy.clone());
        w.program_names.push("commands/copy".into());
        w.programs.len() - 1
    });
    if skipped_textures + skipped_shaders > 0 {
        log(format!(
            "{skipped_textures} textures and {skipped_shaders} shaders skipped"
        ));
    }
    if w.draws.is_empty() {
        return Err(anyhow!("no draw in {}", wallpaper.display())).context("empty workload");
    }
    w.load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(w)
}

type ComboList = Vec<(String, i32)>;

/// Material combos (with the `TEX0FORMAT` forcing) and override combos of a pass, upper-case.
fn split_combos(
    project: &Project,
    idx: ObjectIndex,
    passes: &ImagePasses,
    k: usize,
    main_format: MainTextureFormat,
) -> (ComboList, ComboList) {
    let mut material = ComboList::new();
    let mut overrides = ComboList::new();
    let Some(scene) = &project.scene else {
        return (material, overrides);
    };
    let Some(desc) = passes.passes.get(k) else {
        return (material, overrides);
    };
    let s = &project.strings;
    let add = |list: &mut ComboList, name: &str, v: i32| {
        let name = name.to_ascii_uppercase();
        if !list.iter().any(|(n, _)| *n == name) {
            list.push((name, v));
        }
    };
    if let PassKind::Material { material: m, pass } = desc.kind
        && let Some(mp) = scene.material(m).passes.get(pass as usize)
    {
        match main_format {
            MainTextureFormat::Rg88 => add(&mut material, "TEX0FORMAT", 8),
            MainTextureFormat::R8 => add(&mut material, "TEX0FORMAT", 9),
            MainTextureFormat::Other => {}
        }
        for c in &mp.combos {
            add(&mut material, s.resolve(c.key), c.value);
        }
    }
    match desc.over {
        lumenwall_scene::OverrideRef::Effect { effect, index } => {
            if let Some(ov) = scene
                .object(idx)
                .as_layer()
                .and_then(|i| i.effects.get(effect as usize))
                .and_then(|e| e.overrides.get(index as usize))
            {
                for c in &ov.combos {
                    add(&mut overrides, s.resolve(c.key), c.value);
                }
            }
        }
        lumenwall_scene::OverrideRef::Synthetic(i) => {
            if let Some(ov) = passes.synthetic.get(i as usize) {
                for (key, v) in &ov.combos {
                    add(&mut overrides, s.resolve(*key), *v);
                }
            }
        }
        lumenwall_scene::OverrideRef::None => {}
    }
    (material, overrides)
}

//! Scene construction (spec 02 §13, §14): creation order, render order, transformations,
//! scene size, clear colour, synthetic compatibility materials and the bloom object.

use std::collections::{HashMap, HashSet};

use glam::{UVec2, Vec2, Vec3, Vec4};

use crate::asset::AssetSource;
use crate::error::LoadError;
use crate::intern::Sym;
use crate::load::Loader;
use crate::passes::ImagePasses;
use crate::project::Project;
use crate::scene::*;
use crate::setting::Binder;
use crate::value::Value;

/// Maximum number of ancestors composed into a transformation (§13.3).
pub const MAX_PARENT_DEPTH: usize = 32;

/// Materials of the synthetic bloom effect (WE assets, §14).
pub const BLOOM_MATERIALS: [&str; 4] = [
    "materials/util/downsample_quarter_bloom.json",
    "materials/util/downsample_eighth_blur_v.json",
    "materials/util/blur_h_bloom.json",
    "materials/util/combine.json",
];

/// Material of the magenta-tint compatibility pass (§12.6).
pub const TINT_MATERIAL: &str = "materials/effects/tint.json";
/// Material of the colour-blend compatibility pass (§12.6).
pub const PASSTHROUGH_MATERIAL: &str = "materials/util/effectpassthrough.json";

/// Parameters of [`Project::build`].
#[derive(Clone, Copy, Debug)]
pub struct BuildOptions {
    /// Output resolution (scene size fallback, §6.3).
    pub output_size: UVec2,
    /// Application setting: particles enabled.
    pub particles_enabled: bool,
}

impl BuildOptions {
    /// Options for an output of this size, particles enabled.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            output_size: UVec2::new(width, height),
            particles_enabled: true,
        }
    }
}

/// World transformation of an object (§13.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Origin.
    pub origin: Vec3,
    /// Scale.
    pub scale: Vec3,
    /// Rotation in the plane (radians).
    pub angle: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            origin: Vec3::ZERO,
            scale: Vec3::ONE,
            angle: 0.0,
        }
    }
}

impl Transform {
    /// Composes `child` (local) on `self` (parent, already composed).
    pub fn then(&self, child: &Transform) -> Transform {
        let v = Vec2::new(child.origin.x * self.scale.x, child.origin.y * self.scale.y);
        let (s, c) = self.angle.sin_cos();
        let d = Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c);
        Transform {
            origin: Vec3::new(
                self.origin.x + d.x,
                self.origin.y + d.y,
                self.origin.z + child.origin.z * self.scale.z,
            ),
            scale: child.scale * self.scale,
            angle: child.angle + self.angle,
        }
    }
}

/// Render target format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetFormat {
    /// 8 bits per channel.
    Argb8888,
}

/// Render target created by the implementation (bloom, §14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderTargetDesc {
    /// Name.
    pub name: Sym,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Format.
    pub format: TargetFormat,
    /// UVs clamped.
    pub clamp_uv: bool,
}

/// The synthetic bloom object.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomInfo {
    /// Object index (not reachable by id).
    pub object: ObjectIndex,
    /// `bloomstrength` float view at build.
    pub strength: f32,
    /// `bloomthreshold` float view at build.
    pub threshold: f32,
}

/// Result of [`Project::build`]: everything the renderer needs to set the scene up.
#[derive(Clone, Debug)]
pub struct SceneBuild {
    /// Scene size (camera), §6.3.
    pub size: Vec2,
    /// Clear colour: vec3 view of `clearcolor`, alpha 1.
    pub clear_color: Vec4,
    /// Per object: set up.
    pub created: Vec<bool>,
    /// Objects in creation order (§13.1).
    pub creation_order: Vec<ObjectIndex>,
    /// Objects in render order (§13.2); the bloom object last.
    pub render_order: Vec<ObjectIndex>,
    /// Per object: world transformation at build (identity when not created).
    pub transforms: Vec<Transform>,
    /// Per object: render passes of created images.
    pub image_passes: Vec<Option<ImagePasses>>,
    /// Bloom object, when `general.bloom` is true.
    pub bloom: Option<BloomInfo>,
    /// Render targets created after the scene objects.
    pub render_targets: Vec<RenderTargetDesc>,
    by_id: HashMap<i32, ObjectIndex>,
}

impl SceneBuild {
    /// Created object with this id.
    pub fn created_object(&self, id: i32) -> Option<ObjectIndex> {
        self.by_id.get(&id).copied()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    New,
    Active,
    Done,
}

impl Project {
    fn warn(&mut self, message: String) {
        self.warnings.push("", message);
    }

    /// Scene size (§6.3): the orthogonal projection, its automatic size from the images, or
    /// the output size.
    pub fn scene_size(&self, output_size: UVec2) -> Vec2 {
        let output = output_size.as_vec2();
        let Some(scene) = &self.scene else {
            return output;
        };
        match scene.general.orthogonal_projection {
            Some(OrthogonalProjection {
                auto: false,
                width,
                height,
            }) => Vec2::new(width as f32, height as f32),
            Some(OrthogonalProjection { auto: true, .. }) => {
                let mut e = Vec2::ZERO;
                for o in scene.file_objects() {
                    if let ObjectKind::Image(img) = &o.kind {
                        let origin = self.settings.value(o.origin).vec3();
                        e.x = e.x.max(origin.x.abs() + img.size.x / 2.0);
                        e.y = e.y.max(origin.y.abs() + img.size.y / 2.0);
                    }
                }
                if e.x > 0.0 && e.y > 0.0 {
                    e * 2.0
                } else {
                    output
                }
            }
            None => output,
        }
    }

    /// Builds the scene with the default set-up rules.
    pub fn build(
        &mut self,
        src: &dyn AssetSource,
        opts: &BuildOptions,
    ) -> Result<SceneBuild, LoadError> {
        self.build_with(src, opts, |_, _| true)
    }

    /// Builds the scene (§13, §14). `can_set_up` adds renderer-specific failures (e.g. a
    /// particle system without material): a refused object is not created (§13.1-4).
    pub fn build_with(
        &mut self,
        src: &dyn AssetSource,
        opts: &BuildOptions,
        mut can_set_up: impl FnMut(ObjectIndex, &Object) -> bool,
    ) -> Result<SceneBuild, LoadError> {
        if self.scene.is_none() {
            return Err(LoadError::NotAScene);
        }
        self.load_util_materials(src)?;
        let size = self.scene_size(opts.output_size);
        let Some(scene) = self.scene.as_ref() else {
            return Err(LoadError::NotAScene);
        };
        let clear_color = self
            .settings
            .value(scene.general.clear_color)
            .vec3()
            .extend(1.0);
        let bloom_on = self.settings.value(scene.general.bloom).boolean();
        let strength = self.settings.value(scene.general.bloom_strength).float();
        let threshold = self.settings.value(scene.general.bloom_threshold).float();

        // §13.1 creation order.
        let n = scene.file_objects;
        let mut created = vec![false; scene.objects.len()];
        let mut by_id: HashMap<i32, ObjectIndex> = HashMap::new();
        let mut creation_order = Vec::new();
        let mut state = vec![Visit::New; n];
        let mut stack: Vec<(usize, usize)> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        for root in 0..n {
            enter(root, scene, &by_id, &mut state, &mut stack, &mut warnings);
            while let Some(top) = stack.last_mut() {
                let (i, step) = *top;
                let obj = &scene.objects[i];
                if step < obj.dependencies.len() {
                    top.1 += 1;
                    let d = obj.dependencies[step];
                    if d != obj.id
                        && let Some(j) = scene.find_object(d)
                    {
                        enter(
                            j.index(),
                            scene,
                            &by_id,
                            &mut state,
                            &mut stack,
                            &mut warnings,
                        );
                    }
                    continue;
                }
                if step == obj.dependencies.len() {
                    top.1 += 1;
                    if let Some(p) = obj.parent {
                        match scene.find_object(p) {
                            Some(j) => enter(
                                j.index(),
                                scene,
                                &by_id,
                                &mut state,
                                &mut stack,
                                &mut warnings,
                            ),
                            // Lumenwall: a missing parent makes the object a root instead of
                            // failing the scene (Q17).
                            None => warnings.push(format!(
                                "object {}: parent {p} not found, used as root",
                                obj.id
                            )),
                        }
                    }
                    continue;
                }
                stack.pop();
                state[i] = Visit::Done;
                if by_id.contains_key(&obj.id) {
                    continue;
                }
                let idx = ObjectIndex(i as u32);
                match default_set_up(scene, obj, opts) {
                    Err(why) => warnings.push(format!("object {} not set up: {why}", obj.id)),
                    Ok(()) if !can_set_up(idx, obj) => {
                        warnings.push(format!("object {} not set up", obj.id))
                    }
                    Ok(()) => {
                        created[i] = true;
                        by_id.insert(obj.id, idx);
                        creation_order.push(idx);
                    }
                }
            }
        }

        // §13.2 render order.
        let mut render_order = Vec::new();
        let mut listed: HashSet<i32> = HashSet::new();
        let mut state = vec![Visit::New; n];
        for root in 0..n {
            enter_render(root, &created, &mut state, &mut stack, &mut warnings, scene);
            while let Some(top) = stack.last_mut() {
                let (i, step) = *top;
                let obj = &scene.objects[i];
                if step < obj.dependencies.len() {
                    top.1 += 1;
                    let d = obj.dependencies[step];
                    if d != obj.id {
                        match by_id.get(&d) {
                            Some(j) => enter_render(
                                j.index(),
                                &created,
                                &mut state,
                                &mut stack,
                                &mut warnings,
                                scene,
                            ),
                            None => warnings.push(format!(
                                "object {}: dependency {d} not in the scene",
                                obj.id
                            )),
                        }
                    }
                    continue;
                }
                stack.pop();
                state[i] = Visit::Done;
                if listed.insert(obj.id) {
                    render_order.push(ObjectIndex(i as u32));
                }
            }
        }

        for w in warnings {
            self.warn(w);
        }

        // §14 bloom.
        let mut bloom = None;
        let mut render_targets = Vec::new();
        if bloom_on && let Some(idx) = self.bloom_object(src, size, strength, threshold)? {
            created.resize(self.scene.as_ref().map_or(0, |s| s.objects.len()), false);
            created[idx.index()] = true;
            creation_order.push(idx);
            render_order.push(idx);
            bloom = Some(BloomInfo {
                object: idx,
                strength,
                threshold,
            });
            let (w, h) = (size.x.max(0.0) as u32, size.y.max(0.0) as u32);
            for (name, div) in [
                ("_rt_4FrameBuffer", 4),
                ("_rt_8FrameBuffer", 8),
                ("_rt_Bloom", 8),
            ] {
                render_targets.push(RenderTargetDesc {
                    name: self.strings.intern(name),
                    width: w / div,
                    height: h / div,
                    format: TargetFormat::Argb8888,
                    clamp_uv: true,
                });
            }
        }
        let total = self.scene.as_ref().map_or(0, |s| s.objects.len());
        created.resize(total, false);

        let mut transforms = vec![Transform::default(); total];
        let mut image_passes = vec![None; total];
        let mut deep = Vec::new();
        for i in 0..total {
            if !created[i] {
                continue;
            }
            let idx = ObjectIndex(i as u32);
            let (t, too_deep) = self.compose_transform(idx, &by_id);
            transforms[i] = t;
            if too_deep {
                deep.push(i);
            }
            image_passes[i] = self.image_passes(idx);
        }
        for i in deep {
            self.warn(format!(
                "object index {i}: more than {MAX_PARENT_DEPTH} parents, chain cut"
            ));
        }
        Ok(SceneBuild {
            size,
            clear_color,
            created,
            creation_order,
            render_order,
            transforms,
            image_passes,
            bloom,
            render_targets,
            by_id,
        })
    }

    /// Local transformation of an object (§13.3).
    pub fn local_transform(&self, obj: ObjectIndex) -> Transform {
        let Some(o) = self.scene.as_ref().and_then(|s| s.objects.get(obj.index())) else {
            return Transform::default();
        };
        let (scale, angles) = o.transform_settings();
        Transform {
            origin: self.settings.value(o.origin).vec3(),
            scale: self.settings.value(scale).vec3(),
            angle: angles.map_or(0.0, |a| self.settings.value(a).vec3().z),
        }
    }

    /// World transformation with the current setting values (§13.3).
    pub fn world_transform(&self, obj: ObjectIndex, build: &SceneBuild) -> Transform {
        self.compose_transform(obj, &build.by_id).0
    }

    fn compose_transform(
        &self,
        obj: ObjectIndex,
        by_id: &HashMap<i32, ObjectIndex>,
    ) -> (Transform, bool) {
        let Some(scene) = &self.scene else {
            return (Transform::default(), false);
        };
        let mut chain = vec![obj];
        let mut cur = obj;
        let mut too_deep = false;
        while let Some(p) = scene.objects.get(cur.index()).and_then(|o| o.parent) {
            let Some(&j) = by_id.get(&p) else { break };
            if chain.len() > MAX_PARENT_DEPTH {
                too_deep = true;
                break;
            }
            chain.push(j);
            cur = j;
        }
        let mut it = chain.iter().rev();
        let mut acc = it
            .next()
            .map(|&i| self.local_transform(i))
            .unwrap_or_default();
        for &i in it {
            acc = acc.then(&self.local_transform(i));
        }
        (acc, too_deep)
    }

    fn loader_run<T>(
        &mut self,
        src: &dyn AssetSource,
        f: impl FnOnce(&mut Loader<'_, '_>) -> Result<T, LoadError>,
    ) -> Result<T, LoadError> {
        let Some(scene) = self.scene.as_mut() else {
            return Err(LoadError::NotAScene);
        };
        let mut loader = Loader {
            src,
            b: Binder {
                settings: &mut self.settings,
                props: &mut self.properties,
                strings: &mut self.strings,
            },
            warnings: &mut self.warnings,
            arenas: &mut scene.arenas,
        };
        f(&mut loader)
    }

    /// Loads the compatibility materials once, when some image may need them (§12.6).
    fn load_util_materials(&mut self, src: &dyn AssetSource) -> Result<(), LoadError> {
        let Some(scene) = &self.scene else {
            return Ok(());
        };
        let names = scene.names;
        let mut need_tint = false;
        let mut need_passthrough = false;
        for o in scene.file_objects() {
            if let Some(img) = o.as_layer() {
                need_tint |= img.effects.iter().any(|e| {
                    e.overrides.iter().any(|ov| {
                        ov.combos
                            .iter()
                            .any(|c| c.key == names.composite && c.value == 2)
                    })
                });
                need_passthrough |= self.settings.meta(img.color_blend_mode).property.is_some()
                    || self.settings.value(img.color_blend_mode).int() > 0;
            }
        }
        if need_tint && scene.util_tint.is_none() {
            let m = self.loader_run(src, |l| l.load_material(TINT_MATERIAL))?;
            if let Some(s) = self.scene.as_mut() {
                s.util_tint = Some(m);
            }
        }
        if need_passthrough
            && self
                .scene
                .as_ref()
                .is_some_and(|s| s.util_passthrough.is_none())
        {
            let m = self.loader_run(src, |l| l.load_material(PASSTHROUGH_MATERIAL))?;
            if let Some(s) = self.scene.as_mut() {
                s.util_passthrough = Some(m);
            }
        }
        Ok(())
    }

    /// Creates the synthetic bloom image (§14), or updates its size and constants when it
    /// already exists. `None` when a bloom material is missing.
    fn bloom_object(
        &mut self,
        src: &dyn AssetSource,
        size: Vec2,
        strength: f32,
        threshold: f32,
    ) -> Result<Option<ObjectIndex>, LoadError> {
        let origin = Value::from_vec3(Vec3::new(size.x / 2.0, size.y / 2.0, 0.0));
        if let Some(scene) = &self.scene
            && let Some(idx) = scene.bloom
        {
            let o = &scene.objects[idx.index()];
            let origin_id = o.origin;
            let mut constants = Vec::new();
            if let Some(img) = o.as_image() {
                for ov in img.effects.iter().flat_map(|e| &e.overrides) {
                    for c in &ov.constants {
                        let v = if self.strings.resolve(c.name) == "bloomstrength" {
                            strength
                        } else {
                            threshold
                        };
                        constants.push((c.setting, v));
                    }
                }
            }
            self.settings.set(origin_id, origin);
            for (id, v) in constants {
                self.settings.set(id, Value::from_float(v));
            }
            if let Some(ObjectKind::Image(img)) = self
                .scene
                .as_mut()
                .map(|s| &mut s.objects[idx.index()].kind)
            {
                img.size = size;
            }
            return Ok(Some(idx));
        }
        let built = self.loader_run(src, |l| {
            let mut mats = Vec::with_capacity(4);
            for path in BLOOM_MATERIALS {
                match l.load_material(path)? {
                    Some(m) => mats.push(m),
                    None => return Ok(None),
                }
            }
            let s = |l: &mut Loader<'_, '_>, x: &str| l.b.strings.intern(x);
            let full = s(l, "_rt_FullFrameBuffer");
            let rt4 = s(l, "_rt_4FrameBuffer");
            let rt8 = s(l, "_rt_8FrameBuffer");
            let rtb = s(l, "_rt_Bloom");
            let material = Material {
                file: s(l, "<lumenwall>/bloom/material.json"),
                passes: vec![MaterialPass {
                    shader: s(l, "genericimage2"),
                    blending: Blending::Normal,
                    cull_mode: CullMode::NoCull,
                    depth_test: DepthMode::Disabled,
                    depth_write: DepthMode::Disabled,
                    textures: TextureMap(vec![(0, full)]),
                    user_textures: TextureMap::default(),
                    combos: Vec::new(),
                    constants: Vec::new(),
                }],
            };
            l.arenas.materials.push(material);
            let material = MaterialId(l.arenas.materials.len() as u32 - 1);
            let model_file = s(l, "<lumenwall>/bloom/model.json");
            l.arenas.models.push(Model {
                file: model_file,
                material,
                solid_layer: false,
                fullscreen: false,
                passthrough: false,
                autosize: false,
                nopadding: false,
                width: None,
                height: None,
                puppet: None,
            });
            let model = ModelId(l.arenas.models.len() as u32 - 1);
            let pass = |m: MaterialId, binds: Vec<Bind>, target: Sym| EffectPass {
                material: Some(m),
                binds,
                command: None,
                source: None,
                target: Some(target),
            };
            let bind = |index: i32, source: BindSource| Bind { index, source };
            let effect = Effect {
                file: s(l, "<lumenwall>/bloom/effect.json"),
                name: "".into(),
                description: "".into(),
                group: "".into(),
                preview: "".into(),
                dependencies: Vec::new(),
                passes: vec![
                    pass(mats[0], vec![bind(0, BindSource::Named(full))], rt4),
                    pass(mats[1], vec![bind(0, BindSource::Named(rt4))], rt8),
                    pass(mats[2], vec![bind(0, BindSource::Named(rt8))], rtb),
                    pass(
                        mats[3],
                        vec![
                            bind(0, BindSource::OwnComposite),
                            bind(1, BindSource::Named(rtb)),
                        ],
                        full,
                    ),
                ],
                fbos: Vec::new(),
            };
            l.arenas.effects.push(effect);
            let effect = EffectId(l.arenas.effects.len() as u32 - 1);
            let strength_name = s(l, "bloomstrength");
            let threshold_name = s(l, "bloomthreshold");
            // parity: three identical overrides; the fourth pass gets none (Q21).
            let overrides = (0..3)
                .map(|_| PassOverride {
                    id: -1,
                    combos: Vec::new(),
                    constants: vec![
                        Constant {
                            name: strength_name,
                            setting: l.constant(Value::from_float(strength)),
                        },
                        Constant {
                            name: threshold_name,
                            setting: l.constant(Value::from_float(threshold)),
                        },
                    ],
                    textures: TextureMap::default(),
                    user_textures: TextureMap::default(),
                })
                .collect();
            let image = ImageObject {
                file: s(l, "<lumenwall>/bloom/model.json"),
                model: Some(model),
                scale: l.constant(Value::from_vec3(Vec3::ONE)),
                angles: l.constant(Value::from_vec3(Vec3::ZERO)),
                visible: l.constant(Value::from_bool(true)),
                alpha: l.constant(Value::from_float(1.0)),
                color: l.constant(Value::from_color(Vec4::ONE)),
                alignment: Alignment::default(),
                size,
                parallax_depth: l.constant(Value::from_vec2(Vec2::ZERO)),
                color_blend_mode: l.constant(Value::from_int(0)),
                brightness: l.constant(Value::from_float(1.0)),
                effects: vec![ImageEffect {
                    file: s(l, "<lumenwall>/bloom/effect.json"),
                    effect,
                    id: 15_242_000,
                    name: Some("".into()),
                    visible: l.constant(Value::from_bool(true)),
                    overrides,
                }],
                animation_layers: Vec::new(),
            };
            Ok(Some(Object {
                // Not registered by id, so it cannot collide with a real object −1 (Q17).
                id: -1,
                name: s(l, "<lumenwall>/bloom"),
                dependencies: Box::default(),
                parent: None,
                origin: l.constant(origin),
                scale: l.constant(Value::from_vec3(Vec3::ONE)),
                angles: l.constant(Value::from_vec3(Vec3::ZERO)),
                visible: l.constant(Value::from_bool(true)),
                kind: ObjectKind::Image(Box::new(image)),
            }))
        })?;
        let Some(object) = built else {
            self.warn("bloom disabled: a bloom material is missing".into());
            return Ok(None);
        };
        let Some(scene) = self.scene.as_mut() else {
            return Ok(None);
        };
        scene.objects.push(object);
        let idx = ObjectIndex(scene.objects.len() as u32 - 1);
        scene.bloom = Some(idx);
        Ok(Some(idx))
    }
}

/// Set-up rules known to the model (§13.1-4, §13.4, Q27, Q31).
fn default_set_up(scene: &Scene, obj: &Object, opts: &BuildOptions) -> Result<(), &'static str> {
    match &obj.kind {
        ObjectKind::Image(img) => {
            let Some(model) = img.model else {
                return Err("model or material file missing");
            };
            let model = scene.model(model);
            if scene.material(model.material).passes.is_empty() {
                // Lumenwall: an image whose material has no pass is skipped (Q27).
                return Err("material has no pass");
            }
            if model.passthrough && img.effects.is_empty() {
                return Err("passthrough image without effect");
            }
            Ok(())
        }
        ObjectKind::Particle(_) if !opts.particles_enabled => Err("particles disabled"),
        _ => Ok(()),
    }
}

fn enter(
    j: usize,
    scene: &Scene,
    by_id: &HashMap<i32, ObjectIndex>,
    state: &mut [Visit],
    stack: &mut Vec<(usize, usize)>,
    warnings: &mut Vec<String>,
) {
    if by_id.contains_key(&scene.objects[j].id) {
        return;
    }
    match state[j] {
        Visit::New => {
            state[j] = Visit::Active;
            stack.push((j, 0));
        }
        // Lumenwall: cycles are cut instead of recursing forever (Q17).
        Visit::Active => warnings.push(format!(
            "object {}: dependency or parent cycle cut",
            scene.objects[j].id
        )),
        Visit::Done => {}
    }
}

fn enter_render(
    j: usize,
    created: &[bool],
    state: &mut [Visit],
    stack: &mut Vec<(usize, usize)>,
    warnings: &mut Vec<String>,
    scene: &Scene,
) {
    if !created[j] {
        return;
    }
    match state[j] {
        Visit::New => {
            state[j] = Visit::Active;
            stack.push((j, 0));
        }
        Visit::Active => warnings.push(format!(
            "object {}: dependency cycle cut",
            scene.objects[j].id
        )),
        Visit::Done => {}
    }
}

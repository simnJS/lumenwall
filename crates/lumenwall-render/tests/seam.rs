//! The backend seam without any GPU: the front builds, extends and renders a scene on a
//! backend that is not wgpu, through `SceneBackend` and `SceneDesc` only (what the Direct3D 11
//! bridge of the engine does). The backend here mirrors the dynamic region from the dirty
//! ranges the front hands it, so the test also checks the upload contract.

use std::any::Any;

use glam::{Vec2, Vec3};
use lumenwall_render::backend::{
    BoundTexture, Frame, Gpu, GraphCounts, SceneBackend, SceneDesc, SegmentTime,
};
use lumenwall_render::dynbuf::FrameInputs;
use lumenwall_render::graph::{Geometry, MeshLayout, Op, PassDesc, SceneBuilder, StreamLayout};
use lumenwall_render::image::{ImageFrame, ImagePass, ImageSpec, add_image};
use lumenwall_render::math::{Alignment, NodeTransform, OrthoCamera, unit_quad};
use lumenwall_render::shader::{PreparedShader, ShaderDesc};
use lumenwall_render::targets::RtId;
use lumenwall_render::texture::{
    MipLevel, SamplerKey, TexFlags, TexelFormat, TextureInfo, TextureUpload, VolumeUpload,
};
use lumenwall_render::{Result, TextureRef};

const SRC: &str = r"
struct Pass {
    g_ModelViewProjectionMatrix: mat4x4<f32>,
    g_Time: f32,
    g_Texture0Resolution: vec4<f32>,
}
@group(0) @binding(0) var<uniform> u: Pass;
@group(1) @binding(0) var g_Texture0: texture_2d<f32>;
@group(1) @binding(1) var g_Texture0_s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@location(0) a_Position: vec3<f32>, @location(1) a_TexCoord: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = u.g_ModelViewProjectionMatrix * vec4<f32>(a_Position, 1.0);
    o.uv = a_TexCoord;
    return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(g_Texture0, g_Texture0_s, i.uv) * (1.0 + 0.0 * u.g_Time);
}
";

/// What the backend saw at build.
#[derive(Default)]
struct Built {
    passes: usize,
    ops: Vec<Op>,
    /// `(index of the target, the target whose memory it uses)`.
    shares: Vec<(usize, RtId)>,
    clear_first: usize,
}

/// A backend that records what the front hands it.
#[derive(Default)]
struct Recorder {
    textures: usize,
    shaders: Vec<String>,
    meshes: usize,
    streams: usize,
    built: Option<Built>,
    extended: Vec<usize>,
    replans: usize,
    rebinds: usize,
    frames: usize,
    /// The backend's copy of the dynamic region, kept from the dirty ranges.
    region: Vec<u8>,
    /// Slot 0 of the first pass, per frame.
    slot0: Vec<BoundTexture>,
}

impl SceneBackend for Recorder {
    fn uniform_alignment(&self, _: Gpu<'_>) -> u32 {
        256
    }

    fn supports_samples(&self, _: Gpu<'_>, _: wgpu::TextureFormat, samples: u32) -> bool {
        samples <= 4
    }

    fn add_texture(&mut self, gpu: Gpu<'_>, upload: &TextureUpload<'_>) -> Result<TextureInfo> {
        assert!(gpu.wgpu().is_none());
        self.textures += 1;
        Ok(TextureInfo::for_upload(upload))
    }

    fn add_volume_texture(&mut self, _: Gpu<'_>, upload: &VolumeUpload<'_>) -> Result<TextureInfo> {
        self.textures += 1;
        Ok(TextureInfo::for_volume(upload))
    }

    fn add_shader(&mut self, _: Gpu<'_>, shader: PreparedShader) -> Result<()> {
        assert!(shader.clip_fixed);
        self.shaders.push(shader.label);
        Ok(())
    }

    fn add_mesh(&mut self, _: Gpu<'_>, _: &str, _: &MeshLayout, _: &[u8], _: &[u16]) -> Result<()> {
        self.meshes += 1;
        Ok(())
    }

    fn add_stream(
        &mut self,
        _: Gpu<'_>,
        _: &str,
        _: &StreamLayout,
        _: u32,
        _: &[u32],
    ) -> Result<()> {
        self.streams += 1;
        Ok(())
    }

    fn build(&mut self, _: Gpu<'_>, scene: &SceneDesc<'_>) -> Result<()> {
        self.region = scene.dynamic.bytes().to_vec();
        self.built = Some(Built {
            passes: scene.passes.len(),
            ops: scene.ops.to_vec(),
            shares: scene
                .targets
                .iter()
                .enumerate()
                .filter_map(|(i, t)| t.shares().map(|o| (i, o)))
                .collect(),
            clear_first: scene.passes.iter().filter(|p| p.clear_first).count(),
        });
        Ok(())
    }

    fn extend(&mut self, _: Gpu<'_>, scene: &SceneDesc<'_>, first: usize) -> Result<()> {
        if scene.dynamic.grew() {
            self.region = scene.dynamic.bytes().to_vec();
        }
        self.extended.push(first);
        Ok(())
    }

    fn rebind(&mut self, _: Gpu<'_>, _: &SceneDesc<'_>) -> Result<()> {
        self.rebinds += 1;
        Ok(())
    }

    fn replan(&mut self, _: Gpu<'_>, _: &SceneDesc<'_>) {
        self.replans += 1;
    }

    fn render(&mut self, frame: &mut Frame<'_>, scene: &SceneDesc<'_>) -> bool {
        assert!(matches!(frame, Frame::Other(_)));
        if let Some((start, end)) = scene.dynamic.shadow().dirty_range() {
            let (start, end) = (start as usize, end as usize);
            self.region[start..end].copy_from_slice(&scene.dynamic.bytes()[start..end]);
        }
        let first = &scene.passes[0];
        if let Some(s) = scene.pass_slots(0, first.variant).next() {
            self.slot0.push(s.texture);
        }
        self.frames += 1;
        false
    }

    fn read_target(&mut self, _: Gpu<'_>, scene: &SceneDesc<'_>, target: RtId) -> Result<Vec<u8>> {
        let (w, h) = scene.targets.get(target).size();
        Ok(vec![0; (w * h * 4) as usize])
    }

    fn profile(&mut self, _: Gpu<'_>, _: &SceneDesc<'_>) -> Option<Vec<SegmentTime>> {
        None
    }

    fn counts(&self) -> GraphCounts {
        GraphCounts::default()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn texture<'a>(levels: &'a [MipLevel<'a>]) -> TextureUpload<'a> {
    TextureUpload {
        label: "t",
        format: TexelFormat::Rgba8,
        levels,
        content_size: None,
        flags: TexFlags::default(),
    }
}

fn recorder(scene: &lumenwall_render::RenderScene) -> &Recorder {
    scene.backend().as_any().downcast_ref().unwrap()
}

#[test]
fn a_scene_builds_and_renders_without_wgpu() {
    let mut b = SceneBuilder::with_backend(Gpu::Other, Box::new(Recorder::default()), (256.0, 256.0));
    let st = b
        .targets
        .create_scene_targets(Gpu::Other, (256, 256), SamplerKey::CLAMP_LINEAR);
    b.push(Op::Clear {
        target: st.full,
        color: [0.0, 0.0, 0.0, 1.0],
    });
    let pixels = [255u8; 4 * 4 * 4];
    let level = [MipLevel {
        width: 4,
        height: 4,
        data: &pixels,
    }];
    let red = b.add_texture(&texture(&level)).unwrap();
    let blue = b.add_texture(&texture(&level)).unwrap();
    let sh = b
        .add_shader(ShaderDesc::from_wgsl("pass", SRC, "vs", "fs").unwrap())
        .unwrap();
    // Two images whose composites live in disjoint parts of the frame: they share memory.
    let mut images = Vec::new();
    for (id, main) in [(1, red), (2, blue)] {
        let composites =
            b.targets
                .create_composites(Gpu::Other, id, (128, 128), SamplerKey::CLAMP_LINEAR);
        let spec = ImageSpec {
            label: format!("img{id}"),
            size: Vec2::new(128.0, 128.0),
            alignment: Alignment::default(),
            main: TextureRef::Texture(main),
            main_sizes: ((4, 4), (4, 4)),
            animated: false,
            passthrough: false,
            fullscreen: false,
            composites,
            scene_target: st.full,
            final_to_scene: true,
        };
        let mut passes: Vec<ImagePass> = (0..3)
            .map(|i| ImagePass::new(format!("img{id}.p{i}"), sh))
            .collect();
        if id == 1 {
            passes[0].slot0_variants = vec![TextureRef::Texture(red), TextureRef::Texture(blue)];
        }
        images.push(add_image(&mut b, &spec, &passes).unwrap());
    }
    b.targets.mark_transient_all(&[st.full]);
    let mut scene = b.build().unwrap();

    let r = recorder(&scene);
    let built = r.built.as_ref().unwrap();
    assert_eq!((r.textures, r.shaders.len()), (2, 1));
    assert_eq!(built.passes, 6);
    assert_eq!(built.ops, scene.ops());
    // The aliasing plan reached the backend: shared targets, cleared by their first draw.
    assert!(!built.shares.is_empty(), "transient composites share memory");
    assert!(built.clear_first > 0);
    assert!(scene.targets().iter().any(|t| t.shares().is_some()));
    // At build the backend got the whole region.
    assert_eq!(r.region, scene.desc().dynamic.bytes());

    let vp = OrthoCamera::canonical(256.0, 256.0).view_projection();
    let audio16 = [0.0; 16];
    let audio32 = [0.0; 32];
    let audio64 = [0.0; 64];
    for i in 0..4 {
        let t = i as f32 * 0.1;
        scene.update_frame(&FrameInputs {
            time: t,
            daytime: 0.5,
            pointer: [0.0, 0.0],
            pointer_last: [0.0, 0.0],
            audio16: &audio16,
            audio32: &audio32,
            audio64: &audio64,
        });
        for img in &images {
            img.update(
                &mut scene,
                &ImageFrame {
                    view_projection: vp,
                    transform: NodeTransform {
                        origin: Vec3::new(128.0 + t, 128.0, 0.0),
                        scale: Vec3::ONE,
                        angle: t,
                    },
                    parallax: Vec3::ZERO,
                    scene_size: Vec2::new(256.0, 256.0),
                },
            );
        }
        scene.set_variant(images[0].passes[0], i % 2);
        assert!(scene.needs_redraw());
        scene.render_frame(&mut Frame::Other(&mut ()));
        // The dirty ranges carried every change: the backend's copy is the front's.
        assert_eq!(recorder(&scene).region, scene.desc().dynamic.bytes());
        assert!(!scene.desc().dynamic.shadow().is_dirty());
    }
    let r = recorder(&scene);
    assert_eq!(r.frames, 4);
    assert_eq!(
        r.slot0,
        [
            BoundTexture::Texture(red),
            BoundTexture::Texture(blue),
            BoundTexture::Texture(red),
            BoundTexture::Texture(blue),
        ]
    );
    // `g_Texture0Resolution` follows the variant in the shadow.
    let res = scene
        .uniform(images[0].passes[0], "g_Texture0Resolution")
        .unwrap();
    let at = res.offset as usize;
    let bytes = &scene.desc().dynamic.bytes()[at..at + 4];
    assert_eq!(f32::from_le_bytes(bytes.try_into().unwrap()), 4.0);

    // Visibility: one re-plan, the passes are disabled in the view.
    scene.set_enabled(Gpu::Other, &images[1].passes, false);
    assert_eq!(recorder(&scene).replans, 1);
    assert!(!scene.desc().passes[images[1].passes[0].index()].enabled);

    // A layer added after load: the backend gets the new passes, the front plans the frame.
    let (quad, ext) = scene.extend(Gpu::Other, |b| {
        let v = b.add_vertices(&unit_quad());
        let p = b.add_pass(PassDesc::new("layer", sh, st.full, Geometry::Vertices(v)))?;
        b.push(Op::Draw(p));
        Ok::<_, lumenwall_render::RenderError>(p)
    });
    let ext = ext.unwrap();
    let quad = quad.unwrap();
    assert_eq!(ext.passes, [quad]);
    assert_eq!(recorder(&scene).extended, [6]);
    let mut ops = scene.ops().to_vec();
    ops.extend(ext.ops);
    // Reordered after build: shared targets get memory of their own (nothing to allocate here)
    // and the bindings are recreated once, before the next frame.
    scene.set_ops(ops);
    scene.render_frame(&mut Frame::Other(&mut ()));
    let r = recorder(&scene);
    assert_eq!((r.rebinds, r.frames), (1, 5));
    assert!(scene.targets().iter().all(|t| t.shares().is_none()));
    assert_eq!(r.region, scene.desc().dynamic.bytes());
    assert_eq!(
        scene.read_target(Gpu::Other, st.full).unwrap().len(),
        256 * 256 * 4
    );
}

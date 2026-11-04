//! Cover art of the media playing as scene textures: `$mediaThumbnail` and
//! `$mediaPreviousThumbnail` (spec 04 §12.2 and §16.5, spec 07 §16.4).
//!
//! Both are render targets of the scene's own pool, created only when a slot of the scene
//! names one (`TextureCache::media_target`) and released with the scene. A scene that names
//! neither has no [`MediaThumbnails`]: no target, no subscription, nothing per frame. On a
//! change of cover, never per frame:
//!
//! 1. when `$mediaThumbnail` holds a cover, it is copied into `$mediaPreviousThumbnail` on the
//!    GPU (pixels, size and resolution, spec 04 §16.5);
//! 2. the new cover is written into `$mediaThumbnail`, reallocated only when its size changes;
//!    without art it goes back to 1×1 transparent ("not ready");
//! 3. after a reallocation the bind groups are rebuilt once, and the `g_TextureNResolution` of
//!    the slots sampling the thumbnails follow their new size.
//!
//! The pixels are read from the media hub's decoded artwork for the upload only: no copy stays
//! on the CPU, and the previous cover never comes back from the GPU.

use std::time::{Duration, Instant};

use lumenwall_audio::media::Artwork;
use lumenwall_render::dynbuf::UniformSlot;
use lumenwall_render::{GpuContext, PassId, RenderScene, RtId, SceneBuilder};

use super::SceneRuntime;
use crate::texture::MediaThumbnail;

/// `g_TextureNResolution` by slot (the renderer writes them when a pass is added).
const RESOLUTION: [&str; 8] = [
    "g_Texture0Resolution",
    "g_Texture1Resolution",
    "g_Texture2Resolution",
    "g_Texture3Resolution",
    "g_Texture4Resolution",
    "g_Texture5Resolution",
    "g_Texture6Resolution",
    "g_Texture7Resolution",
];

/// Counters of the cover changes of a scene (statistics and measurements).
#[derive(Clone, Copy, Debug, Default)]
pub struct MediaArtStats {
    /// Changes of cover applied (to none included); repeated states of the same cover are not
    /// counted, they cost a comparison.
    pub changes: u32,
    /// Changes that reallocated a thumbnail (new size) and rebuilt the bind groups.
    pub rebinds: u32,
    /// Pixel bytes written to the GPU.
    pub uploaded_bytes: u64,
    /// CPU time of the changes (GPU copy, upload, bind groups), in total and the longest one.
    pub time: Duration,
    pub longest: Duration,
}

/// What a scene sampling the media thumbnails keeps.
pub(crate) struct MediaThumbnails {
    /// `[current, previous]`; the current one always exists (source of the previous one).
    targets: [Option<RtId>; 2],
    /// Resolution uniforms of the slots sampling each thumbnail.
    resolutions: Vec<(MediaThumbnail, UniformSlot)>,
    /// `(key, width, height)` of the cover in `$mediaThumbnail`; `None` while it holds none.
    current: Option<(u64, u32, u32)>,
    stats: MediaArtStats,
}

impl MediaThumbnails {
    /// `None` when the scene samples no thumbnail.
    pub(crate) fn new(
        targets: [Option<RtId>; 2],
        resolutions: Vec<(MediaThumbnail, UniformSlot)>,
    ) -> Option<Self> {
        targets[0]?;
        Some(Self {
            targets,
            resolutions,
            current: None,
            stats: MediaArtStats::default(),
        })
    }

    pub(crate) fn targets(&self) -> [Option<RtId>; 2] {
        self.targets
    }

    pub(crate) fn stats(&self) -> MediaArtStats {
        self.stats
    }

    /// Applies the cover `art` (`None`: nothing plays, or no art). Returns whether anything
    /// changed; the same cover again costs a comparison.
    pub(crate) fn set_artwork(
        &mut self,
        ctx: &GpuContext,
        scene: &mut RenderScene,
        art: Option<&Artwork>,
    ) -> bool {
        // Ready only when the decoded image is not empty (spec 04 §16.5). The media hub keeps
        // covers within `lumenwall_audio::media::MAX_SIDE`; anything the GPU cannot hold is no
        // cover either.
        let max = ctx.device().limits().max_texture_dimension_2d;
        let art = art.filter(|a| {
            (1..=max).contains(&a.width)
                && (1..=max).contains(&a.height)
                && a.rgba.len() >= a.width as usize * a.height as usize * 4
        });
        let key = art.map(|a| (a.key, a.width, a.height));
        if key == self.current {
            return false;
        }
        let t0 = Instant::now();
        let Some(current) = self.targets[0] else {
            return false;
        };
        let mut realloc = false;
        let pool = scene.targets_mut();
        // 1. The cover shown becomes the previous one: a copy on the GPU.
        if self.current.is_some()
            && let Some(previous) = self.targets[1]
        {
            let size = pool.get(current).size();
            if pool.get(previous).size() != size {
                pool.resize(ctx, previous, size);
                realloc = true;
            }
            let mut enc = ctx
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("lumenwall.media-thumbnail"),
                });
            enc.copy_texture_to_texture(
                pool.get(current).texture().as_image_copy(),
                pool.get(previous).texture().as_image_copy(),
                extent(size),
            );
            // Submitted now: the upload below lands at the start of the next submission.
            ctx.queue().submit([enc.finish()]);
        }
        // 2. The new cover, or 1×1 transparent.
        let (size, pixels): ((u32, u32), &[u8]) = match art {
            Some(a) => (
                (a.width, a.height),
                &a.rgba[..a.width as usize * a.height as usize * 4],
            ),
            None => ((1, 1), &[0; 4]),
        };
        if pool.get(current).size() != size {
            pool.resize(ctx, current, size);
            realloc = true;
        }
        ctx.queue().write_texture(
            pool.get(current).texture().as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size.0 * 4),
                rows_per_image: Some(size.1),
            },
            extent(size),
        );
        // 3. Bindings and resolutions follow the new sizes.
        if realloc && let Err(e) = scene.rebuild_bindings(ctx) {
            tracing::error!("media thumbnails not rebound: {e}");
        }
        for (which, slot) in &self.resolutions {
            if let Some(t) = self.targets[*which as usize] {
                let res = scene.targets().get(t).resolution();
                scene.shadow_mut().write_floats(slot, &res);
            }
        }
        scene.mark_dirty();
        self.current = key;
        let dt = t0.elapsed();
        let s = &mut self.stats;
        s.changes += 1;
        s.rebinds += u32::from(realloc);
        s.uploaded_bytes += pixels.len() as u64;
        s.time += dt;
        s.longest = s.longest.max(dt);
        tracing::debug!(
            "media thumbnail: {}x{}{}, {:.3} ms",
            size.0,
            size.1,
            if realloc { ", bind groups rebuilt" } else { "" },
            dt.as_secs_f64() * 1000.0
        );
        true
    }
}

fn extent((width, height): (u32, u32)) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

/// The resolution uniforms of the slots that sample a media thumbnail, once the passes exist:
/// `media[k]` lists the `(slot, thumbnail)` of the image's pass `k` (`PreparedPass::media`).
pub(crate) fn resolution_uniforms<'a>(
    b: &SceneBuilder<'_>,
    passes: &[PassId],
    media: impl IntoIterator<Item = &'a [(usize, MediaThumbnail)]>,
) -> Vec<(MediaThumbnail, UniformSlot)> {
    let mut out = Vec::new();
    for (&pass, slots) in passes.iter().zip(media) {
        for &(n, which) in slots {
            if let Some(slot) = RESOLUTION.get(n).and_then(|name| b.uniform(pass, name)) {
                out.push((which, slot));
            }
        }
    }
    out
}

impl SceneRuntime {
    /// Whether the scene samples `$mediaThumbnail` or `$mediaPreviousThumbnail`: only then
    /// does the engine follow the media playing for its cover (a layer that scripts create
    /// later is served while the scene's scripts handle media events).
    pub fn wants_media_artwork(&self) -> bool {
        self.media.is_some()
    }

    /// The cover of the media playing (`None`: nothing plays, or the media has no art). A new
    /// cover moves the one shown to `$mediaPreviousThumbnail` and is uploaded now, once; the
    /// same cover again, or a scene sampling no thumbnail, costs a comparison. Static scenes
    /// are marked for one redraw.
    pub fn set_media_artwork(&mut self, ctx: &GpuContext, art: Option<&Artwork>) {
        if let Some(m) = &mut self.media {
            m.set_artwork(ctx, &mut self.scene, art);
        }
    }

    /// Counters of the cover changes; `None` for a scene that samples no thumbnail.
    pub fn media_artwork_stats(&self) -> Option<MediaArtStats> {
        self.media.as_ref().map(|m| m.stats())
    }

    /// A layer created after load samples thumbnails (`targets` from the layers' texture
    /// cache, the same pool as the scene's).
    pub(super) fn add_media_users(
        &mut self,
        targets: [Option<RtId>; 2],
        resolutions: Vec<(MediaThumbnail, UniformSlot)>,
    ) {
        match &mut self.media {
            Some(m) => {
                // A target created by this layer starts transparent; the next change fills it.
                m.targets = targets;
                m.resolutions.extend(resolutions);
            }
            None => self.media = MediaThumbnails::new(targets, resolutions).map(Box::new),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use lumenwall_render::headless::read_texture_rgba8;
    use lumenwall_render::math::{UvRange, pointer_disabled};
    use lumenwall_render::{ContextOptions, GpuContext, SceneBuilder};

    use super::*;
    use crate::scene::{FrameState, SceneOptions, ScreenState, build_scene};
    use crate::texture::TextureCache;
    use crate::wallpaper::Wallpaper;

    const NAMES: [&str; 2] = ["$mediaThumbnail", "$mediaPreviousThumbnail"];

    fn gpu() -> Option<GpuContext> {
        match GpuContext::new_headless(&ContextOptions::default()) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("skipped: no GPU adapter ({e})");
                None
            }
        }
    }

    /// A cover of one colour.
    fn solid(key: u64, (w, h): (u32, u32), rgba: [u8; 4]) -> Artwork {
        Artwork {
            png: b"\x89PNG fake".to_vec(),
            rgba: rgba.repeat((w * h) as usize),
            width: w,
            height: h,
            key,
        }
    }

    /// A wallpaper folder written for the test, and an empty assets folder: no Wallpaper
    /// Engine content, so the shaders are the stand-ins, whose fragment shader shows slot 0.
    struct TempWallpaper(PathBuf);

    impl TempWallpaper {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let root =
                std::env::temp_dir().join(format!("lumenwall-media-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            for (path, text) in files {
                let p = root.join("wallpaper").join(path);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            }
            std::fs::create_dir_all(root.join("assets")).unwrap();
            Self(root)
        }

        fn load(&self) -> Wallpaper {
            Wallpaper::load(
                &self.0.join("wallpaper"),
                &self.0.join("assets"),
                &BTreeMap::new(),
            )
            .expect("wallpaper")
        }
    }

    impl Drop for TempWallpaper {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A 64×32 scene on white: on the left a 32×32 image whose texture is `left`, on the right
    /// one whose texture is `right` (none when empty), both blended over the clear colour.
    fn two_images(name: &str, left: &str, right: &str) -> TempWallpaper {
        let material = |t: &str| {
            let textures = if t.is_empty() {
                String::new()
            } else {
                format!(r#""{t}""#)
            };
            format!(
                r#"{{ "passes": [ {{ "shader": "genericimage2", "blending": "translucent", "textures": [ {textures} ] }} ] }}"#
            )
        };
        let (ml, mr) = (material(left), material(right));
        TempWallpaper::new(
            name,
            &[
                (
                    "project.json",
                    r#"{ "type": "scene", "title": "media", "file": "scene.json" }"#,
                ),
                (
                    "scene.json",
                    r#"{
                      "camera": { "center": "0 0 -1", "eye": "0 0 0", "up": "0 1 0" },
                      "general": { "orthogonalprojection": { "width": 64, "height": 32 },
                                   "clearcolor": "1 1 1" },
                      "objects": [
                        { "id": 1, "name": "left", "image": "models/left.json",
                          "origin": "16 16 0", "size": "32 32" },
                        { "id": 2, "name": "right", "image": "models/right.json",
                          "origin": "48 16 0", "size": "32 32" }
                      ]
                    }"#,
                ),
                (
                    "models/left.json",
                    r#"{ "material": "materials/left.json" }"#,
                ),
                (
                    "models/right.json",
                    r#"{ "material": "materials/right.json" }"#,
                ),
                ("materials/left.json", &ml),
                ("materials/right.json", &mr),
            ],
        )
    }

    fn build(ctx: &GpuContext, wp: &mut Wallpaper) -> SceneRuntime {
        let mut shaders = crate::shaders::default_provider();
        build_scene(
            ctx,
            &mut wp.project,
            &wp.vfs,
            shaders.as_mut(),
            &SceneOptions {
                output_size: (64, 32),
                ..SceneOptions::default()
            },
        )
        .expect("scene")
    }

    fn frame(ctx: &GpuContext, rt: &mut SceneRuntime, wp: &mut Wallpaper, time: f64) {
        let (a16, a32, a64) = ([0.0f32; 16], [0.0f32; 32], [0.0f32; 64]);
        rt.update(
            ctx,
            &mut wp.project,
            &FrameState {
                time,
                dt: 1.0 / 30.0,
                daytime: 0.5,
                pointer: pointer_disabled(UvRange::FULL),
                audio16: &a16,
                audio32: &a32,
                audio64: &a64,
                screen: ScreenState::default(),
            },
        );
        let mut enc = ctx.device().create_command_encoder(&Default::default());
        rt.scene.encode(ctx, &mut enc);
        ctx.queue().submit([enc.finish()]);
    }

    /// Centre pixels of the left and right images in the scene target.
    fn halves(ctx: &GpuContext, rt: &SceneRuntime) -> ([u8; 4], [u8; 4]) {
        let target = rt.scene.targets().get(rt.full);
        let (w, _) = target.size();
        let px = read_texture_rgba8(ctx, target.texture()).expect("readback");
        let at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        (at(16, 16), at(48, 16))
    }

    fn sizes(rt: &SceneRuntime) -> [(u32, u32); 2] {
        let m = rt.media.as_ref().expect("thumbnails");
        m.targets()
            .map(|t| t.map_or((0, 0), |t| rt.scene.targets().get(t).size()))
    }

    const WHITE: [u8; 4] = [255; 4];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    /// Creation at load, a change of cover (GPU copy to the previous one, upload, rebinds only
    /// when a size changes), the same cover again (nothing), no art (1×1 transparent), and the
    /// memory given back with the scene.
    #[test]
    fn thumbnails_follow_the_cover_and_go_with_the_scene() {
        let Some(ctx) = gpu() else {
            return;
        };
        let dir = two_images("lifecycle", NAMES[0], NAMES[1]);
        let mut wp = dir.load();
        let baseline = ctx.memory().bytes();
        let mut rt = build(&ctx, &mut wp);
        assert_eq!(rt.stats.images, 2);
        assert!(rt.wants_media_artwork());
        assert_eq!(sizes(&rt), [(1, 1), (1, 1)], "1×1 until a cover comes");
        frame(&ctx, &mut rt, &mut wp, 1.0);
        assert_eq!(halves(&ctx, &rt), (WHITE, WHITE), "transparent");
        assert!(!rt.animated && !rt.scene.needs_redraw(), "a static scene");

        // First cover: nothing to move to the previous one yet.
        let red = solid(1, (2, 2), RED);
        rt.set_media_artwork(&ctx, Some(&red));
        assert!(rt.scene.needs_redraw(), "the static scene redraws once");
        assert_eq!(sizes(&rt), [(2, 2), (1, 1)]);
        frame(&ctx, &mut rt, &mut wp, 2.0);
        assert_eq!(halves(&ctx, &rt), (RED, WHITE));
        let s = rt.media_artwork_stats().unwrap();
        assert_eq!((s.changes, s.rebinds, s.uploaded_bytes), (1, 1, 16));

        // The same cover again (every playback or timeline event carries it): nothing.
        rt.set_media_artwork(&ctx, Some(&red));
        assert_eq!(rt.media_artwork_stats().unwrap().changes, 1);
        assert!(!rt.scene.needs_redraw());

        // Another size: the previous thumbnail takes the current one's pixels and size.
        rt.set_media_artwork(&ctx, Some(&solid(2, (4, 4), GREEN)));
        assert_eq!(sizes(&rt), [(4, 4), (2, 2)]);
        frame(&ctx, &mut rt, &mut wp, 3.0);
        assert_eq!(halves(&ctx, &rt), (GREEN, RED));
        rt.set_media_artwork(&ctx, Some(&solid(3, (4, 4), BLUE)));
        frame(&ctx, &mut rt, &mut wp, 4.0);
        assert_eq!(halves(&ctx, &rt), (BLUE, GREEN));
        let rebinds = rt.media_artwork_stats().unwrap().rebinds;

        // Same sizes on both sides: a GPU copy and an upload, no reallocation.
        rt.set_media_artwork(&ctx, Some(&solid(4, (4, 4), RED)));
        frame(&ctx, &mut rt, &mut wp, 5.0);
        assert_eq!(halves(&ctx, &rt), (RED, BLUE));
        assert_eq!(rt.media_artwork_stats().unwrap().rebinds, rebinds);

        // Nothing plays: the current one goes back to 1×1 transparent, the previous one keeps
        // the last cover; again: nothing.
        rt.set_media_artwork(&ctx, None);
        assert_eq!(sizes(&rt), [(1, 1), (4, 4)]);
        frame(&ctx, &mut rt, &mut wp, 6.0);
        assert_eq!(halves(&ctx, &rt), (WHITE, RED));
        rt.set_media_artwork(&ctx, None);
        // An empty image is no cover (spec 04 §16.5: ready only when not empty): no change.
        rt.set_media_artwork(&ctx, Some(&solid(5, (0, 0), RED)));
        let s = rt.media_artwork_stats().unwrap();
        assert_eq!(s.changes, 5);
        assert_eq!(s.uploaded_bytes, 16 + 64 * 3 + 4);
        assert!(!rt.scene.needs_redraw());

        drop(rt);
        ctx.wait_idle();
        assert_eq!(ctx.memory().bytes(), baseline, "released with the scene");
    }

    /// A scene that names no thumbnail creates none and ignores covers.
    #[test]
    fn scenes_without_thumbnails_pay_nothing() {
        let Some(ctx) = gpu() else {
            return;
        };
        let dir = two_images("unused", "", "");
        let mut wp = dir.load();
        let mut rt = build(&ctx, &mut wp);
        assert!(!rt.wants_media_artwork());
        assert!(rt.media_artwork_stats().is_none());
        let named = |rt: &SceneRuntime| {
            rt.scene
                .targets()
                .iter()
                .filter(|t| NAMES.contains(&t.label()))
                .count()
        };
        assert_eq!(named(&rt), 0);
        frame(&ctx, &mut rt, &mut wp, 1.0);
        let targets = rt.scene.targets().len();
        rt.set_media_artwork(&ctx, Some(&solid(1, (4, 4), RED)));
        assert!(!rt.scene.needs_redraw(), "no redraw");
        assert_eq!((rt.scene.targets().len(), named(&rt)), (targets, 0));
    }

    /// Only what a slot names exists: the current thumbnail alone, or both for the previous
    /// one (its pixels come from the current one). Neither is a file.
    #[test]
    fn targets_are_created_on_first_use() {
        assert_eq!(
            MediaThumbnail::from_name(NAMES[0]),
            Some(MediaThumbnail::Current)
        );
        assert_eq!(
            MediaThumbnail::from_name(NAMES[1]),
            Some(MediaThumbnail::Previous)
        );
        assert_eq!(MediaThumbnail::from_name("mediaThumbnail"), None);
        let Some(ctx) = gpu() else {
            return;
        };
        let dir = two_images("first-use", "", "");
        let wp = dir.load();
        let mut b = SceneBuilder::new(&ctx, (64.0, 64.0));
        let mut cache = TextureCache::default();
        assert!(cache.exists(&wp.vfs, NAMES[1]), "always resolvable");
        assert_eq!(cache.media_targets(), [None, None]);
        let current = cache.media_target(&mut b, MediaThumbnail::Current);
        assert_eq!(cache.media_targets(), [Some(current), None]);
        assert_eq!(cache.media_target(&mut b, MediaThumbnail::Current), current);
        let previous = cache.media_target(&mut b, MediaThumbnail::Previous);
        assert_eq!(cache.media_targets(), [Some(current), Some(previous)]);
        assert_eq!(b.targets.get(previous).size(), (1, 1));
        assert_eq!(b.targets.get(previous).label(), NAMES[1]);
        assert!(cache.get(&mut b, &wp.vfs, NAMES[0]).is_none(), "not a file");

        let mut b = SceneBuilder::new(&ctx, (64.0, 64.0));
        let mut cache = TextureCache::default();
        cache.media_target(&mut b, MediaThumbnail::Previous);
        assert!(cache.media_targets().iter().all(Option::is_some));
    }

    /// A procedural cover framed in white: four coloured quadrants (orientation), or
    /// concentric squares.
    fn cover(key: u64, side: u32, rings: bool) -> Arc<Artwork> {
        let border = side / 20;
        let mut rgba = Vec::with_capacity((side * side * 4) as usize);
        for y in 0..side {
            for x in 0..side {
                let edge = x < border || y < border || x >= side - border || y >= side - border;
                let c = if edge {
                    [255, 255, 255, 255]
                } else if rings {
                    let d = x.abs_diff(side / 2).max(y.abs_diff(side / 2));
                    if (d / (side / 16)).is_multiple_of(2) {
                        [150, 40, 200, 255]
                    } else {
                        [250, 140, 20, 255]
                    }
                } else {
                    match (x < side / 2, y < side / 2) {
                        (true, true) => [220, 30, 30, 255],
                        (false, true) => [30, 200, 60, 255],
                        (true, false) => [30, 60, 230, 255],
                        (false, false) => [240, 220, 30, 255],
                    }
                };
                rgba.extend_from_slice(&c);
            }
        }
        Arc::new(Artwork {
            png: b"\x89PNG fake".to_vec(),
            rgba,
            width: side,
            height: side,
            key,
        })
    }

    /// 3372348904 ("Soren <3", 4 thumbnail slots in 2 images): the fake media source of the
    /// media tests feeds the scene the way the engine loop does (`pump_media`), and the frames
    /// around each change of track are written as PNG files to `LUMENWALL_MEDIA_RENDER_DIR`
    /// (default: `lumenwall-media-thumbnail` in the system's temporary folder). Prints the cost
    /// of each change and of the frame after it (run it with `--release` for the timings).
    #[test]
    #[ignore = "needs the Workshop item 3372348904, the WE assets and a GPU"]
    fn soren_shows_the_cover_of_the_track() {
        use std::time::{Duration, Instant};

        use lumenwall_audio::media::{MediaState, MediaWaker};
        use lumenwall_render::{
            ClampMode, ComposeSettings, Compositor, HeadlessOutput, ScalingMode,
        };

        use crate::script::media::MediaFeed;
        use crate::script::media::tests::{fake_hub, playing};

        let resolver = crate::assets::WallpaperResolver::new();
        let (Ok(dir), Ok(assets)) = (
            resolver.workshop_dir("3372348904"),
            resolver.assets_dir(None),
        ) else {
            eprintln!("skipped: 3372348904 or the WE assets not found");
            return;
        };
        let out_dir = std::env::var_os("LUMENWALL_MEDIA_RENDER_DIR").map_or_else(
            || std::env::temp_dir().join("lumenwall-media-thumbnail"),
            PathBuf::from,
        );
        std::fs::create_dir_all(&out_dir).unwrap();
        let Some(ctx) = gpu() else {
            return;
        };
        let (w, h) = (1080, 1440);
        let mut wp = Wallpaper::load(&dir, &assets, &BTreeMap::new()).expect("wallpaper");
        // The effects sampling the covers ("Current album cover", "Previous album cover" on 4
        // images) start hidden; their `mediaThumbnailChanged` scripts show them once a cover
        // is known. Effect visibility is read once, at load (spec 04 Q8,
        // README "Script effects on fixed state"): shown from the start here, as Wallpaper
        // Engine shows them with a cover.
        let cover_effects: Vec<lumenwall_scene::SettingId> = wp
            .project
            .scene
            .as_ref()
            .unwrap()
            .objects
            .iter()
            .filter_map(|o| match &o.kind {
                lumenwall_scene::ObjectKind::Image(i) => Some(i),
                _ => None,
            })
            .flat_map(|i| &i.effects)
            .filter(|e| {
                e.name
                    .as_deref()
                    .is_some_and(|n| n.ends_with("album cover"))
            })
            .map(|e| e.visible)
            .collect();
        assert_eq!(cover_effects.len(), 8);
        for id in cover_effects {
            wp.project
                .settings
                .set(id, lumenwall_scene::Value::from_bool(true));
        }
        let mut shaders = crate::shaders::default_provider();
        let mut rt = build_scene(
            &ctx,
            &mut wp.project,
            &wp.vfs,
            shaders.as_mut(),
            &SceneOptions {
                output_size: (w, h),
                particles: false,
                display: Some(((w, h), ScalingMode::Default)),
                ..SceneOptions::default()
            },
        )
        .expect("scene");
        assert!(rt.wants_media_artwork());
        let m = rt.media.as_ref().unwrap();
        assert!(m.targets().iter().all(Option::is_some));
        eprintln!(
            "{} pass(es); {} resolution uniform(s) follow the covers",
            rt.stats.passes,
            m.resolutions.len()
        );

        let out = HeadlessOutput::new(&ctx, w, h);
        let mut compositor = Compositor::new(&ctx);
        let mut composer = compositor.create_output(
            &ctx,
            HeadlessOutput::FORMAT,
            rt.scene.targets().get(rt.full),
            ComposeSettings {
                scaling: ScalingMode::Default,
                clamp: ClampMode::Clamp,
                flip_vertical: false,
            },
        );
        let (a16, a32, a64) = ([0.3f32; 16], [0.3f32; 32], [0.3f32; 64]);
        let mut render = |rt: &mut SceneRuntime, wp: &mut Wallpaper, i: u32| -> Duration {
            let t0 = Instant::now();
            rt.update(
                &ctx,
                &mut wp.project,
                &FrameState {
                    time: 1.0 + f64::from(i) / 30.0,
                    dt: 1.0 / 30.0,
                    daytime: 0.5,
                    pointer: pointer_disabled(UvRange::FULL),
                    audio16: &a16,
                    audio32: &a32,
                    audio64: &a64,
                    screen: ScreenState {
                        size: (w, h),
                        ..ScreenState::default()
                    },
                },
            );
            let mut enc = ctx.device().create_command_encoder(&Default::default());
            rt.scene.encode(&ctx, &mut enc);
            composer.update(&ctx, out.size(), rt.logical);
            compositor.encode(
                &mut enc,
                &composer,
                out.view(),
                [0.0, 0.0, w as f32, h as f32],
                Some(wgpu::Color::BLACK),
            );
            ctx.queue().submit([enc.finish()]);
            t0.elapsed()
        };

        // The fake source, through the scenes' media feed as in the engine loop.
        let (hub, sink) = fake_hub();
        let mut feed = MediaFeed::default();
        feed.sync_with(&hub, true, || -> MediaWaker { Arc::new(|| {}) });
        let publish = |s: MediaState| sink.lock().unwrap().as_ref().unwrap().publish(s);
        let deliver = |feed: &mut MediaFeed, rt: &mut SceneRuntime| -> (Duration, Duration) {
            let t0 = Instant::now();
            let m = feed.poll().expect("a change");
            if let Some(enabled) = m.status {
                rt.media_status(enabled);
            }
            let t1 = Instant::now();
            rt.set_media_artwork(&ctx, m.artwork.as_deref());
            let art = t1.elapsed();
            rt.media_changed(m.snapshot);
            (t0.elapsed(), art)
        };

        let steps: [(&str, Option<Arc<Artwork>>); 4] = [
            ("Track A", Some(cover(1, 300, false))),
            ("Track C (same size)", Some(cover(3, 300, true))),
            ("Track B (512 px)", Some(cover(2, 512, true))),
            ("No cover", None),
        ];
        let mut i = 0;
        let mut steady = Vec::new();
        for _ in 0..30 {
            let d = render(&mut rt, &mut wp, i);
            // After the first frames (pipelines, first uploads).
            if i >= 5 {
                steady.push(d);
            }
            i += 1;
        }
        let png = |name: &str| {
            out.write_png(&ctx, &out_dir.join(format!("soren-{name}.png")))
                .unwrap()
        };
        png("0-no-media");
        let memory = ctx.memory().bytes();
        let mut report = Vec::new();
        for (k, (title, art)) in steps.into_iter().enumerate() {
            publish(playing(title, art));
            let (update, cover_time) = deliver(&mut feed, &mut rt);
            let next = render(&mut rt, &mut wp, i);
            i += 1;
            // The same state again (as with a timeline event): the cover is not uploaded again.
            let changes = rt.media_artwork_stats().unwrap().changes;
            rt.set_media_artwork(&ctx, feed.current().unwrap().artwork.as_deref());
            assert_eq!(rt.media_artwork_stats().unwrap().changes, changes);
            for f in 0..45 {
                let d = render(&mut rt, &mut wp, i);
                if f > 5 {
                    steady.push(d);
                }
                if f == 7 {
                    png(&format!("{}-transition", k + 1));
                }
                i += 1;
            }
            png(&format!("{}-settled", k + 1));
            report.push((title, update, cover_time, next));
        }
        // `$mediaPreviousThumbnail` in the wallpaper's own effect: its fade is a property
        // animation (`getAnimation()` is `undefined` in Lumenwall), so "Previous album cover"
        // keeps `multiply` = 0. Forced to 1, the last cover (B) shows through its gradient.
        let scene = wp.project.scene.as_ref().unwrap();
        let fades: Vec<lumenwall_scene::SettingId> = scene
            .objects
            .iter()
            .filter_map(|o| match &o.kind {
                lumenwall_scene::ObjectKind::Image(i) => Some(i),
                _ => None,
            })
            .flat_map(|i| &i.effects)
            .filter(|e| e.name.as_deref() == Some("Previous album cover"))
            .flat_map(|e| &e.overrides)
            .flat_map(|o| &o.constants)
            .filter(|c| wp.project.strings.resolve(c.name) == "multiply")
            .map(|c| c.setting)
            .collect();
        assert!(!fades.is_empty());
        for id in fades {
            wp.project
                .settings
                .set(id, lumenwall_scene::Value::from_float(1.0));
        }
        super::super::write_live(&mut rt.scene, &rt.live, &wp.project);
        render(&mut rt, &mut wp, i);
        png("5-previous-forced");
        steady.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        eprintln!(
            "steady frame (update + encode + submit): median {:.3} ms, max {:.3} ms",
            ms(steady[steady.len() / 2]),
            ms(*steady.last().unwrap())
        );
        for (title, update, cover_time, next) in &report {
            eprintln!(
                "{title}: media update {:.3} ms (cover {:.3} ms), next frame {:.3} ms",
                ms(*update),
                ms(*cover_time),
                ms(*next)
            );
            // 30 fps pacing: the change and its frame fit in one frame interval.
            #[cfg(not(debug_assertions))]
            assert!(*update + *next < Duration::from_millis(33), "{title}");
        }
        let s = rt.media_artwork_stats().unwrap();
        eprintln!(
            "{} changes, {} rebind(s), {:.0} KiB uploaded, {:.3} ms in total, longest {:.3} ms; tracked GPU memory {:.2} MiB before the first cover, {:.2} MiB after; PNGs in {}",
            s.changes,
            s.rebinds,
            s.uploaded_bytes as f64 / 1024.0,
            ms(s.time),
            ms(s.longest),
            memory as f64 / 1048576.0,
            ctx.memory().bytes() as f64 / 1048576.0,
            out_dir.display()
        );
        assert_eq!(s.changes, 4);
        rt.shutdown_scripts(&mut wp.project);
    }
}

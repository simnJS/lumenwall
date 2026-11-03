//! [`VideoPlayer`]: decoding thread + GPU upload (or zero-copy binding) and YUV → RGBA
//! conversion.

use std::path::Path;
use std::time::{Duration, Instant};

use lumenwall_render::GpuContext;
use lumenwall_render::targets::RenderTarget;

use crate::queue::Notify;
use crate::{DecodePath, VideoError, VideoInfo, VideoInput, VideoOptions};

/// Playback counters (debug statistics, `LUMENWALL_STATS`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VideoStats {
    /// Frames decoded.
    pub decoded: u64,
    /// Frames dropped by the decoding thread because no display instant shows them (video
    /// faster than the cap): never copied nor uploaded.
    pub skipped: u64,
    /// Frames uploaded (or bound) and converted.
    pub shown: u64,
    /// Frames that were due together with a newer one (late wake-up): never uploaded.
    pub late: u64,
    /// Times the clock was resynchronised because decoding fell behind.
    pub resyncs: u64,
    /// Completed loops.
    pub loops: u64,
    /// Time spent uploading planes (or binding images) on the calling thread.
    pub upload: Duration,
}

#[cfg(feature = "ffmpeg")]
mod imp {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::thread::JoinHandle;

    use lumenwall_render::targets::{RenderTargetPool, RtId};
    use lumenwall_render::texture::SamplerKey;
    use lumenwall_render::yuv::{
        ChromaPlanes, PlaneDepth, YuvColor, YuvConverter, YuvLayout, YuvPlanes, YuvSource,
    };

    use super::*;
    use crate::HwDecode;
    use crate::ffmpeg::{Attempt, CpuFrame, DecodedFrame, FramePool, GpuFrame, Source, vulkan};
    use crate::pacing::{Decimator, MAX_LAG, PlayClock};
    use crate::queue::{OFFLINE_TIMEOUT, Queue};
    use crate::select::{self, AdapterHint};

    /// A decoded image of the zero-copy path wrapped for sampling (created once per image
    /// of FFmpeg's pool).
    struct WrappedImage {
        source: YuvSource,
        _views: [wgpu::TextureView; 2],
        _texture: wgpu::Texture,
    }

    /// Zero-copy state: wgpu handles and the wrapped images of FFmpeg's frame pool.
    struct ZeroCopy {
        raw: ash::Device,
        device: wgpu::Device,
        queue: wgpu::Queue,
        pool: usize,
        images: HashMap<u64, WrappedImage>,
    }

    /// The frame the output image is (or will be) converted from.
    enum Current {
        /// The CPU-uploaded planes.
        Planes,
        /// A decoded image on the GPU.
        Gpu(GpuFrame),
    }

    /// A looping video decoded on its own thread and converted to an RGBA render target.
    pub struct VideoPlayer {
        queue: Arc<Queue<DecodedFrame>>,
        pool: FramePool,
        thread: Option<JoinHandle<Source>>,
        info: VideoInfo,
        path: DecodePath,
        converter: YuvConverter,
        planes: YuvPlanes,
        zero_copy: Option<ZeroCopy>,
        current: Current,
        color: YuvColor,
        targets: RenderTargetPool,
        rt: RtId,
        clock: PlayClock,
        /// Media time of the current frame.
        shown: f64,
        convert_pending: bool,
        generation: u64,
        stats: VideoStats,
    }

    impl VideoPlayer {
        /// Opens the video file at `path`; see [`Self::open_input`].
        pub fn open(
            ctx: &GpuContext,
            path: &Path,
            opts: &VideoOptions,
            notify: Option<Notify>,
        ) -> Result<Self, VideoError> {
            Self::open_input(ctx, &VideoInput::File(path.to_path_buf()), opts, notify)
        }

        /// Opens `input`, decodes its first frame (picking the decoding path, sizing the GPU
        /// resources) and starts the decoding thread. `notify` is called from that thread
        /// when a frame arrives while none was queued (wake the render loop).
        pub fn open_input(
            ctx: &GpuContext,
            input: &VideoInput,
            opts: &VideoOptions,
            notify: Option<Notify>,
        ) -> Result<Self, VideoError> {
            let t0 = Instant::now();
            let adapter = AdapterHint::from_wgpu(&ctx.adapter().get_info());
            let mut attempts = Vec::new();
            if opts.hwdec == HwDecode::Auto && select::zero_copy_allowed() {
                match vulkan::shared_device(ctx) {
                    Ok(dev) => attempts.push(Attempt::Shared(dev)),
                    Err(e) => tracing::debug!("video: no zero-copy decoding: {e}"),
                }
            }
            attempts.extend(
                select::candidates(opts.hwdec, &adapter, cfg!(windows))
                    .into_iter()
                    .map(Attempt::Device),
            );
            let pool = FramePool::default();
            let (source, first) = Source::open(input, attempts, &adapter, pool.clone())?;
            let info = source.info().clone();
            let decode_path = source.decode_path();
            let frame_size = match &first {
                DecodedFrame::Cpu(f) => (f.layout.width, f.layout.height),
                DecodedFrame::Gpu(g) => g.visible,
            };
            let display = if info.coded_size == frame_size && info.display_size.0 > 0 {
                info.display_size
            } else {
                frame_size
            };
            let converter = YuvConverter::new(ctx);
            let plane_layout = match &first {
                DecodedFrame::Cpu(f) => f.layout,
                // Placeholder planes (never sampled) on the zero-copy path.
                DecodedFrame::Gpu(_) => YuvLayout {
                    width: 2,
                    height: 2,
                    chroma_width: 1,
                    chroma_height: 1,
                    ..YuvLayout::default()
                },
            };
            let planes = converter.create_planes(ctx, &plane_layout);
            let zero_copy = match (&first, vulkan::raw_device(ctx.device())) {
                (DecodedFrame::Gpu(_), Some(raw)) => Some(ZeroCopy {
                    raw,
                    device: ctx.device().clone(),
                    queue: ctx.queue().clone(),
                    pool: 0,
                    images: HashMap::new(),
                }),
                _ => None,
            };
            let mut targets = RenderTargetPool::new();
            let rt =
                targets.create_anonymous(ctx, "lumenwall.video", display, SamplerKey::CLAMP_LINEAR);
            let queue = Arc::new(Queue::new(opts.queue_frames, opts.max_fps, notify));
            let mut player = Self {
                queue: queue.clone(),
                pool,
                thread: None,
                info,
                path: decode_path,
                converter,
                planes,
                zero_copy,
                current: Current::Planes,
                color: YuvColor::default(),
                targets,
                rt,
                clock: PlayClock::default(),
                shown: 0.0,
                convert_pending: false,
                generation: 0,
                stats: VideoStats::default(),
            };
            player.upload(ctx, first);
            let thread = std::thread::Builder::new()
                .name("lumenwall-video".into())
                .spawn(move || decode_loop(source, &queue))
                .map_err(|e| {
                    VideoError::Decode(format!("cannot start the decoding thread: {e}"))
                })?;
            player.thread = Some(thread);
            tracing::info!(
                "video '{}': {} {}x{} ({}), {:.3} fps, {:.2} s, {}; {}; opened in {:.0} ms",
                input.name(),
                player.info.codec,
                player.info.coded_size.0,
                player.info.coded_size.1,
                player.info.pixel_format,
                player.info.fps,
                player.info.duration,
                if player.info.has_audio {
                    "with sound"
                } else {
                    "no sound"
                },
                player.path,
                t0.elapsed().as_secs_f64() * 1000.0,
            );
            Ok(player)
        }

        fn resize_output(&mut self, ctx: &GpuContext, size: (u32, u32)) {
            if self.targets.get(self.rt).size() != size {
                tracing::info!("video: stream reconfigured to {}x{}", size.0, size.1);
                self.targets.resize(ctx, self.rt, size);
                self.generation += 1;
            }
        }

        fn upload(&mut self, ctx: &GpuContext, frame: DecodedFrame) {
            let t0 = Instant::now();
            match frame {
                DecodedFrame::Cpu(f) => self.upload_planes(ctx, f),
                DecodedFrame::Gpu(g) => self.bind_image(ctx, g),
            }
            self.convert_pending = true;
            self.stats.shown += 1;
            self.stats.upload += t0.elapsed();
        }

        fn upload_planes(&mut self, ctx: &GpuContext, frame: CpuFrame) {
            if frame.layout != *self.planes.layout() {
                self.planes = self.converter.create_planes(ctx, &frame.layout);
                self.resize_output(ctx, (frame.layout.width, frame.layout.height));
            }
            for i in 0..frame.layout.plane_count() {
                if let Some((data, stride)) = frame.plane(i) {
                    self.planes.write_plane(ctx, i, data, stride);
                }
            }
            self.color = frame.color;
            frame.recycle(&self.pool);
            self.current = Current::Planes;
        }

        /// Zero copy: wraps the decoded image once, then only switches the bind group.
        fn bind_image(&mut self, ctx: &GpuContext, frame: GpuFrame) {
            let Some(zc) = self.zero_copy.as_mut() else {
                return;
            };
            if zc.pool != frame.pool {
                // New frames context (stream reconfigured): its images are new.
                zc.images.clear();
                zc.pool = frame.pool;
            }
            if !zc.images.contains_key(&frame.image) {
                use ash::vk::Handle;
                let image = ash::vk::Image::from_raw(frame.image);
                // SAFETY: an NV12 image of FFmpeg's pool on this device (SAMPLED and
                // MUTABLE_FORMAT, see `ffmpeg::vulkan`), alive while its frames context
                // lives; the cache is cleared before the decoder is closed.
                let Some(texture) =
                    (unsafe { vulkan::wrap_nv12(&zc.device, image, frame.allocated) })
                else {
                    return;
                };
                let view = |aspect, format| {
                    texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("lumenwall.video.plane"),
                        format: Some(format),
                        aspect,
                        ..Default::default()
                    })
                };
                let y = view(wgpu::TextureAspect::Plane0, wgpu::TextureFormat::R8Unorm);
                let uv = view(wgpu::TextureAspect::Plane1, wgpu::TextureFormat::Rg8Unorm);
                let source = self.converter.source_from_views(
                    ctx,
                    PlaneDepth::U8,
                    ChromaPlanes::Interleaved,
                    [&y, &uv, &uv],
                );
                zc.images.insert(
                    frame.image,
                    WrappedImage {
                        source,
                        _views: [y, uv],
                        _texture: texture,
                    },
                );
                tracing::debug!("video: {} decoded images wrapped", zc.images.len());
            }
            self.resize_output(ctx, frame.visible);
            self.color = frame.color;
            self.current = Current::Gpu(frame);
        }

        pub fn info(&self) -> &VideoInfo {
            &self.info
        }

        pub fn decode_path(&self) -> DecodePath {
            self.path
        }

        /// The RGBA image of the current frame (the wallpaper image).
        pub fn output(&self) -> &RenderTarget {
            self.targets.get(self.rt)
        }

        /// Wallpaper size (display size of the stream).
        pub fn size(&self) -> (u32, u32) {
            self.output().size()
        }

        /// Incremented when [`Self::output`] is reallocated (bind groups must be rebuilt).
        pub fn generation(&self) -> u64 {
            self.generation
        }

        /// Live playback: uploads the newest frame due at `now`. `true` when the output must
        /// be converted and presented again.
        pub fn update(&mut self, ctx: &GpuContext, now: Instant) -> bool {
            if self.clock.paused() {
                return self.convert_pending;
            }
            if !self.clock.started() {
                self.clock.start(self.shown, now);
            }
            let Some(t) = self.clock.time(now) else {
                return self.convert_pending;
            };
            let pool = &self.pool;
            let mut late = 0;
            let due = self.queue.pop_due(t, |f| {
                late += 1;
                f.recycle(pool);
            });
            self.stats.late += late;
            if let Some((ft, frame)) = due {
                if t - ft > MAX_LAG.as_secs_f64() && self.queue.next_time().is_none() {
                    // Decoding is slower than real time: play slower rather than skip.
                    self.clock.start(ft, now);
                    self.stats.resyncs += 1;
                    if self.stats.resyncs == 1 {
                        tracing::warn!(
                            "video: decoding is slower than real time ({:.0} ms late)",
                            (t - ft) * 1000.0
                        );
                    }
                }
                self.shown = ft;
                self.upload(ctx, frame);
            }
            self.convert_pending
        }

        /// Offline rendering: uploads the frame shown at media time `t`, waiting for the
        /// decoder as needed (at most a few seconds).
        pub fn update_at(&mut self, ctx: &GpuContext, t: f64) -> bool {
            let deadline = Instant::now() + OFFLINE_TIMEOUT;
            let mut newest: Option<(f64, DecodedFrame)> = None;
            loop {
                self.queue.wait_beyond(t, deadline);
                let pool = &self.pool;
                if let Some(f) = self.queue.pop_due(t, |f| f.recycle(pool))
                    && let Some((_, old)) = newest.replace(f)
                {
                    old.recycle(pool);
                }
                let ahead = self.queue.next_time().is_some_and(|n| n > t);
                if ahead || self.queue.is_stopped() || Instant::now() >= deadline {
                    break;
                }
            }
            if let Some((ft, frame)) = newest {
                self.shown = ft;
                self.upload(ctx, frame);
            }
            self.convert_pending
        }

        /// The output must be converted (a new frame was uploaded).
        pub fn needs_redraw(&self) -> bool {
            self.convert_pending
        }

        /// Records the YUV → RGBA conversion of the current frame, if not done yet. The
        /// returned command buffer (zero-copy path) must be submitted right before
        /// `encoder`'s, in the same `Queue::submit`.
        #[must_use]
        pub fn encode(
            &mut self,
            ctx: &GpuContext,
            encoder: &mut wgpu::CommandEncoder,
        ) -> Option<wgpu::CommandBuffer> {
            if !self.convert_pending {
                return None;
            }
            self.convert_pending = false;
            let size = self.targets.get(self.rt).size();
            match &self.current {
                Current::Planes => {
                    self.planes
                        .source_mut()
                        .set_params(ctx, &self.color, size, [1.0, 1.0]);
                    self.converter
                        .encode(encoder, self.planes.source(), self.targets.get(self.rt));
                    None
                }
                Current::Gpu(frame) => {
                    let zc = self.zero_copy.as_mut()?;
                    let image = zc.images.get_mut(&frame.image)?;
                    // SAFETY: a Vulkan frame decoded on this device; the caller submits the
                    // command buffer with `encoder` in the next submission.
                    let pre = unsafe {
                        vulkan::begin_sampling(frame.frame.get(), &zc.raw, &zc.device, &zc.queue)
                    };
                    match pre {
                        Ok(pre) => {
                            let crop = [
                                frame.visible.0 as f32 / frame.allocated.0.max(1) as f32,
                                frame.visible.1 as f32 / frame.allocated.1.max(1) as f32,
                            ];
                            image.source.set_params(ctx, &self.color, size, crop);
                            self.converter.encode(
                                encoder,
                                &image.source,
                                self.targets.get(self.rt),
                            );
                            Some(pre)
                        }
                        Err(e) => {
                            tracing::warn!("video: frame not shown: {e}");
                            None
                        }
                    }
                }
            }
        }

        /// When the next frame is due (`None`: paused, or nothing decoded yet; the decoding
        /// thread then calls `notify` when a frame arrives).
        pub fn next_deadline(&self) -> Option<Instant> {
            if self.clock.paused() {
                return None;
            }
            let t = self.queue.next_time()?;
            match self.clock.instant_of(t) {
                Some(i) => Some(i),
                // Not started: the first update starts the clock.
                None => Some(Instant::now()),
            }
        }

        /// Pause (fullscreen window, covered or locked outputs): the clock stops, the
        /// decoder blocks once the queue is full.
        pub fn set_paused(&mut self, paused: bool, now: Instant) {
            if paused {
                self.clock.pause(now);
            } else {
                self.clock.resume(now);
            }
        }

        pub fn is_paused(&self) -> bool {
            self.clock.paused()
        }

        /// New frame cap (configuration reload).
        pub fn set_max_fps(&self, fps: u32) {
            self.queue.max_fps.store(fps, Ordering::Relaxed);
        }

        pub fn stats(&self) -> VideoStats {
            VideoStats {
                decoded: self.queue.decoded.load(Ordering::Relaxed),
                skipped: self.queue.dropped_early.load(Ordering::Relaxed),
                loops: self.queue.loops.load(Ordering::Relaxed),
                ..self.stats
            }
        }

        /// GPU bytes of the uploaded planes and the output image (decoded images of the
        /// zero-copy path belong to FFmpeg and are not counted).
        pub fn gpu_bytes(&self) -> u64 {
            let (w, h) = self.size();
            self.planes.layout().bytes() + u64::from(w) * u64::from(h) * 4
        }
    }

    impl Drop for VideoPlayer {
        fn drop(&mut self) {
            // wgpu objects wrapping FFmpeg's images go first, and the GPU must be done with
            // them before the decoder (and its images) is destroyed.
            self.current = Current::Planes;
            if let Some(zc) = self.zero_copy.take() {
                let ZeroCopy { device, images, .. } = zc;
                drop(images);
                let _ = device.poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                });
            }
            self.queue.stop();
            let source = self.thread.take().and_then(|t| t.join().ok());
            drop(self.queue.clear());
            drop(source);
        }
    }

    /// The decoding thread: decode, decimate, download/convert, queue (blocks when full).
    /// Returns the source so that the player closes it after releasing the GPU objects that
    /// reference its images.
    fn decode_loop(mut source: Source, queue: &Queue<DecodedFrame>) -> Source {
        let mut decimator = Decimator::default();
        let mut refused_logged = false;
        while !queue.is_stopped() {
            let (frame, t) = match source.next() {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!("video: {e}; playback stopped on the last frame");
                    break;
                }
            };
            queue.decoded.fetch_add(1, Ordering::Relaxed);
            queue.loops.store(source.loops(), Ordering::Relaxed);
            if !refused_logged && source.hw_refused() {
                refused_logged = true;
                tracing::warn!("video: the hardware decoder gave up, decoding in software");
            }
            let cap = queue.max_fps.load(Ordering::Relaxed);
            let Some((kt, keep)) = decimator.push(t, frame, cap) else {
                queue.dropped_early.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            match source.prepare(keep) {
                Ok(f) => {
                    if !queue.push(kt, f) {
                        break;
                    }
                }
                Err(e) => tracing::warn!("video: frame skipped: {e}"),
            }
        }
        source
    }
}

#[cfg(not(feature = "ffmpeg"))]
mod imp {
    use super::*;

    /// Placeholder of builds without FFmpeg: [`VideoPlayer::open`] always fails.
    pub struct VideoPlayer {
        never: std::convert::Infallible,
    }

    #[allow(clippy::unused_self)]
    impl VideoPlayer {
        pub fn open(
            _ctx: &GpuContext,
            _path: &Path,
            _opts: &VideoOptions,
            _notify: Option<Notify>,
        ) -> Result<Self, VideoError> {
            Err(VideoError::Unsupported)
        }
        pub fn open_input(
            _ctx: &GpuContext,
            _input: &VideoInput,
            _opts: &VideoOptions,
            _notify: Option<Notify>,
        ) -> Result<Self, VideoError> {
            Err(VideoError::Unsupported)
        }
        pub fn info(&self) -> &VideoInfo {
            match self.never {}
        }
        pub fn decode_path(&self) -> DecodePath {
            match self.never {}
        }
        pub fn output(&self) -> &RenderTarget {
            match self.never {}
        }
        pub fn size(&self) -> (u32, u32) {
            match self.never {}
        }
        pub fn generation(&self) -> u64 {
            match self.never {}
        }
        pub fn update(&mut self, _ctx: &GpuContext, _now: Instant) -> bool {
            match self.never {}
        }
        pub fn update_at(&mut self, _ctx: &GpuContext, _t: f64) -> bool {
            match self.never {}
        }
        pub fn needs_redraw(&self) -> bool {
            match self.never {}
        }
        pub fn encode(
            &mut self,
            _ctx: &GpuContext,
            _encoder: &mut wgpu::CommandEncoder,
        ) -> Option<wgpu::CommandBuffer> {
            match self.never {}
        }
        pub fn next_deadline(&self) -> Option<Instant> {
            match self.never {}
        }
        pub fn set_paused(&mut self, _paused: bool, _now: Instant) {
            match self.never {}
        }
        pub fn is_paused(&self) -> bool {
            match self.never {}
        }
        pub fn set_max_fps(&self, _fps: u32) {
            match self.never {}
        }
        pub fn stats(&self) -> VideoStats {
            match self.never {}
        }
        pub fn gpu_bytes(&self) -> u64 {
            match self.never {}
        }
    }
}

pub use imp::VideoPlayer;

//! FFmpeg demuxing and decoding (feature `ffmpeg`), raw `ffmpeg-sys-next` FFI through [`sys`]
//! (FFmpeg's libraries are opened at the first video on Linux and Windows).
//!
//! [`Source`] owns the demuxer and the decoder and runs on the decoding thread (it is opened
//! on the caller's thread, which decodes the first frame to size the GPU resources). It
//! loops forever: at the end of the file the decoder is drained (the last frames are
//! shown), flushed, and the demuxer seeks back to the start while [`LoopTimeline`] keeps the
//! media time continuous.

#[cfg(windows)]
pub mod d3d11;
#[cfg(windows)]
mod dxgi;
pub(crate) mod hw;
mod io;
pub(crate) mod sys;
pub(crate) mod vulkan;

use std::ffi::{CStr, CString};
use std::os::raw::c_int;
use std::path::PathBuf;
use std::ptr::{self, NonNull};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Once, PoisonError};

use lumenwall_render::yuv::{ChromaPlanes, PlaneDepth, YuvColor, YuvLayout, YuvMatrix, YuvRange};
use sys as ff;

use crate::pacing::LoopTimeline;
use crate::select::AdapterHint;
use crate::{DecodePath, HwKind, VideoError, VideoInfo, VideoInput};
use hw::{FormatChoice, HwDevice};

const EAGAIN: c_int = ff::AVERROR(libc::EAGAIN);

pub(crate) fn av_err(code: c_int) -> String {
    let mut buf = [0 as libc::c_char; 128];
    // SAFETY: the buffer is large enough and NUL-terminated by av_strerror.
    unsafe {
        ff::av_strerror(code, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

/// FFmpeg's own log goes to stderr: errors only, unless `LUMENWALL_FFMPEG_LOG` asks for more
/// (`warning`, `info`, `verbose`, `debug`).
fn init_logging() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let level = match std::env::var("LUMENWALL_FFMPEG_LOG").as_deref() {
            Ok("quiet") => ff::AV_LOG_QUIET,
            Ok("warning") => ff::AV_LOG_WARNING,
            Ok("info") => ff::AV_LOG_INFO,
            Ok("verbose") => ff::AV_LOG_VERBOSE,
            Ok("debug") => ff::AV_LOG_DEBUG,
            _ => ff::AV_LOG_ERROR,
        };
        // SAFETY: plain setter.
        unsafe { ff::av_log_set_level(level) };
    });
}

/// An owned `AVFrame`. FFmpeg frames have no thread affinity, so they can be handed from
/// the decoding thread to the render thread.
pub(crate) struct Frame(NonNull<ff::AVFrame>);

// SAFETY: an AVFrame is plain reference-counted data, used by one thread at a time.
unsafe impl Send for Frame {}

impl Frame {
    fn new() -> Result<Self, VideoError> {
        // SAFETY: allocation; null on OOM.
        NonNull::new(unsafe { ff::av_frame_alloc() })
            .map(Self)
            .ok_or(VideoError::OutOfMemory)
    }

    fn ptr(&self) -> *mut ff::AVFrame {
        self.0.as_ptr()
    }

    pub(crate) fn get(&self) -> &ff::AVFrame {
        // SAFETY: valid for the lifetime of `self`.
        unsafe { self.0.as_ref() }
    }

    fn format(&self) -> c_int {
        self.get().format
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        let mut p = self.0.as_ptr();
        // SAFETY: owned frame; unrefs its buffers.
        unsafe { ff::av_frame_free(&mut p) };
    }
}

/// Recycled CPU frames (hardware downloads, format conversions): their buffers are
/// allocated once and reused, so a 4K stream does not map and fault 12 MiB per frame.
pub(crate) type FramePool = Arc<Mutex<Vec<Frame>>>;

/// A decoded frame in system memory, ready for upload.
pub(crate) struct CpuFrame {
    frame: Frame,
    pub layout: YuvLayout,
    pub color: YuvColor,
    /// FFmpeg plane of each of our planes (Y, U or UV, V).
    plane_map: [usize; 3],
    pooled: bool,
}

impl CpuFrame {
    /// Bytes and stride of plane `i` (our plane order).
    pub fn plane(&self, i: usize) -> Option<(&[u8], u32)> {
        let f = self.frame.get();
        let p = self.plane_map[i];
        let stride = f.linesize[p];
        let data = f.data[p];
        let (_, h) = self.layout.plane_size(i);
        let row = self.layout.plane_row_bytes(i) as usize;
        if stride <= 0 || data.is_null() || h == 0 || (stride as usize) < row {
            return None;
        }
        let len = stride as usize * (h as usize - 1) + row;
        // SAFETY: FFmpeg planes hold `linesize * height` bytes (the last row at least `row`).
        Some((
            unsafe { std::slice::from_raw_parts(data, len) },
            stride as u32,
        ))
    }

    /// Returns a pooled frame to `pool` (else frees it, releasing decoder buffers).
    pub fn recycle(self, pool: &FramePool) {
        if self.pooled {
            pool.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(self.frame);
        }
    }
}

/// A decoded image left on the GPU (zero-copy Vulkan path).
pub(crate) struct GpuFrame {
    pub frame: Frame,
    /// `VkImage` handle (cache key of the wrapping texture).
    pub image: u64,
    /// Allocated image size (coded size, at least the visible size).
    pub allocated: (u32, u32),
    /// Visible picture size.
    pub visible: (u32, u32),
    /// Identity of the frames context the image belongs to.
    pub pool: usize,
    pub color: YuvColor,
}

/// What the decoding thread hands to the render thread.
pub(crate) enum DecodedFrame {
    Cpu(CpuFrame),
    Gpu(GpuFrame),
}

impl DecodedFrame {
    pub fn recycle(self, pool: &FramePool) {
        if let Self::Cpu(f) = self {
            f.recycle(pool);
        }
    }
}

/// One decoder to try.
pub(crate) enum Attempt {
    /// FFmpeg's Vulkan decoder on wgpu's device (zero copy).
    Shared(HwDevice),
    /// A hardware device opened by FFmpeg (frames downloaded).
    Device(HwKind),
    /// FFmpeg's D3D11VA decoder on the Direct3D 11 renderer's device (zero copy, [`d3d11`]).
    #[cfg(windows)]
    D3d11(HwDevice, c_int),
}

/// How a pixel format maps onto [`YuvLayout`].
#[derive(Clone, Copy, Debug)]
struct PixelLayout {
    layout: YuvLayout,
    bits: u32,
    shift: u32,
    plane_map: [usize; 3],
    /// `yuvj*` formats: full range unless the frame says otherwise.
    jpeg: bool,
}

fn pix_desc(format: c_int) -> Option<&'static ff::AVPixFmtDescriptor> {
    if format < 0 {
        return None;
    }
    // SAFETY: `format` comes from FFmpeg itself (a valid AVPixelFormat of these headers);
    // the descriptor table is static.
    unsafe {
        let f: ff::AVPixelFormat = std::mem::transmute::<c_int, ff::AVPixelFormat>(format);
        ff::av_pix_fmt_desc_get(f).as_ref()
    }
}

fn pix_name(format: c_int) -> String {
    pix_desc(format)
        .and_then(|d| {
            // SAFETY: static NUL-terminated name.
            (!d.name.is_null()).then(|| unsafe { CStr::from_ptr(d.name) })
        })
        .map_or_else(
            || format!("#{format}"),
            |n| n.to_string_lossy().into_owned(),
        )
}

/// Planar or semi-planar Y'CbCr formats the converter reads directly; `None` = convert with
/// libswscale first.
fn describe(format: c_int, width: u32, height: u32) -> Option<PixelLayout> {
    let d = pix_desc(format)?;
    let unsupported = ff::AV_PIX_FMT_FLAG_BE
        | ff::AV_PIX_FMT_FLAG_PAL
        | ff::AV_PIX_FMT_FLAG_BITSTREAM
        | ff::AV_PIX_FMT_FLAG_HWACCEL
        | ff::AV_PIX_FMT_FLAG_RGB
        | ff::AV_PIX_FMT_FLAG_FLOAT
        | ff::AV_PIX_FMT_FLAG_BAYER
        | ff::AV_PIX_FMT_FLAG_XYZ;
    if d.flags & unsupported as u64 != 0 || d.nb_components < 3 {
        return None;
    }
    let c = &d.comp;
    let depth = c[0].depth;
    if c[1].depth != depth || c[2].depth != depth || c[1].shift != c[0].shift {
        return None;
    }
    let (container, bytes) = match depth {
        1..=8 => (PlaneDepth::U8, 1),
        9..=16 => (PlaneDepth::U16, 2),
        _ => return None,
    };
    if c[0].plane != 0 || c[0].step != bytes || c[0].offset != 0 {
        return None;
    }
    let planes = if c[1].plane == c[2].plane {
        if c[1].plane == 0 || c[1].step != 2 * bytes || c[2].step != 2 * bytes {
            return None;
        }
        match (c[1].offset, c[2].offset) {
            (0, o) if o == bytes => ChromaPlanes::Interleaved,
            (o, 0) if o == bytes => ChromaPlanes::InterleavedSwapped,
            _ => return None,
        }
    } else {
        if c[1].plane == 0
            || c[2].plane == 0
            || c[1].step != bytes
            || c[2].step != bytes
            || c[1].offset != 0
            || c[2].offset != 0
        {
            return None;
        }
        ChromaPlanes::Planar
    };
    let ceil_shift = |v: u32, s: u8| (v + (1 << s) - 1) >> s;
    // SAFETY: static NUL-terminated name.
    let name = unsafe { CStr::from_ptr(d.name) }.to_bytes();
    Some(PixelLayout {
        layout: YuvLayout {
            width,
            height,
            chroma_width: ceil_shift(width, d.log2_chroma_w),
            chroma_height: ceil_shift(height, d.log2_chroma_h),
            planes,
            depth: container,
        },
        bits: depth as u32,
        shift: c[0].shift as u32,
        plane_map: [0, c[1].plane as usize, c[2].plane as usize],
        jpeg: name.starts_with(b"yuvj"),
    })
}

/// Colorimetry of a frame. Unspecified values follow the usual player conventions: BT.709
/// for HD sizes, BT.601 below; limited range except `yuvj*`; MPEG-2 "left" chroma siting.
fn colorimetry(f: &ff::AVFrame, px: &PixelLayout) -> YuvColor {
    use ff::AVChromaLocation as L;
    use ff::AVColorRange as R;
    use ff::AVColorSpace as S;
    let l = &px.layout;
    let matrix = match f.colorspace {
        S::AVCOL_SPC_BT709 => YuvMatrix::Bt709,
        S::AVCOL_SPC_BT470BG | S::AVCOL_SPC_SMPTE170M => YuvMatrix::Bt601,
        S::AVCOL_SPC_FCC => YuvMatrix::Fcc,
        S::AVCOL_SPC_SMPTE240M => YuvMatrix::Smpte240m,
        S::AVCOL_SPC_BT2020_NCL | S::AVCOL_SPC_BT2020_CL => YuvMatrix::Bt2020,
        _ if l.width >= 1280 || l.height > 576 => YuvMatrix::Bt709,
        _ => YuvMatrix::Bt601,
    };
    let range = match f.color_range {
        R::AVCOL_RANGE_JPEG => YuvRange::Full,
        R::AVCOL_RANGE_MPEG => YuvRange::Limited,
        _ if px.jpeg => YuvRange::Full,
        _ => YuvRange::Limited,
    };
    let rx = l.chroma_width as f32 / l.width.max(1) as f32;
    let ry = l.chroma_height as f32 / l.height.max(1) as f32;
    let left = 0.5 - 0.5 * rx;
    let top = 0.5 - 0.5 * ry;
    let chroma_offset = match f.chroma_location {
        L::AVCHROMA_LOC_CENTER => [0.0, 0.0],
        L::AVCHROMA_LOC_TOPLEFT => [left, top],
        L::AVCHROMA_LOC_TOP => [0.0, top],
        L::AVCHROMA_LOC_BOTTOMLEFT => [left, -top],
        L::AVCHROMA_LOC_BOTTOM => [0.0, -top],
        _ => [left, 0.0],
    };
    YuvColor {
        matrix,
        range,
        bits: px.bits,
        shift: px.shift,
        chroma_offset,
    }
}

/// Demuxer + decoder of one video file.
pub(crate) struct Source {
    path: PathBuf,
    fmt: *mut ff::AVFormatContext,
    /// Custom I/O of in-memory inputs (dropped after `fmt` is closed).
    _io: Option<io::MemoryIo>,
    ctx: *mut ff::AVCodecContext,
    pkt: *mut ff::AVPacket,
    recv: Frame,
    stream: c_int,
    time_base: f64,
    /// Fallback frame duration (1 / frame rate).
    frame_duration: f64,
    /// Seek target of a loop (stream time base).
    start_ts: i64,
    timeline: LoopTimeline,
    draining: bool,
    packet_pending: bool,
    frames_in_loop: u64,
    hw: Option<HwDevice>,
    choice: Box<FormatChoice>,
    pool: FramePool,
    sws: *mut ff::SwsContext,
    info: VideoInfo,
    path_kind: DecodePath,
    warned_convert: bool,
    /// Decoding on wgpu's device: NV12 frames stay on the GPU.
    zero_copy: bool,
    /// Decoding on the Direct3D 11 renderer's device ([`d3d11`]): frames stay on the GPU.
    native_d3d11: bool,
    /// Hardware frames kept in the decoder's pool beyond what it needs itself.
    extra_hw_frames: c_int,
}

// SAFETY: the FFmpeg contexts are only used by the thread that owns the `Source`.
unsafe impl Send for Source {}

impl Drop for Source {
    fn drop(&mut self) {
        // SAFETY: owned contexts; every free function accepts null.
        unsafe {
            ff::avcodec_free_context(&mut self.ctx);
            ff::avformat_close_input(&mut self.fmt);
            ff::av_packet_free(&mut self.pkt);
            if !self.sws.is_null() {
                ff::sws_freeContext(self.sws);
            }
        }
    }
}

fn q2d(r: ff::AVRational) -> f64 {
    if r.den == 0 {
        0.0
    } else {
        f64::from(r.num) / f64::from(r.den)
    }
}

impl Source {
    /// Opens `path` and decodes its first frame, trying `attempts` in order and software
    /// last.
    pub fn open(
        input: &VideoInput,
        attempts: Vec<Attempt>,
        adapter: &AdapterHint,
        pool: FramePool,
    ) -> Result<(Self, DecodedFrame), VideoError> {
        init_logging();
        let mut last = None;
        for attempt in attempts.into_iter().map(Some).chain([None]) {
            let what = match &attempt {
                Some(Attempt::Shared(_)) => "Vulkan Video on the render device",
                Some(Attempt::Device(k)) => k.name(),
                #[cfg(windows)]
                Some(Attempt::D3d11(..)) => "D3D11VA on the render device",
                None => "software",
            };
            match Self::open_with(input, attempt, adapter, pool.clone()) {
                Ok(r) => return Ok(r),
                Err(e @ (VideoError::Open { .. } | VideoError::NoVideoStream { .. })) => {
                    return Err(e);
                }
                Err(e) => {
                    tracing::info!("video: {what} decoding unavailable: {e}");
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or(VideoError::Decode("no decoder".into())))
    }

    fn open_with(
        input: &VideoInput,
        attempt: Option<Attempt>,
        adapter: &AdapterHint,
        pool: FramePool,
    ) -> Result<(Self, DecodedFrame), VideoError> {
        let (mut s, first, _) = Self::open_raw(input, attempt, adapter, pool)?;
        let prepared = s.prepare(first)?;
        s.info.pixel_format = s.decoded_format_name(&prepared);
        if let (DecodedFrame::Gpu(_), Some(dev)) = (&prepared, &s.hw) {
            s.path_kind = DecodePath::ZeroCopy(dev.kind);
        }
        Ok((s, prepared))
    }

    /// Opens the demuxer and the decoder of one attempt and decodes the first frame, left
    /// as FFmpeg gave it (with its media time).
    fn open_raw(
        input: &VideoInput,
        attempt: Option<Attempt>,
        adapter: &AdapterHint,
        pool: FramePool,
    ) -> Result<(Self, Frame, f64), VideoError> {
        let path = PathBuf::from(input.name());
        let open_err = |message: String| VideoError::Open {
            path: path.clone(),
            message,
        };
        let mut fmt = ptr::null_mut();
        let mut custom_io = None;
        let r = match input {
            VideoInput::File(file) => {
                let c_path = CString::new(file.to_string_lossy().as_bytes())
                    .map_err(|e| open_err(e.to_string()))?;
                // SAFETY: out-pointer, NUL-terminated path; on failure `fmt` stays null.
                unsafe {
                    ff::avformat_open_input(&mut fmt, c_path.as_ptr(), ptr::null(), ptr::null_mut())
                }
            }
            VideoInput::Memory { data, .. } => {
                let mem = io::MemoryIo::new(data.clone()).ok_or(VideoError::OutOfMemory)?;
                // SAFETY: a fresh format context reading through the custom I/O, which
                // outlives it (closed before `_io` drops); freed by FFmpeg on failure.
                let r = unsafe {
                    fmt = ff::avformat_alloc_context();
                    if fmt.is_null() {
                        return Err(VideoError::OutOfMemory);
                    }
                    (*fmt).pb = mem.as_ptr();
                    (*fmt).flags |= ff::AVFMT_FLAG_CUSTOM_IO;
                    ff::avformat_open_input(&mut fmt, ptr::null(), ptr::null(), ptr::null_mut())
                };
                custom_io = Some(mem);
                r
            }
        };
        if r < 0 {
            return Err(open_err(av_err(r)));
        }
        // SAFETY: allocations; null on OOM (checked below).
        let (pkt, recv) = (unsafe { ff::av_packet_alloc() }, Frame::new());
        let mut s = Self {
            path: path.clone(),
            fmt,
            _io: custom_io,
            ctx: ptr::null_mut(),
            pkt,
            recv: recv?,
            stream: -1,
            time_base: 0.0,
            frame_duration: 1.0 / 30.0,
            start_ts: 0,
            timeline: LoopTimeline::new(None),
            draining: false,
            packet_pending: false,
            frames_in_loop: 0,
            hw: None,
            choice: Box::new(FormatChoice {
                wanted: -1,
                refused: Default::default(),
            }),
            pool,
            sws: ptr::null_mut(),
            info: VideoInfo::default(),
            path_kind: DecodePath::Software,
            warned_convert: false,
            zero_copy: false,
            native_d3d11: false,
            extra_hw_frames: 6,
        };
        if s.pkt.is_null() {
            return Err(VideoError::OutOfMemory);
        }
        // SAFETY: open context.
        let r = unsafe { ff::avformat_find_stream_info(s.fmt, ptr::null_mut()) };
        if r < 0 {
            return Err(open_err(av_err(r)));
        }
        let mut codec: *const ff::AVCodec = ptr::null();
        // SAFETY: open context, out-pointer.
        let idx = unsafe {
            ff::av_find_best_stream(
                s.fmt,
                ff::AVMediaType::AVMEDIA_TYPE_VIDEO,
                -1,
                -1,
                &mut codec,
                0,
            )
        };
        if idx < 0 || codec.is_null() {
            return Err(VideoError::NoVideoStream { path });
        }
        s.stream = idx;
        s.read_stream_info(codec);
        let missing = |k: HwKind, codec_name: &str| {
            VideoError::Decode(format!("{codec_name} has no {} decoder", k.name()))
        };
        match attempt {
            Some(Attempt::Device(k)) => {
                let pix = hw::hw_pix_fmt(codec, k).ok_or_else(|| missing(k, &s.info.codec))?;
                let dev = hw::open_device(k, adapter, pix).map_err(VideoError::Decode)?;
                s.choice.wanted = pix as c_int;
                s.hw = Some(dev);
            }
            #[cfg(windows)]
            Some(Attempt::D3d11(mut dev, extra_frames)) => {
                let pix = hw::hw_pix_fmt(codec, HwKind::D3d11va)
                    .ok_or_else(|| missing(HwKind::D3d11va, &s.info.codec))?;
                dev.pix_fmt = pix;
                s.choice.wanted = pix as c_int;
                s.hw = Some(dev);
                s.native_d3d11 = true;
                s.extra_hw_frames = extra_frames;
            }
            Some(Attempt::Shared(mut dev)) => {
                let pix = hw::hw_pix_fmt(codec, HwKind::Vulkan)
                    .ok_or_else(|| missing(HwKind::Vulkan, &s.info.codec))?;
                dev.pix_fmt = pix;
                s.choice.wanted = pix as c_int;
                s.hw = Some(dev);
                s.zero_copy = true;
            }
            None => {}
        }
        s.open_codec(codec)?;
        let first = s.next()?;
        if let Some(dev) = &s.hw {
            if first.0.format() != dev.pix_fmt as c_int {
                return Err(VideoError::Decode(format!(
                    "the decoder fell back to software ({} refused the stream)",
                    dev.kind.name()
                )));
            }
            tracing::info!(
                "video: {} decoder on device {}",
                dev.kind.name(),
                dev.device
            );
            s.path_kind = DecodePath::HardwareCopy(dev.kind);
        }
        Ok((s, first.0, first.1))
    }

    fn read_stream_info(&mut self, codec: *const ff::AVCodec) {
        // SAFETY: `stream` indexes the open context's streams.
        let st = unsafe { &**(*self.fmt).streams.add(self.stream as usize) };
        // SAFETY: streams always have codec parameters.
        let par = unsafe { &*st.codecpar };
        self.time_base = q2d(st.time_base);
        let fps = [q2d(st.avg_frame_rate), q2d(st.r_frame_rate)]
            .into_iter()
            .find(|f| f.is_finite() && *f > 0.0 && *f < 1000.0)
            .unwrap_or(0.0);
        if fps > 0.0 {
            self.frame_duration = 1.0 / fps;
        }
        self.start_ts = if st.start_time != ff::AV_NOPTS_VALUE {
            st.start_time
        } else {
            0
        };
        let duration = if st.duration != ff::AV_NOPTS_VALUE && st.duration > 0 {
            st.duration as f64 * self.time_base
        } else {
            // SAFETY: open context.
            let d = unsafe { (*self.fmt).duration };
            if d > 0 {
                d as f64 / f64::from(ff::AV_TIME_BASE)
            } else {
                0.0
            }
        };
        let (w, h) = (par.width.max(0) as u32, par.height.max(0) as u32);
        let sar = [st.sample_aspect_ratio, par.sample_aspect_ratio]
            .into_iter()
            .map(q2d)
            .find(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(1.0);
        let display = display_size(w, h, sar);
        // SAFETY: open context; streams array of nb_streams entries.
        let has_audio = unsafe {
            let n = (*self.fmt).nb_streams as usize;
            (0..n).any(|i| {
                let st = &**(*self.fmt).streams.add(i);
                (*st.codecpar).codec_type == ff::AVMediaType::AVMEDIA_TYPE_AUDIO
            })
        };
        // Only the video stream is demuxed (the sound track is played by the integration).
        // SAFETY: as above; `discard` is a demuxer input field.
        unsafe {
            let n = (*self.fmt).nb_streams as usize;
            for i in 0..n {
                if i as c_int != self.stream {
                    (**(*self.fmt).streams.add(i)).discard = ff::AVDiscard::AVDISCARD_ALL;
                }
            }
        }
        // SAFETY: static name of a valid decoder.
        let codec_name = unsafe { CStr::from_ptr((*codec).name) }
            .to_string_lossy()
            .into_owned();
        self.info = VideoInfo {
            codec: codec_name,
            coded_size: (w, h),
            display_size: display,
            fps,
            duration,
            has_audio,
            pixel_format: String::new(),
        };
    }

    fn open_codec(&mut self, codec: *const ff::AVCodec) -> Result<(), VideoError> {
        let fail = |what: &str, r: c_int| VideoError::Decode(format!("{what}: {}", av_err(r)));
        // SAFETY: allocation for a valid decoder.
        self.ctx = unsafe { ff::avcodec_alloc_context3(codec) };
        if self.ctx.is_null() {
            return Err(VideoError::OutOfMemory);
        }
        // SAFETY: valid stream and freshly allocated context.
        unsafe {
            let st = &**(*self.fmt).streams.add(self.stream as usize);
            let r = ff::avcodec_parameters_to_context(self.ctx, st.codecpar);
            if r < 0 {
                return Err(fail("codec parameters", r));
            }
            (*self.ctx).pkt_timebase = st.time_base;
            match &self.hw {
                Some(dev) => {
                    (*self.ctx).hw_device_ctx = ff::av_buffer_ref(dev.buf);
                    if (*self.ctx).hw_device_ctx.is_null() {
                        return Err(VideoError::OutOfMemory);
                    }
                    (*self.ctx).get_format = Some(hw::get_format);
                    (*self.ctx).opaque = (&*self.choice as *const FormatChoice).cast_mut().cast();
                    (*self.ctx).thread_count = 1;
                    // Frames held by the queue, the decimator and the displayed one.
                    (*self.ctx).extra_hw_frames = self.extra_hw_frames;
                }
                None => {
                    // Frame + slice threads, count chosen by FFmpeg (one per core).
                    (*self.ctx).thread_count = 0;
                    (*self.ctx).thread_type = ff::FF_THREAD_FRAME | ff::FF_THREAD_SLICE;
                }
            }
            let r = ff::avcodec_open2(self.ctx, codec, ptr::null_mut());
            if r < 0 {
                return Err(fail("opening the decoder", r));
            }
        }
        Ok(())
    }

    pub fn info(&self) -> &VideoInfo {
        &self.info
    }

    pub fn decode_path(&self) -> DecodePath {
        self.path_kind
    }

    pub fn loops(&self) -> u64 {
        self.timeline.loops()
    }

    fn decoded_format_name(&self, frame: &DecodedFrame) -> String {
        let name = match frame {
            DecodedFrame::Cpu(cpu) => pix_name(cpu.frame.format()),
            DecodedFrame::Gpu(_) => "nv12".into(),
        };
        match &self.hw {
            Some(dev) => format!("{} ({name})", pix_name(dev.pix_fmt as c_int)),
            None => name,
        }
    }

    /// Next decoded frame and its media time. Loops at the end of the file.
    pub fn next(&mut self) -> Result<(Frame, f64), VideoError> {
        loop {
            // SAFETY: open decoder, owned receive frame.
            let r = unsafe { ff::avcodec_receive_frame(self.ctx, self.recv.ptr()) };
            if r == 0 {
                let out = Frame::new()?;
                // SAFETY: both frames valid; moves the references.
                unsafe { ff::av_frame_move_ref(out.ptr(), self.recv.ptr()) };
                let t = self.frame_time(out.get());
                self.frames_in_loop += 1;
                return Ok((out, t));
            }
            if r == ff::AVERROR_EOF {
                if self.frames_in_loop == 0 {
                    return Err(VideoError::Decode(format!(
                        "{}: no decodable frame",
                        self.path.display()
                    )));
                }
                self.rewind()?;
                continue;
            }
            if r != EAGAIN {
                return Err(VideoError::Decode(av_err(r)));
            }
            self.feed()?;
        }
    }

    fn frame_time(&mut self, f: &ff::AVFrame) -> f64 {
        let ts = [f.best_effort_timestamp, f.pts]
            .into_iter()
            .find(|t| *t != ff::AV_NOPTS_VALUE);
        let pts = ts.map(|t| t as f64 * self.time_base);
        let duration = if f.duration > 0 {
            f.duration as f64 * self.time_base
        } else {
            self.frame_duration
        };
        self.timeline.frame(pts, duration)
    }

    /// Sends the next packet of the video stream (or the end-of-stream flush).
    fn feed(&mut self) -> Result<(), VideoError> {
        if self.draining {
            // The flush was sent: receive_frame will end with EOF.
            return Err(VideoError::Decode("decoder stalled while draining".into()));
        }
        loop {
            if !self.packet_pending {
                // SAFETY: open demuxer, owned packet.
                let r = unsafe { ff::av_read_frame(self.fmt, self.pkt) };
                if r < 0 {
                    if r != ff::AVERROR_EOF {
                        tracing::debug!("video: read error ({}), restarting the loop", av_err(r));
                    }
                    // SAFETY: null packet = flush.
                    unsafe { ff::avcodec_send_packet(self.ctx, ptr::null()) };
                    self.draining = true;
                    return Ok(());
                }
                // SAFETY: filled packet.
                if unsafe { (*self.pkt).stream_index } != self.stream {
                    // SAFETY: owned packet.
                    unsafe { ff::av_packet_unref(self.pkt) };
                    continue;
                }
            }
            // SAFETY: open decoder and filled packet.
            let r = unsafe { ff::avcodec_send_packet(self.ctx, self.pkt) };
            if r == EAGAIN {
                // Output must be drained first; resend this packet next time.
                self.packet_pending = true;
                return Ok(());
            }
            self.packet_pending = false;
            // SAFETY: owned packet.
            unsafe { ff::av_packet_unref(self.pkt) };
            if r < 0 && r != ff::AVERROR_EOF {
                // Corrupt packet: skip it, like players do.
                tracing::debug!("video: packet rejected: {}", av_err(r));
                continue;
            }
            return Ok(());
        }
    }

    /// End of file: back to the first frame, timeline continued.
    fn rewind(&mut self) -> Result<(), VideoError> {
        // SAFETY: open contexts; owned packet.
        unsafe {
            ff::avcodec_flush_buffers(self.ctx);
            ff::av_packet_unref(self.pkt);
            let mut r = ff::av_seek_frame(
                self.fmt,
                self.stream,
                self.start_ts,
                ff::AVSEEK_FLAG_BACKWARD,
            );
            if r < 0 {
                r = ff::avformat_seek_file(self.fmt, -1, i64::MIN, 0, 0, 0);
            }
            if r < 0 {
                return Err(VideoError::Decode(format!(
                    "cannot seek back to the start: {}",
                    av_err(r)
                )));
            }
        }
        self.draining = false;
        self.packet_pending = false;
        self.frames_in_loop = 0;
        self.timeline.wrap();
        Ok(())
    }

    fn pooled_frame(&mut self, format: c_int, w: u32, h: u32) -> Result<Frame, VideoError> {
        let mut pool = self.pool.lock().unwrap_or_else(PoisonError::into_inner);
        while let Some(f) = pool.pop() {
            let g = f.get();
            if g.format == format && g.width as u32 == w && g.height as u32 == h {
                return Ok(f);
            }
        }
        drop(pool);
        let f = Frame::new()?;
        // SAFETY: fresh frame; buffers allocated for the format and size set here.
        unsafe {
            let p = f.ptr();
            (*p).format = format;
            (*p).width = w as c_int;
            (*p).height = h as c_int;
            let r = ff::av_frame_get_buffer(p, 0);
            if r < 0 {
                return Err(VideoError::Decode(format!("frame buffer: {}", av_err(r))));
            }
        }
        Ok(f)
    }

    /// Prepares a decoded frame for the render thread: NV12 frames of the zero-copy path
    /// stay on the GPU; other hardware frames are downloaded into a pooled frame, unusual
    /// formats converted by libswscale.
    pub fn prepare(&mut self, frame: Frame) -> Result<DecodedFrame, VideoError> {
        let (w, h) = (
            frame.get().width.max(1) as u32,
            frame.get().height.max(1) as u32,
        );
        if self
            .hw
            .as_ref()
            .is_some_and(|d| frame.format() == d.pix_fmt as c_int)
        {
            // SAFETY: hardware frames carry their frames context.
            let sw_format = unsafe {
                let fc = (*(*frame.ptr()).hw_frames_ctx).data as *const ff::AVHWFramesContext;
                (*fc).sw_format as c_int
            };
            if self.zero_copy
                && sw_format == ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int
                && let Some((image, allocated, pool)) = vulkan::frame_image(frame.get())
                && let Some(px) = describe(sw_format, w, h)
            {
                use ash::vk::Handle;
                return Ok(DecodedFrame::Gpu(GpuFrame {
                    color: colorimetry(frame.get(), &px),
                    frame,
                    image: image.as_raw(),
                    allocated,
                    visible: (w, h),
                    pool,
                }));
            }
            let dst = self.pooled_frame(sw_format, w, h)?;
            // SAFETY: valid frames; `dst` owns writable buffers of the frames context's
            // software format.
            let r = unsafe { ff::av_hwframe_transfer_data(dst.ptr(), frame.ptr(), 0) };
            if r < 0 {
                return Err(VideoError::Decode(format!("download: {}", av_err(r))));
            }
            let px = describe(sw_format, w, h).ok_or_else(|| {
                VideoError::Decode(format!(
                    "unsupported download format {}",
                    pix_name(sw_format)
                ))
            })?;
            return Ok(DecodedFrame::Cpu(CpuFrame {
                color: colorimetry(frame.get(), &px),
                layout: px.layout,
                plane_map: px.plane_map,
                frame: dst,
                pooled: true,
            }));
        }
        if let Some(px) = describe(frame.format(), w, h) {
            return Ok(DecodedFrame::Cpu(CpuFrame {
                color: colorimetry(frame.get(), &px),
                layout: px.layout,
                plane_map: px.plane_map,
                frame,
                pooled: false,
            }));
        }
        self.convert(frame, w, h).map(DecodedFrame::Cpu)
    }

    /// libswscale fallback for packed, RGB, gray or big-endian formats: 8-bit 4:4:4.
    fn convert(&mut self, frame: Frame, w: u32, h: u32) -> Result<CpuFrame, VideoError> {
        let src_format = frame.format();
        if !self.warned_convert {
            self.warned_convert = true;
            tracing::warn!(
                "video: pixel format {} converted on the CPU (libswscale)",
                pix_name(src_format)
            );
        }
        let dst_format = ff::AVPixelFormat::AV_PIX_FMT_YUV444P as c_int;
        let dst = self.pooled_frame(dst_format, w, h)?;
        // SAFETY: valid frames of the declared formats and sizes.
        unsafe {
            let src_fmt = std::mem::transmute::<c_int, ff::AVPixelFormat>(src_format);
            self.sws = ff::sws_getCachedContext(
                self.sws,
                w as c_int,
                h as c_int,
                src_fmt,
                w as c_int,
                h as c_int,
                ff::AVPixelFormat::AV_PIX_FMT_YUV444P,
                ff::SwsFlags::SWS_BILINEAR as c_int,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            if self.sws.is_null() {
                return Err(VideoError::Decode("libswscale context".into()));
            }
            let r = ff::sws_scale_frame(self.sws, dst.ptr(), frame.ptr());
            if r < 0 {
                return Err(VideoError::Decode(format!("libswscale: {}", av_err(r))));
            }
        }
        let px = describe(dst_format, w, h)
            .ok_or_else(|| VideoError::Decode("yuv444p layout".into()))?;
        let mut color = colorimetry(frame.get(), &px);
        if pix_desc(src_format).is_some_and(|d| d.flags & ff::AV_PIX_FMT_FLAG_RGB as u64 != 0) {
            // libswscale's default RGB → YUV conversion.
            color.matrix = YuvMatrix::Bt601;
            color.range = YuvRange::Limited;
        }
        Ok(CpuFrame {
            color,
            layout: px.layout,
            plane_map: px.plane_map,
            frame: dst,
            pooled: true,
        })
    }

    /// Whether the decoder refused the hardware format after open (logged once).
    pub fn hw_refused(&self) -> bool {
        self.choice.refused.load(Ordering::Acquire)
    }
}

/// Display size from the coded size and the sample aspect ratio (wider or taller pixels
/// stretch one dimension, like video players do).
pub(crate) fn display_size(w: u32, h: u32, sar: f64) -> (u32, u32) {
    if (sar - 1.0).abs() < 1e-3 || !sar.is_finite() || sar <= 0.0 {
        (w, h)
    } else if sar > 1.0 {
        ((f64::from(w) * sar).round() as u32, h)
    } else {
        (w, (f64::from(h) / sar).round() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every FFmpeg function this crate calls goes through the run-time table of `sys`: a call
    /// to a function missing there would link FFmpeg to the executable again.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn every_ffmpeg_call_is_loaded_at_run_time() {
        sys::same_signatures();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ffmpeg");
        let mut missing = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            for (i, _) in text.match_indices("ff::") {
                let name: String = text[i + 4..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                let function = name.starts_with(|c: char| c.is_ascii_lowercase())
                    && text[i + 4 + name.len()..].trim_start().starts_with('(');
                if function && !sys::LOADED.contains(&name.as_str()) {
                    missing.push(format!("{}: {name}", path.display()));
                }
            }
        }
        assert!(missing.is_empty(), "not in sys's table: {missing:?}");
    }

    fn fmt(f: ff::AVPixelFormat) -> c_int {
        f as c_int
    }

    #[test]
    fn layouts_of_common_formats() {
        use ff::AVPixelFormat as P;
        let nv12 = describe(fmt(P::AV_PIX_FMT_NV12), 3840, 2160).unwrap();
        assert_eq!(nv12.layout.planes, ChromaPlanes::Interleaved);
        assert_eq!(nv12.layout.depth, PlaneDepth::U8);
        assert_eq!(
            (nv12.layout.chroma_width, nv12.layout.chroma_height),
            (1920, 1080)
        );
        let nv21 = describe(fmt(P::AV_PIX_FMT_NV21), 64, 64).unwrap();
        assert_eq!(nv21.layout.planes, ChromaPlanes::InterleavedSwapped);
        let p010 = describe(fmt(P::AV_PIX_FMT_P010LE), 1920, 1080).unwrap();
        assert_eq!(
            (p010.layout.depth, p010.bits, p010.shift),
            (PlaneDepth::U16, 10, 6)
        );
        let i420 = describe(fmt(P::AV_PIX_FMT_YUV420P), 1919, 1079).unwrap();
        assert_eq!(i420.layout.planes, ChromaPlanes::Planar);
        assert_eq!(
            (i420.layout.chroma_width, i420.layout.chroma_height),
            (960, 540)
        );
        assert_eq!(i420.plane_map, [0, 1, 2]);
        let i420p10 = describe(fmt(P::AV_PIX_FMT_YUV420P10LE), 64, 64).unwrap();
        assert_eq!(
            (i420p10.bits, i420p10.shift, i420p10.layout.depth),
            (10, 0, PlaneDepth::U16)
        );
        assert!(describe(fmt(P::AV_PIX_FMT_YUVJ420P), 64, 64).unwrap().jpeg);
        // Packed and RGB formats go through libswscale.
        assert!(describe(fmt(P::AV_PIX_FMT_YUYV422), 64, 64).is_none());
        assert!(describe(fmt(P::AV_PIX_FMT_RGB24), 64, 64).is_none());
        assert!(describe(fmt(P::AV_PIX_FMT_CUDA), 64, 64).is_none());
    }

    #[test]
    fn display_size_applies_the_sample_aspect_ratio() {
        assert_eq!(display_size(3840, 2160, 1.0), (3840, 2160));
        assert_eq!(display_size(1440, 1080, 4.0 / 3.0), (1920, 1080));
        assert_eq!(display_size(720, 576, 0.5), (720, 1152));
    }
}

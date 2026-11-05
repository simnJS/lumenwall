//! Zero-copy decoding for the Direct3D 11 renderer (`lumenwall-d3d11`): FFmpeg's D3D11VA
//! decoder runs on the renderer's own [`Device`], and the decoded NV12/P010 slices of its
//! texture array are handed over as [`DecodedFrame`]s, never copied (no
//! `av_hwframe_transfer_data`).
//!
//! - **Device.** [`hw_device`] makes an `AVD3D11VADeviceContext` that holds the renderer's
//!   `ID3D11Device` and immediate context (FFmpeg releases its references with the context),
//!   after turning the runtime's lock around the context on (`Device::set_multithread_protected`:
//!   the decoder works from the decoding thread, the renderer from its own). The device must
//!   not have been created with `D3D11_CREATE_DEVICE_SINGLETHREADED`. Decoded textures get the
//!   `D3D11_BIND_SHADER_RESOURCE` flag so that the renderer can sample them.
//! - **Frame lifetime.** A [`HardwareFrame`] owns its `AVFrame` reference: the slot of the
//!   decoder's pool is not rewritten before the frame is dropped, so the renderer drops it only
//!   once the draw that samples the slice is recorded (the GPU orders the decoder's writes and
//!   the renderer's reads itself, both are on one device).
//! - **Fallback.** When the stream cannot be decoded in hardware (codec, profile, size or bit
//!   depth the GPU lacks, WARP, no D3D11VA in FFmpeg) or its frames are not NV12/P010, the
//!   decoder gives [`SoftwareFrame`]s (libavcodec on the CPU, or the hardware frame downloaded)
//!   and [`PlaneTextures`] uploads their planes to writable textures.
//!
//! The adapter is the device's: `AdapterHint::from_luid` names it for the DXGI-based paths.

use std::ffi::c_void;
use std::os::raw::c_int;
use std::path::PathBuf;

use lumenwall_d3d11::windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    ID3D11ShaderResourceView, ID3D11Texture2D,
};
use lumenwall_d3d11::windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R16_UNORM,
    DXGI_FORMAT_R16G16_UNORM, DXGI_SAMPLE_DESC,
};
use lumenwall_d3d11::windows::core::Interface;
use lumenwall_d3d11::{DecodedFrame, Device, PlaneFormat};
use lumenwall_render::yuv::{ChromaPlanes, PlaneDepth, YuvColor, YuvLayout};

use super::hw::HwDevice;
use super::sys as ff;
use super::{
    Attempt, CpuFrame, Frame, FramePool, Source, av_err, colorimetry, describe, init_logging,
};
use crate::select::AdapterHint;
use crate::{DecodePath, HwDecode, HwKind, VideoError, VideoInfo, VideoInput};

/// `AVD3D11VADeviceContext` (`libavutil/hwcontext_d3d11va.h`, FFmpeg 8): the bindings of
/// `ffmpeg-sys-next` do not include that header.
#[repr(C)]
struct AvD3d11vaDeviceContext {
    device: *mut c_void,
    device_context: *mut c_void,
    video_device: *mut c_void,
    video_context: *mut c_void,
    lock: Option<unsafe extern "C" fn(*mut c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    lock_ctx: *mut c_void,
    bind_flags: u32,
    misc_flags: u32,
}

const _: () = assert!(size_of::<AvD3d11vaDeviceContext>() == 7 * size_of::<usize>() + 8);

/// An FFmpeg D3D11VA device context on the renderer's `device`.
pub(crate) fn hw_device(device: &Device) -> Result<HwDevice, VideoError> {
    let fail = |what: &str, e: &dyn std::fmt::Display| {
        VideoError::Decode(format!("D3D11VA device: {what}: {e}"))
    };
    device
        .set_multithread_protected(true)
        .map_err(|e| fail("multithread protection", &e))?;
    // SAFETY: allocation; null on OOM (checked).
    let mut buf =
        unsafe { ff::av_hwdevice_ctx_alloc(ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA) };
    if buf.is_null() {
        return Err(VideoError::OutOfMemory);
    }
    // SAFETY: the buffer of a D3D11VA device holds an `AVHWDeviceContext` whose `hwctx` points to
    // a zeroed `AVD3D11VADeviceContext` (FFmpeg's `hwcontext_d3d11va.c`), not yet initialised.
    // Each pointer stored is an owned reference (`into_raw` of a clone): FFmpeg releases the
    // device and the context with the device context.
    let r = unsafe {
        let hw = (*(*buf).data.cast::<ff::AVHWDeviceContext>())
            .hwctx
            .cast::<AvD3d11vaDeviceContext>();
        (*hw).device = device.raw().clone().into_raw();
        (*hw).device_context = device.context().clone().into_raw();
        (*hw).bind_flags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
        ff::av_hwdevice_ctx_init(buf)
    };
    if r < 0 {
        // SAFETY: owned reference (releases the interfaces stored above).
        unsafe { ff::av_buffer_unref(&mut buf) };
        return Err(fail("initialisation", &av_err(r)));
    }
    Ok(HwDevice {
        kind: HwKind::D3d11va,
        buf,
        pix_fmt: ff::AVPixelFormat::AV_PIX_FMT_D3D11,
        device: device.info().name.clone(),
        _keep: None,
    })
}

/// Options of [`D3d11Decoder::open`].
#[derive(Clone, Copy, Debug)]
pub struct D3d11Options {
    /// [`HwDecode::No`] = software decoding only; anything else tries D3D11VA first.
    pub hwdec: HwDecode,
    /// Frames the caller keeps at once (queued, being converted, shown): slices of the decoder's
    /// pool reserved beyond what the decoder needs for its references.
    pub held_frames: usize,
}

impl Default for D3d11Options {
    fn default() -> Self {
        Self {
            hwdec: HwDecode::Auto,
            held_frames: 4,
        }
    }
}

/// A frame decoded by D3D11VA: a slice of the decoder's texture array.
pub struct HardwareFrame {
    decoded: DecodedFrame,
    /// Keeps the slice reserved: the pool reuses it when the last reference goes.
    _frame: Frame,
}

// SAFETY: the texture is an interface of a device created without SINGLETHREADED and with the
// runtime's multithread protection on (`hw_device`), usable from any thread; the `AVFrame` is
// plain reference-counted data.
unsafe impl Send for HardwareFrame {}

impl HardwareFrame {
    /// The slice to sample (`lumenwall_d3d11::plane_views`).
    #[must_use]
    pub fn decoded(&self) -> &DecodedFrame {
        &self.decoded
    }
}

/// A decoded frame in system memory.
pub struct SoftwareFrame {
    cpu: Option<CpuFrame>,
    pool: FramePool,
}

impl SoftwareFrame {
    #[must_use]
    pub fn layout(&self) -> YuvLayout {
        self.cpu().layout
    }

    /// Bytes and row stride of plane `i` (Y, then U or UV, then V).
    #[must_use]
    pub fn plane(&self, i: usize) -> Option<(&[u8], u32)> {
        self.cpu().plane(i)
    }

    fn cpu(&self) -> &CpuFrame {
        // Only `Drop` takes it.
        self.cpu.as_ref().expect("frame alive")
    }
}

impl Drop for SoftwareFrame {
    fn drop(&mut self) {
        if let Some(cpu) = self.cpu.take() {
            cpu.recycle(&self.pool);
        }
    }
}

/// What a decoded frame is made of.
pub enum FrameImage {
    /// Zero copy: the decoder's own texture.
    Hardware(HardwareFrame),
    /// CPU planes, to upload with [`PlaneTextures`].
    Software(SoftwareFrame),
}

/// One decoded frame.
pub struct VideoFrame {
    /// Media time in seconds, continuous across loops.
    pub time: f64,
    pub color: YuvColor,
    pub image: FrameImage,
}

/// Demuxer + decoder of one video, decoding on the renderer's device. Every frame, in
/// order, endlessly (the file loops with a continuous timeline); pacing, decimation and
/// threading are the caller's.
pub struct D3d11Decoder {
    source: Source,
    pool: FramePool,
    native: bool,
    warned_format: bool,
}

impl D3d11Decoder {
    /// Opens `input`, decodes its first frame. `device` is the renderer's (created without
    /// `single_threaded`).
    pub fn open(
        device: &Device,
        input: &VideoInput,
        opts: &D3d11Options,
    ) -> Result<(Self, VideoFrame), VideoError> {
        crate::load_decoder()?;
        init_logging();
        let pool = FramePool::default();
        let luid = device.info().luid;
        let adapter = AdapterHint::from_luid(luid.low, luid.high);
        let mut last = None;
        let mut attempts = Vec::new();
        if opts.hwdec != HwDecode::No {
            match hw_device(device) {
                Ok(dev) => attempts.push(Some(Attempt::D3d11(dev, opts.held_frames as c_int + 2))),
                Err(e) => tracing::info!("video: no D3D11VA decoding on the render device: {e}"),
            }
        }
        attempts.push(None);
        for attempt in attempts {
            let hardware = attempt.is_some();
            match Source::open_raw(input, attempt, &adapter, pool.clone()) {
                Ok((source, first, time)) => {
                    let mut d = Self {
                        source,
                        pool,
                        native: hardware,
                        warned_format: false,
                    };
                    let frame = d.finish(first, time)?;
                    return Ok((d, frame));
                }
                Err(e @ (VideoError::Open { .. } | VideoError::NoVideoStream { .. })) => {
                    return Err(e);
                }
                Err(e) => {
                    tracing::info!(
                        "video: {} decoding unavailable: {e}",
                        if hardware { "D3D11VA" } else { "software" }
                    );
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or(VideoError::Decode("no decoder".into())))
    }

    /// The next frame.
    pub fn next_frame(&mut self) -> Result<VideoFrame, VideoError> {
        let (frame, time) = self.source.next()?;
        self.finish(frame, time)
    }

    fn finish(&mut self, frame: Frame, time: f64) -> Result<VideoFrame, VideoError> {
        let (w, h) = (
            frame.get().width.max(1) as u32,
            frame.get().height.max(1) as u32,
        );
        if self.native && frame.format() == self.source.hw_pix_fmt() {
            let sw_format = self.source.hw_sw_format(&frame);
            let plane_format = if sw_format == ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int {
                Some(PlaneFormat::Nv12)
            } else if sw_format == ff::AVPixelFormat::AV_PIX_FMT_P010LE as c_int {
                Some(PlaneFormat::P010)
            } else {
                None
            };
            let decoded = plane_format.and_then(|f| native_frame(frame.get(), f, (w, h)));
            if let (Some(decoded), Some(px)) = (decoded, describe(sw_format, w, h)) {
                self.source.path_kind = DecodePath::ZeroCopy(HwKind::D3d11va);
                return Ok(VideoFrame {
                    time,
                    color: colorimetry(frame.get(), &px),
                    image: FrameImage::Hardware(HardwareFrame {
                        decoded,
                        _frame: frame,
                    }),
                });
            }
            if !self.warned_format {
                self.warned_format = true;
                tracing::warn!(
                    "video: D3D11VA frames of format {} are downloaded to system memory",
                    super::pix_name(sw_format)
                );
            }
        }
        match self.source.prepare(frame)? {
            super::DecodedFrame::Cpu(cpu) => {
                let color = cpu.color;
                Ok(VideoFrame {
                    time,
                    color,
                    image: FrameImage::Software(SoftwareFrame {
                        cpu: Some(cpu),
                        pool: self.pool.clone(),
                    }),
                })
            }
            super::DecodedFrame::Gpu(_) => Err(VideoError::Decode(
                "unexpected Vulkan frame on the Direct3D 11 path".into(),
            )),
        }
    }

    #[must_use]
    pub fn info(&self) -> &VideoInfo {
        self.source.info()
    }

    /// How frames reach the GPU, known from the first frame: [`DecodePath::ZeroCopy`] with
    /// [`HwKind::D3d11va`] for [`FrameImage::Hardware`], else [`DecodePath::Software`] or
    /// [`DecodePath::HardwareCopy`].
    #[must_use]
    pub fn decode_path(&self) -> DecodePath {
        self.source.decode_path()
    }

    /// Times the file looped.
    #[must_use]
    pub fn loops(&self) -> u64 {
        self.source.loops()
    }

    /// Path of the video in messages.
    #[must_use]
    pub fn name(&self) -> PathBuf {
        self.source.path.clone()
    }
}

impl Source {
    fn hw_pix_fmt(&self) -> c_int {
        self.hw.as_ref().map_or(-1, |d| d.pix_fmt as c_int)
    }

    /// Software format of the frames context of a hardware `frame`.
    fn hw_sw_format(&self, frame: &Frame) -> c_int {
        // SAFETY: hardware frames carry their frames context, whose data is an
        // `AVHWFramesContext`.
        unsafe {
            let fc = (*frame.ptr()).hw_frames_ctx;
            if fc.is_null() {
                return -1;
            }
            (*(*fc).data.cast::<ff::AVHWFramesContext>()).sw_format as c_int
        }
    }
}

/// The slice of a D3D11VA frame (`data[0]` = `ID3D11Texture2D*`, `data[1]` = array index,
/// `AVD3D11FrameDescriptor`), `None` when it cannot be sampled.
fn native_frame(f: &ff::AVFrame, format: PlaneFormat, size: (u32, u32)) -> Option<DecodedFrame> {
    let raw = f.data[0].cast::<c_void>();
    if raw.is_null() {
        return None;
    }
    // SAFETY: `data[0]` of a D3D11 frame is an `ID3D11Texture2D` kept alive by the frame's
    // buffer, which the caller holds; `clone` adds a reference of our own.
    let texture = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }?.clone();
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: live out-parameter.
    unsafe { texture.GetDesc(&mut desc) };
    if desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 == 0 {
        tracing::warn!("video: the D3D11VA texture cannot be sampled (no shader-resource bind)");
        return None;
    }
    let slice = f.data[1] as usize as u32;
    (slice < desc.ArraySize).then_some(DecodedFrame {
        texture,
        slice,
        format,
        size,
    })
}

/// Writable textures that hold the planes of a [`SoftwareFrame`]: the fallback of the zero-copy
/// path, one upload per plane per frame. Formats follow the plane layout: `R8`/`R8G8` UNORM for
/// 8-bit samples, `R16`/`R16G16` UNORM for deeper ones (what the views of a P010 decoder
/// texture have too, so one shader reads both; [`YuvColor::shift`] says where the bits are).
pub struct PlaneTextures {
    layout: YuvLayout,
    textures: Vec<ID3D11Texture2D>,
    views: Vec<ID3D11ShaderResourceView>,
}

impl PlaneTextures {
    /// Textures for `layout`, filled with zeros.
    pub fn new(device: &Device, layout: YuvLayout) -> Result<Self, VideoError> {
        let fail = |what: &str, e: &dyn std::fmt::Display| {
            VideoError::Decode(format!("plane texture: {what}: {e}"))
        };
        let mut textures = Vec::new();
        let mut views = Vec::new();
        for i in 0..layout.plane_count() {
            let (w, h) = layout.plane_size(i);
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: plane_dxgi_format(&layout, i),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let zeros = vec![0u8; layout.plane_row_bytes(i) as usize * h as usize];
            let init = D3D11_SUBRESOURCE_DATA {
                pSysMem: zeros.as_ptr().cast(),
                SysMemPitch: layout.plane_row_bytes(i),
                SysMemSlicePitch: 0,
            };
            let mut texture = None;
            // SAFETY: valid descriptor; `init` points to `zeros`, which outlives the call and
            // holds `h` rows of the pitch given; live out-parameter.
            unsafe {
                device
                    .raw()
                    .CreateTexture2D(&desc, Some(&init), Some(&mut texture))
            }
            .map_err(|e| fail("CreateTexture2D", &e))?;
            let texture = texture.ok_or_else(|| fail("CreateTexture2D", &"no texture"))?;
            let mut view = None;
            // SAFETY: a texture of this device with the shader-resource bind flag; default view.
            unsafe {
                device
                    .raw()
                    .CreateShaderResourceView(&texture, None, Some(&mut view))
            }
            .map_err(|e| fail("CreateShaderResourceView", &e))?;
            views.push(view.ok_or_else(|| fail("CreateShaderResourceView", &"no view"))?);
            textures.push(texture);
        }
        Ok(Self {
            layout,
            textures,
            views,
        })
    }

    #[must_use]
    pub fn layout(&self) -> YuvLayout {
        self.layout
    }

    /// Queues the copy of `frame`'s planes into the textures (on the immediate context: call
    /// from the render thread). The frame can be dropped on return.
    pub fn upload(&self, device: &Device, frame: &SoftwareFrame) -> Result<(), VideoError> {
        if frame.layout() != self.layout {
            return Err(VideoError::Decode(
                "the frame's plane layout changed: build new PlaneTextures".into(),
            ));
        }
        for (i, texture) in self.textures.iter().enumerate() {
            let (bytes, stride) = frame
                .plane(i)
                .ok_or_else(|| VideoError::Decode(format!("plane {i} is missing")))?;
            // SAFETY: a DEFAULT texture of this device; `bytes` holds `height - 1` rows of
            // `stride` bytes and a last row of at least the row size, which is all that is
            // read; `UpdateSubresource` copies before returning.
            unsafe {
                device.context().UpdateSubresource(
                    texture,
                    0,
                    None,
                    bytes.as_ptr().cast(),
                    stride,
                    0,
                );
            }
        }
        Ok(())
    }

    /// Views of the planes in order (Y, then UV, or U and V).
    #[must_use]
    pub fn views(&self) -> &[ID3D11ShaderResourceView] {
        &self.views
    }

    /// Plane `i` (tests, read back).
    #[must_use]
    pub fn texture(&self, i: usize) -> Option<&ID3D11Texture2D> {
        self.textures.get(i)
    }
}

fn plane_dxgi_format(layout: &YuvLayout, i: usize) -> DXGI_FORMAT {
    let two = i > 0 && layout.planes != ChromaPlanes::Planar;
    match (layout.depth, two) {
        (PlaneDepth::U8, false) => DXGI_FORMAT_R8_UNORM,
        (PlaneDepth::U8, true) => DXGI_FORMAT_R8G8_UNORM,
        (PlaneDepth::U16, false) => DXGI_FORMAT_R16_UNORM,
        (PlaneDepth::U16, true) => DXGI_FORMAT_R16G16_UNORM,
    }
}

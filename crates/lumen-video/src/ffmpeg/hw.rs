//! FFmpeg hardware devices: creation on the rendering GPU and the `get_format` callback.

use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use ffmpeg_sys_next as ff;

use super::av_err;
use crate::HwKind;
use crate::select::{AdapterHint, VENDOR_AMD, VENDOR_INTEL, pick_render_node, render_nodes};

fn device_type(kind: HwKind) -> ff::AVHWDeviceType {
    match kind {
        HwKind::Cuda => ff::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
        HwKind::Vaapi => ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
        HwKind::Vulkan => ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN,
        HwKind::D3d11va => ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
    }
}

/// Hardware pixel format of `codec` for `kind`, if the decoder supports that device.
pub(crate) fn hw_pix_fmt(codec: *const ff::AVCodec, kind: HwKind) -> Option<ff::AVPixelFormat> {
    let ty = device_type(kind);
    for i in 0.. {
        // SAFETY: `codec` is a valid decoder; the function returns null past the last config.
        let cfg = unsafe { ff::avcodec_get_hw_config(codec, i) };
        // SAFETY: non-null configs are static FFmpeg data.
        let cfg = unsafe { cfg.as_ref() }?;
        let by_device = ff::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int;
        if cfg.methods & by_device != 0 && cfg.device_type == ty {
            return Some(cfg.pix_fmt);
        }
    }
    None
}

/// A reference to an FFmpeg hardware device context.
pub(crate) struct HwDevice {
    pub kind: HwKind,
    pub buf: *mut ff::AVBufferRef,
    pub pix_fmt: ff::AVPixelFormat,
    /// Device string given to FFmpeg (logged).
    pub device: String,
    /// Data FFmpeg points to for the device's lifetime (shared Vulkan device strings).
    pub _keep: Option<Box<dyn std::any::Any + Send>>,
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        // SAFETY: owned reference.
        unsafe { ff::av_buffer_unref(&mut self.buf) };
    }
}

/// Opens a `kind` device on the rendering GPU.
pub(crate) fn open_device(
    kind: HwKind,
    adapter: &AdapterHint,
    pix_fmt: ff::AVPixelFormat,
) -> Result<HwDevice, String> {
    let device: Option<String> = match kind {
        HwKind::Vaapi => {
            let nodes = render_nodes();
            match pick_render_node(&nodes, adapter) {
                Some(n) => Some(n.path.clone()),
                // Unknown adapter (software renderer): FFmpeg's default node.
                None if adapter.vendor == 0 => None,
                None if matches!(adapter.vendor, VENDOR_AMD | VENDOR_INTEL) => {
                    return Err("no DRM render node for the rendering GPU".into());
                }
                None => None,
            }
        }
        // FFmpeg selects a Vulkan device by a substring of its name: wgpu reports the same
        // `deviceName`.
        HwKind::Vulkan if !adapter.name.is_empty() => Some(adapter.name.clone()),
        HwKind::Vulkan | HwKind::Cuda | HwKind::D3d11va => None,
    };
    let c_device = device
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut buf = ptr::null_mut();
    // SAFETY: out-pointer and optional NUL-terminated device string.
    let r = unsafe {
        ff::av_hwdevice_ctx_create(
            &mut buf,
            device_type(kind),
            c_device.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            ptr::null_mut(),
            0,
        )
    };
    if r < 0 || buf.is_null() {
        return Err(format!(
            "cannot open the {} device{}: {}",
            kind.name(),
            device
                .as_deref()
                .map(|d| format!(" '{d}'"))
                .unwrap_or_default(),
            av_err(r)
        ));
    }
    Ok(HwDevice {
        kind,
        buf,
        pix_fmt,
        device: device.unwrap_or_else(|| "default".into()),
        _keep: None,
    })
}

/// State read by [`get_format`] through `AVCodecContext::opaque`.
pub(crate) struct FormatChoice {
    /// Hardware pixel format to pick when offered.
    pub wanted: c_int,
    /// The decoder offered no hardware format (unsupported profile/size, init failure).
    pub refused: AtomicBool,
}

/// `AVCodecContext::get_format`: the hardware format when offered, else FFmpeg's default
/// (software) choice.
///
/// # Safety
///
/// Called by libavcodec with a valid context whose `opaque` is null or a live
/// [`FormatChoice`], and a list terminated by `AV_PIX_FMT_NONE`.
pub(crate) unsafe extern "C" fn get_format(
    ctx: *mut ff::AVCodecContext,
    list: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    // SAFETY: see the function contract.
    let choice = unsafe { ((*ctx).opaque as *const FormatChoice).as_ref() };
    if let Some(choice) = choice {
        let mut p = list;
        loop {
            // SAFETY: the list is terminated by AV_PIX_FMT_NONE.
            let f = unsafe { *p };
            if f == ff::AVPixelFormat::AV_PIX_FMT_NONE {
                break;
            }
            if f as c_int == choice.wanted {
                return f;
            }
            // SAFETY: still inside the terminated list.
            p = unsafe { p.add(1) };
        }
        choice.refused.store(true, Ordering::Release);
    }
    // SAFETY: forwarded arguments.
    unsafe { ff::avcodec_default_get_format(ctx, list) }
}

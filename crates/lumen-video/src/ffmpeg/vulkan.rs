//! Zero-copy decoding: FFmpeg's Vulkan Video decoder on wgpu's own `VkDevice`.
//!
//! The device is created by lumen-render with extra decode/compute/transfer queues
//! (`lumen_render::video_device`); here it is handed to FFmpeg as a user-provided
//! `AVVulkanDeviceContext` whose queue families are the extra ones plus wgpu's family, the
//! latter listed with the graphics flag only: FFmpeg never submits to it, but lists it in
//! the concurrent sharing of every image, so wgpu's queue may sample the decoded images.
//!
//! Per displayed frame (render thread, [`begin_sampling`]): lock the `AVVkFrame`, wait on
//! its timeline semaphore at its current value, transition it from whatever layout FFmpeg
//! left to `SHADER_READ_ONLY_OPTIMAL` (the state wgpu tracks for the wrapped texture), signal
//! value + 1 after the frame's submission, and record that in the `AVVkFrame` so FFmpeg's
//! next use of the image (as a reference or a new picture) waits for our read and
//! transitions from the right layout. The layout transition is a raw-Vulkan command buffer
//! submitted just before the frame's (wgpu forbids mixing raw and wgpu commands in one
//! encoder).
//!
//! Struct layouts are FFmpeg 8 (libavutil 60, `FF_API_VULKAN_FIXED_QUEUES` still set);
//! offsets are checked at compile time against a C probe of the installed header.

use std::ffi::{CString, c_char, c_int, c_void};
use std::mem::offset_of;

use ash::vk;
use ffmpeg_sys_next as ff;
use lumen_render::GpuContext;

use super::av_err;
use super::hw::HwDevice;
use crate::HwKind;

const AV_NUM_DATA_POINTERS: usize = 8;

#[repr(C)]
#[derive(Clone, Copy)]
struct AVVulkanDeviceQueueFamily {
    idx: c_int,
    num: c_int,
    flags: u32,
    video_caps: u32,
}

#[repr(C)]
struct AVVulkanDeviceContext {
    alloc: *const c_void,
    get_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    inst: vk::Instance,
    phys_dev: vk::PhysicalDevice,
    act_dev: vk::Device,
    device_features: vk::PhysicalDeviceFeatures2<'static>,
    enabled_inst_extensions: *const *const c_char,
    nb_enabled_inst_extensions: c_int,
    enabled_dev_extensions: *const *const c_char,
    nb_enabled_dev_extensions: c_int,
    /// Deprecated fixed-queue fields (`FF_API_VULKAN_FIXED_QUEUES`): left at 0, which
    /// FFmpeg reads as "not set" once `nb_qf` is filled.
    deprecated_queues: [c_int; 10],
    lock_queue: Option<unsafe extern "C" fn(*mut ff::AVHWDeviceContext, u32, u32)>,
    unlock_queue: Option<unsafe extern "C" fn(*mut ff::AVHWDeviceContext, u32, u32)>,
    qf: [AVVulkanDeviceQueueFamily; 64],
    nb_qf: c_int,
}

#[repr(C)]
struct AVVulkanFramesContext {
    tiling: vk::ImageTiling,
    usage: u32,
    create_pnext: *mut c_void,
    alloc_pnext: [*mut c_void; AV_NUM_DATA_POINTERS],
    flags: c_int,
    img_flags: u32,
    format: [vk::Format; AV_NUM_DATA_POINTERS],
    nb_layers: c_int,
    lock_frame: Option<unsafe extern "C" fn(*mut ff::AVHWFramesContext, *mut AVVkFrame)>,
    unlock_frame: Option<unsafe extern "C" fn(*mut ff::AVHWFramesContext, *mut AVVkFrame)>,
}

#[repr(C)]
struct AVVkFrame {
    img: [vk::Image; AV_NUM_DATA_POINTERS],
    tiling: vk::ImageTiling,
    mem: [vk::DeviceMemory; AV_NUM_DATA_POINTERS],
    size: [usize; AV_NUM_DATA_POINTERS],
    flags: u32,
    access: [vk::AccessFlags; AV_NUM_DATA_POINTERS],
    layout: [vk::ImageLayout; AV_NUM_DATA_POINTERS],
    sem: [vk::Semaphore; AV_NUM_DATA_POINTERS],
    sem_value: [u64; AV_NUM_DATA_POINTERS],
    internal: *mut c_void,
    offset: [isize; AV_NUM_DATA_POINTERS],
    queue_family: [u32; AV_NUM_DATA_POINTERS],
}

// Offsets printed by a C program compiled against the installed hwcontext_vulkan.h.
const _: () = {
    assert!(offset_of!(AVVulkanDeviceContext, device_features) == 40);
    assert!(offset_of!(AVVulkanDeviceContext, enabled_inst_extensions) == 280);
    assert!(offset_of!(AVVulkanDeviceContext, enabled_dev_extensions) == 296);
    assert!(offset_of!(AVVulkanDeviceContext, lock_queue) == 352);
    assert!(offset_of!(AVVulkanDeviceContext, qf) == 368);
    assert!(offset_of!(AVVulkanDeviceContext, nb_qf) == 1392);
    assert!(size_of::<AVVulkanDeviceContext>() == 1400);
    assert!(offset_of!(AVVulkanFramesContext, flags) == 80);
    assert!(offset_of!(AVVulkanFramesContext, format) == 88);
    assert!(offset_of!(AVVulkanFramesContext, lock_frame) == 128);
    assert!(size_of::<AVVulkanFramesContext>() == 144);
    assert!(offset_of!(AVVkFrame, mem) == 72);
    assert!(offset_of!(AVVkFrame, access) == 204);
    assert!(offset_of!(AVVkFrame, layout) == 236);
    assert!(offset_of!(AVVkFrame, sem) == 272);
    assert!(offset_of!(AVVkFrame, sem_value) == 336);
    assert!(offset_of!(AVVkFrame, queue_family) == 472);
    assert!(size_of::<AVVkFrame>() == 504);
};

/// Extension name strings FFmpeg keeps pointers to for the device's lifetime.
struct Strings {
    _instance: Vec<CString>,
    _device: Vec<CString>,
    instance_ptrs: Vec<*const c_char>,
    device_ptrs: Vec<*const c_char>,
}

// SAFETY: immutable C strings, only read by FFmpeg.
unsafe impl Send for Strings {}

/// An FFmpeg Vulkan device context wrapping wgpu's device, when it has decode queues.
pub(crate) fn shared_device(ctx: &GpuContext) -> Result<HwDevice, String> {
    let queues = *ctx
        .video_queues()
        .ok_or("the render device has no video decode queue")?;
    // SAFETY: the hal objects are only read here; wgpu keeps them alive as long as `ctx`,
    // which outlives the player (and with it this FFmpeg device).
    let (inst, entry_fn, phys, dev, inst_exts, dev_exts) = unsafe {
        let hal_dev = ctx
            .device()
            .as_hal::<wgpu::hal::api::Vulkan>()
            .ok_or("not a Vulkan device")?;
        let shared = hal_dev.shared_instance();
        (
            shared.raw_instance().handle(),
            shared.entry().static_fn().get_instance_proc_addr,
            hal_dev.raw_physical_device(),
            hal_dev.raw_device().handle(),
            shared
                .extensions()
                .iter()
                .map(|e| CString::from(*e))
                .collect::<Vec<_>>(),
            hal_dev
                .enabled_device_extensions()
                .iter()
                .map(|e| CString::from(*e))
                .collect::<Vec<_>>(),
        )
    };
    let strings = Box::new(Strings {
        instance_ptrs: inst_exts.iter().map(|s| s.as_ptr()).collect(),
        device_ptrs: dev_exts.iter().map(|s| s.as_ptr()).collect(),
        _instance: inst_exts,
        _device: dev_exts,
    });
    // SAFETY: allocation of a Vulkan device context.
    let mut buf = unsafe { ff::av_hwdevice_ctx_alloc(ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN) };
    if buf.is_null() {
        return Err("av_hwdevice_ctx_alloc failed".into());
    }
    // SAFETY: a freshly allocated, zeroed Vulkan device context of this FFmpeg's layout.
    let r = unsafe {
        let device_ctx = (*buf).data as *mut ff::AVHWDeviceContext;
        let hw = &mut *((*device_ctx).hwctx as *mut AVVulkanDeviceContext);
        hw.get_proc_addr = entry_fn;
        hw.inst = inst;
        hw.phys_dev = phys;
        hw.act_dev = dev;
        hw.device_features = vk::PhysicalDeviceFeatures2::default();
        hw.enabled_inst_extensions = strings.instance_ptrs.as_ptr();
        hw.nb_enabled_inst_extensions = strings.instance_ptrs.len() as c_int;
        hw.enabled_dev_extensions = strings.device_ptrs.as_ptr();
        hw.nb_enabled_dev_extensions = strings.device_ptrs.len() as c_int;
        let mut n = 0;
        let mut add = |idx: u32, flags: vk::QueueFlags, caps: u32| {
            hw.qf[n] = AVVulkanDeviceQueueFamily {
                idx: idx as c_int,
                num: 1,
                flags: flags.as_raw(),
                video_caps: caps,
            };
            n += 1;
        };
        add(
            queues.decode_family,
            vk::QueueFlags::VIDEO_DECODE_KHR,
            queues.decode_operations,
        );
        if queues.transfer_family == queues.compute_family {
            add(
                queues.compute_family,
                vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER,
                0,
            );
        } else {
            add(queues.compute_family, vk::QueueFlags::COMPUTE, 0);
            add(queues.transfer_family, vk::QueueFlags::TRANSFER, 0);
        }
        // wgpu's family: only so that images are shared with it (never submitted to).
        add(queues.render_family, vk::QueueFlags::GRAPHICS, 0);
        hw.nb_qf = n as c_int;
        ff::av_hwdevice_ctx_init(buf)
    };
    if r < 0 {
        // SAFETY: owned reference.
        unsafe { ff::av_buffer_unref(&mut buf) };
        return Err(format!("FFmpeg refused the render device: {}", av_err(r)));
    }
    Ok(HwDevice {
        kind: HwKind::Vulkan,
        buf,
        pix_fmt: ff::AVPixelFormat::AV_PIX_FMT_VULKAN,
        device: "render device (shared with wgpu)".into(),
        _keep: Some(strings),
    })
}

/// The decoded image of a Vulkan frame, its allocated size and its frames context.
pub(crate) fn frame_image(frame: &ff::AVFrame) -> Option<(vk::Image, (u32, u32), usize)> {
    if frame.hw_frames_ctx.is_null() || frame.data[0].is_null() {
        return None;
    }
    // SAFETY: Vulkan hardware frames carry an AVVkFrame in data[0] and their frames
    // context; the image handle never changes for the life of the pool entry.
    unsafe {
        let vkf = &*(frame.data[0] as *const AVVkFrame);
        let fc = (*frame.hw_frames_ctx).data as *const ff::AVHWFramesContext;
        Some((
            vkf.img[0],
            ((*fc).width.max(1) as u32, (*fc).height.max(1) as u32),
            fc as usize,
        ))
    }
}

/// Makes the decoded image of `frame` readable by the next submission on wgpu's queue:
/// returns the layout-transition command buffer to submit first, and schedules the
/// semaphore wait/signal on that submission.
///
/// # Safety
///
/// `frame` is a Vulkan frame decoded on `device`'s `VkDevice`; the returned command buffer
/// and the frame's sampling commands must be submitted together by the next
/// `queue.submit`.
pub(crate) unsafe fn begin_sampling(
    frame: &ff::AVFrame,
    raw: &ash::Device,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> Result<wgpu::CommandBuffer, String> {
    // SAFETY: see the function contract.
    unsafe {
        let vkf = frame.data[0] as *mut AVVkFrame;
        let fc = (*frame.hw_frames_ctx).data as *mut ff::AVHWFramesContext;
        let vkfc = (*fc).hwctx as *mut AVVulkanFramesContext;
        let (Some(lock), Some(unlock)) = ((*vkfc).lock_frame, (*vkfc).unlock_frame) else {
            return Err("frames context without lock functions".into());
        };
        let hal_queue = queue
            .as_hal::<wgpu::hal::api::Vulkan>()
            .ok_or("not a Vulkan queue")?;
        lock(fc, vkf);
        let image = (*vkf).img[0];
        let old = (*vkf).layout[0];
        let sem = (*vkf).sem[0];
        let value = (*vkf).sem_value[0];
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lumen.video.acquire"),
        });
        let recorded = enc.as_hal_mut::<wgpu::hal::api::Vulkan, _, _>(|e| {
            let e = e?;
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(old)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            // The semaphore wait (all commands) precedes this barrier: no source access.
            raw.cmd_pipeline_barrier(
                e.raw_handle(),
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
            Some(())
        });
        if recorded.is_none() {
            unlock(fc, vkf);
            return Err("cannot record the layout transition".into());
        }
        let cmd = enc.finish();
        hal_queue.add_wait_semaphore(sem, Some(value), vk::PipelineStageFlags::ALL_COMMANDS);
        hal_queue.add_signal_semaphore(sem, Some(value + 1));
        (*vkf).sem_value[0] = value + 1;
        (*vkf).layout[0] = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        (*vkf).access[0] = vk::AccessFlags::SHADER_READ;
        unlock(fc, vkf);
        Ok(cmd)
    }
}

/// Wraps a decoded NV12 image as a sampled wgpu texture (FFmpeg keeps ownership of the
/// image and its memory). Its tracked state is `RESOURCE` (`SHADER_READ_ONLY_OPTIMAL`),
/// which [`begin_sampling`] restores before every use.
///
/// # Safety
///
/// `image` is an NV12 image of `size`, created by FFmpeg on `device`'s `VkDevice` with
/// `SAMPLED` usage and `MUTABLE_FORMAT`, alive as long as the returned texture.
pub(crate) unsafe fn wrap_nv12(
    device: &wgpu::Device,
    image: vk::Image,
    size: (u32, u32),
) -> Option<wgpu::Texture> {
    let extent = wgpu::Extent3d {
        width: size.0,
        height: size.1,
        depth_or_array_layers: 1,
    };
    // SAFETY: see the function contract.
    unsafe {
        let hal_dev = device.as_hal::<wgpu::hal::api::Vulkan>()?;
        let hal_tex = hal_dev.texture_from_raw(
            image,
            &wgpu::hal::TextureDescriptor {
                label: Some("lumen.video.decoded"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::NV12,
                usage: wgpu::wgt::TextureUses::RESOURCE,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: Vec::new(),
            },
            // FFmpeg owns the image: never destroyed by wgpu.
            Some(Box::new(|| {})),
            wgpu::hal::vulkan::TextureMemory::External,
        );
        drop(hal_dev);
        Some(device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(
            hal_tex,
            &wgpu::TextureDescriptor {
                label: Some("lumen.video.decoded"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::NV12,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::wgt::TextureUses::RESOURCE,
        ))
    }
}

/// The raw `ash::Device` of a wgpu device (cloned once per player).
pub(crate) fn raw_device(device: &wgpu::Device) -> Option<ash::Device> {
    // SAFETY: read-only access to the function table; wgpu keeps the device alive.
    unsafe {
        device
            .as_hal::<wgpu::hal::api::Vulkan>()
            .map(|d| d.raw_device().clone())
    }
}

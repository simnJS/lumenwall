//! Optional Vulkan Video queues on the wgpu device, for zero-copy video decoding.
//!
//! When a video wallpaper is loaded ([`crate::ContextOptions::video_decode`]), the device is
//! created through wgpu-hal with the Vulkan Video decode extensions, `synchronization2`, and
//! one extra queue in each of a decode, a compute and a transfer family — never in the
//! render queue's family, so FFmpeg's Vulkan decoder (lumenwall-video) can submit on its own
//! queues while wgpu keeps exclusive use of its queue. FFmpeg then decodes straight into
//! images this device can sample: no copy between the decoder and the renderer.
//!
//! Falls back to the regular device (and the copy paths of lumenwall-video) when the adapter is
//! not Vulkan 1.3, lacks the video extensions, or has no separate compute/transfer
//! families.

use std::ffi::CStr;

use ash::vk;

/// Queue families set up for the video decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoQueues {
    /// Family of wgpu's queue (index 0 of it is wgpu's).
    pub render_family: u32,
    /// Family with `VIDEO_DECODE`, one queue created.
    pub decode_family: u32,
    /// `VkVideoCodecOperationFlagsKHR` of the decode family.
    pub decode_operations: u32,
    /// Compute family (not the render one), one queue created.
    pub compute_family: u32,
    /// Transfer family (not the render one), one queue created; may equal `compute_family`.
    pub transfer_family: u32,
}

const PRIORITY: [f32; 1] = [1.0];

fn has_ext(available: &[vk::ExtensionProperties], name: &CStr) -> bool {
    available
        .iter()
        .any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == name))
}

/// Picks the families; `None` when the adapter cannot host the decoder next to wgpu.
fn pick_families(families: &[(vk::QueueFlags, u32)], render_family: u32) -> Option<VideoQueues> {
    let wanted = (vk::VideoCodecOperationFlagsKHR::DECODE_H264
        | vk::VideoCodecOperationFlagsKHR::DECODE_H265
        | vk::VideoCodecOperationFlagsKHR::DECODE_AV1)
        .as_raw();
    let (decode_family, &(_, decode_operations)) = families
        .iter()
        .enumerate()
        .filter(|(i, (f, ops))| {
            *i as u32 != render_family
                && f.contains(vk::QueueFlags::VIDEO_DECODE_KHR)
                && ops & wanted != 0
        })
        .max_by_key(|(_, (_, ops))| (ops & wanted).count_ones())?;
    let other = |i: usize| i as u32 != render_family && i != decode_family;
    // Compute: prefer a family without graphics. Transfer: prefer a dedicated one.
    let compute_family = families
        .iter()
        .enumerate()
        .filter(|(i, (f, _))| other(*i) && f.contains(vk::QueueFlags::COMPUTE))
        .min_by_key(|(_, (f, _))| f.contains(vk::QueueFlags::GRAPHICS))?
        .0;
    let transfer_family = families
        .iter()
        .enumerate()
        .filter(|(i, (f, _))| other(*i) && f.contains(vk::QueueFlags::TRANSFER))
        .min_by_key(|(_, (f, _))| {
            (
                f.contains(vk::QueueFlags::GRAPHICS),
                f.contains(vk::QueueFlags::COMPUTE),
            )
        })
        .map_or(compute_family, |(i, _)| i);
    Some(VideoQueues {
        render_family,
        decode_family: decode_family as u32,
        decode_operations,
        compute_family: compute_family as u32,
        transfer_family: transfer_family as u32,
    })
}

/// Creates the device and queue with the video decoder's queues and extensions.
pub(crate) fn open(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
) -> Option<(wgpu::Device, wgpu::Queue, VideoQueues)> {
    // SAFETY: the hal adapter is only used during this call, while `adapter` lives.
    let hal = unsafe { adapter.as_hal::<wgpu::hal::api::Vulkan>() }?;
    let instance = hal.shared_instance().raw_instance();
    let phd = hal.raw_physical_device();
    // SAFETY: valid instance and physical device.
    let props = unsafe { instance.get_physical_device_properties(phd) };
    if props.api_version < vk::API_VERSION_1_3 {
        tracing::debug!(target: "lumenwall_render", "video decode queues: Vulkan 1.3 required");
        return None;
    }
    // SAFETY: as above.
    let available = unsafe { instance.enumerate_device_extension_properties(phd) }.ok()?;
    let codecs = [
        ash::khr::video_decode_h264::NAME,
        ash::khr::video_decode_h265::NAME,
        ash::khr::video_decode_av1::NAME,
    ];
    let mut extensions: Vec<&'static CStr> = vec![
        ash::khr::video_queue::NAME,
        ash::khr::video_decode_queue::NAME,
    ];
    if !extensions.iter().all(|e| has_ext(&available, e)) {
        tracing::debug!(target: "lumenwall_render", "video decode queues: no VK_KHR_video_decode_queue");
        return None;
    }
    extensions.extend(codecs.iter().copied().filter(|c| has_ext(&available, c)));
    if extensions.len() == 2 {
        return None;
    }
    for optional in [
        ash::khr::video_maintenance1::NAME,
        ash::khr::push_descriptor::NAME,
    ] {
        if has_ext(&available, optional) {
            extensions.push(optional);
        }
    }
    let mut sync2 = vk::PhysicalDeviceSynchronization2Features::default();
    let mut maintenance1 = vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::default();
    {
        let mut f2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut sync2);
        if extensions.contains(&ash::khr::video_maintenance1::NAME) {
            f2 = f2.push_next(&mut maintenance1);
        }
        // SAFETY: valid physical device and feature chain.
        unsafe { instance.get_physical_device_features2(phd, &mut f2) };
    }
    if sync2.synchronization2 == vk::FALSE {
        return None;
    }
    if maintenance1.video_maintenance1 == vk::FALSE {
        extensions.retain(|e| *e != ash::khr::video_maintenance1::NAME);
    }
    // SAFETY: valid physical device.
    let count = unsafe { instance.get_physical_device_queue_family_properties2_len(phd) };
    let mut video = vec![vk::QueueFamilyVideoPropertiesKHR::default(); count];
    let mut families: Vec<vk::QueueFamilyProperties2<'_>> = video
        .iter_mut()
        .map(|v| vk::QueueFamilyProperties2::default().push_next(v))
        .collect();
    // SAFETY: arrays of `count` chained structures.
    unsafe { instance.get_physical_device_queue_family_properties2(phd, &mut families) };
    let flags: Vec<vk::QueueFlags> = families
        .iter()
        .map(|f| f.queue_family_properties.queue_flags)
        .collect();
    drop(families);
    let summary: Vec<(vk::QueueFlags, u32)> = flags
        .into_iter()
        .zip(video.iter().map(|v| v.video_codec_operations.as_raw()))
        .collect();
    // wgpu-hal always creates its queue in family 0.
    let queues = pick_families(&summary, 0)?;
    let use_maintenance1 = extensions.contains(&ash::khr::video_maintenance1::NAME);
    let mut sync2_on = vk::PhysicalDeviceSynchronization2Features::default().synchronization2(true);
    let mut maintenance1_on =
        vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::default().video_maintenance1(true);
    let mut families_to_add = vec![queues.decode_family, queues.compute_family];
    if queues.transfer_family != queues.compute_family {
        families_to_add.push(queues.transfer_family);
    }
    let added = extensions.clone();
    let (sync2_ref, maintenance1_ref) = (&mut sync2_on, &mut maintenance1_on);
    let callback: Box<wgpu::hal::vulkan::CreateDeviceCallback<'_>> = Box::new(move |args| {
        for e in added {
            if !args.extensions.contains(&e) {
                args.extensions.push(e);
            }
        }
        for f in families_to_add {
            args.queue_create_infos.push(
                vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(f)
                    .queue_priorities(&PRIORITY),
            );
        }
        let mut info = std::mem::take(args.create_info).push_next(sync2_ref);
        if use_maintenance1 {
            info = info.push_next(maintenance1_ref);
        }
        *args.create_info = info;
    });
    // SAFETY: only extensions, queues and features the physical device reports are added.
    let open = unsafe {
        hal.open_with_callback(
            desc.required_features,
            &desc.required_limits,
            &desc.memory_hints,
            Some(callback),
        )
    };
    let open = match open {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(target: "lumenwall_render", "device with video decode queues: {e}");
            return None;
        }
    };
    drop(hal);
    // SAFETY: `open` was created from this adapter with `desc`'s features and limits.
    match unsafe { adapter.create_device_from_hal(open, desc) } {
        Ok((device, queue)) => Some((device, queue, queues)),
        Err(e) => {
            tracing::warn!(target: "lumenwall_render", "device with video decode queues: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: vk::QueueFlags = vk::QueueFlags::GRAPHICS;
    const C: vk::QueueFlags = vk::QueueFlags::COMPUTE;
    const T: vk::QueueFlags = vk::QueueFlags::TRANSFER;
    const D: vk::QueueFlags = vk::QueueFlags::VIDEO_DECODE_KHR;
    const E: vk::QueueFlags = vk::QueueFlags::VIDEO_ENCODE_KHR;

    fn ops(f: vk::VideoCodecOperationFlagsKHR) -> u32 {
        f.as_raw()
    }

    #[test]
    fn nvidia_like_families() {
        let h264 = ops(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
        let all = ops(vk::VideoCodecOperationFlagsKHR::DECODE_H264
            | vk::VideoCodecOperationFlagsKHR::DECODE_H265
            | vk::VideoCodecOperationFlagsKHR::DECODE_AV1);
        let fams = [
            (G | C | T, 0),
            (T, 0),
            (C | T, 0),
            (D | T, h264),
            (D | T, all),
            (E | T, 0),
        ];
        let q = pick_families(&fams, 0).unwrap();
        assert_eq!(q.decode_family, 4);
        assert_eq!(q.compute_family, 2);
        assert_eq!(q.transfer_family, 1);
        assert_eq!(q.decode_operations, all);
    }

    #[test]
    fn no_separate_compute_family() {
        let h264 = ops(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
        assert!(pick_families(&[(G | C | T, 0), (D, h264)], 0).is_none());
        // Transfer falls back to the compute family.
        let q = pick_families(&[(G | C | T, 0), (C | T, 0), (D, h264)], 0).unwrap();
        assert_eq!((q.compute_family, q.transfer_family), (1, 1));
    }
}

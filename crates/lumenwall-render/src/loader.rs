//! Which Vulkan driver serves an adapter, in the terms of the Vulkan loader's driver filter.
//!
//! The loader opens every installed driver (ICD) to enumerate the GPUs, and the Mesa ones stay
//! mapped for the life of the process (RADV, lavapipe and their `libLLVM`: ≈ 50 MiB RSS, and
//! 25–40 ms of instance creation, measured with 9 ICDs installed). `VK_LOADER_DRIVERS_SELECT`
//! restricts the loader to the drivers whose manifest file name matches a glob; the
//! integration remembers the glob of the adapter it used and sets the variable before the next
//! instance (lumenwall-engine `vulkan_driver`). Manifest names are those the drivers install on
//! Linux (`nvidia_icd.json`, `radeon_icd.x86_64.json`, …); on Windows the drivers are
//! registered under other names, so the integration does not use this there.

use ash::vk;

/// Glob of `VK_LOADER_DRIVERS_SELECT` matching the manifest of the driver `id`, for the drivers
/// whose Linux manifest name is known. Pure, for testing.
#[must_use]
pub fn manifest_glob(id: vk::DriverId) -> Option<&'static str> {
    Some(match id {
        vk::DriverId::NVIDIA_PROPRIETARY => "*nvidia*",
        vk::DriverId::MESA_RADV => "*radeon*",
        vk::DriverId::AMD_OPEN_SOURCE => "*amd_icd*",
        // ANV and HASVK (both report this id): `intel_icd`, `intel_hasvk_icd`.
        vk::DriverId::INTEL_OPEN_SOURCE_MESA => "*intel*",
        vk::DriverId::MESA_NVK => "*nouveau*",
        vk::DriverId::MESA_LLVMPIPE => "*lvp*",
        vk::DriverId::MESA_TURNIP => "*freedreno*",
        vk::DriverId::MESA_PANVK => "*panfrost*",
        vk::DriverId::MESA_V3DV => "*broadcom*",
        vk::DriverId::MESA_VENUS => "*virtio*",
        _ => return None,
    })
}

/// [`manifest_glob`] of the driver behind `adapter` (Vulkan 1.2 drivers); `None` for another
/// backend or an unknown driver.
#[must_use]
pub fn adapter_manifest_glob(adapter: &wgpu::Adapter) -> Option<&'static str> {
    // SAFETY: the hal adapter is only used during this call, while `adapter` lives.
    let hal = unsafe { adapter.as_hal::<wgpu::hal::api::Vulkan>() }?;
    let instance = hal.shared_instance().raw_instance();
    let phd = hal.raw_physical_device();
    // SAFETY: valid instance and physical device.
    let props = unsafe { instance.get_physical_device_properties(phd) };
    if props.api_version < vk::API_VERSION_1_2 {
        return None;
    }
    let mut driver = vk::PhysicalDeviceDriverProperties::default();
    let mut p2 = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
    // SAFETY: valid physical device of a Vulkan 1.2 driver, chain of one known structure.
    unsafe { instance.get_physical_device_properties2(phd, &mut p2) };
    manifest_glob(driver.driver_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_of_the_usual_drivers() {
        assert_eq!(
            manifest_glob(vk::DriverId::NVIDIA_PROPRIETARY),
            Some("*nvidia*")
        );
        assert_eq!(manifest_glob(vk::DriverId::MESA_RADV), Some("*radeon*"));
        assert_eq!(
            manifest_glob(vk::DriverId::INTEL_OPEN_SOURCE_MESA),
            Some("*intel*")
        );
        assert_eq!(manifest_glob(vk::DriverId::MOLTENVK), None);
    }
}

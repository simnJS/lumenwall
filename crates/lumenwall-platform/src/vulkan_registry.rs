//! Where Windows registers each display adapter's Vulkan driver and implicit layers.
//!
//! The Vulkan loader on Windows finds drivers through the display adapters: each driver's
//! registry key (`HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-…}\NNNN`) names its
//! driver manifest (`VulkanDriverName`) and the manifests of its implicit layers
//! (`VulkanImplicitLayers`), next to the PCI ids it serves (`MatchingDeviceId`). An implicit
//! layer is loaded into every Vulkan process whatever driver is selected: AMD's
//! "switchable graphics" layer lives in its driver DLL (`amdvlk64.dll`), so on a PC with an AMD
//! iGPU and an NVIDIA card the AMD driver is mapped even when the loader is restricted to the
//! NVIDIA driver (measured). lumenwall-engine reads these entries to restrict the loader to the
//! adapter it uses (`vulkan_driver`). Elsewhere [`adapter_entries`] is empty.

use std::path::PathBuf;

/// The Vulkan registration of one display adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdapterEntry {
    /// PCI vendor id (`0x10de` NVIDIA, `0x1002` AMD, `0x8086` Intel).
    pub vendor: u32,
    /// PCI device id.
    pub device: u32,
    /// Driver manifests (64-bit).
    pub drivers: Vec<PathBuf>,
    /// Implicit layer manifests (64-bit).
    pub implicit_layers: Vec<PathBuf>,
}

/// Vulkan registrations of the display adapters (Windows; empty elsewhere or on error).
#[must_use]
pub fn adapter_entries() -> Vec<AdapterEntry> {
    #[cfg(windows)]
    {
        windows_impl::entries()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// PCI vendor and device ids of a `MatchingDeviceId` value (`pci\ven_10de&dev_2702…`, any
/// case). Pure, for testing.
#[must_use]
pub fn parse_matching_device_id(id: &str) -> Option<(u32, u32)> {
    let id = id.to_ascii_lowercase();
    let hex = |key: &str| {
        let start = id.find(key)? + key.len();
        let digits: String = id[start..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        (digits.len() == 4)
            .then(|| u32::from_str_radix(&digits, 16).ok())
            .flatten()
    };
    if !id.starts_with("pci\\") {
        return None;
    }
    Some((hex("ven_")?, hex("dev_")?))
}

/// Strings of a `REG_SZ` or `REG_MULTI_SZ` value (UTF-16, NUL separated). Pure, for testing.
#[must_use]
pub fn split_multi_sz(data: &[u16]) -> Vec<String> {
    data.split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(windows)]
mod windows_impl {
    use std::path::PathBuf;

    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, RRF_RT_REG_MULTI_SZ,
        RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR, w};

    use super::{AdapterEntry, parse_matching_device_id, split_multi_sz};

    const DISPLAY_CLASS: PCWSTR =
        w!("SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11ce-bfc1-08002be10318}");

    /// Closes the key on drop.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: a key opened by `RegOpenKeyExW`, closed once.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn strings(key: HKEY, value: PCWSTR) -> Vec<String> {
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_MULTI_SZ;
        let mut bytes = 0u32;
        // SAFETY: size query on an open key; no buffer.
        let r = unsafe {
            RegGetValueW(
                key,
                PCWSTR::null(),
                value,
                flags,
                None,
                None,
                Some(&mut bytes),
            )
        };
        if r != ERROR_SUCCESS || bytes == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: the buffer holds `len` bytes.
        let r = unsafe {
            RegGetValueW(
                key,
                PCWSTR::null(),
                value,
                flags,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        if r != ERROR_SUCCESS {
            return Vec::new();
        }
        buf.truncate((len as usize) / 2);
        split_multi_sz(&buf)
    }

    pub(super) fn entries() -> Vec<AdapterEntry> {
        let mut class = HKEY::default();
        // SAFETY: valid key path and out pointer.
        let r = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                DISPLAY_CLASS,
                None,
                KEY_ENUMERATE_SUB_KEYS | KEY_QUERY_VALUE,
                &mut class,
            )
        };
        if r != ERROR_SUCCESS {
            return Vec::new();
        }
        let class = Key(class);
        let mut out = Vec::new();
        for index in 0..256 {
            let mut name = [0u16; 64];
            let mut len = name.len() as u32;
            // SAFETY: the name buffer holds `len` characters.
            let r = unsafe {
                RegEnumKeyExW(
                    class.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if r != ERROR_SUCCESS {
                break;
            }
            let sub = HSTRING::from_wide(&name[..len as usize]);
            let mut key = HKEY::default();
            // SAFETY: valid parent key, NUL-terminated sub-key name, out pointer.
            let r = unsafe { RegOpenKeyExW(class.0, &sub, None, KEY_QUERY_VALUE, &mut key) };
            if r != ERROR_SUCCESS {
                continue;
            }
            let key = Key(key);
            let Some((vendor, device)) = strings(key.0, w!("MatchingDeviceId"))
                .first()
                .and_then(|id| parse_matching_device_id(id))
            else {
                continue;
            };
            let drivers: Vec<PathBuf> = strings(key.0, w!("VulkanDriverName"))
                .into_iter()
                .map(PathBuf::from)
                .collect();
            let implicit_layers: Vec<PathBuf> = strings(key.0, w!("VulkanImplicitLayers"))
                .into_iter()
                .map(PathBuf::from)
                .collect();
            if drivers.is_empty() && implicit_layers.is_empty() {
                continue;
            }
            out.push(AdapterEntry {
                vendor,
                device,
                drivers,
                implicit_layers,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_device_ids() {
        assert_eq!(
            parse_matching_device_id("pci\\ven_10de&dev_2702"),
            Some((0x10de, 0x2702))
        );
        assert_eq!(
            parse_matching_device_id("PCI\\VEN_1002&DEV_164E&SUBSYS_7D751462&REV_C9"),
            Some((0x1002, 0x164e))
        );
        assert_eq!(parse_matching_device_id("root\\basicrender"), None);
        assert_eq!(parse_matching_device_id("pci\\ven_10de"), None);
        assert_eq!(parse_matching_device_id("pci\\ven_10d&dev_2702"), None);
    }

    #[test]
    fn multi_strings() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        let mut data = w("C:\\a\\nv-vk64.json");
        data.push(0);
        data.extend(w("C:\\b\\x.json"));
        data.extend([0, 0]);
        assert_eq!(
            split_multi_sz(&data),
            vec!["C:\\a\\nv-vk64.json".to_owned(), "C:\\b\\x.json".to_owned()]
        );
        assert!(split_multi_sz(&[0, 0]).is_empty());
    }

    /// The real registry of the machine running the tests: whatever it holds, each entry has
    /// a vendor id and at least one manifest.
    #[test]
    fn entries_are_well_formed() {
        for e in adapter_entries() {
            assert_ne!(e.vendor, 0);
            assert!(!e.drivers.is_empty() || !e.implicit_layers.is_empty());
        }
    }
}

//! Decoding of the Windows power notifications (`GUID_ACDC_POWER_SOURCE`), kept apart from
//! the system calls so that it is tested on every system.
// Elsewhere than on Windows, this module is only compiled for its own tests.
#![cfg_attr(not(windows), allow(dead_code))]

/// `GUID_ACDC_POWER_SOURCE`, as laid out in memory (`Data1`, `Data2`, `Data3` little-endian,
/// then `Data4`): 5d3e9a59-e9d5-4b00-a6bd-ff34ff516548.
pub(crate) const ACDC_POWER_SOURCE_GUID: [u8; 16] = [
    0x59, 0x9a, 0x3e, 0x5d, 0xd5, 0xe9, 0x00, 0x4b, 0xa6, 0xbd, 0xff, 0x34, 0xff, 0x51, 0x65, 0x48,
];

/// Size of a `POWERBROADCAST_SETTING` header: the setting's GUID and the data length.
pub(crate) const SETTING_HEADER: usize = 20;

/// The value of a `GUID_ACDC_POWER_SOURCE` notification: `POWERBROADCAST_SETTING` bytes (GUID,
/// `DataLength`, data). `None` for another setting or a short payload.
pub(crate) fn acdc_value(setting: &[u8]) -> Option<u32> {
    let guid = setting.get(..16)?;
    if guid != ACDC_POWER_SOURCE_GUID {
        return None;
    }
    let len = u32::from_le_bytes(setting.get(16..SETTING_HEADER)?.try_into().ok()?) as usize;
    if len < 4 {
        return None;
    }
    let data = setting.get(SETTING_HEADER..SETTING_HEADER + 4)?;
    Some(u32::from_le_bytes(data.try_into().ok()?))
}

/// `SYSTEM_POWER_SOURCE`: 0 = AC (`PoAc`), 1 = battery (`PoDc`), 2 = short-term source such as
/// a UPS (`PoHot`). The last two count as "on battery".
pub(crate) fn acdc_on_battery(value: u32) -> bool {
    matches!(value, 1 | 2)
}

/// `SYSTEM_POWER_STATUS::ACLineStatus`: 0 offline (battery), 1 online, 255 unknown.
pub(crate) fn ac_line_on_battery(status: u8) -> Option<bool> {
    match status {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(guid: [u8; 16], data: &[u8]) -> Vec<u8> {
        let mut b = guid.to_vec();
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(data);
        b
    }

    #[test]
    fn acdc_notifications_are_decoded() {
        let ac = setting(ACDC_POWER_SOURCE_GUID, &0u32.to_le_bytes());
        let dc = setting(ACDC_POWER_SOURCE_GUID, &1u32.to_le_bytes());
        let ups = setting(ACDC_POWER_SOURCE_GUID, &2u32.to_le_bytes());
        assert_eq!(acdc_value(&ac).map(acdc_on_battery), Some(false));
        assert_eq!(acdc_value(&dc).map(acdc_on_battery), Some(true));
        assert_eq!(acdc_value(&ups).map(acdc_on_battery), Some(true));
        // Another setting, a short payload, a truncated buffer.
        let mut other = ACDC_POWER_SOURCE_GUID;
        other[0] ^= 1;
        assert_eq!(acdc_value(&setting(other, &1u32.to_le_bytes())), None);
        assert_eq!(acdc_value(&setting(ACDC_POWER_SOURCE_GUID, &[1])), None);
        assert_eq!(acdc_value(&dc[..21]), None);
        assert!(!acdc_on_battery(3));
    }

    #[test]
    fn guid_bytes_match_the_windows_layout() {
        // Data1 = 0x5d3e9a59, Data2 = 0xe9d5, Data3 = 0x4b00, Data4 = a6 bd ff 34 ff 51 65 48.
        let mut expected = Vec::new();
        expected.extend_from_slice(&0x5d3e_9a59u32.to_le_bytes());
        expected.extend_from_slice(&0xe9d5u16.to_le_bytes());
        expected.extend_from_slice(&0x4b00u16.to_le_bytes());
        expected.extend_from_slice(&[0xa6, 0xbd, 0xff, 0x34, 0xff, 0x51, 0x65, 0x48]);
        assert_eq!(expected, ACDC_POWER_SOURCE_GUID);
    }

    #[test]
    fn ac_line_status() {
        assert_eq!(ac_line_on_battery(0), Some(true));
        assert_eq!(ac_line_on_battery(1), Some(false));
        assert_eq!(ac_line_on_battery(255), None);
    }
}

//! Windows power source: `PowerSettingRegisterNotification` in callback mode
//! (`DEVICE_NOTIFY_CALLBACK`) for `GUID_ACDC_POWER_SOURCE`. The system calls
//! [`power_callback`] on one of its threads; nothing of ours runs or wakes in between.
//!
//! The callback's context is a registration number, not a pointer: a notification that races
//! with the unregistration finds no entry in [`REGISTRATIONS`] and is ignored, so nothing can
//! be used after it was freed.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, GetSystemPowerStatus, HPOWERNOTIFY,
    PowerSettingRegisterNotification, PowerSettingUnregisterNotification, SYSTEM_POWER_STATUS,
};
use windows::Win32::System::SystemServices::GUID_ACDC_POWER_SOURCE;
use windows::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_POWERSETTINGCHANGE};

use super::PowerCallback;
use super::acdc::{SETTING_HEADER, ac_line_on_battery, acdc_on_battery, acdc_value};

/// Live registrations by number.
static REGISTRATIONS: Mutex<Vec<(usize, PowerCallback)>> = Mutex::new(Vec::new());
static NEXT: AtomicUsize = AtomicUsize::new(1);

pub(super) struct Registration {
    id: usize,
    handle: HPOWERNOTIFY,
    /// The subscription the system was given (kept until the unregistration).
    _params: Box<DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS>,
}

// SAFETY: the handle is an opaque registration token, usable from any thread; the parameters
// are only read by the system.
unsafe impl Send for Registration {}

fn registrations() -> std::sync::MutexGuard<'static, Vec<(usize, PowerCallback)>> {
    REGISTRATIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn start(on_change: PowerCallback) -> Option<Registration> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    registrations().push((id, on_change.clone()));

    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid, writable SYSTEM_POWER_STATUS.
    match unsafe { GetSystemPowerStatus(&mut status) } {
        Ok(()) => match ac_line_on_battery(status.ACLineStatus) {
            Some(on_battery) => on_change(on_battery),
            None => tracing::debug!("power source: AC line status unknown"),
        },
        Err(e) => tracing::debug!("power source: GetSystemPowerStatus: {e}"),
    }

    let params = Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
        Callback: Some(power_callback),
        Context: id as *mut c_void,
    });
    let mut handle: *mut c_void = std::ptr::null_mut();
    // SAFETY: with DEVICE_NOTIFY_CALLBACK the recipient is a DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS,
    // which lives in `params` until the unregistration; `handle` is writable.
    let err = unsafe {
        PowerSettingRegisterNotification(
            &GUID_ACDC_POWER_SOURCE,
            DEVICE_NOTIFY_CALLBACK,
            HANDLE(&*params as *const DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as *mut c_void),
            &mut handle,
        )
    };
    if err != ERROR_SUCCESS {
        registrations().retain(|(i, _)| *i != id);
        tracing::warn!(
            "power source: PowerSettingRegisterNotification failed ({}); battery is not followed",
            err.0
        );
        return None;
    }
    tracing::debug!("power source: following GUID_ACDC_POWER_SOURCE");
    Some(Registration {
        id,
        handle: HPOWERNOTIFY(handle as isize),
        _params: params,
    })
}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: `handle` came from a successful PowerSettingRegisterNotification.
        let err = unsafe { PowerSettingUnregisterNotification(self.handle) };
        if err != ERROR_SUCCESS {
            tracing::debug!("power source: unregistration failed ({})", err.0);
        }
        registrations().retain(|(i, _)| *i != self.id);
    }
}

/// `DEVICE_NOTIFY_CALLBACK_ROUTINE`.
unsafe extern "system" fn power_callback(
    context: *const c_void,
    kind: u32,
    setting: *const c_void,
) -> u32 {
    if kind != PBT_POWERSETTINGCHANGE || setting.is_null() {
        return ERROR_SUCCESS.0;
    }
    let base = setting.cast::<u8>();
    // SAFETY: for PBT_POWERSETTINGCHANGE `setting` points to a POWERBROADCAST_SETTING: a
    // 20-byte header (GUID, DataLength) followed by DataLength bytes.
    let bytes = unsafe {
        let header = std::slice::from_raw_parts(base, SETTING_HEADER);
        let len = u32::from_le_bytes([header[16], header[17], header[18], header[19]]) as usize;
        std::slice::from_raw_parts(base, SETTING_HEADER + len.min(64))
    };
    let Some(value) = acdc_value(bytes) else {
        return ERROR_SUCCESS.0;
    };
    let id = context as usize;
    let callback = registrations()
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, cb)| cb.clone());
    if let Some(cb) = callback {
        cb(acdc_on_battery(value));
    }
    ERROR_SUCCESS.0
}

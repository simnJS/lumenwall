//! Errors of the backend. A lost device or an out-of-memory is a value the caller handles (drop
//! the device, build a new one), never a panic.

use std::fmt;

/// `HRESULT` values the backend tells apart (DXGI and COM error codes, Microsoft documentation
/// "DXGI_ERROR" and "COM Error Codes"). Kept as plain numbers so the classification is tested on
/// every platform.
pub mod hresult {
    pub const E_OUTOFMEMORY: i32 = 0x8007_000E_u32 as i32;
    pub const E_INVALIDARG: i32 = 0x8007_0057_u32 as i32;
    pub const DXGI_ERROR_DEVICE_REMOVED: i32 = 0x887A_0005_u32 as i32;
    pub const DXGI_ERROR_DEVICE_HUNG: i32 = 0x887A_0006_u32 as i32;
    pub const DXGI_ERROR_DEVICE_RESET: i32 = 0x887A_0007_u32 as i32;
    pub const DXGI_ERROR_DRIVER_INTERNAL_ERROR: i32 = 0x887A_0020_u32 as i32;
    pub const DXGI_ERROR_SDK_COMPONENT_MISSING: i32 = 0x887A_002D_u32 as i32;
    pub const DXGI_ERROR_UNSUPPORTED: i32 = 0x887A_0004_u32 as i32;
    pub const DXGI_ERROR_NOT_FOUND: i32 = 0x887A_0002_u32 as i32;
    /// Success code of `Present` when the window is not visible.
    pub const DXGI_STATUS_OCCLUDED: i32 = 0x087A_0001;
    /// `D3D11_ERROR_OUT_OF_MEMORY`-like code some drivers return from resource creation
    /// (`D3DERR_OUTOFVIDEOMEMORY`).
    pub const D3DERR_OUTOFVIDEOMEMORY: i32 = 0x8876_017C_u32 as i32;
}

/// Why a device stopped working (`GetDeviceRemovedReason`, or the code of the failed call).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LostReason {
    /// `DXGI_ERROR_DEVICE_REMOVED`: driver update, adapter removed, TDR of another process.
    Removed,
    /// `DXGI_ERROR_DEVICE_HUNG`: this process's commands made the GPU hang.
    Hung,
    /// `DXGI_ERROR_DEVICE_RESET`: badly formed commands.
    Reset,
    /// `DXGI_ERROR_DRIVER_INTERNAL_ERROR`.
    DriverInternal,
    /// Any other code `GetDeviceRemovedReason` returned.
    Other(i32),
}

impl LostReason {
    /// The reason a device-loss `HRESULT` stands for, `None` when `code` is not one.
    #[must_use]
    pub fn from_hresult(code: i32) -> Option<Self> {
        Some(match code {
            hresult::DXGI_ERROR_DEVICE_REMOVED => Self::Removed,
            hresult::DXGI_ERROR_DEVICE_HUNG => Self::Hung,
            hresult::DXGI_ERROR_DEVICE_RESET => Self::Reset,
            hresult::DXGI_ERROR_DRIVER_INTERNAL_ERROR => Self::DriverInternal,
            _ => return None,
        })
    }
}

impl fmt::Display for LostReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Removed => f.write_str("device removed"),
            Self::Hung => f.write_str("device hung"),
            Self::Reset => f.write_str("device reset"),
            Self::DriverInternal => f.write_str("driver internal error"),
            Self::Other(c) => write!(f, "HRESULT {c:#010x}"),
        }
    }
}

/// An error of the Direct3D 11 backend.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The device is gone: every object made from it is dead. Drop them all, the device too,
    /// and create a new one (the caller's recovery path; nothing here panics).
    #[error("GPU device lost in {call}: {reason}")]
    DeviceLost {
        call: &'static str,
        reason: LostReason,
    },
    /// The driver could not allocate memory. The device still works: release what can be
    /// released, `Device::trim`, then retry smaller or give up on this wallpaper.
    #[error("out of GPU memory in {call}")]
    OutOfMemory { call: &'static str },
    /// Any other failed call.
    #[error("{call} failed: HRESULT {code:#010x}")]
    Call { call: &'static str, code: i32 },
    /// A capability the backend needs is missing (feature level, D3D11.1, a format).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// naga → HLSL, the binding contract or `D3DCompile`.
    #[error("shader: {0}")]
    Shader(String),
    /// Texture or buffer data that does not match its description.
    #[error("invalid data: {0}")]
    InvalidData(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The error a failed call with this `HRESULT` stands for: device loss and out-of-memory
    /// are told apart from the rest.
    #[must_use]
    pub fn from_hresult(call: &'static str, code: i32) -> Self {
        if let Some(reason) = LostReason::from_hresult(code) {
            return Self::DeviceLost { call, reason };
        }
        match code {
            hresult::E_OUTOFMEMORY | hresult::D3DERR_OUTOFVIDEOMEMORY => Self::OutOfMemory { call },
            _ => Self::Call { call, code },
        }
    }

    /// Whether the device must be recreated.
    #[must_use]
    pub fn is_device_lost(&self) -> bool {
        matches!(self, Self::DeviceLost { .. })
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// `windows::core::Error` → [`Error`] with the name of the call.
#[cfg(windows)]
pub(crate) trait Context<T> {
    fn ctx(self, call: &'static str) -> Result<T>;
}

#[cfg(windows)]
impl<T> Context<T> for windows::core::Result<T> {
    fn ctx(self, call: &'static str) -> Result<T> {
        self.map_err(|e| Error::from_hresult(call, e.code().0))
    }
}

/// `Option` out-parameter of a call that succeeded but returned nothing.
#[cfg(windows)]
pub(crate) fn out<T>(value: Option<T>, call: &'static str) -> Result<T> {
    value.ok_or(Error::Call {
        call,
        code: hresult::E_INVALIDARG,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_loss_codes_are_classified() {
        for (code, reason) in [
            (hresult::DXGI_ERROR_DEVICE_REMOVED, LostReason::Removed),
            (hresult::DXGI_ERROR_DEVICE_HUNG, LostReason::Hung),
            (hresult::DXGI_ERROR_DEVICE_RESET, LostReason::Reset),
            (
                hresult::DXGI_ERROR_DRIVER_INTERNAL_ERROR,
                LostReason::DriverInternal,
            ),
        ] {
            let e = Error::from_hresult("Present", code);
            assert!(e.is_device_lost(), "{code:#x}");
            assert!(matches!(e, Error::DeviceLost { reason: r, .. } if r == reason));
        }
    }

    #[test]
    fn out_of_memory_is_not_a_device_loss() {
        for code in [hresult::E_OUTOFMEMORY, hresult::D3DERR_OUTOFVIDEOMEMORY] {
            let e = Error::from_hresult("CreateTexture2D", code);
            assert!(matches!(e, Error::OutOfMemory { call: "CreateTexture2D" }));
            assert!(!e.is_device_lost());
        }
        let e = Error::from_hresult("CreateBuffer", hresult::E_INVALIDARG);
        assert!(matches!(e, Error::Call { .. }));
        assert_eq!(
            e.to_string(),
            "CreateBuffer failed: HRESULT 0x80070057",
            "readable message"
        );
    }

    // `Present` returns it as a success: it must never be read as a failure.
    const _: () = assert!(hresult::DXGI_STATUS_OCCLUDED > 0);

    #[test]
    fn occluded_is_not_a_device_loss() {
        assert!(LostReason::from_hresult(hresult::DXGI_STATUS_OCCLUDED).is_none());
    }
}

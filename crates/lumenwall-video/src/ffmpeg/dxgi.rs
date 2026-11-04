//! DXGI adapters of this machine (Windows), in DXGI's enumeration order: FFmpeg's D3D11VA
//! device string is an index in that order (FFmpeg's `-init_hw_device` documentation), and
//! the decoder should run on the GPU wgpu renders on ([`crate::select::pick_dxgi_adapter`]).
//!
//! Raw COM through the vtables (`IDXGIFactory1::EnumAdapters1`, `IDXGIAdapter1::GetDesc1`,
//! `Release`; layouts from the Windows SDK's `dxgi.h`), so that no new crate is needed.

use std::ffi::c_void;

use crate::select::DxgiAdapter;

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// `IID_IDXGIFactory1`, {770aae78-f26f-4dba-a829-253c83d1b387}.
const IID_IDXGI_FACTORY1: Guid = Guid {
    data1: 0x770a_ae78,
    data2: 0xf26f,
    data3: 0x4dba,
    data4: [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87],
};

/// `DXGI_ADAPTER_DESC1`.
#[repr(C)]
#[allow(dead_code, reason = "complete C layout, written by DXGI")]
struct AdapterDesc1 {
    description: [u16; 128],
    vendor_id: u32,
    device_id: u32,
    sub_sys_id: u32,
    revision: u32,
    dedicated_video_memory: usize,
    dedicated_system_memory: usize,
    shared_system_memory: usize,
    /// `LUID` (`DWORD LowPart; LONG HighPart`).
    adapter_luid: [u32; 2],
    flags: u32,
}

// 256 + 4 × 4 + 3 × 8 + 8 + 4, padded to the 8-byte alignment of SIZE_T.
const _: () = assert!(size_of::<AdapterDesc1>() == 312);

/// `DXGI_ERROR_NOT_FOUND`: past the last adapter.
const DXGI_ERROR_NOT_FOUND: i32 = 0x887A_0002_u32 as i32;

// Vtable slots: IUnknown (0 QueryInterface, 1 AddRef, 2 Release), IDXGIObject (3–6),
// IDXGIFactory (7–11), IDXGIFactory1 (12 EnumAdapters1, 13 IsCurrent); IDXGIAdapter (7–9),
// IDXGIAdapter1 (10 GetDesc1).
const RELEASE: usize = 2;
const ENUM_ADAPTERS1: usize = 12;
const GET_DESC1: usize = 10;

type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
type EnumAdapters1Fn = unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
type GetDesc1Fn = unsafe extern "system" fn(*mut c_void, *mut AdapterDesc1) -> i32;

#[link(name = "dxgi")]
unsafe extern "system" {
    fn CreateDXGIFactory1(riid: *const Guid, factory: *mut *mut c_void) -> i32;
}

/// Method `slot` of the COM object `obj`.
///
/// # Safety
///
/// `obj` is a live COM interface pointer whose vtable has a method of type `F` at `slot`.
unsafe fn method<F: Copy>(obj: *mut c_void, slot: usize) -> F {
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    // SAFETY: a COM object starts with its vtable pointer; see the function contract.
    unsafe {
        let vtable = *obj.cast::<*const *const c_void>();
        std::mem::transmute_copy(&*vtable.add(slot))
    }
}

/// Releases the COM object `obj`.
///
/// # Safety
///
/// `obj` is a live COM interface pointer owned by the caller.
unsafe fn release(obj: *mut c_void) {
    // SAFETY: see the function contract.
    unsafe { method::<ReleaseFn>(obj, RELEASE)(obj) };
}

/// The adapters DXGI enumerates, in order (empty if DXGI is unavailable).
pub(crate) fn adapters() -> Vec<DxgiAdapter> {
    let mut out = Vec::new();
    let mut factory = std::ptr::null_mut();
    // SAFETY: valid IID and out-pointer.
    if unsafe { CreateDXGIFactory1(&IID_IDXGI_FACTORY1, &mut factory) } < 0 || factory.is_null() {
        return out;
    }
    for index in 0.. {
        let mut adapter = std::ptr::null_mut();
        // SAFETY: live factory; out-pointer.
        let hr = unsafe {
            method::<EnumAdapters1Fn>(factory, ENUM_ADAPTERS1)(factory, index, &mut adapter)
        };
        if hr == DXGI_ERROR_NOT_FOUND || hr < 0 || adapter.is_null() {
            break;
        }
        // SAFETY: plain C struct, filled by GetDesc1.
        let mut desc: AdapterDesc1 = unsafe { std::mem::zeroed() };
        // SAFETY: live adapter; out-pointer to a DXGI_ADAPTER_DESC1.
        let hr = unsafe { method::<GetDesc1Fn>(adapter, GET_DESC1)(adapter, &mut desc) };
        // SAFETY: owned reference returned by EnumAdapters1.
        unsafe { release(adapter) };
        if hr >= 0 {
            let len = desc
                .description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.description.len());
            out.push(DxgiAdapter {
                vendor: desc.vendor_id,
                device: desc.device_id,
                name: String::from_utf16_lossy(&desc.description[..len]),
            });
        }
    }
    // SAFETY: owned reference returned by CreateDXGIFactory1.
    unsafe { release(factory) };
    out
}

//! A window below the monitors for tests and measurements: shown (so DWM composes it and the
//! swap chain presents like a wallpaper's), never activated, absent from the task bar, nothing
//! visible on the desktop.

use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassExW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SetWindowPos, ShowWindow, TranslateMessage, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::error::{Context, Result};

const CLASS: PCWSTR = w!("lumenwall_d3d11_offscreen");
/// Far below any monitor arrangement in use.
const POSITION: (i32, i32) = (0, 20_000);

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: forwards the window's own message to the default procedure.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// The window. It belongs to the thread that created it (pump its messages from there).
pub struct OffscreenWindow {
    hwnd: HWND,
}

impl OffscreenWindow {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        static REGISTER: Once = Once::new();
        // SAFETY: plain Win32 calls with valid arguments; the class name is a static string and
        // the window procedure a static function.
        unsafe {
            let hinstance = GetModuleHandleW(None).ctx("GetModuleHandleW")?;
            REGISTER.call_once(|| {
                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: hinstance.into(),
                    lpszClassName: CLASS,
                    ..Default::default()
                };
                RegisterClassExW(&wc);
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("lumenwall-d3d11"),
                WS_POPUP,
                POSITION.0,
                POSITION.1,
                width as i32,
                height as i32,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
            .ctx("CreateWindowExW")?;
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Ok(Self { hwnd })
        }
    }

    #[must_use]
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Resizes the window (the swap chain follows with `SwapChain::resize`).
    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        // SAFETY: a window of this thread.
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                width as i32,
                height as i32,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .ctx("SetWindowPos")
    }

    /// Handles the messages queued for this thread.
    pub fn pump(&self) {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a live out-parameter.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

impl Drop for OffscreenWindow {
    fn drop(&mut self) {
        // SAFETY: the window belongs to this thread (the type is not Send).
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

//! Standard output availability.
//!
//! Windows release builds use the GUI subsystem so that no console window opens when the
//! wallpaper starts with the session. When launched from a terminal, the process attaches to
//! the terminal's console so that `--help`, `--list-properties` and the logs stay visible;
//! otherwise the logs only go to the log file.

/// Prepares stdout/stderr and tells whether they lead anywhere.
pub fn prepare() -> bool {
    #[cfg(all(windows, not(debug_assertions)))]
    {
        gui::attach_parent_console()
    }
    #[cfg(not(all(windows, not(debug_assertions))))]
    {
        true
    }
}

#[cfg(all(windows, not(debug_assertions)))]
mod gui {
    use std::ptr;

    use windows_sys::Win32::Foundation::{
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };

    fn valid(handle: HANDLE) -> bool {
        !handle.is_null() && handle != INVALID_HANDLE_VALUE
    }

    fn std_handle(id: STD_HANDLE) -> HANDLE {
        // SAFETY: no pointer arguments.
        unsafe { GetStdHandle(id) }
    }

    pub(super) fn attach_parent_console() -> bool {
        // Output redirected to a file or a pipe by the parent: already usable.
        if valid(std_handle(STD_OUTPUT_HANDLE)) {
            return true;
        }
        // SAFETY: no pointer arguments.
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
            return false;
        }
        // Rust's std looks the handles up on every write: point the missing ones at the console.
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        for id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if valid(std_handle(id)) {
                continue;
            }
            // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call; the
            // other pointers are null, which CreateFileW accepts.
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                )
            };
            if valid(handle) {
                // SAFETY: `handle` is a console handle owned by the process from now on.
                unsafe { SetStdHandle(id, handle) };
            }
        }
        true
    }
}

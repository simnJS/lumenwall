//! Small OS helpers: opening folders and URLs, finding and starting the daemon,
//! the app's own memory use, and (Windows) the registry.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Opens a folder in the file manager or a URL (including `steam://`) with the
/// system handler.
pub fn open(target: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut cmd = Command::new("explorer.exe");
    #[cfg(not(windows))]
    let mut cmd = Command::new("xdg-open");
    cmd.arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    reap(cmd.spawn()?);
    Ok(())
}

/// Waits for a child on a parked thread so it never lingers as a zombie.
fn reap(mut child: std::process::Child) {
    let _ = std::thread::Builder::new()
        .name("lumen-ui-reap".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

pub const DAEMON_EXE: &str = if cfg!(windows) { "lumen.exe" } else { "lumen" };

/// The daemon binary: next to this executable first (installed together), then `PATH`.
pub fn find_daemon() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(DAEMON_EXE);
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(DAEMON_EXE))
        .find(|p| p.is_file())
}

/// Starts the daemon detached from this process (it outlives the window).
pub fn spawn_daemon(exe: &Path) -> std::io::Result<()> {
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group: closing the terminal that started the UI does not kill it.
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    reap(cmd.spawn()?);
    Ok(())
}

/// Starts another instance of this executable with `args` (the tray opens the window).
pub fn spawn_self(args: &[&str]) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    reap(cmd.spawn()?);
    Ok(())
}

/// Resident memory of this process, in bytes.
#[cfg(target_os = "linux")]
pub fn own_rss() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

#[cfg(windows)]
pub fn own_rss() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: valid pseudo-handle and a correctly sized, writable struct.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    (ok != 0).then_some(c.WorkingSetSize as u64)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn own_rss() -> Option<u64> {
    None
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a `REG_SZ` value from HKCU (`user`) or HKLM.
#[cfg(windows)]
pub fn registry_string(user: bool, key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
    };
    let root = if user {
        HKEY_CURRENT_USER
    } else {
        HKEY_LOCAL_MACHINE
    };
    let (key, value) = (wide(key), wide(value));
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: null-terminated strings and a buffer whose byte size is passed in `len`.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if status != 0 {
        return None;
    }
    let chars = (len as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..chars]))
}

/// Sets (or deletes, with `None`) a `REG_SZ` value under HKCU.
#[cfg(windows)]
pub fn registry_set_run(key: &str, value: &str, data: Option<&str>) -> std::io::Result<()> {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
        RegOpenKeyExW, RegSetValueExW,
    };
    let (key, value) = (wide(key), wide(value));
    let mut hkey: HKEY = std::ptr::null_mut();
    // SAFETY: valid strings; `hkey` receives an open key that is closed below.
    let status =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    let status = match data {
        Some(data) => {
            let data = wide(data);
            // SAFETY: `data` is a null-terminated UTF-16 buffer of the given byte length.
            unsafe {
                RegSetValueExW(
                    hkey,
                    value.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr().cast(),
                    (data.len() * 2) as u32,
                )
            }
        }
        // SAFETY: open key and null-terminated value name.
        None => match unsafe { RegDeleteValueW(hkey, value.as_ptr()) } {
            2 => 0, // ERROR_FILE_NOT_FOUND: already absent
            s => s,
        },
    };
    // SAFETY: closing the key opened above.
    unsafe { RegCloseKey(hkey) };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

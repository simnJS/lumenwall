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

/// Resource id of the app icon embedded by `build.rs` (`lumen.ico`).
#[cfg(windows)]
const APP_ICON_ID: u16 = 1;

/// Windows: replaces the window's icons with the hinted sizes of the embedded
/// `lumen.ico` (title bar and Alt+Tab at the window's DPI). Elsewhere: nothing.
#[cfg(windows)]
pub fn set_window_icon(raw: u64) {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON, SM_CXSMICON,
        SendMessageW, WM_SETICON,
    };
    let hwnd = raw as usize as windows_sys::Win32::Foundation::HWND;
    // SAFETY: `hwnd` is this process's window (winit's id is its HWND); the icon
    // is loaded from our own module by integer resource id (MAKEINTRESOURCE).
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let dpi = GetDpiForWindow(hwnd).max(96);
        for (which, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
            let size = GetSystemMetricsForDpi(metric, dpi);
            let icon = LoadImageW(
                module,
                APP_ICON_ID as usize as *const u16,
                IMAGE_ICON,
                size,
                size,
                LR_DEFAULTCOLOR,
            );
            if !icon.is_null() {
                SendMessageW(hwnd, WM_SETICON, which as usize, icon as isize);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn set_window_icon(_raw: u64) {}

/// Whether the system asks for reduced motion, read off the UI thread.
pub async fn reduce_motion_async() -> bool {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("lumen-ui-settings".into())
        .spawn(move || {
            let _ = tx.send(reduce_motion());
        });
    if spawned.is_err() {
        return false;
    }
    rx.await.unwrap_or(false)
}

/// The system's "reduce animations" setting. `LUMEN_UI_REDUCE_MOTION=1` (or `0`)
/// overrides it.
pub fn reduce_motion() -> bool {
    if let Some(v) = std::env::var_os("LUMEN_UI_REDUCE_MOTION") {
        return v != "0" && !v.is_empty();
    }
    system_reduce_motion()
}

/// Windows: Settings › Accessibility › Visual effects › Animation effects.
#[cfg(windows)]
fn system_reduce_motion() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };
    let mut enabled: windows_sys::core::BOOL = 1;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the given pointer.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0)
    };
    ok != 0 && enabled == 0
}

/// Linux: GNOME's `org.gnome.desktop.interface enable-animations` and KDE's
/// animation speed (`AnimationDurationFactor` 0), through the settings portal,
/// then from `gsettings` / `kdeglobals` when there is no portal.
#[cfg(target_os = "linux")]
fn system_reduce_motion() -> bool {
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.contains("KDE"));
    match settings_portal::reduce_motion(kde) {
        Ok(Some(reduced)) => return reduced,
        Ok(None) => {}
        Err(e) => tracing::debug!("settings portal: {e}"),
    }
    if kde {
        return kde_animations_off();
    }
    // No portal: ask GSettings directly when it is installed.
    let out = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim() == "false",
        _ => kde_animations_off(),
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn system_reduce_motion() -> bool {
    false
}

/// Linux: the desktop asks for a light colour scheme (settings portal,
/// `org.freedesktop.appearance color-scheme` = 2).
#[cfg(target_os = "linux")]
pub fn prefers_light() -> bool {
    settings_portal::color_scheme().is_ok_and(|s| s == Some(2))
}

/// `[KDE] AnimationDurationFactor=0` in `kdeglobals` (System Settings › Animation speed: Instant).
#[cfg(target_os = "linux")]
fn kde_animations_off() -> bool {
    let Some(path) = dirs::config_dir().map(|d| d.join("kdeglobals")) else {
        return false;
    };
    std::fs::read_to_string(path).is_ok_and(|s| kde_factor(&s).is_some_and(|f| f <= 0.0))
}

/// `AnimationDurationFactor` from the `[KDE]` group of a `kdeglobals` file.
#[cfg(any(target_os = "linux", test))]
fn kde_factor(ini: &str) -> Option<f64> {
    let mut in_kde = false;
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') {
            in_kde = line == "[KDE]";
        } else if in_kde
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == "AnimationDurationFactor"
        {
            return v.trim().parse().ok();
        }
    }
    None
}

#[cfg(target_os = "linux")]
mod settings_portal {
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedValue, Value};

    /// `Ok(None)`: the portal answered but knows neither setting.
    pub fn reduce_motion(kde_first: bool) -> zbus::Result<Option<bool>> {
        let conn = Connection::session()?;
        let proxy = Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        )?;
        let gnome = || {
            read(&proxy, "org.gnome.desktop.interface", "enable-animations")
                .and_then(|v| as_bool(&v))
                .map(|enabled| !enabled)
        };
        let kde = || {
            read(&proxy, "org.kde.kdeglobals.KDE", "AnimationDurationFactor")
                .and_then(|v| as_f64(&v))
                .map(|f| f <= 0.0)
        };
        Ok(if kde_first {
            kde().or_else(gnome)
        } else {
            gnome().or_else(kde)
        })
    }

    /// 0: no preference, 1: dark, 2: light.
    pub fn color_scheme() -> zbus::Result<Option<u32>> {
        let conn = Connection::session()?;
        let proxy = Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        )?;
        Ok(read(&proxy, "org.freedesktop.appearance", "color-scheme")
            .and_then(|v| as_f64(&v))
            .map(|f| f as u32))
    }

    fn read(proxy: &Proxy<'_>, namespace: &str, key: &str) -> Option<OwnedValue> {
        // `ReadOne` (portal version 2), then the deprecated `Read`, whose value
        // comes wrapped in a second variant.
        proxy
            .call::<_, _, OwnedValue>("ReadOne", &(namespace, key))
            .or_else(|_| proxy.call::<_, _, OwnedValue>("Read", &(namespace, key)))
            .ok()
    }

    fn as_bool(v: &Value<'_>) -> Option<bool> {
        match v {
            Value::Bool(b) => Some(*b),
            Value::Value(inner) => as_bool(inner),
            Value::Str(s) => s.parse().ok(),
            _ => None,
        }
    }

    fn as_f64(v: &Value<'_>) -> Option<f64> {
        match v {
            Value::F64(f) => Some(*f),
            Value::I32(i) => Some(f64::from(*i)),
            Value::U32(u) => Some(f64::from(*u)),
            Value::Value(inner) => as_f64(inner),
            Value::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_kde_animation_factor() {
        let ini = "[General]\nAnimationDurationFactor=3\n[KDE]\nLookAndFeelPackage=x\nAnimationDurationFactor=0\n";
        assert_eq!(super::kde_factor(ini), Some(0.0));
        assert_eq!(
            super::kde_factor("[KDE]\nAnimationDurationFactor=0.5"),
            Some(0.5)
        );
        assert_eq!(
            super::kde_factor("[General]\nAnimationDurationFactor=0"),
            None
        );
    }
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

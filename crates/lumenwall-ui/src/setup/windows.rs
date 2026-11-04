//! Windows: the programs in `%LOCALAPPDATA%\Programs\Lumenwall`, a Start menu shortcut
//! and the `Run` registry value, all for the current user (no administrator rights).
//! A copy installed by the installer (`uninstall.exe` next to the programs) is removed
//! by its uninstaller. The registry and shortcuts are reached only through [`Ops`], so
//! the logic is tested on any system.

use std::path::{Path, PathBuf};

use super::{Item, ItemId, Outcome, Report, State, files};

const UI: &str = "lumenwall-ui.exe";
const ENGINE: &str = "lumenwall.exe";
/// Written by the installer (`packaging/windows/lumenwall.nsi`).
pub const UNINSTALLER: &str = "uninstall.exe";
const SHORTCUT: &str = "Lumenwall.lnk";
/// `HKCU\…\Run` value, shared with the installer and the Settings toggle.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_VALUE: &str = "Lumenwall";
/// The installer's entry in Settings › Apps (current user).
#[cfg(windows)]
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Lumenwall";

#[derive(Debug, Clone)]
pub struct Env {
    /// `%LOCALAPPDATA%\Programs\Lumenwall`.
    pub install_dir: PathBuf,
    /// `%APPDATA%\Microsoft\Windows\Start Menu\Programs`.
    pub start_menu: PathBuf,
    /// The running `lumenwall-ui.exe`.
    pub exe: PathBuf,
}

impl Env {
    #[cfg(windows)]
    pub fn system() -> Option<Self> {
        Some(Self {
            install_dir: dirs::data_local_dir()?.join("Programs").join("Lumenwall"),
            start_menu: dirs::data_dir()?
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
            exe: std::env::current_exe().ok()?,
        })
    }

    fn exe_dir(&self) -> &Path {
        self.exe.parent().unwrap_or(Path::new("."))
    }

    fn shortcut(&self) -> PathBuf {
        self.start_menu.join(SHORTCUT)
    }
}

/// The registry and the shell.
pub trait Ops: Send + Sync {
    fn run_value(&self) -> Option<String>;
    fn set_run_value(&self, command: Option<&str>) -> Result<(), String>;
    fn create_shortcut(&self, link: &Path, target: &Path, args: &str) -> Result<(), String>;
    /// `None` when the shortcut cannot be read.
    fn shortcut_target(&self, link: &Path) -> Option<PathBuf>;
}

/// What the `Run` value starts: the engine (if needed) and the tray icon.
pub fn run_command(ui: &Path) -> String {
    format!("\"{}\" --background", ui.display())
}

/// The program of a command line (`"C:\…\x.exe" --flag`, or unquoted).
pub fn command_program(cmd: &str) -> Option<PathBuf> {
    let cmd = cmd.trim();
    let prog = match cmd.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => cmd.split_whitespace().next()?,
    };
    (!prog.is_empty()).then(|| PathBuf::from(prog))
}

pub struct Setup {
    pub env: Env,
    pub ops: Box<dyn Ops>,
}

impl Setup {
    #[cfg(windows)]
    pub fn system() -> Option<Self> {
        Some(Self {
            env: Env::system()?,
            ops: Box::new(SystemOps),
        })
    }

    /// Started from an extracted zip somewhere else than the installation folder.
    fn copies_programs(&self) -> bool {
        self.env
            .exe_dir()
            .join(lumenwall_update::BUNDLE_MARKER)
            .is_file()
            && !files::same_dir(self.env.exe_dir(), &self.env.install_dir)
    }

    fn programs_state(&self) -> State {
        files::copies_state(self.env.exe_dir(), &self.env.install_dir, &[UI, ENGINE])
    }

    fn program_dir(&self, copied: bool) -> PathBuf {
        if self.copies_programs() && (copied || self.programs_state() == State::Ok) {
            self.env.install_dir.clone()
        } else {
            self.env.exe_dir().to_owned()
        }
    }

    /// The installer's uninstaller for this copy, or for the installed one.
    fn uninstaller(&self) -> Option<PathBuf> {
        [self.env.exe_dir(), self.env.install_dir.as_path()]
            .into_iter()
            .map(|d| d.join(UNINSTALLER))
            .find(|p| p.is_file())
    }

    pub fn detect(&self) -> Report {
        let mut items = Vec::new();
        if self.copies_programs() {
            items.push(Item::new(
                ItemId::Programs,
                self.programs_state(),
                self.env.install_dir.clone(),
            ));
        }
        let link = self.env.shortcut();
        let menu = if !link.is_file() {
            State::Missing
        } else {
            match self.ops.shortcut_target(&link) {
                Some(t) if !t.is_file() => State::Outdated,
                _ => State::Ok,
            }
        };
        items.push(Item::new(ItemId::Menu, menu, link));
        let auto = match self.ops.run_value() {
            None => State::Missing,
            Some(cmd) if command_program(&cmd).is_some_and(|p| p.is_file()) => State::Ok,
            Some(_) => State::Outdated,
        };
        items.push(Item::new(
            ItemId::Autostart,
            auto,
            PathBuf::from(format!(r"HKCU\{RUN_KEY}\{RUN_VALUE}")),
        ));
        Report {
            items,
            uninstaller: self.uninstaller(),
            managed: false,
            development: false,
            gnome: false,
        }
    }

    pub fn apply(&self, which: &[ItemId]) -> Outcome {
        let mut out = Outcome::default();
        let mut copied = false;
        if which.contains(&ItemId::Programs) && self.copies_programs() {
            let r = files::copy_tree(self.env.exe_dir(), &self.env.install_dir);
            copied = r.is_ok();
            if copied {
                out.copied_to = Some(self.env.install_dir.clone());
            }
            out.results.push((ItemId::Programs, r));
        }
        let ui = self.program_dir(copied).join(UI);
        if which.contains(&ItemId::Menu) {
            let link = self.env.shortcut();
            let r = std::fs::create_dir_all(&self.env.start_menu)
                .map_err(|e| files::err(&self.env.start_menu, e))
                .and_then(|()| self.ops.create_shortcut(&link, &ui, ""));
            out.results.push((ItemId::Menu, r));
        }
        if which.contains(&ItemId::Autostart) {
            let r = self.ops.set_run_value(Some(&run_command(&ui)));
            out.results.push((ItemId::Autostart, r));
        }
        out
    }

    /// Removes the shortcut, the `Run` value and the folder the setup copied the
    /// programs to (not while Lumenwall runs from it: then it is reported in
    /// [`Outcome::leftover`]). Installer copies go through [`Report::uninstaller`].
    pub fn uninstall(&self) -> Outcome {
        let mut out = Outcome::default();
        out.results
            .push((ItemId::Menu, files::remove(&self.env.shortcut())));
        let auto = if self.ops.run_value().is_some() {
            self.ops.set_run_value(None)
        } else {
            Ok(())
        };
        out.results.push((ItemId::Autostart, auto));
        let dir = &self.env.install_dir;
        if dir.join(lumenwall_update::BUNDLE_MARKER).is_file() && !dir.join(UNINSTALLER).exists() {
            if files::same_dir(dir, self.env.exe_dir()) {
                out.leftover = Some(dir.clone());
            } else {
                let r = std::fs::remove_dir_all(dir).map_err(|e| files::err(dir, e));
                out.results.push((ItemId::Programs, r));
            }
        }
        out
    }
}

/// The installer's "Apps" entry shows the version installed by the installer; after
/// an in-app update, say the new one. Cheap: two registry reads.
#[cfg(windows)]
pub fn refresh_uninstall_entry() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_owned))
    else {
        return;
    };
    if !dir.join(UNINSTALLER).is_file() {
        return;
    }
    let read = |v: &str| crate::platform::registry_string(true, UNINSTALL_KEY, v);
    let Some(location) = read("InstallLocation") else {
        return;
    };
    let version = env!("CARGO_PKG_VERSION");
    if files::same_dir(Path::new(location.trim_matches('"')), &dir)
        && read("DisplayVersion").as_deref() != Some(version)
        && let Err(e) =
            crate::platform::registry_set_run(UNINSTALL_KEY, "DisplayVersion", Some(version))
    {
        tracing::debug!("uninstall entry: {e}");
    }
}

#[cfg(windows)]
struct SystemOps;

#[cfg(windows)]
impl Ops for SystemOps {
    fn run_value(&self) -> Option<String> {
        crate::platform::registry_string(true, RUN_KEY, RUN_VALUE)
    }

    fn set_run_value(&self, command: Option<&str>) -> Result<(), String> {
        crate::platform::registry_set_run(RUN_KEY, RUN_VALUE, command)
            .map_err(|e| format!(r"HKCU\{RUN_KEY}: {e}"))
    }

    fn create_shortcut(&self, link: &Path, target: &Path, args: &str) -> Result<(), String> {
        com::create_shortcut(link, target, args)
    }

    fn shortcut_target(&self, link: &Path) -> Option<PathBuf> {
        com::shortcut_target(link)
    }
}

/// Shell links through COM (`IShellLinkW`, `IPersistFile`). `windows-sys` has the
/// functions and the class id but no interface definitions: the two vtables are
/// declared here in their documented order (`shobjidl_core.h`, `objidl.h`).
#[cfg(windows)]
mod com {
    use std::ffi::{OsStr, OsString, c_void};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::ptr::null_mut;

    use windows_sys::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::ShellLink;
    use windows_sys::core::{GUID, HRESULT};

    const IID_ISHELLLINKW: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
    const IID_IPERSISTFILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);
    const STGM_READ: u32 = 0;

    type Raw = *mut c_void;
    type Unused = usize;

    #[repr(C)]
    struct Unknown {
        query_interface: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> HRESULT,
        add_ref: unsafe extern "system" fn(Raw) -> u32,
        release: unsafe extern "system" fn(Raw) -> u32,
    }

    #[repr(C)]
    struct ShellLinkW {
        base: Unknown,
        get_path: unsafe extern "system" fn(Raw, *mut u16, i32, *mut c_void, u32) -> HRESULT,
        get_id_list: Unused,
        set_id_list: Unused,
        get_description: Unused,
        set_description: unsafe extern "system" fn(Raw, *const u16) -> HRESULT,
        get_working_directory: Unused,
        set_working_directory: unsafe extern "system" fn(Raw, *const u16) -> HRESULT,
        get_arguments: Unused,
        set_arguments: unsafe extern "system" fn(Raw, *const u16) -> HRESULT,
        get_hotkey: Unused,
        set_hotkey: Unused,
        get_show_cmd: Unused,
        set_show_cmd: Unused,
        get_icon_location: Unused,
        set_icon_location: unsafe extern "system" fn(Raw, *const u16, i32) -> HRESULT,
        set_relative_path: Unused,
        resolve: Unused,
        set_path: unsafe extern "system" fn(Raw, *const u16) -> HRESULT,
    }

    #[repr(C)]
    struct PersistFile {
        base: Unknown,
        get_class_id: Unused,
        is_dirty: Unused,
        load: unsafe extern "system" fn(Raw, *const u16, u32) -> HRESULT,
        save: unsafe extern "system" fn(Raw, *const u16, i32) -> HRESULT,
        save_completed: Unused,
        get_cur_file: Unused,
    }

    /// An interface pointer (to an object whose first field is its vtable),
    /// released when dropped.
    struct Ptr<V>(*mut *const V);

    impl<V> Ptr<V> {
        fn raw(&self) -> Raw {
            self.0.cast()
        }

        fn vtbl(&self) -> &V {
            // SAFETY: a live interface pointer returned by COM points to its vtable.
            unsafe { &**self.0 }
        }
    }

    impl<V> Drop for Ptr<V> {
        fn drop(&mut self) {
            // SAFETY: every vtable starts with IUnknown; one reference is held.
            unsafe {
                let unknown = &**(self.0 as *mut *const Unknown);
                (unknown.release)(self.raw());
            }
        }
    }

    /// COM for the current thread, uninitialised on drop if this call initialised it.
    struct Apartment(bool);

    impl Apartment {
        fn enter() -> Self {
            // SAFETY: no reserved pointer; balanced by CoUninitialize on success.
            let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as _) };
            Self(hr >= 0)
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: paired with the successful CoInitializeEx above.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
    }

    fn check(hr: HRESULT, what: &str) -> Result<(), String> {
        if hr >= 0 {
            Ok(())
        } else {
            Err(format!("{what} failed (0x{:08x})", hr as u32))
        }
    }

    fn shell_link() -> Result<(Ptr<ShellLinkW>, Ptr<PersistFile>), String> {
        let mut link: Raw = null_mut();
        // SAFETY: valid class and interface ids; `link` receives one reference.
        let hr = unsafe {
            CoCreateInstance(
                &ShellLink,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ISHELLLINKW,
                &mut link,
            )
        };
        check(hr, "creating a shell link")?;
        let link = Ptr::<ShellLinkW>(link.cast());
        let mut file: Raw = null_mut();
        // SAFETY: QueryInterface on a live object, `file` receives one reference.
        let hr =
            unsafe { (link.vtbl().base.query_interface)(link.raw(), &IID_IPERSISTFILE, &mut file) };
        check(hr, "IPersistFile")?;
        Ok((link, Ptr(file.cast())))
    }

    pub fn create_shortcut(at: &Path, target: &Path, args: &str) -> Result<(), String> {
        let _com = Apartment::enter();
        let (link, file) = shell_link()?;
        let dir = target.parent().unwrap_or(Path::new("."));
        let (target_w, dir_w, args_w, at_w) = (wide(target), wide(dir), wide(args), wide(at));
        let desc = wide("Animated wallpapers");
        let v = link.vtbl();
        // SAFETY: live interfaces; every string is NUL-terminated UTF-16 that outlives the call.
        unsafe {
            check((v.set_path)(link.raw(), target_w.as_ptr()), "SetPath")?;
            check(
                (v.set_working_directory)(link.raw(), dir_w.as_ptr()),
                "SetWorkingDirectory",
            )?;
            check(
                (v.set_arguments)(link.raw(), args_w.as_ptr()),
                "SetArguments",
            )?;
            check(
                (v.set_description)(link.raw(), desc.as_ptr()),
                "SetDescription",
            )?;
            check(
                (v.set_icon_location)(link.raw(), target_w.as_ptr(), 0),
                "SetIconLocation",
            )?;
            check(
                (file.vtbl().save)(file.raw(), at_w.as_ptr(), 1),
                "saving the shortcut",
            )?;
        }
        Ok(())
    }

    pub fn shortcut_target(at: &Path) -> Option<PathBuf> {
        let _com = Apartment::enter();
        let (link, file) = shell_link().ok()?;
        let at_w = wide(at);
        let mut buf = vec![0u16; 1024];
        // SAFETY: live interfaces; `buf` holds `len` UTF-16 units; no find data wanted.
        unsafe {
            if (file.vtbl().load)(file.raw(), at_w.as_ptr(), STGM_READ) < 0 {
                return None;
            }
            // S_FALSE (1): the link has no path.
            if (link.vtbl().get_path)(
                link.raw(),
                buf.as_mut_ptr(),
                buf.len() as i32,
                null_mut(),
                0,
            ) != 0
            {
                return None;
            }
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(PathBuf::from(OsString::from_wide(&buf[..n])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::TempDir;
    use std::sync::Mutex;

    /// The registry value and shortcuts, in memory (a shortcut is a file holding its
    /// target).
    #[derive(Default)]
    struct FakeOps {
        run: Mutex<Option<String>>,
    }

    impl Ops for FakeOps {
        fn run_value(&self) -> Option<String> {
            self.run.lock().unwrap().clone()
        }
        fn set_run_value(&self, command: Option<&str>) -> Result<(), String> {
            *self.run.lock().unwrap() = command.map(str::to_owned);
            Ok(())
        }
        fn create_shortcut(&self, link: &Path, target: &Path, _args: &str) -> Result<(), String> {
            std::fs::write(link, target.to_string_lossy().as_bytes()).map_err(|e| e.to_string())
        }
        fn shortcut_target(&self, link: &Path) -> Option<PathBuf> {
            std::fs::read_to_string(link).ok().map(PathBuf::from)
        }
    }

    fn setup(tmp: &TempDir) -> Setup {
        let root = tmp.path();
        let zip = root.join("Downloads/lumenwall-0.2.0-x86_64-pc-windows-msvc");
        std::fs::create_dir_all(&zip).unwrap();
        for (name, body) in [(UI, "ui"), (ENGINE, "engine"), ("LICENSE-MIT", "mit")] {
            std::fs::write(zip.join(name), body).unwrap();
        }
        std::fs::write(zip.join(lumenwall_update::BUNDLE_MARKER), "").unwrap();
        Setup {
            env: Env {
                install_dir: root.join("AppData/Local/Programs/Lumenwall"),
                start_menu: root.join("AppData/Roaming/Microsoft/Windows/Start Menu/Programs"),
                exe: zip.join(UI),
            },
            ops: Box::new(FakeOps::default()),
        }
    }

    fn state(s: &Setup, id: ItemId) -> Option<State> {
        s.detect()
            .items
            .into_iter()
            .find(|i| i.id == id)
            .map(|i| i.state)
    }

    #[test]
    fn from_the_zip_to_the_installation_folder() {
        let tmp = TempDir::new("windows");
        let s = setup(&tmp);
        let r = s.detect();
        assert!(r.items.iter().all(|i| i.state == State::Missing), "{r:?}");
        assert_eq!(r.uninstaller, None);
        let out = s.apply(&[ItemId::Programs, ItemId::Menu, ItemId::Autostart]);
        assert!(out.results.iter().all(|(_, r)| r.is_ok()), "{out:?}");
        let dir = &s.env.install_dir;
        assert_eq!(out.copied_to.as_ref(), Some(dir));
        for f in [UI, ENGINE, "LICENSE-MIT", lumenwall_update::BUNDLE_MARKER] {
            assert!(dir.join(f).is_file(), "{f}");
        }
        assert_eq!(s.ops.shortcut_target(&s.env.shortcut()), Some(dir.join(UI)));
        let run = s.ops.run_value().unwrap();
        assert_eq!(run, format!("\"{}\" --background", dir.join(UI).display()));
        assert_eq!(command_program(&run), Some(dir.join(UI)));
        let r = s.detect();
        assert!(r.items.iter().all(|i| i.state == State::Ok), "{r:?}");

        // Uninstall from the zip copy: the installed folder goes too.
        let out = s.uninstall();
        assert!(out.results.iter().all(|(_, r)| r.is_ok()), "{out:?}");
        assert_eq!(out.leftover, None);
        assert!(!dir.exists() && !s.env.shortcut().exists());
        assert_eq!(s.ops.run_value(), None);
        assert!(s.env.exe.is_file(), "the zip folder stays");
    }

    #[test]
    fn running_from_the_installed_copy() {
        let tmp = TempDir::new("windows");
        let mut s = setup(&tmp);
        s.apply(&[ItemId::Programs, ItemId::Menu]);
        s.env.exe = s.env.install_dir.join(UI);
        assert_eq!(state(&s, ItemId::Programs), None, "nothing to copy");
        assert_eq!(state(&s, ItemId::Menu), Some(State::Ok));
        // Shortcut to a removed program.
        std::fs::write(s.env.shortcut(), "C:/nowhere/lumenwall-ui.exe").unwrap();
        assert_eq!(state(&s, ItemId::Menu), Some(State::Outdated));
        s.apply(&[ItemId::Menu]);
        assert_eq!(state(&s, ItemId::Menu), Some(State::Ok));
        s.ops
            .set_run_value(Some(r#""C:\gone\lumenwall-ui.exe" --background"#))
            .unwrap();
        assert_eq!(state(&s, ItemId::Autostart), Some(State::Outdated));
        // The running folder cannot be deleted: reported instead.
        let out = s.uninstall();
        assert_eq!(out.leftover.as_ref(), Some(&s.env.install_dir));
        assert!(s.env.install_dir.join(UI).is_file());
        assert_eq!(s.ops.run_value(), None);
    }

    #[test]
    fn installer_copies_use_their_uninstaller() {
        let tmp = TempDir::new("windows");
        let mut s = setup(&tmp);
        s.apply(&[ItemId::Programs]);
        std::fs::write(s.env.install_dir.join(UNINSTALLER), "nsis").unwrap();
        assert_eq!(
            s.detect().uninstaller,
            Some(s.env.install_dir.join(UNINSTALLER)),
            "also from another copy"
        );
        s.env.exe = s.env.install_dir.join(UI);
        assert_eq!(
            s.detect().uninstaller,
            Some(s.env.install_dir.join(UNINSTALLER))
        );
        // The in-app removal never deletes an installer's folder.
        s.uninstall();
        assert!(s.env.install_dir.join(UI).is_file());
        // A newer zip run next to an older installed copy updates it.
        std::fs::write(
            tmp.path()
                .join("Downloads/lumenwall-0.2.0-x86_64-pc-windows-msvc")
                .join(UI),
            "ui 2",
        )
        .unwrap();
        s.env.exe = tmp
            .path()
            .join("Downloads/lumenwall-0.2.0-x86_64-pc-windows-msvc")
            .join(UI);
        assert_eq!(state(&s, ItemId::Programs), Some(State::Outdated));
        s.apply(&[ItemId::Programs]);
        assert_eq!(
            std::fs::read_to_string(s.env.install_dir.join(UI)).unwrap(),
            "ui 2"
        );
        assert!(
            s.env.install_dir.join(UNINSTALLER).is_file(),
            "installer files are kept"
        );
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            command_program(r#""C:\Program Files\x\lumenwall-ui.exe" --background"#),
            Some(PathBuf::from(r"C:\Program Files\x\lumenwall-ui.exe"))
        );
        assert_eq!(
            command_program(r"C:\x\lumenwall-ui.exe --background"),
            Some(PathBuf::from(r"C:\x\lumenwall-ui.exe"))
        );
        assert_eq!(command_program("  "), None);
    }
}

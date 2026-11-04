//! Linux: the programs in `~/.local/bin`, the menu entry and icons under
//! `~/.local/share`, the session autostart entry in `~/.config/autostart`, and on GNOME
//! the Shell extension. Every path comes from [`Env`], so tests run on a temporary home;
//! GNOME Shell is reached only through [`Shell`].

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{Item, ItemId, Outcome, Report, State, assets, desktop, files};

const UI: &str = "lumenwall-ui";
const ENGINE: &str = "lumenwall";
const APP_ENTRY: &str = "lumenwall-ui.desktop";
const AUTOSTART_ENTRY: &str = "lumenwall.desktop";

/// Where things go, for this user.
#[derive(Debug, Clone)]
pub struct Env {
    /// `$XDG_DATA_HOME` (`~/.local/share`).
    pub data_home: PathBuf,
    /// `$XDG_CONFIG_HOME` (`~/.config`).
    pub config_home: PathBuf,
    /// `~/.local/bin`.
    pub bin_home: PathBuf,
    /// `$XDG_DATA_DIRS`: where a system package puts its entry, icons and extension.
    pub data_dirs: Vec<PathBuf>,
    /// `$PATH`, to resolve an entry's `Exec=lumenwall-ui`.
    pub path: Vec<PathBuf>,
    /// The running `lumenwall-ui`.
    pub exe: PathBuf,
    /// A GNOME session (the Shell extension applies).
    pub gnome: bool,
}

impl Env {
    pub fn system() -> Option<Self> {
        let home = dirs::home_dir()?;
        let split = |var: &str| -> Option<Vec<PathBuf>> {
            let v = std::env::var_os(var).filter(|v| !v.is_empty())?;
            Some(std::env::split_paths(&v).collect())
        };
        Some(Self {
            data_home: dirs::data_dir()?,
            config_home: dirs::config_dir()?,
            bin_home: dirs::executable_dir().unwrap_or_else(|| home.join(".local/bin")),
            data_dirs: split("XDG_DATA_DIRS")
                .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]),
            path: split("PATH").unwrap_or_default(),
            exe: std::env::current_exe().ok()?,
            gnome: is_gnome(
                &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
                &std::env::var("DESKTOP_SESSION").unwrap_or_default(),
            ),
        })
    }

    fn exe_dir(&self) -> &Path {
        self.exe.parent().unwrap_or(Path::new("/"))
    }

    fn app_entry(&self) -> PathBuf {
        self.data_home.join("applications").join(APP_ENTRY)
    }

    fn icons(&self) -> PathBuf {
        self.data_home.join("icons/hicolor")
    }

    fn autostart(&self) -> PathBuf {
        self.config_home.join("autostart").join(AUTOSTART_ENTRY)
    }

    fn extensions(&self) -> PathBuf {
        self.data_home.join("gnome-shell/extensions")
    }
}

/// `XDG_CURRENT_DESKTOP` is a `:`-separated list (`ubuntu:GNOME`).
pub fn is_gnome(current_desktop: &str, session: &str) -> bool {
    current_desktop
        .split(':')
        .any(|d| d.eq_ignore_ascii_case("GNOME"))
        || (current_desktop.is_empty() && session.to_ascii_lowercase().contains("gnome"))
}

/// Whether enabling took effect now or does at the next login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enabled {
    Now,
    NextLogin,
}

/// GNOME Shell's list of enabled extensions.
pub trait Shell: Send + Sync {
    /// `None` when it cannot be read (no GSettings).
    fn enabled(&self, uuid: &str) -> Option<bool>;
    fn enable(&self, uuid: &str) -> Result<Enabled, String>;
    fn disable(&self, uuid: &str) -> Result<(), String>;
}

/// The real Shell: `gnome-extensions` (immediate, for an extension the running Shell
/// knows), else the `org.gnome.shell enabled-extensions` key (read at the next login:
/// a Wayland session only discovers a new extension then).
pub struct SystemShell;

const SHELL_SCHEMA: &str = "org.gnome.shell";

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{program} {}: {}", args.join(" "), err.trim()))
    }
}

fn read_list(key: &str) -> Result<Vec<String>, String> {
    let text = run("gsettings", &["get", SHELL_SCHEMA, key])?;
    desktop::parse_strv(&text).ok_or_else(|| format!("gsettings {key}: unexpected value {text:?}"))
}

fn edit_list(key: &str, edit: impl FnOnce(&mut Vec<String>)) -> Result<(), String> {
    let mut list = read_list(key)?;
    let before = list.clone();
    edit(&mut list);
    if list != before {
        run(
            "gsettings",
            &["set", SHELL_SCHEMA, key, &desktop::format_strv(&list)],
        )?;
    }
    Ok(())
}

impl Shell for SystemShell {
    fn enabled(&self, uuid: &str) -> Option<bool> {
        let on = read_list("enabled-extensions").ok()?;
        Some(on.iter().any(|u| u == uuid))
    }

    fn enable(&self, uuid: &str) -> Result<Enabled, String> {
        if run("gnome-extensions", &["enable", uuid]).is_ok() {
            return Ok(Enabled::Now);
        }
        edit_list("enabled-extensions", |l| {
            if !l.iter().any(|u| u == uuid) {
                l.push(uuid.to_owned());
            }
        })?;
        // Older Shells also keep a list of explicitly disabled extensions.
        let _ = edit_list("disabled-extensions", |l| l.retain(|u| u != uuid));
        Ok(Enabled::NextLogin)
    }

    fn disable(&self, uuid: &str) -> Result<(), String> {
        if run("gnome-extensions", &["disable", uuid]).is_ok() {
            return Ok(());
        }
        edit_list("enabled-extensions", |l| l.retain(|u| u != uuid))
    }
}

/// `version-name` (or `version`) of an extension's `metadata.json`.
pub fn metadata_version(bytes: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    match (&v["version-name"], &v["version"]) {
        (serde_json::Value::String(s), _) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        (_, serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// `a` older than `b`, comparing dotted numbers ("2" < "2.1" < "10"). An unreadable
/// version is older than any readable one.
pub fn older(a: Option<&str>, b: &str) -> bool {
    let parse =
        |s: &str| -> Option<Vec<u64>> { s.split('.').map(|p| p.trim().parse().ok()).collect() };
    match (a.and_then(parse), parse(b)) {
        (Some(a), Some(b)) => a < b,
        (None, Some(_)) => true,
        _ => false,
    }
}

/// Setup on Linux, for one [`Env`].
pub struct Setup {
    pub env: Env,
    pub shell: Box<dyn Shell>,
}

impl Setup {
    pub fn system() -> Option<Self> {
        Some(Self {
            env: Env::system()?,
            shell: Box::new(SystemShell),
        })
    }

    /// Started from an extracted release archive somewhere other than `~/.local/bin`:
    /// the programs can be copied there.
    fn copies_programs(&self) -> bool {
        self.env
            .exe_dir()
            .join(lumenwall_update::BUNDLE_MARKER)
            .is_file()
            && !files::same_dir(self.env.exe_dir(), &self.env.bin_home)
    }

    fn programs_state(&self) -> State {
        files::copies_state(self.env.exe_dir(), &self.env.bin_home, &[UI, ENGINE])
    }

    /// Where the entries point: `~/.local/bin` once the programs are there.
    fn program_dir(&self, copied: bool) -> PathBuf {
        if self.copies_programs() && (copied || self.programs_state() == State::Ok) {
            self.env.bin_home.clone()
        } else {
            self.env.exe_dir().to_owned()
        }
    }

    /// `Exec=` target exists (a bare name is looked up in `PATH`).
    fn resolves(&self, program: &str) -> bool {
        if program.contains('/') {
            Path::new(program).is_file()
        } else {
            self.env.path.iter().any(|d| d.join(program).is_file())
        }
    }

    fn system_file(&self, rel: &str) -> Option<PathBuf> {
        self.env
            .data_dirs
            .iter()
            .map(|d| d.join(rel))
            .find(|p| p.exists())
    }

    fn menu_item(&self) -> Item {
        let path = self.env.app_entry();
        let state = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let program = desktop::exec_program(&text).is_some_and(|p| self.resolves(&p));
                let icons = assets::ICONS
                    .iter()
                    .all(|(rel, _)| self.env.icons().join(rel).is_file())
                    || self
                        .system_file("icons/hicolor/scalable/apps/lumenwall.svg")
                        .is_some();
                if program && icons {
                    State::Ok
                } else {
                    State::Outdated
                }
            }
            Err(_) => match self.system_file(&format!("applications/{APP_ENTRY}")) {
                Some(p) => {
                    return Item::new(ItemId::Menu, State::System, p);
                }
                None => State::Missing,
            },
        };
        Item::new(ItemId::Menu, state, path)
    }

    fn autostart_item(&self) -> Item {
        let path = self.env.autostart();
        let state = match std::fs::read_to_string(&path) {
            Ok(text) if desktop::hidden(&text) => State::Missing,
            Ok(text) => {
                if desktop::exec_program(&text).is_some_and(|p| self.resolves(&p)) {
                    State::Ok
                } else {
                    State::Outdated
                }
            }
            Err(_) => State::Missing,
        };
        Item::new(ItemId::Autostart, state, path)
    }

    /// The installed extension folder (the user's copy wins, as in GNOME Shell) and
    /// its version.
    fn installed_extension(&self) -> Option<(PathBuf, Option<String>)> {
        let user = self.env.extensions().join(assets::EXTENSION_UUID);
        let rel = format!("gnome-shell/extensions/{}", assets::EXTENSION_UUID);
        let dir = if user.join("metadata.json").is_file() {
            user
        } else {
            self.env
                .data_dirs
                .iter()
                .map(|d| d.join(&rel))
                .find(|d| d.join("metadata.json").is_file())?
        };
        let version = std::fs::read(dir.join("metadata.json"))
            .ok()
            .and_then(|b| metadata_version(&b));
        Some((dir, version))
    }

    fn extension_item(&self) -> Option<Item> {
        if !self.env.gnome {
            return None;
        }
        let embedded = assets::extension_version().unwrap_or_default();
        let user = self.env.extensions().join(assets::EXTENSION_UUID);
        let item = match self.installed_extension() {
            None => Item::new(ItemId::Extension, State::Missing, user),
            // The update goes to the user's folder (it wins over a system copy).
            Some((_, v)) if older(v.as_deref(), &embedded) => {
                let mut i = Item::new(ItemId::Extension, State::Outdated, user);
                i.versions = Some((v.unwrap_or_else(|| "?".into()), embedded));
                i
            }
            Some((dir, _)) => match self.shell.enabled(assets::EXTENSION_UUID) {
                Some(false) => Item::new(ItemId::Extension, State::Disabled, dir),
                _ => Item::new(ItemId::Extension, State::Ok, dir),
            },
        };
        Some(item)
    }

    pub fn detect(&self) -> Report {
        let mut items = Vec::new();
        if self.copies_programs() {
            items.push(Item::new(
                ItemId::Programs,
                self.programs_state(),
                self.env.bin_home.clone(),
            ));
        }
        items.push(self.menu_item());
        items.push(self.autostart_item());
        items.extend(self.extension_item());
        Report {
            items,
            uninstaller: None,
            managed: false,
            development: false,
            gnome: self.env.gnome,
        }
    }

    /// Sets up `which` (items already done are written again: this is also "Repair").
    pub fn apply(&self, which: &[ItemId]) -> Outcome {
        let mut out = Outcome::default();
        let mut copied = false;
        if which.contains(&ItemId::Programs) && self.copies_programs() {
            let r = self.copy_programs();
            copied = r.is_ok();
            if copied {
                out.copied_to = Some(self.env.bin_home.clone());
            }
            out.results.push((ItemId::Programs, r));
        }
        let ui = self.program_dir(copied).join(UI);
        if which.contains(&ItemId::Menu) {
            out.results.push((ItemId::Menu, self.write_menu(&ui)));
        }
        if which.contains(&ItemId::Autostart) {
            out.results.push((
                ItemId::Autostart,
                write_autostart(&self.env.autostart(), &ui),
            ));
        }
        if which.contains(&ItemId::Extension) && self.env.gnome {
            let r = self
                .install_extension()
                .map(|relogin| out.relogin |= relogin);
            out.results.push((ItemId::Extension, r));
        }
        out
    }

    fn copy_programs(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.env.bin_home)
            .map_err(|e| files::err(&self.env.bin_home, e))?;
        // The engine first: the app's menu entry never names a program without its engine.
        for name in [ENGINE, UI] {
            let src = self.env.exe_dir().join(name);
            files::replace(&src, &self.env.bin_home.join(name), true)?;
        }
        Ok(())
    }

    fn write_menu(&self, ui: &Path) -> Result<(), String> {
        for (rel, bytes) in assets::ICONS {
            files::write_if_changed(&self.env.icons().join(rel), bytes)?;
        }
        let text = desktop::app_entry(assets::APP_ENTRY, ui)?;
        files::write_if_changed(&self.env.app_entry(), text.as_bytes())
    }

    /// Installs or updates the user's copy when needed, then enables it. Returns
    /// whether a new login is needed for it to run.
    fn install_extension(&self) -> Result<bool, String> {
        let uuid = assets::EXTENSION_UUID;
        let embedded = assets::extension_version().unwrap_or_default();
        let mut relogin = false;
        let needs_files = match self.installed_extension() {
            None => true,
            Some((_, v)) => older(v.as_deref(), &embedded),
        };
        if needs_files {
            files::replace_dir(&self.env.extensions().join(uuid), assets::EXTENSION)?;
            relogin = true;
        }
        if self.shell.enabled(uuid) != Some(true) {
            match self.shell.enable(uuid)? {
                Enabled::Now => {}
                Enabled::NextLogin => relogin = true,
            }
        }
        Ok(relogin)
    }

    /// Removes what [`Setup::apply`] creates, for this user: the menu entry and icons,
    /// the autostart entry, the extension (turned off, user copy deleted) and the
    /// programs in `~/.local/bin`. Settings and the library are kept.
    pub fn uninstall(&self) -> Outcome {
        let mut out = Outcome::default();
        let mut menu = files::remove(&self.env.app_entry());
        for (rel, _) in assets::ICONS {
            let r = files::remove(&self.env.icons().join(rel));
            if menu.is_ok() {
                menu = r;
            }
        }
        out.results.push((ItemId::Menu, menu));
        out.results
            .push((ItemId::Autostart, files::remove(&self.env.autostart())));
        let uuid = assets::EXTENSION_UUID;
        let user_ext = self.env.extensions().join(uuid);
        if self.env.gnome || user_ext.exists() {
            let mut r = Ok(());
            if self.shell.enabled(uuid) == Some(true) {
                r = self.shell.disable(uuid);
            }
            if user_ext.exists() {
                let removed =
                    std::fs::remove_dir_all(&user_ext).map_err(|e| files::err(&user_ext, e));
                r = r.and(removed);
            }
            out.results.push((ItemId::Extension, r));
        }
        let mut programs = Ok(());
        let mut any = false;
        for name in [UI, ENGINE] {
            let p = self.env.bin_home.join(name);
            if p.symlink_metadata().is_ok() {
                any = true;
                programs = programs.and(files::remove(&p));
            }
        }
        if any {
            out.results.push((ItemId::Programs, programs));
        }
        out
    }
}

/// Writes the session autostart entry for `ui` (also used by the Settings toggle).
pub fn write_autostart(path: &Path, ui: &Path) -> Result<(), String> {
    let text = desktop::autostart_entry(ui)?;
    files::write_if_changed(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::TempDir;
    use std::sync::Mutex;

    /// GNOME Shell's enabled list, in memory.
    #[derive(Default)]
    struct FakeShell {
        enabled: Mutex<Vec<String>>,
        known_to_shell: bool,
        calls: Mutex<Vec<String>>,
    }

    impl Shell for FakeShell {
        fn enabled(&self, uuid: &str) -> Option<bool> {
            Some(self.enabled.lock().unwrap().iter().any(|u| u == uuid))
        }
        fn enable(&self, uuid: &str) -> Result<Enabled, String> {
            self.calls.lock().unwrap().push(format!("enable {uuid}"));
            self.enabled.lock().unwrap().push(uuid.to_owned());
            Ok(if self.known_to_shell {
                Enabled::Now
            } else {
                Enabled::NextLogin
            })
        }
        fn disable(&self, uuid: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("disable {uuid}"));
            self.enabled.lock().unwrap().retain(|u| u != uuid);
            Ok(())
        }
    }

    struct World {
        tmp: TempDir,
        setup: Setup,
    }

    impl World {
        fn root(&self) -> &Path {
            self.tmp.path()
        }
        fn state(&self, id: ItemId) -> Option<State> {
            self.setup
                .detect()
                .items
                .into_iter()
                .find(|i| i.id == id)
                .map(|i| i.state)
        }
    }

    /// A home with an extracted release archive in `~/Downloads/lumenwall-0.2.0-…`.
    fn world(gnome: bool, shell: FakeShell) -> World {
        let tmp = TempDir::new("linux");
        let root = tmp.path().to_owned();
        let bundle = root.join("home/Downloads/lumenwall-0.2.0-x86_64-unknown-linux-gnu");
        std::fs::create_dir_all(&bundle).unwrap();
        for (name, body) in [(UI, "ui v2"), (ENGINE, "engine v2")] {
            std::fs::write(bundle.join(name), body).unwrap();
        }
        std::fs::write(bundle.join(lumenwall_update::BUNDLE_MARKER), "").unwrap();
        let env = Env {
            data_home: root.join("home/.local/share"),
            config_home: root.join("home/.config"),
            bin_home: root.join("home/.local/bin"),
            data_dirs: vec![root.join("usr/share")],
            path: vec![root.join("home/.local/bin"), root.join("usr/bin")],
            exe: bundle.join(UI),
            gnome,
        };
        World {
            tmp,
            setup: Setup {
                env,
                shell: Box::new(shell),
            },
        }
    }

    #[test]
    fn fresh_home_needs_everything() {
        let w = world(true, FakeShell::default());
        let r = w.setup.detect();
        let ids: Vec<ItemId> = r.items.iter().map(|i| i.id).collect();
        assert_eq!(
            ids,
            [
                ItemId::Programs,
                ItemId::Menu,
                ItemId::Autostart,
                ItemId::Extension
            ]
        );
        assert!(r.items.iter().all(|i| i.state == State::Missing), "{r:?}");
        assert!(r.needs_setup());
        // Not GNOME: no extension item.
        let w = world(false, FakeShell::default());
        assert!(
            !w.setup
                .detect()
                .items
                .iter()
                .any(|i| i.id == ItemId::Extension)
        );
    }

    #[test]
    fn set_up_everything_then_detect_it() {
        let w = world(true, FakeShell::default());
        let all = [
            ItemId::Programs,
            ItemId::Menu,
            ItemId::Autostart,
            ItemId::Extension,
        ];
        let out = w.setup.apply(&all);
        assert!(out.results.iter().all(|(_, r)| r.is_ok()), "{out:?}");
        assert!(out.relogin, "a new extension needs a new login");
        let bin = w.root().join("home/.local/bin");
        assert_eq!(out.copied_to.as_deref(), Some(bin.as_path()));
        assert_eq!(std::fs::read_to_string(bin.join(UI)).unwrap(), "ui v2");
        assert_eq!(
            std::fs::read_to_string(bin.join(ENGINE)).unwrap(),
            "engine v2"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(bin.join(UI))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        // Entries point at the copies, with absolute paths.
        let entry = std::fs::read_to_string(
            w.root()
                .join("home/.local/share/applications/lumenwall-ui.desktop"),
        )
        .unwrap();
        assert_eq!(
            desktop::exec_program(&entry),
            Some(bin.join(UI).to_string_lossy().into_owned())
        );
        let auto =
            std::fs::read_to_string(w.root().join("home/.config/autostart/lumenwall.desktop"))
                .unwrap();
        assert_eq!(
            desktop::exec_program(&auto),
            Some(bin.join(UI).to_string_lossy().into_owned())
        );
        for (rel, bytes) in assets::ICONS {
            let p = w.root().join("home/.local/share/icons/hicolor").join(rel);
            assert_eq!(&std::fs::read(&p).unwrap(), bytes, "{}", p.display());
        }
        let ext = w
            .root()
            .join("home/.local/share/gnome-shell/extensions")
            .join(assets::EXTENSION_UUID);
        for (name, bytes) in assets::EXTENSION {
            assert_eq!(&std::fs::read(ext.join(name)).unwrap(), bytes);
        }
        let leftovers: Vec<_> = std::fs::read_dir(ext.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            leftovers.len(),
            1,
            "no temporary folder left: {leftovers:?}"
        );
        let r = w.setup.detect();
        assert!(r.items.iter().all(|i| i.state == State::Ok), "{r:?}");
        assert!(!r.needs_setup());
    }

    #[test]
    fn extension_known_to_the_shell_needs_no_new_login() {
        let shell = FakeShell {
            known_to_shell: true,
            ..Default::default()
        };
        let w = world(true, shell);
        // Installed system-wide (Debian package), not enabled for this user.
        let sys = w
            .root()
            .join("usr/share/gnome-shell/extensions")
            .join(assets::EXTENSION_UUID);
        std::fs::create_dir_all(&sys).unwrap();
        for (name, bytes) in assets::EXTENSION {
            std::fs::write(sys.join(name), bytes).unwrap();
        }
        assert_eq!(w.state(ItemId::Extension), Some(State::Disabled));
        let out = w.setup.apply(&[ItemId::Extension]);
        assert_eq!(out.results, vec![(ItemId::Extension, Ok(()))]);
        assert!(!out.relogin);
        assert!(
            !w.root()
                .join("home/.local/share/gnome-shell/extensions")
                .exists(),
            "the system copy is used as is"
        );
        assert_eq!(w.state(ItemId::Extension), Some(State::Ok));
    }

    #[test]
    fn older_extension_is_updated_and_stays_enabled() {
        let shell = FakeShell::default();
        shell
            .enabled
            .lock()
            .unwrap()
            .push(assets::EXTENSION_UUID.into());
        let w = world(true, shell);
        let ext = w
            .root()
            .join("home/.local/share/gnome-shell/extensions")
            .join(assets::EXTENSION_UUID);
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(
            ext.join("metadata.json"),
            format!(
                r#"{{"uuid": "{}", "version-name": "1"}}"#,
                assets::EXTENSION_UUID
            ),
        )
        .unwrap();
        std::fs::write(ext.join("extension.js"), "// old").unwrap();
        std::fs::write(ext.join("stale.js"), "// removed in version 2").unwrap();
        let r = w.setup.detect();
        let item = r.items.iter().find(|i| i.id == ItemId::Extension).unwrap();
        assert_eq!(item.state, State::Outdated);
        let embedded = assets::extension_version().unwrap();
        assert_eq!(item.versions, Some(("1".into(), embedded)));
        let out = w.setup.apply(&[ItemId::Extension]);
        assert_eq!(out.results, vec![(ItemId::Extension, Ok(()))]);
        assert!(out.relogin, "updated code runs after a new login");
        for (name, bytes) in assets::EXTENSION {
            assert_eq!(&std::fs::read(ext.join(name)).unwrap(), bytes);
        }
        assert!(
            !ext.join("stale.js").exists(),
            "the folder is replaced as a whole"
        );
        assert_eq!(w.state(ItemId::Extension), Some(State::Ok));
        // A newer copy (a later version installed by hand) is left alone.
        std::fs::write(
            ext.join("metadata.json"),
            format!(
                r#"{{"uuid": "{}", "version-name": "99"}}"#,
                assets::EXTENSION_UUID
            ),
        )
        .unwrap();
        assert_eq!(w.state(ItemId::Extension), Some(State::Ok));
    }

    #[test]
    fn repair_after_the_programs_moved() {
        let w = world(false, FakeShell::default());
        w.setup.apply(&[ItemId::Menu, ItemId::Autostart]);
        assert_eq!(w.state(ItemId::Menu), Some(State::Ok));
        // Entries point at the downloaded folder (programs not copied).
        let entry = std::fs::read_to_string(w.setup.env.app_entry()).unwrap();
        assert_eq!(
            desktop::exec_program(&entry),
            Some(w.setup.env.exe.to_string_lossy().into_owned())
        );
        // The folder is deleted: the entries are broken.
        std::fs::remove_file(&w.setup.env.exe).unwrap();
        assert_eq!(w.state(ItemId::Menu), Some(State::Outdated));
        assert_eq!(w.state(ItemId::Autostart), Some(State::Outdated));
        // A lost icon also counts.
        std::fs::write(&w.setup.env.exe, "ui v2").unwrap();
        assert_eq!(w.state(ItemId::Menu), Some(State::Ok));
        std::fs::remove_file(w.setup.env.icons().join("48x48/apps/lumenwall.png")).unwrap();
        assert_eq!(w.state(ItemId::Menu), Some(State::Outdated));
        let out = w.setup.apply(&[ItemId::Menu]);
        assert_eq!(out.results, vec![(ItemId::Menu, Ok(()))]);
        assert_eq!(w.state(ItemId::Menu), Some(State::Ok));
        // An entry naming a program on PATH (installed by hand) is fine.
        std::fs::write(
            w.setup.env.app_entry(),
            "[Desktop Entry]\nExec=lumenwall-ui\n",
        )
        .unwrap();
        assert_eq!(
            w.state(ItemId::Menu),
            Some(State::Outdated),
            "not on PATH yet"
        );
        std::fs::create_dir_all(&w.setup.env.bin_home).unwrap();
        std::fs::write(w.setup.env.bin_home.join(UI), "x").unwrap();
        assert_eq!(w.state(ItemId::Menu), Some(State::Ok));
        // A different version in ~/.local/bin.
        assert_eq!(w.state(ItemId::Programs), Some(State::Outdated));
        // Autostart turned off by the user.
        std::fs::write(w.setup.env.autostart(), "[Desktop Entry]\nHidden=true\n").unwrap();
        assert_eq!(w.state(ItemId::Autostart), Some(State::Missing));
    }

    #[test]
    fn system_package_provides_the_menu_entry() {
        let w = world(false, FakeShell::default());
        let apps = w.root().join("usr/share/applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join(APP_ENTRY), "[Desktop Entry]\nExec=lumenwall-ui\n").unwrap();
        assert_eq!(w.state(ItemId::Menu), Some(State::System));
    }

    #[test]
    fn programs_are_not_copied_from_elsewhere() {
        // Run from ~/.local/bin already, or from a build folder: nothing to copy.
        let mut w = world(false, FakeShell::default());
        std::fs::create_dir_all(&w.setup.env.bin_home).unwrap();
        w.setup.env.exe = w.setup.env.bin_home.join(UI);
        std::fs::write(&w.setup.env.exe, "ui").unwrap();
        std::fs::write(
            w.setup.env.bin_home.join(lumenwall_update::BUNDLE_MARKER),
            "",
        )
        .unwrap();
        assert_eq!(w.state(ItemId::Programs), None);
        w.setup.env.exe = w.root().join("src/target/release").join(UI);
        assert_eq!(w.state(ItemId::Programs), None);
        let out = w.setup.apply(&[ItemId::Programs]);
        assert!(out.results.is_empty() && out.copied_to.is_none());
    }

    #[test]
    fn uninstall_removes_what_setup_created() {
        let shell = FakeShell::default();
        let w = world(true, shell);
        let all = [
            ItemId::Programs,
            ItemId::Menu,
            ItemId::Autostart,
            ItemId::Extension,
        ];
        w.setup.apply(&all);
        // Something of the user's own next to ours stays.
        let mine = w
            .root()
            .join("home/.local/share/applications/other.desktop");
        std::fs::write(&mine, "[Desktop Entry]\n").unwrap();
        let out = w.setup.uninstall();
        assert!(out.results.iter().all(|(_, r)| r.is_ok()), "{out:?}");
        assert!(mine.is_file());
        let r = w.setup.detect();
        assert!(r.items.iter().all(|i| i.state == State::Missing), "{r:?}");
        for p in [
            "home/.local/bin/lumenwall",
            "home/.local/bin/lumenwall-ui",
            "home/.config/autostart/lumenwall.desktop",
            "home/.local/share/applications/lumenwall-ui.desktop",
            "home/.local/share/icons/hicolor/scalable/apps/lumenwall.svg",
            "home/.local/share/gnome-shell/extensions/lumenwall@simnjs.github.io",
        ] {
            assert!(!w.root().join(p).exists(), "{p}");
        }
        // The downloaded folder is not ours to delete.
        assert!(w.setup.env.exe.is_file());
        // Twice is fine.
        let again = w.setup.uninstall();
        assert!(again.results.iter().all(|(_, r)| r.is_ok()), "{again:?}");
    }

    #[test]
    fn versions_and_desktops() {
        assert!(older(Some("1"), "2"));
        assert!(older(Some("2"), "2.1"));
        assert!(older(Some("9"), "10"));
        assert!(!older(Some("2"), "2"));
        assert!(!older(Some("3"), "2"));
        assert!(older(None, "2"));
        assert!(older(Some("beta"), "2"));
        assert_eq!(
            metadata_version(br#"{"version-name": "2", "version": 5}"#).as_deref(),
            Some("2")
        );
        assert_eq!(metadata_version(br#"{"version": 5}"#).as_deref(), Some("5"));
        assert_eq!(metadata_version(b"{}"), None);
        assert!(is_gnome("ubuntu:GNOME", ""));
        assert!(is_gnome("GNOME-Classic:GNOME", ""));
        assert!(!is_gnome("KDE", "plasma"));
        assert!(is_gnome("", "gnome-xorg"));
    }
}

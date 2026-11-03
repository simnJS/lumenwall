//! Whether this copy of Lumenwall may replace itself.
//!
//! A copy installed by a package manager is updated by it: replacing its files would
//! fight the package database (and Flatpak or Snap files are read-only anyway).
//! Packagers can say so explicitly in two ways, besides the detection below:
//!
//! - at build time: `LUMENWALL_PACKAGED=<name> cargo build --release`;
//! - at install time: a `lumenwall.packaged` file next to the programs, whose first
//!   line names the package manager.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::{Layout, REPOSITORY, Source};

/// File next to the programs that marks a package-managed installation.
pub const PACKAGED_MARKER: &str = "lumenwall.packaged";

/// Who keeps a package-managed copy up to date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Manager {
    Flatpak,
    Snap,
    AppImage,
    Nix,
    Guix,
    Homebrew,
    /// A distribution package (`/usr/bin`, ...).
    Distribution,
    Scoop,
    Winget,
    Chocolatey,
    MicrosoftStore,
    /// Named by the packager (build-time variable or marker file).
    Named(String),
}

impl Manager {
    /// The product name, or `None` for [`Manager::Distribution`] (say "your
    /// distribution" in the user's language).
    pub fn name(&self) -> Option<&str> {
        Some(match self {
            Manager::Flatpak => "Flatpak",
            Manager::Snap => "Snap",
            Manager::AppImage => "AppImage",
            Manager::Nix => "Nix",
            Manager::Guix => "Guix",
            Manager::Homebrew => "Homebrew",
            Manager::Distribution => return None,
            Manager::Scoop => "Scoop",
            Manager::Winget => "winget",
            Manager::Chocolatey => "Chocolatey",
            Manager::MicrosoftStore => "Microsoft Store",
            Manager::Named(n) => n,
        })
    }
}

/// What the updater may do with this copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// May check, download and install into this folder.
    Ready(Layout),
    /// Installed by a package manager, which updates it.
    Managed(Manager),
    /// The folder cannot be written by this user.
    ReadOnly(PathBuf),
    /// Debug build, or a program run from a Cargo `target` folder.
    Development,
    /// Built without [`crate::RELEASE_PUBLIC_KEY`]: nothing could be verified.
    NoKey,
    /// No release archives for this platform.
    Unsupported,
    /// `LUMENWALL_NO_UPDATE` is set.
    Disabled,
}

/// Decides for the program at `exe` (usually `std::env::current_exe()`).
pub fn availability(exe: &Path) -> Availability {
    decide(
        exe,
        &Facts {
            has_key: crate::RELEASE_PUBLIC_KEY.is_some(),
            debug: cfg!(debug_assertions),
            packaged: option_env!("LUMENWALL_PACKAGED").filter(|s| !s.trim().is_empty()),
            flatpak_info: cfg!(target_os = "linux") && Path::new("/.flatpak-info").exists(),
            env: &|k| std::env::var_os(k),
        },
    )
}

struct Facts<'a> {
    has_key: bool,
    debug: bool,
    packaged: Option<&'a str>,
    flatpak_info: bool,
    env: &'a dyn Fn(&str) -> Option<OsString>,
}

fn decide(exe: &Path, f: &Facts<'_>) -> Availability {
    if crate::TARGET.is_none() {
        return Availability::Unsupported;
    }
    if (f.env)("LUMENWALL_NO_UPDATE").is_some_and(|v| !v.is_empty() && v != "0") {
        return Availability::Disabled;
    }
    if let Some(name) = f.packaged {
        return Availability::Managed(Manager::Named(name.trim().to_owned()));
    }
    let Some(dir) = exe.parent() else {
        return Availability::Unsupported;
    };
    if let Some(m) = marker(dir).or_else(|| manager(exe, f)) {
        return Availability::Managed(m);
    }
    if f.debug || from_cargo_target(exe) {
        return Availability::Development;
    }
    if !f.has_key {
        return Availability::NoKey;
    }
    if !writable(dir) {
        return Availability::ReadOnly(dir.to_owned());
    }
    Availability::Ready(Layout::of_dir(dir))
}

fn marker(dir: &Path) -> Option<Manager> {
    let text = std::fs::read_to_string(dir.join(PACKAGED_MARKER)).ok()?;
    let name = text.lines().next().unwrap_or_default().trim();
    Some(if name.is_empty() {
        Manager::Distribution
    } else {
        Manager::Named(name.chars().take(40).collect())
    })
}

fn manager(exe: &Path, f: &Facts<'_>) -> Option<Manager> {
    let env = |k: &str| (f.env)(k).is_some_and(|v| !v.is_empty());
    if f.flatpak_info || env("FLATPAK_ID") {
        return Some(Manager::Flatpak);
    }
    if env("SNAP") {
        return Some(Manager::Snap);
    }
    if env("APPIMAGE") {
        return Some(Manager::AppImage);
    }
    let path = exe
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let has = |p: &str| path.contains(p);
    if path.starts_with("/nix/store/") {
        Some(Manager::Nix)
    } else if path.starts_with("/gnu/store/") {
        Some(Manager::Guix)
    } else if path.starts_with("/snap/") {
        Some(Manager::Snap)
    } else if path.starts_with("/app/") || path.starts_with("/var/lib/flatpak/") {
        Some(Manager::Flatpak)
    } else if has("/.linuxbrew/") || has("/homebrew/") || has("/cellar/") {
        Some(Manager::Homebrew)
    } else if (path.starts_with("/usr/") && !path.starts_with("/usr/local/"))
        || path.starts_with("/bin/")
        || path.starts_with("/sbin/")
    {
        Some(Manager::Distribution)
    } else if has("/scoop/apps/") {
        Some(Manager::Scoop)
    } else if has("/winget/packages/") || has("/microsoft/winget/") {
        Some(Manager::Winget)
    } else if has("/chocolatey/") {
        Some(Manager::Chocolatey)
    } else if has("/windowsapps/") {
        Some(Manager::MicrosoftStore)
    } else {
        None
    }
}

/// `.../target/[<triple>/]debug|release/<exe>` (or a custom `target/<name>/...`).
fn from_cargo_target(exe: &Path) -> bool {
    let parts: Vec<_> = exe.components().collect();
    parts.iter().enumerate().any(|(i, c)| {
        *c == Component::Normal("target".as_ref())
            && parts[i + 1..]
                .iter()
                .any(|c| matches!(c.as_os_str().to_str(), Some("debug" | "release")))
    })
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".lumenwall-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Development only (debug builds; always `None` in release builds): a test server
/// standing in for GitHub, such as `cargo run -p lumenwall-update --example fake_github`.
///
/// - `LUMENWALL_UPDATE_SERVER`: its loopback URL (`http://127.0.0.1:<port>`);
/// - `LUMENWALL_UPDATE_KEY`: the public key it signs with;
/// - `LUMENWALL_UPDATE_DIR`: a scratch folder that plays the installation folder.
pub fn dev_override() -> Option<(Source, Layout)> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let server = std::env::var("LUMENWALL_UPDATE_SERVER").ok()?;
    let key = std::env::var("LUMENWALL_UPDATE_KEY").ok()?;
    let dir = PathBuf::from(std::env::var_os("LUMENWALL_UPDATE_DIR")?);
    match Source::new(&server, REPOSITORY, &key) {
        Ok(source) => {
            tracing::warn!(server, dir = %dir.display(), "development update server");
            Some((source, Layout::of_dir(dir)))
        }
        Err(e) => {
            tracing::error!("LUMENWALL_UPDATE_SERVER: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(env: &'a dyn Fn(&str) -> Option<OsString>) -> Facts<'a> {
        Facts {
            has_key: true,
            debug: false,
            packaged: None,
            flatpak_info: false,
            env,
        }
    }

    fn none(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn package_managers_are_detected() {
        let f = facts(&none);
        let m = |p: &str| manager(Path::new(p), &f);
        assert_eq!(m("/usr/bin/lumenwall-ui"), Some(Manager::Distribution));
        assert_eq!(m("/usr/local/bin/lumenwall-ui"), None);
        assert_eq!(
            m("/nix/store/abc-lumenwall/bin/lumenwall-ui"),
            Some(Manager::Nix)
        );
        assert_eq!(m("/app/bin/lumenwall-ui"), Some(Manager::Flatpak));
        assert_eq!(m("/home/me/Apps/lumenwall/lumenwall-ui"), None);
        assert_eq!(
            m(r"C:\Users\me\scoop\apps\lumenwall\current\lumenwall-ui.exe"),
            Some(Manager::Scoop)
        );
        assert_eq!(
            m(r"C:\Users\me\AppData\Local\Microsoft\WinGet\Packages\Lumenwall\lumenwall-ui.exe"),
            Some(Manager::Winget)
        );
        assert_eq!(
            m(r"C:\Users\me\AppData\Local\Programs\Lumenwall\lumenwall-ui.exe"),
            None
        );
        let flatpak = |k: &str| (k == "FLATPAK_ID").then(|| OsString::from("x"));
        assert_eq!(
            manager(Path::new("/home/me/x/lumenwall-ui"), &facts(&flatpak)),
            Some(Manager::Flatpak)
        );
    }

    #[test]
    fn decisions() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("lumenwall-ui");
        if crate::TARGET.is_none() {
            return;
        }
        let f = facts(&none);
        assert_eq!(
            decide(&exe, &f),
            Availability::Ready(Layout::of_dir(dir.path()))
        );
        assert_eq!(
            decide(
                &exe,
                &Facts {
                    has_key: false,
                    ..facts(&none)
                }
            ),
            Availability::NoKey
        );
        assert_eq!(
            decide(
                &exe,
                &Facts {
                    debug: true,
                    ..facts(&none)
                }
            ),
            Availability::Development
        );
        assert_eq!(
            decide(
                &exe,
                &Facts {
                    packaged: Some("AUR"),
                    ..facts(&none)
                }
            ),
            Availability::Managed(Manager::Named("AUR".into()))
        );
        let off = |k: &str| (k == "LUMENWALL_NO_UPDATE").then(|| OsString::from("1"));
        assert_eq!(decide(&exe, &facts(&off)), Availability::Disabled);
        std::fs::write(dir.path().join(PACKAGED_MARKER), "Debian\n").unwrap();
        assert_eq!(
            decide(&exe, &f),
            Availability::Managed(Manager::Named("Debian".into()))
        );
        assert!(from_cargo_target(Path::new(
            "/src/lumenwall/target/release/lumenwall-ui"
        )));
        assert!(from_cargo_target(Path::new(
            "/src/lumenwall/target/agent-x/x86_64-pc-windows-gnu/debug/lumenwall-ui.exe"
        )));
        assert!(!from_cargo_target(Path::new("/opt/lumenwall/lumenwall-ui")));
    }

    #[cfg(unix)]
    #[test]
    fn read_only_folder() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root ignores permissions: nothing to check then.
        if writable(&ro) {
            return;
        }
        if crate::TARGET.is_some() {
            assert_eq!(
                decide(&ro.join("lumenwall-ui"), &facts(&none)),
                Availability::ReadOnly(ro.clone())
            );
        }
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

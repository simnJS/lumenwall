//! Finding Steam, its libraries, the Wallpaper Engine Workshop folders and the
//! Wallpaper Engine install (whose assets scenes need). Behind [`Discovery`] so that
//! `lumenwall-assets` can replace this implementation once it is integrated.

use std::path::{Path, PathBuf};

/// Steam app id of Wallpaper Engine.
pub const WE_APP_ID: &str = "431960";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SteamInstall {
    /// Steam installations found (several when e.g. native and Flatpak coexist).
    pub roots: Vec<PathBuf>,
    /// Every Steam library folder (each has a `steamapps` directory).
    pub libraries: Vec<PathBuf>,
    /// `steamapps/workshop/content/431960` folders that exist.
    pub workshop_dirs: Vec<PathBuf>,
    /// `steamapps/common/wallpaper_engine` when Wallpaper Engine is installed.
    pub we_dir: Option<PathBuf>,
    /// Places Lumenwall looked for Steam, for the "Steam not found" message.
    pub searched: Vec<PathBuf>,
}

impl SteamInstall {
    /// Wallpaper Engine's `assets` folder, required to play scenes.
    pub fn we_assets(&self) -> Option<PathBuf> {
        let assets = self.we_dir.as_ref()?.join("assets");
        assets.is_dir().then_some(assets)
    }

    /// Wallpapers that ship with Wallpaper Engine or that the user made in its editor.
    pub fn we_project_dirs(&self) -> Vec<PathBuf> {
        let Some(we) = &self.we_dir else {
            return Vec::new();
        };
        ["projects/defaultprojects", "projects/myprojects"]
            .iter()
            .map(|p| we.join(p))
            .filter(|p| p.is_dir())
            .collect()
    }
}

pub trait Discovery: Send + Sync {
    fn discover(&self) -> SteamInstall;
}

/// Looks in the usual places of the current platform.
pub struct NativeDiscovery;

impl Discovery for NativeDiscovery {
    fn discover(&self) -> SteamInstall {
        let candidates = candidate_roots();
        let mut install = SteamInstall {
            searched: candidates.clone(),
            ..Default::default()
        };
        for root in candidates {
            if !root.join("steamapps").is_dir() {
                continue;
            }
            let root = canonical(&root);
            if install.roots.contains(&root) {
                continue;
            }
            for lib in libraries_of(&root) {
                if !install.libraries.contains(&lib) {
                    install.libraries.push(lib);
                }
            }
            install.roots.push(root);
        }
        // Test override documented in docs/TESTING.md.
        if let Some(dir) = std::env::var_os("LUMENWALL_WE_WORKSHOP").map(PathBuf::from)
            && dir.is_dir()
        {
            install.workshop_dirs.push(canonical(&dir));
        }
        for lib in &install.libraries {
            let ws = lib.join("steamapps/workshop/content").join(WE_APP_ID);
            if ws.is_dir() && !install.workshop_dirs.contains(&canonical(&ws)) {
                install.workshop_dirs.push(canonical(&ws));
            }
            let we = lib.join("steamapps/common/wallpaper_engine");
            if install.we_dir.is_none() && we.is_dir() {
                install.we_dir = Some(canonical(&we));
            }
        }
        if install.we_dir.is_none()
            && let Some(assets) = std::env::var_os("LUMENWALL_WE_ASSETS").map(PathBuf::from)
            && let Some(we) = assets.parent()
            && assets.is_dir()
        {
            install.we_dir = Some(canonical(we));
        }
        install
    }
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned())
}

#[cfg(not(windows))]
fn candidate_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut v = Vec::new();
    if let Some(data) = dirs::data_dir() {
        v.push(data.join("Steam"));
    }
    v.extend([
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/root"),
        // Flatpak
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
        // Snap
        home.join("snap/steam/common/.local/share/Steam"),
        home.join("snap/steam/common/.steam/steam"),
    ]);
    v.dedup();
    v
}

#[cfg(windows)]
fn candidate_roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = crate::platform::registry_string(true, r"Software\Valve\Steam", "SteamPath") {
        v.push(PathBuf::from(p.replace('/', "\\")));
    }
    if let Some(p) =
        crate::platform::registry_string(false, r"SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath")
    {
        v.push(PathBuf::from(p));
    }
    v.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
    v.push(PathBuf::from(r"C:\Program Files\Steam"));
    v
}

/// The root itself plus the folders listed in `libraryfolders.vdf`.
fn libraries_of(root: &Path) -> Vec<PathBuf> {
    let mut libs = vec![root.to_owned()];
    for vdf in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
        let Ok(text) = std::fs::read_to_string(root.join(vdf)) else {
            continue;
        };
        for path in vdf_paths(&text) {
            let p = PathBuf::from(path);
            if p.join("steamapps").is_dir() {
                let p = canonical(&p);
                if !libs.contains(&p) {
                    libs.push(p);
                }
            }
        }
    }
    libs
}

/// Values of every `"path"` key in a Valve KeyValues text file. Only what is needed
/// here: quoted tokens with `\\` and `\"` escapes, braces, `//` comments.
pub fn vdf_paths(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => break,
                        },
                        '"' => break,
                        _ => s.push(c),
                    }
                }
                tokens.push(Some(s));
            }
            '{' | '}' => tokens.push(None),
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    tokens
        .windows(2)
        .filter_map(|w| match (&w[0], &w[1]) {
            (Some(k), Some(v)) if k.eq_ignore_ascii_case("path") => Some(v.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_library_paths() {
        let vdf = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/me/.local/share/Steam"
		"label"		""
		"apps" { "431960" "1234" }
	}
	// second disk
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
}"#;
        assert_eq!(
            vdf_paths(vdf),
            ["/home/me/.local/share/Steam", "D:\\SteamLibrary"]
        );
    }

    #[test]
    fn discovery_on_a_fake_tree() {
        let tmp = std::env::temp_dir().join(format!("lumenwall-ui-steam-{}", std::process::id()));
        let ws = tmp.join("lib/steamapps/workshop/content").join(WE_APP_ID);
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(tmp.join("lib/steamapps/common/wallpaper_engine/assets")).unwrap();
        let libs = libraries_of(&tmp.join("lib"));
        assert_eq!(libs.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

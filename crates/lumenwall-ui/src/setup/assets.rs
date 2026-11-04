//! Files the Linux setup writes, compiled into the program so it works from a bare
//! binary: the app's desktop entry, the `hicolor` icons and the GNOME Shell extension,
//! all from `packaging/` (a test checks that nothing there is missing here).

/// The app's desktop entry (`Exec` is replaced by the installed program's path).
pub const APP_ENTRY: &str = include_str!("../../../../packaging/linux/lumenwall-ui.desktop");

macro_rules! files {
    ($dir:literal: $($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../../../../", $dir, "/", $name)) as &[u8])),*]
    };
}

/// Icons, relative to `icons/hicolor/`.
pub const ICONS: &[(&str, &[u8])] = files!("packaging/linux/icons/hicolor":
    "16x16/apps/lumenwall.png",
    "24x24/apps/lumenwall.png",
    "32x32/apps/lumenwall.png",
    "48x48/apps/lumenwall.png",
    "64x64/apps/lumenwall.png",
    "128x128/apps/lumenwall.png",
    "256x256/apps/lumenwall.png",
    "512x512/apps/lumenwall.png",
    "scalable/apps/lumenwall.svg",
    "symbolic/apps/lumenwall-symbolic.svg",
    "symbolic/apps/lumenwall-paused-symbolic.svg",
);

/// UUID (and folder name) of Lumenwall's GNOME Shell extension.
pub const EXTENSION_UUID: &str = "lumenwall@simnjs.github.io";

/// The extension's files, relative to its folder.
pub const EXTENSION: &[(&str, &[u8])] =
    files!("packaging/gnome-extension/lumenwall@simnjs.github.io": "metadata.json", "extension.js");

/// `version-name` of the embedded extension.
pub fn extension_version() -> Option<String> {
    let (_, metadata) = EXTENSION.iter().find(|(n, _)| *n == "metadata.json")?;
    super::linux::metadata_version(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn files_under(root: &Path) -> Vec<String> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else {
                    out.push(
                        p.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    fn packaging() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging")
    }

    /// A file added to `packaging/` must be added to the lists above.
    #[test]
    fn every_packaged_file_is_embedded() {
        let mut icons: Vec<String> = ICONS.iter().map(|(n, _)| (*n).to_owned()).collect();
        icons.sort();
        assert_eq!(icons, files_under(&packaging().join("linux/icons/hicolor")));
        let mut ext: Vec<String> = EXTENSION.iter().map(|(n, _)| (*n).to_owned()).collect();
        ext.sort();
        assert_eq!(
            ext,
            files_under(&packaging().join("gnome-extension").join(EXTENSION_UUID))
        );
        let meta: serde_json::Value = serde_json::from_slice(
            EXTENSION
                .iter()
                .find(|(n, _)| *n == "metadata.json")
                .unwrap()
                .1,
        )
        .unwrap();
        assert_eq!(meta["uuid"], EXTENSION_UUID);
        assert!(extension_version().is_some());
        assert!(APP_ENTRY.contains("\nExec=lumenwall-ui\n"));
    }

    /// `packaging/linux/install.sh` writes the same files as the app.
    #[test]
    fn install_script_matches_the_app() {
        let script = std::fs::read_to_string(packaging().join("linux/install.sh")).unwrap();
        let list = script.split("ICONS=\"").nth(1).unwrap();
        let list = &list[..list.find('"').unwrap()];
        let mut theirs: Vec<&str> = list.split_whitespace().collect();
        theirs.sort();
        let mut ours: Vec<&str> = ICONS.iter().map(|(n, _)| *n).collect();
        ours.sort();
        assert_eq!(theirs, ours, "ICONS in install.sh");
        assert!(script.contains(&format!("UUID=\"{EXTENSION_UUID}\"")));
        assert!(script.contains(&format!(
            "RELEASE_KEY=\"{}\"",
            lumenwall_update::RELEASE_PUBLIC_KEY.unwrap()
        )));
        // The autostart entry, line by line (the program path lines differ only by
        // how they are produced).
        let body = script.split("write_autostart_entry() {").nth(1).unwrap();
        let body = &body[..body.find("} >").unwrap()];
        let sh: Vec<String> = body
            .lines()
            .filter_map(|l| l.trim().strip_prefix("echo \""))
            .map(|l| l.trim_end_matches('"').to_owned())
            .collect();
        let rust = super::super::desktop::autostart_entry(Path::new("/x/lumenwall-ui")).unwrap();
        let rust: Vec<&str> = rust.lines().collect();
        assert_eq!(sh.len(), rust.len(), "{sh:#?}");
        for (a, b) in sh.iter().zip(&rust) {
            if a.starts_with("TryExec=") || a.starts_with("Exec=") {
                assert_eq!(a.split('=').next(), b.split('=').next());
            } else {
                assert_eq!(a, b);
            }
        }
    }
}

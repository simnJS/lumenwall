//! Which local files a page may read through `file://` URLs.
//!
//! The WE contract tells pages to load user-selected files with `'file:///' + value`, so web
//! security cannot simply forbid `file://`. Lumenwall allows exactly: the wallpaper folder, the files
//! chosen in `file` properties and the folders chosen in `directory` properties. Everything else
//! is refused, so a page cannot read arbitrary files of the user.

use std::path::{Path, PathBuf};

use crate::files::percent_decode;
use crate::properties::{PropertySet, PropertyValue};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalFileAccess {
    dirs: Vec<PathBuf>,
    files: Vec<PathBuf>,
}

impl LocalFileAccess {
    pub fn from_properties(wallpaper_root: &Path, properties: &PropertySet) -> Self {
        let mut access = Self::default();
        if let Ok(root) = wallpaper_root.canonicalize() {
            access.dirs.push(root);
        }
        for property in properties.iter() {
            match &property.value {
                PropertyValue::File(path) if !path.is_empty() => {
                    if let Ok(p) = Path::new(path).canonicalize() {
                        access.files.push(p);
                    }
                }
                PropertyValue::Directory { path, .. } if !path.is_empty() => {
                    if let Ok(p) = Path::new(path).canonicalize() {
                        access.dirs.push(p);
                    }
                }
                _ => {}
            }
        }
        access
    }

    /// Whether the file at `path` may be read.
    pub fn allows_path(&self, path: &Path) -> bool {
        let Ok(canonical) = path.canonicalize() else {
            return false;
        };
        self.files.contains(&canonical) || self.dirs.iter().any(|d| canonical.starts_with(d))
    }

    /// Whether a `file://` URL may be loaded. Non-`file:` URLs are not this policy's concern.
    pub fn allows_url(&self, url: &str) -> bool {
        let Some(rest) = url
            .strip_prefix("file://")
            .or_else(|| url.strip_prefix("FILE://"))
        else {
            return true;
        };
        // file:///home/a → /home/a ; file:///C:/a → C:/a ; file://host/... is refused.
        let Some(path) = rest.strip_prefix('/') else {
            return false;
        };
        let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
        let Some(decoded) = percent_decode(path) else {
            return false;
        };
        let trimmed = decoded.trim_start_matches('/');
        let candidate = if cfg!(windows) && trimmed.as_bytes().get(1) == Some(&b':') {
            PathBuf::from(trimmed)
        } else {
            PathBuf::from(format!("/{trimmed}"))
        };
        self.allows_path(&candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn allows_only_selected_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("wp");
        let pics = tmp.path().join("pics");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&pics).unwrap();
        for f in [
            root.join("a.png"),
            pics.join("b.png"),
            tmp.path().join("chosen.png"),
            tmp.path().join("secret"),
        ] {
            std::fs::write(f, b"x").unwrap();
        }
        let props = PropertySet::from_project_properties(&json!({
            "img": { "type": "file", "value": tmp.path().join("chosen.png").to_str().unwrap() },
            "dir": { "type": "directory", "value": pics.to_str().unwrap() }
        }));
        let access = LocalFileAccess::from_properties(&root, &props);
        assert!(access.allows_path(&root.join("a.png")));
        assert!(access.allows_path(&pics.join("b.png")));
        assert!(access.allows_path(&tmp.path().join("chosen.png")));
        assert!(!access.allows_path(&tmp.path().join("secret")));
        assert!(!access.allows_path(&pics.join("../secret")));
        assert!(access.allows_url("https://example.com/x"));
        if cfg!(unix) {
            let url = format!(
                "file:///{}",
                crate::properties::file_url_suffix(pics.join("b.png").to_str().unwrap())
            );
            assert!(access.allows_url(&url));
            let url = format!(
                "file:///{}",
                crate::properties::file_url_suffix(tmp.path().join("secret").to_str().unwrap())
            );
            assert!(!access.allows_url(&url));
            assert!(!access.allows_url("file://host/etc/passwd"));
        }
    }
}

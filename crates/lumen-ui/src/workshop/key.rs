//! The user's personal Steam Web API key: stored only in a private file of the user
//! configuration directory (mode 0600 on Unix), never in the repository, never in
//! logs. Lumen ships no key.

use std::io::Write;
use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("steam-web-api-key"))
}

/// Steam Web API keys are 32 hexadecimal characters.
pub fn is_valid(key: &str) -> bool {
    key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn load() -> Option<String> {
    let path = path()?;
    let key = std::fs::read_to_string(&path).ok()?.trim().to_owned();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Tighten a file created by hand with a looser mode.
        if std::fs::metadata(&path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
    is_valid(&key).then_some(key)
}

pub fn save(key: &str) -> std::io::Result<()> {
    let key = key.trim();
    if !is_valid(key) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a Steam Web API key",
        ));
    }
    let path = path().ok_or(std::io::ErrorKind::NotFound)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&tmp)?;
    f.write_all(key.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(tmp, path)
}

pub fn remove() -> std::io::Result<()> {
    match path().map(std::fs::remove_file) {
        Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// `••••1a2b`: enough to recognise the key without revealing it.
pub fn masked(key: &str) -> String {
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("••••••••{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_and_mask() {
        assert!(is_valid("0123456789ABCDEF0123456789abcdef"));
        assert!(!is_valid("0123456789ABCDEF0123456789abcde"));
        assert!(!is_valid("0123456789ABCDEF0123456789abcdeg"));
        assert_eq!(masked("0123456789ABCDEF0123456789abcdef"), "••••••••cdef");
    }
}

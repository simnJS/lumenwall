//! Endpoint naming and line framing.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use interprocess::local_socket::{GenericFilePath, GenericNamespaced, Name, prelude::*};
use serde::Serialize;

use crate::proto::MAX_FRAME_LEN;

/// Environment variable that overrides the default endpoint (a socket path on Unix,
/// a pipe name on Windows). Useful to run a second daemon for tests.
pub const ENDPOINT_ENV: &str = "LUMENWALL_IPC_ENDPOINT";

/// Where the daemon listens.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// A socket file (Unix only). Access is limited by the permissions of its directory.
    Path(PathBuf),
    /// A namespaced name: a named pipe `\\.\pipe\<name>` on Windows, an abstract
    /// socket on Linux (no file, but no permission check either: tests only).
    Name(String),
}

impl Endpoint {
    /// The per-user endpoint the daemon and its clients agree on.
    ///
    /// - Linux: `$XDG_RUNTIME_DIR/lumenwall/ipc.sock` (the runtime dir is private to the user),
    ///   or `<tmp>/lumenwall-<user>/ipc.sock` when it is not set.
    /// - Windows: the named pipe `\\.\pipe\lumenwall-ipc-<user>`.
    ///
    /// [`ENDPOINT_ENV`] overrides both.
    pub fn for_current_user() -> Self {
        if let Some(value) = std::env::var_os(ENDPOINT_ENV).filter(|v| !v.is_empty()) {
            return if cfg!(unix) {
                Endpoint::Path(PathBuf::from(value))
            } else {
                Endpoint::Name(value.to_string_lossy().into_owned())
            };
        }
        Self::platform_default()
    }

    #[cfg(unix)]
    fn platform_default() -> Self {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .map(|d| PathBuf::from(d).join("lumenwall"))
            .unwrap_or_else(|| std::env::temp_dir().join(format!("lumenwall-{}", user_name())));
        Endpoint::Path(dir.join("ipc.sock"))
    }

    #[cfg(not(unix))]
    fn platform_default() -> Self {
        Endpoint::Name(format!("lumenwall-ipc-{}", user_name()))
    }

    pub(crate) fn name(&self) -> io::Result<Name<'_>> {
        match self {
            Endpoint::Path(path) => path.as_path().to_fs_name::<GenericFilePath>(),
            Endpoint::Name(name) => name.as_str().to_ns_name::<GenericNamespaced>(),
        }
    }

    /// Creates the private parent directory of a socket path (mode 0700 on Unix).
    pub(crate) fn prepare(&self) -> io::Result<()> {
        if let Endpoint::Path(path) = self
            && let Some(dir) = path.parent()
        {
            create_private_dir(dir)?;
        }
        Ok(())
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Endpoint::Path(p) => write!(f, "{}", p.display()),
            Endpoint::Name(n) => write!(f, "{n}"),
        }
    }
}

/// User name restricted to characters safe in a file or pipe name.
fn user_name() -> String {
    let raw = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(64)
        .collect();
    if clean.is_empty() {
        "user".to_owned()
    } else {
        clean
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
    {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    // A directory under a shared /tmp could have been created by someone else first.
    let meta = std::fs::metadata(dir)?;
    #[cfg(target_os = "linux")]
    {
        let me = std::fs::metadata("/proc/self")?.uid();
        if meta.uid() != me {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} belongs to another user", dir.display()),
            ));
        }
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Reads one `\n`-terminated frame into `buf` (cleared first, newline stripped).
/// Returns `Ok(false)` on a clean end of stream.
pub(crate) fn read_frame<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<bool> {
    buf.clear();
    let limit = MAX_FRAME_LEN as u64 + 1;
    let n = io::Read::take(&mut *reader, limit).read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        Ok(true)
    } else if buf.len() > MAX_FRAME_LEN {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ))
    } else {
        // End of stream in the middle of a frame.
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated frame",
        ))
    }
}

/// Serializes `value` as one line (compact JSON never contains a raw newline).
pub(crate) fn encode_frame<T: Serialize>(value: &T) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).expect("IPC types always serialize");
    line.push(b'\n');
    line
}

pub(crate) fn write_frame(writer: &mut impl Write, line: &[u8]) -> io::Result<()> {
    writer.write_all(line)?;
    writer.flush()
}

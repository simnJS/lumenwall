/// Why a check, a download, an installation or a restart did not happen.
///
/// Messages never contain URLs or response bodies: they are shown to the user and
/// written to logs as they are.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// No connection, DNS failure, timeout, TLS failure.
    #[error("GitHub cannot be reached")]
    Offline,
    /// GitHub's request limit for this network; no request before `retry_at`
    /// (Unix seconds).
    #[error("GitHub's request limit is reached")]
    RateLimited { retry_at: u64 },
    /// Another HTTP status.
    #[error("GitHub answered HTTP {0}")]
    Status(u16),
    /// The answer is not what the documented API returns.
    #[error("unexpected answer from GitHub: {0}")]
    BadResponse(String),
    /// A URL outside the allowed hosts (GitHub's API, release downloads and their
    /// storage hosts) was refused.
    #[error("refused to contact {0}")]
    Refused(String),
    /// A minisign signature is missing, unreadable, made with another key, or does
    /// not match the file. Nothing was installed.
    #[error("invalid signature: {0}")]
    Signature(String),
    /// The archive's SHA-256 differs from the signed `SHA256SUMS`, or is not listed
    /// there. Nothing was installed.
    #[error("checksum mismatch: {0}")]
    Checksum(String),
    /// More bytes than announced, or more than the size limits allow.
    #[error("the download is larger than announced")]
    TooLarge,
    /// The caller cancelled the download.
    #[error("cancelled")]
    Cancelled,
    /// The verified archive does not have the expected content. Nothing was installed.
    #[error("not a Lumenwall release archive: {0}")]
    BadArchive(String),
    /// A file operation failed (the message says which). During installation, every
    /// file already replaced was put back.
    #[error("{0}")]
    Io(String),
    /// The programs were replaced, but the engine could not be restarted.
    #[error("the engine could not be restarted: {0}")]
    Restart(String),
}

impl Error {
    pub(crate) fn io(what: impl std::fmt::Display, e: &std::io::Error) -> Self {
        Error::Io(format!("{what}: {e}"))
    }
}

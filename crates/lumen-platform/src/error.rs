//! Errors.

/// Error raised while opening or running a backend.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// No display server could be identified from the environment.
    #[error(
        "cannot detect the display server: XDG_SESSION_TYPE, WAYLAND_DISPLAY and DISPLAY are all unset"
    )]
    NoDisplay,
    /// The backend needed for this session was not compiled in.
    #[error("no driver for this mode and session type ({0} support is not compiled in)")]
    BackendUnavailable(&'static str),
    /// Connecting to the display server failed.
    #[error("cannot connect to the display server: {0}")]
    Connect(String),
    /// A protocol or extension the backend cannot work without is missing.
    #[error("the display server lacks a required interface: {0}")]
    MissingInterface(&'static str),
    /// None of the outputs named on the command line exists (spec 07 §4.3).
    #[error(
        "none of the requested outputs was found (requested: {requested:?}, detected: {detected:?})"
    )]
    NoMatchingOutput {
        /// Names given by the user.
        requested: Vec<String>,
        /// Names detected on the system.
        detected: Vec<String>,
    },
    /// The Windows desktop window hierarchy could not be found (Explorer not running as shell?).
    #[error("cannot attach to the desktop window: {0}")]
    Desktop(String),
    /// The connection to the display server was lost.
    #[error("connection to the display server lost: {0}")]
    ConnectionLost(String),
    /// Other OS failure.
    #[error("{0}")]
    Os(String),
}

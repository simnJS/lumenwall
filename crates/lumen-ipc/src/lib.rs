//! IPC protocol between the Lumen daemon and its clients (UI, tray, command line).
//!
//! # Transport
//!
//! A local socket: a Unix domain socket file on Linux (`$XDG_RUNTIME_DIR/lumen/ipc.sock`),
//! a named pipe on Windows (`\\.\pipe\lumen-ipc-<user>`). See [`Endpoint`].
//!
//! # Framing and format
//!
//! One JSON object per line (`\n`), at most [`MAX_FRAME_LEN`] bytes:
//!
//! ```text
//! -> {"id":1,"method":"hello","params":{"protocol":1,"client":"lumen-ui"}}
//! <- {"reply":{"id":1,"ok":{"type":"welcome","protocol":1,"server":"lumen","version":"0.1.0"}}}
//! -> {"id":2,"method":"subscribe","params":{"topics":["state"]}}
//! <- {"reply":{"id":2,"ok":{"type":"done"}}}
//! -> {"id":3,"method":"set_paused","params":{"paused":true}}
//! <- {"reply":{"id":3,"ok":{"type":"done"}}}
//! <- {"event":{"type":"state","paused":true,"pause_reason":"user","muted":false,"volume":0.5,"outputs":[]}}
//! ```
//!
//! JSON rather than a binary format: messages are rare (a few per user action, one
//! stats event per second while a client asks for it) and under 1 KiB, so encoding
//! costs microseconds either way, while JSON keeps the protocol scriptable
//! (`socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/lumen/ipc.sock`) and lets old and new
//! builds ignore each other's extra fields. See `proto` for the compatibility rules.
//!
//! # Cost
//!
//! Nothing polls. The daemon's accept thread and each connection's threads sleep in
//! blocking system calls; events are serialized only when a client subscribed to them.

mod client;
mod proto;
mod server;
#[cfg(test)]
mod tests;
mod transport;

pub use client::{Client, ClientEvent, DEFAULT_TIMEOUT};
pub use proto::*;
pub use server::{Handler, Publisher, Server};
pub use transport::{ENDPOINT_ENV, Endpoint};

/// Errors seen by clients and by [`Server::bind`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No daemon listens on the endpoint.
    #[error("the Lumen daemon is not running")]
    NotRunning,
    /// [`Server::bind`]: another daemon already listens on the endpoint.
    #[error("another Lumen daemon is already running")]
    AlreadyRunning,
    #[error("incompatible daemon: {0}")]
    VersionMismatch(String),
    #[error("the daemon did not answer in time")]
    Timeout,
    #[error("the connection to the daemon was closed")]
    Disconnected,
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The daemon refused the request; the connection is still usable.
    #[error("daemon error: {0}")]
    Remote(#[from] RemoteError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

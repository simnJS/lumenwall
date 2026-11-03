//! Linux backends. Both run on a `calloop` event loop: the thread sleeps in `epoll` until the display
//! connection, the waker or the earliest frame deadline needs it.

#[cfg(feature = "x11")]
pub(crate) mod gnome_shell;
#[cfg(feature = "wayland")]
pub(crate) mod wayland;
#[cfg(feature = "x11")]
pub(crate) mod x11;

use crate::PlatformError;

pub(crate) fn loop_error(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Os(format!("event loop: {e}"))
}

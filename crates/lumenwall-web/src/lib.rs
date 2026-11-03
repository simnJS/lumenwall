//! Web wallpapers rendered offscreen.
//!
//! - [`bridge`], [`session`], [`properties`], [`audio`], [`media`]: the public Wallpaper Engine
//!   web API (JavaScript bridge injected before page scripts, and the host-side state that
//!   feeds it). Independent of the browser engine.
//! - [`files`], [`mime`], [`access`]: the private `wp<id>://` scheme serving the wallpaper folder,
//!   and the `file://` allow-list.
//! - [`ipc`], [`process`]: the separate host process and the channel to it, including the shared
//!   GPU frame exchange ([`ipc::frame::SharedFrame`]) imported by `lumenwall-render`.
//! - `cef_host` (feature `cef`): the Chromium Embedded Framework host.
//!
//! Specs: `specs/07-plateforme-audio-video-web.md` §3.5, §8.2, §10.4, §15, QO-17…QO-21; public
//! Wallpaper Engine documentation for the web API. See README.md.

pub mod access;
pub mod audio;
pub mod bridge;
pub mod files;
pub mod ipc;
pub mod media;
pub mod mime;
pub mod mouse;
pub mod process;
pub mod properties;
pub mod session;

#[cfg(feature = "cef")]
pub mod cef_host;

pub use bridge::BRIDGE_JS;
pub use files::{FileService, initial_url, scheme_name};
pub use ipc::frame::{PixelFormat, SharedFrame, SharedTexture};
pub use ipc::{AudioOutput, FramePacing, HostCommand, LaunchParams};
pub use process::{WebEvent, WebHost};
pub use properties::{PropertySet, PropertyValue, UserProperty};

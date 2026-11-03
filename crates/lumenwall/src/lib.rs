//! Lumenwall application layer: command line, configuration file, execution plan, logging and
//! process lifecycle (spec 07 §1–§4.3, §17, §18.1–§18.2, §19, §20).
//!
//! The flow is `cli::parse` → `config::load` → `plan::build` → [`services::Engine`]. The
//! plan is plain data, built without touching the OS: wallpaper lookups and the engine are
//! traits implemented by the other crates.

pub mod app;
pub mod cli;
pub mod config;
pub mod console;
pub mod daemon;
pub mod logging;
pub mod plan;
pub mod playlist;
pub mod screenshot;
pub mod services;
pub mod settings;
pub mod signals;
pub mod watch;

//! Web host process: one offscreen Chromium browser per web wallpaper, started by lumenwall.
//! Also runs as every CEF subprocess (`--type=…`).

// No console window for the host or the CEF subprocesses it spawns; the IPC pipes are passed
// explicitly by lumenwall.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    lumenwall_web::cef_host::run()
}

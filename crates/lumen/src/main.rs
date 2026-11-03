// Release builds on Windows are GUI-subsystem programs: no console window opens at login.
// Logs go to a rotated file; a parent terminal is reattached when there is one.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, mpsc};

use lumen::app::{self, Console, Services};
use lumen::plan::ExecutionPlan;
use lumen::playlist::{PathStyle, RealFs, WeConfigCatalog};
use lumen::services::{Control, Engine, ServiceError, StopReason, Waker, WallpaperResolver};
use lumen::settings::WallpaperSource;

fn main() -> ExitCode {
    let attached = lumen::console::prepare();
    let services = Services {
        resolver: Arc::new(PendingResolver),
        playlists: Arc::new(WeConfigCatalog::new(
            || Err("locating the Wallpaper Engine installation is not integrated yet".into()),
            RealFs,
            PathStyle::native(),
        )),
        engine: Box::new(PendingEngine),
    };
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let console = Console {
        out: &mut out,
        err: &mut err,
        attached,
    };
    app::run(std::env::args_os().collect(), services, console).into()
}

// Placeholders until lumen-assets and the engine crates are wired in.

struct PendingResolver;

impl WallpaperResolver for PendingResolver {
    fn resolve(&self, source: &WallpaperSource) -> Result<PathBuf, ServiceError> {
        match source {
            WallpaperSource::Path(path) => Ok(path.clone()),
            WallpaperSource::WorkshopId(id) => Err(format!(
                "cannot look up Workshop item '{id}': Steam lookup is not integrated yet"
            )
            .into()),
        }
    }
}

struct NoWaker;

impl Waker for NoWaker {
    fn wake(&self) {}
}

struct PendingEngine;

impl Engine for PendingEngine {
    fn waker(&self) -> Arc<dyn Waker> {
        Arc::new(NoWaker)
    }

    fn load(&mut self, _plan: &ExecutionPlan) -> Result<(), ServiceError> {
        Err("the rendering engine is not integrated yet".into())
    }

    fn write_properties(&mut self, _out: &mut dyn std::io::Write) -> Result<(), ServiceError> {
        Err("the rendering engine is not integrated yet".into())
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        Err("the rendering engine is not integrated yet".into())
    }

    fn write_structure(&mut self, _out: &mut dyn std::io::Write) -> Result<(), ServiceError> {
        Err("the rendering engine is not integrated yet".into())
    }

    fn run(&mut self, _control: mpsc::Receiver<Control>) -> Result<StopReason, ServiceError> {
        Err("the rendering engine is not integrated yet".into())
    }
}

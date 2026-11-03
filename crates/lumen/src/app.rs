//! Process lifecycle (spec 07 §1): parse, report, load, run, stop, exit code.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use crate::cli::{self, CliArgs, CliOutcome};
use crate::config::{self, LoadedConfig};
use crate::daemon::{Daemon, DaemonInit};
use crate::logging::{self, LogOptions};
use crate::plan::{self, PlanServices};
use crate::services::{self, Engine, PlaylistCatalog, WallpaperResolver};
use crate::settings::{Scaling, WallpaperRef};
use crate::signals;
use crate::watch::{self, FileWatcher};

/// Process exit status (spec 07 §1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Normal stop, `--help`, `--version`, `--list-properties`.
    Success = 0,
    /// Any fatal error.
    Failure = 1,
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        ExitCode::from(exit as u8)
    }
}

/// The subsystems the application drives.
pub struct Services {
    pub resolver: Arc<dyn WallpaperResolver>,
    pub playlists: Arc<dyn PlaylistCatalog>,
    pub engine: Box<dyn Engine>,
    /// IPC endpoint of the daemon (`None`: no IPC, no single-instance check).
    pub ipc: Option<lumen_ipc::Endpoint>,
}

/// Where user-facing text goes: help, "running with", property listing, argument errors.
pub struct Console<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    /// Whether stdout/stderr lead anywhere (false for a Windows GUI process without
    /// console); logs then only go to the log file.
    pub attached: bool,
}

/// Runs Lumen with `args` (`argv[0]` first).
pub fn run(args: Vec<OsString>, services: Services, console: Console<'_>) -> Exit {
    let argv0 = args
        .first()
        .map_or_else(|| "lumen".into(), |a| a.to_string_lossy().into_owned());
    let cli = match cli::parse(args) {
        Ok(CliOutcome::Help(text) | CliOutcome::Version(text)) => {
            let _ = console.out.write_all(text.as_bytes());
            let _ = console.out.flush();
            return Exit::Success;
        }
        Ok(CliOutcome::Run(cli)) => cli,
        Err(e) => {
            let _ = writeln!(console.err, "error: {e}\nRun '{argv0} --help' for usage.");
            if !console.attached {
                // Windows GUI process without console: keep a trace in the log file.
                let _logging = logging::init(&LogOptions::default());
                tracing::error!("invalid command line: {e}");
            }
            return Exit::Failure;
        }
    };

    // §1.1 step 2: unconditional, even with logging disabled.
    let command_line = shown_command_line(&cli.raw);
    let _ = writeln!(console.out, "Running with: {command_line}");

    let loaded = config::load(&cli.config);
    let log_config = loaded
        .as_ref()
        .ok()
        .and_then(|l| l.config.as_ref())
        .map(|c| c.log.clone());
    let logging = logging::init(&LogOptions {
        console: console.attached,
        config: log_config.unwrap_or_default(),
    });
    tracing::info!(target: logging::CMDLINE_TARGET, "running with: {command_line}");
    if let Some(dir) = &logging.file_dir {
        tracing::debug!("log files in {}", dir.display());
    }
    if !cli.ignored.is_empty() {
        tracing::debug!("ignored unknown arguments: {}", cli.ignored.join(" "));
    }
    let loaded = match loaded {
        Ok(loaded) => loaded,
        Err(e) => return fatal(e),
    };
    if let (Some(path), Some(_)) = (&loaded.path, &loaded.config) {
        tracing::info!("configuration read from {}", path.display());
    }

    // Single instance: a second `lumen` hands its wallpaper to the running daemon.
    if !cli.settings.list_properties
        && let Some(endpoint) = &services.ipc
        && let Ok(client) = lumen_ipc::Client::connect(endpoint, "lumen")
    {
        return forward(&cli, &client, &*services.resolver, console.out);
    }

    let plan_services = PlanServices {
        resolver: &*services.resolver,
        playlists: &*services.playlists,
    };
    // `lumen` alone with nothing configured starts as an idle daemon, waiting for a client.
    let idle_allowed = cli.raw.len() <= 1;
    let plan = match plan::build_with(&cli, loaded.config.as_ref(), plan_services, idle_allowed) {
        Ok(plan) => plan,
        Err(e) => return fatal(e),
    };
    if let Some(dir) = &plan.assets_dir {
        tracing::info!("using assets from {}", dir.display());
    }
    for line in plan.to_string().lines() {
        tracing::info!("{line}");
    }

    let mut engine = services.engine;
    if let Err(e) = engine.load(&plan) {
        return fatal(e);
    }
    if plan.list_properties {
        return match engine.write_properties(console.out) {
            Ok(()) => Exit::Success,
            Err(e) => fatal(e),
        };
    }

    let (control, receiver) = services::control_channel(engine.waker());
    let _signals = signals::install(control.clone())
        .inspect_err(|e| tracing::warn!("cannot install the stop handlers: {e}"))
        .ok();
    let plan = Arc::new(plan);
    let daemon = Daemon::new(DaemonInit {
        cli,
        config_path: loaded.path.clone(),
        config_bytes: loaded.bytes.clone(),
        config: loaded.config.clone(),
        plan: plan.clone(),
        resolver: services.resolver.clone(),
        playlists: services.playlists.clone(),
        control,
        monitor: engine.monitor(),
        log_filter: logging.filter_handle(),
        log_config: loaded
            .config
            .as_ref()
            .map(|c| c.log.clone())
            .unwrap_or_default(),
    });
    let server = match &services.ipc {
        None => None,
        Some(endpoint) => {
            match lumen_ipc::Server::bind(
                endpoint,
                daemon.handler(),
                "lumen",
                env!("CARGO_PKG_VERSION"),
            ) {
                Ok(server) => {
                    daemon.set_publisher(server.publisher());
                    Some(server)
                }
                Err(lumen_ipc::Error::AlreadyRunning) => {
                    // Lost a start-up race with another daemon.
                    let _ = writeln!(console.out, "Lumen is already running");
                    return Exit::Success;
                }
                Err(e) => {
                    tracing::warn!("IPC unavailable ({e}): clients cannot control this instance");
                    None
                }
            }
        }
    };
    let _watcher = watch_config(&loaded, &daemon);

    if let Err(e) = engine.start() {
        daemon.detach();
        return fatal(e);
    }
    if plan.dump_structure
        && let Err(e) = engine.write_structure(console.out)
    {
        daemon.detach();
        return fatal(e);
    }
    let result = engine.run(receiver);
    daemon.detach();
    drop(server);
    match result {
        Ok(reason) => {
            tracing::info!("{reason}");
            tracing::info!("stopping");
            Exit::Success
        }
        Err(e) => fatal(e),
    }
}

/// Another daemon runs: send it the wallpaper named on the command line, if any.
fn forward(
    cli: &CliArgs,
    client: &lumen_ipc::Client,
    resolver: &dyn WallpaperResolver,
    out: &mut dyn Write,
) -> Exit {
    let scaling = |s: Option<Scaling>| match s {
        Some(Scaling::Fit) => lumen_ipc::Scaling::Fit,
        Some(Scaling::Stretch) => lumen_ipc::Scaling::Stretch,
        _ => lumen_ipc::Scaling::Fill,
    };
    let mut requests: Vec<(Option<String>, &WallpaperRef, lumen_ipc::Scaling)> = cli
        .layout
        .screens
        .iter()
        .filter_map(|s| {
            let w = s.settings.wallpaper.as_ref()?;
            Some((Some(s.name.clone()), w, scaling(s.settings.scaling)))
        })
        .collect();
    if requests.is_empty()
        && let Some(w) = &cli.layout.default_wallpaper
    {
        requests.push((None, w, scaling(cli.layout.global_scaling)));
    }
    if requests.is_empty() {
        let _ = writeln!(out, "Lumen is already running");
        return Exit::Success;
    }
    for (output, reference, scaling) in requests {
        let WallpaperRef::Source(source) = reference else {
            tracing::warn!("playlists are not sent to the running daemon");
            continue;
        };
        let path = match resolver.resolve(source) {
            Ok(p) => p,
            Err(e) => return fatal(e),
        };
        if let Err(e) = client.apply(output.as_deref(), &path, scaling) {
            return fatal(format!(
                "the running daemon refused {}: {e}",
                path.display()
            ));
        }
    }
    let _ = writeln!(out, "Lumen is already running: wallpaper sent to it");
    Exit::Success
}

fn fatal(error: impl std::fmt::Display) -> Exit {
    tracing::error!("{error}");
    Exit::Failure
}

fn shown_command_line(raw: &[OsString]) -> String {
    raw.iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn watch_config(loaded: &LoadedConfig, daemon: &Arc<Daemon>) -> Option<FileWatcher> {
    let path: PathBuf = loaded.path.clone()?;
    let watched = path.clone();
    let daemon = Arc::downgrade(daemon);
    match watch::watch_file(&path, loaded.bytes.clone(), move |change| {
        if let Some(d) = daemon.upgrade() {
            d.on_config_change(&watched, change);
        }
    }) {
        Ok(watcher) => {
            tracing::debug!("watching {} for changes", path.display());
            Some(watcher)
        }
        Err(e) => {
            tracing::debug!("not watching {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests;

//! Process lifecycle (spec 07 §1): parse, report, load, run, stop, exit code.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use crate::cli::{self, CliArgs, CliOutcome};
use crate::config::{self, LoadedConfig};
use crate::logging::{self, LogFilterHandle, LogOptions};
use crate::plan::{self, ExecutionPlan, PlanServices};
use crate::services::{self, Control, ControlSender, Engine, PlaylistCatalog, WallpaperResolver};
use crate::signals;
use crate::watch::{self, FileChange, FileWatcher};

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

    let plan_services = PlanServices {
        resolver: &*services.resolver,
        playlists: &*services.playlists,
    };
    let plan = match plan::build(&cli, loaded.config.as_ref(), plan_services) {
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
    let _watcher = watch_config(
        &loaded,
        Reloader {
            cli,
            resolver: services.resolver.clone(),
            playlists: services.playlists.clone(),
            current: plan.clone(),
            control,
            log_filter: logging.filter_handle(),
            log_config: loaded
                .config
                .as_ref()
                .map(|c| c.log.clone())
                .unwrap_or_default(),
        },
    );

    if let Err(e) = engine.start() {
        return fatal(e);
    }
    if plan.dump_structure
        && let Err(e) = engine.write_structure(console.out)
    {
        return fatal(e);
    }
    match engine.run(receiver) {
        Ok(reason) => {
            tracing::info!("{reason}");
            tracing::info!("stopping");
            Exit::Success
        }
        Err(e) => fatal(e),
    }
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

/// Rebuilds the plan when the configuration file changes.
struct Reloader {
    cli: Box<CliArgs>,
    resolver: Arc<dyn WallpaperResolver>,
    playlists: Arc<dyn PlaylistCatalog>,
    current: Arc<ExecutionPlan>,
    control: ControlSender,
    log_filter: Option<LogFilterHandle>,
    log_config: config::LogConfig,
}

impl Reloader {
    fn on_change(&mut self, path: &std::path::Path, change: FileChange) {
        let config = match change {
            FileChange::Contents(bytes) => match config::parse_bytes(path, &bytes) {
                Ok(config) => Some(config),
                Err(e) => {
                    tracing::error!("{e}; keeping the current settings");
                    return;
                }
            },
            FileChange::Removed => {
                tracing::info!(
                    "configuration file {} removed; using the defaults",
                    path.display()
                );
                None
            }
            FileChange::Unreadable(e) => {
                tracing::warn!("cannot read the configuration file {}: {e}", path.display());
                return;
            }
        };
        let log = config.as_ref().map(|c| c.log.clone()).unwrap_or_default();
        if log.level != self.log_config.level
            && let Some(filter) = &self.log_filter
        {
            filter.set_level(log.level.as_deref());
        }
        if (&log.file, &log.directory, &log.max_files)
            != (
                &self.log_config.file,
                &self.log_config.directory,
                &self.log_config.max_files,
            )
        {
            tracing::warn!("log file settings change on the next start");
        }
        self.log_config = log;

        let services = PlanServices {
            resolver: &*self.resolver,
            playlists: &*self.playlists,
        };
        match plan::build(&self.cli, config.as_ref(), services) {
            Ok(plan) => {
                let changes = plan::diff(&self.current, &plan);
                if changes.is_empty() {
                    tracing::debug!("configuration reloaded, nothing to change");
                    return;
                }
                tracing::info!("configuration reloaded: {changes:?}");
                self.current = Arc::new(plan);
                self.control.send(Control::Reload {
                    plan: self.current.clone(),
                    changes,
                });
            }
            Err(e) => tracing::error!("configuration not applied: {e}"),
        }
    }
}

fn watch_config(loaded: &LoadedConfig, mut reloader: Reloader) -> Option<FileWatcher> {
    let path: PathBuf = loaded.path.clone()?;
    let watched = path.clone();
    match watch::watch_file(&path, loaded.bytes.clone(), move |change| {
        reloader.on_change(&watched, change)
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

//! Logging (spec 07 §19): information on stdout, warnings and errors on stderr, one line per
//! message; optionally a daily-rotated file (on by default on Windows, where release builds
//! have no console).
//!
//! Filter precedence: `LUMENWALL_LOG`, `RUST_LOG`, `[log] level` of the configuration file,
//! then `info` (`info,lumen=debug` in debug builds: debug messages are for debug builds).
//! The file's level is reloaded with the configuration.

use std::io::{self, IsTerminal};
use std::path::PathBuf;

use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt, reload};

use crate::config::LogConfig;

/// Target of the "running with" record: it already went to stdout unconditionally, so only
/// the log file receives it.
pub const CMDLINE_TARGET: &str = "lumenwall::cmdline";

const DEFAULT_FILTER: &str = if cfg!(debug_assertions) {
    "info,lumen=debug"
} else {
    "info"
};
const ENV_VARS: [&str; 2] = ["LUMENWALL_LOG", "RUST_LOG"];
const DEFAULT_MAX_FILES: usize = 7;

#[derive(Clone, Debug, Default)]
pub struct LogOptions {
    /// Whether stdout/stderr lead somewhere (false for a Windows GUI process without console).
    pub console: bool,
    pub config: LogConfig,
}

/// Keeps the file writer alive (flushes on drop) and allows changing the filter.
#[must_use = "dropping the guard stops the log file writer"]
pub struct Logging {
    _file_guard: Option<WorkerGuard>,
    filter: Option<LogFilterHandle>,
    /// Folder of the log files, if any.
    pub file_dir: Option<PathBuf>,
}

impl Logging {
    pub fn filter_handle(&self) -> Option<LogFilterHandle> {
        self.filter.clone()
    }
}

/// Changes the filter at run time (configuration reload).
#[derive(Clone)]
pub struct LogFilterHandle {
    handle: reload::Handle<EnvFilter, Registry>,
    /// Set by an environment variable, which wins over the configuration file.
    from_env: bool,
}

impl LogFilterHandle {
    /// Applies the configuration file's level (`None`: default). No effect when an
    /// environment variable sets the filter.
    pub fn set_level(&self, level: Option<&str>) {
        if self.from_env {
            return;
        }
        let directives = level.unwrap_or(DEFAULT_FILTER);
        match EnvFilter::try_new(directives) {
            Ok(filter) => {
                if let Err(e) = self.handle.reload(filter) {
                    tracing::warn!("cannot change the log filter: {e}");
                }
            }
            Err(e) => tracing::warn!("invalid log level '{directives}': {e}"),
        }
    }
}

/// Returns the filter and whether it comes from the environment.
fn initial_filter(config_level: Option<&str>) -> (EnvFilter, bool) {
    for var in ENV_VARS {
        if let Ok(value) = std::env::var(var)
            && !value.is_empty()
        {
            match EnvFilter::try_new(&value) {
                Ok(filter) => return (filter, true),
                Err(e) => eprintln!("warning: ignoring invalid {var}='{value}': {e}"),
            }
        }
    }
    let directives = config_level.unwrap_or(DEFAULT_FILTER);
    let filter = EnvFilter::try_new(directives).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    (filter, false)
}

/// Default folder of the log files: `%LOCALAPPDATA%\Lumenwall\logs` on Windows,
/// `$XDG_STATE_HOME/lumenwall/logs` (`~/.local/state/lumenwall/logs`) elsewhere.
pub fn default_log_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs::data_local_dir().map(|d| d.join("Lumenwall").join("logs"))
    } else {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .map(|d| d.join("lumenwall").join("logs"))
    }
}

fn file_writer(
    config: &LogConfig,
) -> Option<(
    tracing_appender::non_blocking::NonBlocking,
    WorkerGuard,
    PathBuf,
)> {
    if !config.file.unwrap_or(cfg!(windows)) {
        return None;
    }
    let dir = config.directory.clone().or_else(default_log_dir)?;
    let appender = std::fs::create_dir_all(&dir)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix("lumenwall")
                .filename_suffix("log")
                .max_log_files(config.max_files.unwrap_or(DEFAULT_MAX_FILES).max(1))
                .build(&dir)
                .map_err(|e| e.to_string())
        });
    match appender {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            Some((writer, guard, dir))
        }
        Err(e) => {
            eprintln!("warning: cannot write logs to {}: {e}", dir.display());
            None
        }
    }
}

/// Installs the global subscriber. If one is already installed (tests), the existing one is
/// kept and the returned handle has no filter control.
pub fn init(options: &LogOptions) -> Logging {
    let (filter, from_env) = initial_filter(options.config.level.as_deref());
    let (filter, handle) = reload::Layer::new(filter);

    let console = options.console;
    let stdout = console.then(|| {
        fmt::layer()
            .with_writer(io::stdout)
            .with_ansi(io::stdout().is_terminal())
            .with_target(false)
            .with_filter(filter_fn(|meta| {
                *meta.level() > Level::WARN && meta.target() != CMDLINE_TARGET
            }))
    });
    let stderr = console.then(|| {
        fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(io::stderr().is_terminal())
            .with_target(false)
            .with_filter(filter_fn(|meta| *meta.level() <= Level::WARN))
    });
    let (file, guard, file_dir) = match file_writer(&options.config) {
        Some((writer, guard, dir)) => (
            Some(fmt::layer().with_writer(writer).with_ansi(false)),
            Some(guard),
            Some(dir),
        ),
        None => (None, None, None),
    };

    let installed = tracing_subscriber::registry()
        .with(filter)
        .with(stdout)
        .with(stderr)
        .with(file)
        .try_init();
    Logging {
        _file_guard: guard,
        filter: installed
            .is_ok()
            .then_some(LogFilterHandle { handle, from_env }),
        file_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_filter_parses() {
        assert!(EnvFilter::try_new(DEFAULT_FILTER).is_ok());
    }

    #[test]
    fn writes_a_log_file_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let config = LogConfig {
            file: Some(true),
            directory: Some(dir.path().join("logs")),
            ..LogConfig::default()
        };
        let (mut writer, guard, path) = file_writer(&config).unwrap();
        io::Write::write_all(&mut writer, b"hello\n").unwrap();
        drop(guard);
        let files: Vec<_> = std::fs::read_dir(&path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1);
        let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("lumenwall") && name.ends_with(".log"),
            "{name}"
        );
        assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), "hello\n");
    }

    #[test]
    fn no_log_file_when_disabled() {
        let config = LogConfig {
            file: Some(false),
            ..LogConfig::default()
        };
        assert!(file_writer(&config).is_none());
    }
}

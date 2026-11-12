//! Structured logging: human-readable on stderr, JSON lines in a daily file.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

use crate::config::LoggingConfig;

/// Overrides the configured level, using `tracing` filter syntax
/// (for example `RC_LOG=rc_core=trace,info`).
pub const LOG_FILTER_ENV: &str = "RC_LOG";

pub const LOG_FILE_PREFIX: &str = "remote-control";

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("cannot create log directory {}: {source}", .path.display())]
    CreateDir {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("cannot open log file: {0}")]
    Appender(#[from] tracing_appender::rolling::InitError),
    #[error("logging is already initialised: {0}")]
    AlreadyInitialised(#[from] tracing_subscriber::util::TryInitError),
}

/// Keeps the background log writer alive. Buffered lines are flushed when it
/// is dropped, so hold it until the process exits.
#[must_use = "dropping the guard stops file logging"]
pub struct LogGuard {
    _file: Option<WorkerGuard>,
}

/// Installs the global subscriber. Call once, early in `main`.
pub fn init(config: &LoggingConfig, log_dir: &Path) -> Result<LogGuard, LoggingError> {
    let filter = EnvFilter::try_from_env(LOG_FILTER_ENV)
        .unwrap_or_else(|_| EnvFilter::new(config.level.as_str()));

    let (file_layer, guard) = if config.file_enabled {
        std::fs::create_dir_all(log_dir).map_err(|source| LoggingError::CreateDir {
            path: log_dir.to_path_buf(),
            source,
        })?;
        let appender = RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(LOG_FILE_PREFIX)
            .filename_suffix("log")
            .max_log_files(config.retention_days as usize)
            .build(log_dir)?;
        let (writer, guard) = tracing_appender::non_blocking(appender);
        let layer = fmt::layer().json().with_ansi(false).with_writer(writer);
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .try_init()?;

    Ok(LogGuard { _file: guard })
}

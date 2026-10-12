//! `tracing` initialisation: daily-rolling files in `<data>/logs/` plus optional
//! console output (`-console` / `-verbose`).

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

use crate::error::{CoreError, Result};

/// Logging configuration.
#[derive(Debug, Clone)]
pub struct LogConfig<'a> {
    /// Directory for rolling log files; `None` disables file logging.
    pub dir: Option<&'a Path>,
    /// File name prefix (`fennec` → `fennec.2026-10-12.log`).
    pub prefix: &'a str,
    /// Default level filter (`info`); `FENNEC_LOG` overrides it.
    pub level: &'a str,
    /// Number of rotated files to keep.
    pub max_files: usize,
    /// Also log to stderr.
    pub console: bool,
}

/// Keeps the background writer alive; drop it at shutdown to flush.
#[must_use = "dropping the guard stops file logging"]
pub struct LogGuard {
    _file: Option<WorkerGuard>,
}

/// Installs the global subscriber. Fails if one is already installed.
pub fn init(cfg: &LogConfig<'_>) -> Result<LogGuard> {
    let filter = || {
        EnvFilter::try_from_env("FENNEC_LOG")
            .or_else(|_| EnvFilter::try_new(cfg.level))
            .unwrap_or_else(|_| EnvFilter::new("info"))
    };

    let (file_layer, guard) = match cfg.dir {
        Some(dir) => {
            std::fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
            let appender = Builder::new()
                .rotation(Rotation::DAILY)
                .filename_prefix(cfg.prefix)
                .filename_suffix("log")
                .max_log_files(cfg.max_files.max(1))
                .build(dir)
                .map_err(|e| CoreError::Logging(e.to_string()))?;
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = fmt::layer().with_writer(writer).with_ansi(false).with_filter(filter());
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };
    let console_layer = cfg.console.then(|| fmt::layer().with_writer(std::io::stderr).with_filter(filter()));

    tracing_subscriber::registry()
        .with(file_layer)
        .with(console_layer)
        .try_init()
        .map_err(|e| CoreError::Logging(e.to_string()))?;
    Ok(LogGuard { _file: guard })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_rolling_file() {
        let dir = tempfile::tempdir().unwrap();
        let guard =
            init(&LogConfig { dir: Some(dir.path()), prefix: "test", level: "info", max_files: 2, console: false })
                .unwrap();
        tracing::info!("hello from the log test");
        drop(guard);
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let name = files[0].file_name().to_string_lossy().to_string();
        assert!(name.starts_with("test.") && name.ends_with(".log"), "{name}");
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(text.contains("hello from the log test"));
    }
}

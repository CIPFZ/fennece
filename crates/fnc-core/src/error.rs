//! Error type shared by the core crate.

use std::path::PathBuf;

/// Errors produced by `fnc-core`.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// I/O failure on a specific path.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path that was being accessed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A settings file could not be parsed.
    #[error("invalid settings file {path}: {message}")]
    SettingsParse {
        /// Path of the settings file.
        path: PathBuf,
        /// Parser message.
        message: String,
    },
    /// A setting could not be serialised.
    #[error("cannot serialise settings: {0}")]
    SettingsSerialize(String),
    /// A string exceeded the arena's per-item length limit.
    #[error("string of {0} units exceeds the arena limit of 65535")]
    StringTooLong(usize),
    /// The arena grew beyond its 32-bit offset space.
    #[error("string arena exhausted (4 Gi units)")]
    ArenaFull,
    /// Logging could not be initialised.
    #[error("logging initialisation failed: {0}")]
    Logging(String),
}

/// Convenience alias.
pub type Result<T, E = CoreError> = std::result::Result<T, E>;

impl CoreError {
    /// Wraps an [`std::io::Error`] with the path it happened on.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }
}

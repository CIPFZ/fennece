//! Error type.

/// Errors produced by `fnc-index`.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// Core error (string arena limits).
    #[error(transparent)]
    Core(#[from] fnc_core::CoreError),
    /// More than 2^32-1 entries.
    #[error("index is full (2^32 entries)")]
    Full,
    /// More than 65535 volumes.
    #[error("too many volumes")]
    TooManyVolumes,
    /// A volume has no root entry.
    #[error("volume has no root entry")]
    NoRoot,
    /// I/O error.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path.
        path: std::path::PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// Malformed file list.
    #[error("{path}:{line}: {message}")]
    FileList {
        /// File.
        path: std::path::PathBuf,
        /// 1-based line.
        line: usize,
        /// Problem.
        message: String,
    },
}

/// Convenience alias.
pub type Result<T, E = IndexError> = std::result::Result<T, E>;

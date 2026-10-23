//! Error types shared across Nova.

use std::path::PathBuf;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Everything that can go wrong while scanning a library or driving playback.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Wraps a lower level I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// A file could not be parsed as audio or its tags could not be read.
    #[error("could not read metadata for {path}: {message}")]
    Metadata {
        /// File that failed.
        path: PathBuf,
        /// Human readable reason.
        message: String,
    },

    /// The audio backend rejected a command (no device, unsupported seek, ...).
    #[error("audio error: {0}")]
    Audio(String),

    /// JSON (de)serialization of the library cache failed.
    #[error("cache error: {0}")]
    Cache(#[from] serde_json::Error),
}

impl CoreError {
    /// Build a [`CoreError::Metadata`] from a path and any displayable error.
    pub fn metadata(path: impl Into<PathBuf>, err: impl std::fmt::Display) -> Self {
        CoreError::Metadata {
            path: path.into(),
            message: err.to_string(),
        }
    }
}

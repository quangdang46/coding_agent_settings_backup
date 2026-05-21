//! Error types for casb.
//!
//! Defines a single [`CasbError`] enum capturing every error condition that
//! can arise across the crate, plus a [`Result`] alias used by every fallible
//! function.

use std::path::PathBuf;

/// Top-level error type for casb operations.
#[derive(Debug, thiserror::Error)]
pub enum CasbError {
    /// IO error from the standard library.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML deserialization error.
    #[error("config parse error: {0}")]
    TomlDe(#[from] toml::de::Error),

    /// TOML serialization error.
    #[error("config serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    /// JSON serialization/deserialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// glob pattern compilation error.
    #[error("invalid glob pattern: {0}")]
    Glob(#[from] glob::PatternError),

    /// walkdir traversal error.
    #[error("walkdir error: {0}")]
    WalkDir(#[from] walkdir::Error),

    /// A git CLI invocation failed (non-zero exit).
    #[error("git command failed: {command}\n{stderr}")]
    GitCommand {
        /// Joined argv string of the git invocation.
        command: String,
        /// Captured stderr.
        stderr: String,
    },

    /// A subprocess invocation failed (non-zero exit, non-git).
    #[error("command failed: {command}\n{stderr}")]
    CommandFailed {
        /// Joined argv string.
        command: String,
        /// Captured stderr.
        stderr: String,
    },

    /// The requested agent key does not exist.
    #[error("agent not found: {key}")]
    AgentNotFound {
        /// The agent key that was requested.
        key: String,
    },

    /// The agent has no installed source location on this system.
    #[error("agent '{key}' is not installed (no source location found)")]
    AgentNotInstalled {
        /// Agent key.
        key: String,
    },

    /// A required config value was missing or invalid.
    #[error("config error: {0}")]
    Config(String),

    /// A path was expected to exist but does not.
    #[error("path does not exist: {0}")]
    PathMissing(PathBuf),

    /// A backup repository was not found.
    #[error("no backup repository for agent '{key}' (run `casb backup {key}` first)")]
    NoBackupRepo {
        /// Agent key.
        key: String,
    },

    /// A user confirmation was declined.
    #[error("operation cancelled by user")]
    Cancelled,

    /// Generic invalid argument.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// Catch-all for descriptive errors.
    #[error("{0}")]
    Other(String),
}

/// Convenience alias for `Result<T, CasbError>`.
pub type Result<T> = std::result::Result<T, CasbError>;

impl CasbError {
    /// Build an [`Other`](CasbError::Other) error from any displayable value.
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

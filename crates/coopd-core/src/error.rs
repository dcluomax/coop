//! Error types for `coopd-core`.

use thiserror::Error;

/// Result alias for core errors.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Errors emitted by the core layer.
#[derive(Debug, Error)]
pub enum CoreError {
    /// An identifier failed validation.
    #[error("invalid identifier: {0}")]
    InvalidId(String),

    /// A manifest failed validation.
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    /// A state transition is illegal.
    #[error("invalid state transition: {from:?} -> {to:?}")]
    InvalidTransition {
        /// Source state.
        from: String,
        /// Target state.
        to: String,
    },

    /// A Hen could not be found.
    #[error("hen not found: {0}")]
    HenNotFound(String),

    /// YAML parsing failed.
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// JSON parsing failed.
    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// I/O error from upstream layer.
    #[error("io: {0}")]
    Io(String),

    /// Anything else.
    #[error("{0}")]
    Other(String),
}

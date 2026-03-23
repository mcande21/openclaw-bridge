//! Shared error types for the openclaw-bridge crate.

use thiserror::Error;

/// Common error type for bridge operations that don't use module-specific errors.
#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("SSH error: {0}")]
    Ssh(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("config directory error: {0}")]
    ConfigDir(String),
}


//! Errors for the core crate. I/O errors live in their respective crates.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("compression error: {0}")]
    Compression(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

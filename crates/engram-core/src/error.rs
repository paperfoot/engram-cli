//! Errors for the core crate. I/O errors live in their respective crates.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

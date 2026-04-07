use thiserror::Error;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("storage error: {0}")]
    Storage(#[from] engram_storage::StorageError),

    #[error("unsupported format: {0}")]
    Unsupported(String),

    #[error("invalid input: {0}")]
    Invalid(String),
}

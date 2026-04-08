use thiserror::Error;

#[derive(Debug, Error)]
pub enum BenchError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("dataset not downloaded — run `engram bench longmemeval --download`")]
    DatasetMissing,

    #[error("storage error: {0}")]
    Storage(#[from] engram_storage::StorageError),

    #[error("embed error: {0}")]
    Embed(#[from] engram_embed::EmbedError),

    #[error("rerank error: {0}")]
    Rerank(#[from] engram_rerank::RerankError),

    #[error("llm error: {0}")]
    Llm(#[from] engram_llm::LlmError),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("invalid dataset: {0}")]
    InvalidDataset(String),
}

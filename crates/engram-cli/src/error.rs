//! Semantic exit codes per the agent-cli-framework.

use serde_json::json;
use std::io::{IsTerminal, Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("transient error: {0}")]
    Transient(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("bad input: {0}")]
    BadInput(String),

    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("storage error: {0}")]
    Storage(#[from] engram_storage::StorageError),

    #[error("embed error: {0}")]
    Embed(#[from] engram_embed::EmbedError),

    #[error("rerank error: {0}")]
    Rerank(#[from] engram_rerank::RerankError),

    #[error("ingest error: {0}")]
    Ingest(#[from] engram_ingest::IngestError),

    #[error("bench error: {0}")]
    Bench(#[from] engram_bench::BenchError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        use CliError::*;
        match self {
            Transient(_) | Storage(_) | Io(_) | Serde(_) => 1,
            Config(_) => 2,
            BadInput(_) | Ingest(_) | Bench(_) => 3,
            RateLimited(_) => 4,
            Embed(e) => match e {
                engram_embed::EmbedError::MissingKey { .. } => 2,
                engram_embed::EmbedError::RateLimited { .. } => 4,
                _ => 1,
            },
            Rerank(e) => match e {
                engram_rerank::RerankError::MissingKey { .. } => 2,
                engram_rerank::RerankError::RateLimited { .. } => 4,
                _ => 1,
            },
        }
    }

    pub fn code(&self) -> &'static str {
        use CliError::*;
        match self {
            Transient(_) => "transient_error",
            Config(_) => "config_error",
            BadInput(_) => "bad_input",
            RateLimited(_) => "rate_limited",
            Storage(_) => "storage_error",
            Embed(_) => "embed_error",
            Rerank(_) => "rerank_error",
            Ingest(_) => "ingest_error",
            Bench(_) => "bench_error",
            Io(_) => "io_error",
            Serde(_) => "serde_error",
        }
    }

    pub fn suggestion(&self) -> Option<&'static str> {
        use CliError::*;
        match self {
            Config(_) => Some("Run `engram config check` to validate setup."),
            RateLimited(_) => Some("Wait a few seconds and retry; reduce concurrency."),
            BadInput(_) => Some("Run `engram --help` for argument syntax."),
            _ => None,
        }
    }

    /// Always emits to stderr — JSON when stderr is piped or stdout was JSON.
    pub fn emit_to_stderr(&self) {
        let want_json = !std::io::stderr().is_terminal();
        let mut err = std::io::stderr().lock();
        if want_json {
            let payload = json!({
                "version": "1",
                "status": "error",
                "error": {
                    "code": self.code(),
                    "message": self.to_string(),
                    "suggestion": self.suggestion(),
                    "exit_code": self.exit_code(),
                }
            });
            let _ = writeln!(err, "{}", payload);
        } else {
            let _ = writeln!(err, "error: {}", self);
            if let Some(s) = self.suggestion() {
                let _ = writeln!(err, "  hint: {}", s);
            }
        }
    }
}


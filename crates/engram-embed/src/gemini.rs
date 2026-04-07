//! Gemini Embed 2 client (gemini-embedding-001 / -002).
//!
//! Uses Google's `:embedContent` REST endpoint with the v1beta API.
//! Reads `GEMINI_API_KEY` or accepts an explicit key.

use crate::{Embedder, EmbedError, TaskMode};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

const DEFAULT_MODEL: &str = "gemini-embedding-001";
const DEFAULT_DIMS: usize = 768;
const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    dimensions: usize,
}

impl GeminiEmbedder {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .connect_timeout(std::time::Duration::from_secs(15))
                .user_agent(concat!("engram/", env!("CARGO_PKG_VERSION")))
                .pool_idle_timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("failed to build reqwest client"),
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_string(),
            dimensions: DEFAULT_DIMS,
        }
    }

    pub fn from_env() -> Result<Self, EmbedError> {
        let key = std::env::var("GEMINI_API_KEY")
            .map_err(|_| EmbedError::MissingKey { provider: "gemini" })?;
        Ok(Self::new(key))
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_dimensions(mut self, dims: usize) -> Self {
        self.dimensions = dims;
        self
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: String,
    content: Content<'a>,
    #[serde(rename = "taskType")]
    task_type: &'static str,
    #[serde(rename = "outputDimensionality", skip_serializing_if = "Option::is_none")]
    output_dimensionality: Option<usize>,
}

#[derive(Serialize)]
struct BatchEmbedRequest<'a> {
    requests: Vec<EmbedRequest<'a>>,
}

#[derive(Serialize)]
struct Content<'a> {
    parts: Vec<Part<'a>>,
}

#[derive(Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embedding: Embedding,
}

#[derive(Deserialize)]
struct BatchEmbedResponse {
    embeddings: Vec<Embedding>,
}

#[derive(Deserialize)]
struct Embedding {
    values: Vec<f32>,
}

fn task_type_str(mode: TaskMode) -> &'static str {
    match mode {
        TaskMode::RetrievalQuery => "RETRIEVAL_QUERY",
        TaskMode::RetrievalDocument => "RETRIEVAL_DOCUMENT",
    }
}

#[async_trait]
impl Embedder for GeminiEmbedder {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed_one(&self, text: &str, mode: TaskMode) -> Result<Vec<f32>, EmbedError> {
        let req = EmbedRequest {
            model: format!("models/{}", self.model),
            content: Content {
                parts: vec![Part { text }],
            },
            task_type: task_type_str(mode),
            output_dimensionality: Some(self.dimensions),
        };

        let url = format!(
            "{}/{}:embedContent?key={}",
            ENDPOINT, self.model, self.api_key
        );

        let resp = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| EmbedError::Http { provider: "gemini", source: e })?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(EmbedError::RateLimited { provider: "gemini" });
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(EmbedError::Api {
                provider: "gemini",
                message: format!("status {}: {}", status, body),
            });
        }

        let body: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| EmbedError::Http { provider: "gemini", source: e })?;
        Ok(body.embedding.values)
    }

    /// Override default loop with a real batch call. Gemini's
    /// `:batchEmbedContents` accepts up to 250 texts BUT enforces both a
    /// 2,048 input-token-per-text cap and a 20,000 token-per-request cap.
    /// We approximate tokens as `chars / 4` and pack each batch under 16,000
    /// estimated tokens for safety. Inputs longer than ~8,000 chars are
    /// truncated client-side so we don't even ship the bytes.
    async fn embed_batch(
        &self,
        texts: &[&str],
        mode: TaskMode,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        const MAX_CHARS_PER_TEXT: usize = 7800; // ~1950 tokens, under the 2048 cap
        const MAX_CHARS_PER_BATCH: usize = 60_000; // ~15000 tokens, under the 20K cap
        const MAX_TEXTS_PER_BATCH: usize = 100;

        let truncated: Vec<String> = texts
            .iter()
            .map(|t| {
                if t.len() <= MAX_CHARS_PER_TEXT {
                    (*t).to_string()
                } else {
                    // Truncate at a char boundary.
                    let mut end = MAX_CHARS_PER_TEXT;
                    while !t.is_char_boundary(end) && end > 0 {
                        end -= 1;
                    }
                    t[..end].to_string()
                }
            })
            .collect();

        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        let url = format!(
            "{}/{}:batchEmbedContents?key={}",
            ENDPOINT, self.model, self.api_key
        );

        let mut idx = 0usize;
        while idx < truncated.len() {
            // Pack the next batch under both limits.
            let mut end = idx;
            let mut batch_chars = 0usize;
            while end < truncated.len()
                && (end - idx) < MAX_TEXTS_PER_BATCH
                && batch_chars + truncated[end].len() <= MAX_CHARS_PER_BATCH
            {
                batch_chars += truncated[end].len();
                end += 1;
            }
            // Always include at least one text per batch (in case a single
            // text is bigger than the budget — we already truncated above).
            if end == idx {
                end = idx + 1;
            }

            let requests: Vec<EmbedRequest> = truncated[idx..end]
                .iter()
                .map(|t| EmbedRequest {
                    model: format!("models/{}", self.model),
                    content: Content { parts: vec![Part { text: t }] },
                    task_type: task_type_str(mode),
                    output_dimensionality: Some(self.dimensions),
                })
                .collect();
            let body = BatchEmbedRequest { requests };
            let resp = self
                .client
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| EmbedError::Http { provider: "gemini", source: e })?;
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(EmbedError::RateLimited { provider: "gemini" });
            }
            if !status.is_success() {
                let body_text = resp.text().await.unwrap_or_default();
                return Err(EmbedError::Api {
                    provider: "gemini",
                    message: format!("status {}: {}", status, body_text),
                });
            }
            let parsed: BatchEmbedResponse = resp
                .json()
                .await
                .map_err(|e| EmbedError::Http { provider: "gemini", source: e })?;
            for emb in parsed.embeddings {
                out.push(emb.values);
            }
            idx = end;
        }
        Ok(out)
    }
}

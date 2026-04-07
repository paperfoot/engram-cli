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
                .timeout(std::time::Duration::from_secs(30))
                .user_agent(concat!("engram/", env!("CARGO_PKG_VERSION")))
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

    /// Override default loop with a real batch call. Gemini accepts up to 250
    /// texts per `:batchEmbedContents` request.
    async fn embed_batch(
        &self,
        texts: &[&str],
        mode: TaskMode,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        const BATCH: usize = 100;
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        let url = format!(
            "{}/{}:batchEmbedContents?key={}",
            ENDPOINT, self.model, self.api_key
        );
        for chunk in texts.chunks(BATCH) {
            let requests: Vec<EmbedRequest> = chunk
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
                let body = resp.text().await.unwrap_or_default();
                return Err(EmbedError::Api {
                    provider: "gemini",
                    message: format!("status {}: {}", status, body),
                });
            }
            let parsed: BatchEmbedResponse = resp
                .json()
                .await
                .map_err(|e| EmbedError::Http { provider: "gemini", source: e })?;
            for emb in parsed.embeddings {
                out.push(emb.values);
            }
        }
        Ok(out)
    }
}

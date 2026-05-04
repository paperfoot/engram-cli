//! Gemini Embedding 2 client.
//!
//! Uses Google's `:embedContent` REST endpoint with the v1beta API.
//! Reads `GEMINI_API_KEY` or accepts an explicit key.
//!
//! Model override: set `GEMINI_EMBED_MODEL` to force a specific model name
//! (e.g. "gemini-embedding-001" to pin to the legacy stable version).

use crate::{EmbedError, Embedder, TaskMode};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MODEL: &str = "gemini-embedding-2";
pub const DEFAULT_DIMS: usize = 1536;
pub const PROMPT_FORMAT: &str = "gemini-embedding-2-v1";
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
        let mut e = Self::new(key);
        if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
            e = e.with_model(model);
        }
        Ok(e)
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_dimensions(mut self, dims: usize) -> Self {
        self.dimensions = dims;
        self
    }

    pub fn model_name(&self) -> &str {
        &self.model
    }
}

#[derive(Serialize)]
struct EmbedRequest {
    model: String,
    content: Content,
    #[serde(rename = "taskType", skip_serializing_if = "Option::is_none")]
    task_type: Option<&'static str>,
    #[serde(
        rename = "outputDimensionality",
        skip_serializing_if = "Option::is_none"
    )]
    output_dimensionality: Option<usize>,
}

#[derive(Serialize)]
struct BatchEmbedRequest {
    requests: Vec<EmbedRequest>,
}

#[derive(Serialize)]
struct Content {
    parts: Vec<Part>,
}

#[derive(Serialize)]
struct Part {
    text: String,
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

fn supports_task_type(model: &str) -> bool {
    !model.starts_with("gemini-embedding-2")
}

fn format_input(text: &str, mode: TaskMode) -> String {
    match mode {
        TaskMode::RetrievalQuery => format!("query: {}", text.trim()),
        TaskMode::RetrievalDocument => format!("document: {}", text.trim()),
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

    fn model(&self) -> String {
        self.model.clone()
    }

    fn prompt_format(&self) -> &'static str {
        PROMPT_FORMAT
    }

    async fn embed_one(&self, text: &str, mode: TaskMode) -> Result<Vec<f32>, EmbedError> {
        let formatted = format_input(text, mode);
        let req = EmbedRequest {
            model: format!("models/{}", self.model),
            content: Content {
                parts: vec![Part { text: formatted }],
            },
            task_type: supports_task_type(&self.model).then_some(task_type_str(mode)),
            output_dimensionality: Some(self.dimensions),
        };

        let url = format!(
            "{}/{}:embedContent?key={}",
            ENDPOINT, self.model, self.api_key
        );

        // Full-lifecycle retry: send + 429 + 5xx + body read + JSON parse.
        // Long benchmark runs (1500+ queries) hit transient drops; without
        // retry the whole bench dies on the first one.
        let delays_secs: [u64; 6] = [2, 4, 8, 16, 32, 64];
        let mut attempt: u32 = 0;
        loop {
            // 1. send
            let send_result = self.client.post(&url).json(&req).send().await;
            let resp = match send_result {
                Ok(r) => r,
                Err(e) => {
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tracing::warn!(
                            "gemini embed_one network error, backing off {}s (attempt {}): {}",
                            wait,
                            attempt,
                            e
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(EmbedError::Http {
                        provider: "gemini",
                        source: e,
                    });
                }
            };

            // 2. status
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if (attempt as usize) < delays_secs.len() {
                    let wait = delays_secs[attempt as usize];
                    attempt += 1;
                    tracing::info!(
                        "gemini embed_one 429, backing off {}s (attempt {})",
                        wait,
                        attempt
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(EmbedError::RateLimited { provider: "gemini" });
            }
            if status.is_server_error() {
                let body = resp.text().await.unwrap_or_default();
                if (attempt as usize) < delays_secs.len() {
                    let wait = delays_secs[attempt as usize];
                    attempt += 1;
                    tracing::warn!(
                        "gemini embed_one 5xx ({}), backing off {}s (attempt {}): {}",
                        status,
                        wait,
                        attempt,
                        body
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(EmbedError::Api {
                    provider: "gemini",
                    message: format!("status {}: {}", status, body),
                });
            }
            if !status.is_success() {
                // 4xx (non-429) — real client error, never retry.
                let body = resp.text().await.unwrap_or_default();
                return Err(EmbedError::Api {
                    provider: "gemini",
                    message: format!("status {}: {}", status, body),
                });
            }

            // 3. body read (TLS drop mid-stream lands here)
            let body_text =
                match resp.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        if (attempt as usize) < delays_secs.len() {
                            let wait = delays_secs[attempt as usize];
                            attempt += 1;
                            tracing::warn!(
                            "gemini embed_one body-read error, backing off {}s (attempt {}): {}",
                            wait, attempt, e
                        );
                            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                            continue;
                        }
                        return Err(EmbedError::Http {
                            provider: "gemini",
                            source: e,
                        });
                    }
                };

            // 4. JSON parse
            match serde_json::from_str::<EmbedResponse>(&body_text) {
                Ok(parsed) => return Ok(parsed.embedding.values),
                Err(e) => {
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tracing::warn!(
                            "gemini embed_one parse error, backing off {}s (attempt {}): {}",
                            wait,
                            attempt,
                            e
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(EmbedError::Api {
                        provider: "gemini",
                        message: format!(
                            "parse failed: {}; body: {}",
                            e,
                            body_text.chars().take(500).collect::<String>()
                        ),
                    });
                }
            }
        }
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
                    format_input(t, mode)
                } else {
                    // Truncate at a char boundary.
                    let mut end = MAX_CHARS_PER_TEXT;
                    while !t.is_char_boundary(end) && end > 0 {
                        end -= 1;
                    }
                    format_input(&t[..end], mode)
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
            if end == idx {
                end = idx + 1;
            }

            let requests: Vec<EmbedRequest> = truncated[idx..end]
                .iter()
                .map(|t| EmbedRequest {
                    model: format!("models/{}", self.model),
                    content: Content {
                        parts: vec![Part { text: t.clone() }],
                    },
                    task_type: supports_task_type(&self.model).then_some(task_type_str(mode)),
                    output_dimensionality: Some(self.dimensions),
                })
                .collect();
            let body = BatchEmbedRequest { requests };

            // Retry with exponential backoff on 429 and transient network
            // errors. Gemini free tier hits the per-minute quota fast on
            // large corpora; we try up to 6 times with backoffs 4s, 8s,
            // 16s, 32s, 60s, 60s (~3 min total) before surfacing the error.
            // Transport errors (connection reset, DNS hiccup) get the same
            // backoff since they're usually transient.
            let mut attempt: u32 = 0;
            let delays_secs: [u64; 6] = [4, 8, 16, 32, 60, 60];
            let parsed: BatchEmbedResponse = loop {
                let send_result = self.client.post(&url).json(&body).send().await;
                let resp = match send_result {
                    Ok(r) => r,
                    Err(e) => {
                        if (attempt as usize) < delays_secs.len() {
                            let wait = delays_secs[attempt as usize];
                            attempt += 1;
                            tracing::info!(
                                "gemini network error ({}), backing off {}s (attempt {})",
                                e,
                                wait,
                                attempt
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                            continue;
                        }
                        return Err(EmbedError::Http {
                            provider: "gemini",
                            source: e,
                        });
                    }
                };
                let status = resp.status();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tracing::info!("gemini 429, backing off {}s (attempt {})", wait, attempt);
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(EmbedError::RateLimited { provider: "gemini" });
                }
                if !status.is_success() {
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(EmbedError::Api {
                        provider: "gemini",
                        message: format!("status {}: {}", status, body_text),
                    });
                }
                let parsed: BatchEmbedResponse =
                    resp.json().await.map_err(|e| EmbedError::Http {
                        provider: "gemini",
                        source: e,
                    })?;
                break parsed;
            };
            for emb in parsed.embeddings {
                out.push(emb.values);
            }
            idx = end;
        }
        Ok(out)
    }
}

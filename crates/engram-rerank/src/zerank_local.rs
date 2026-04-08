//! Local zerank-2 reranker via the Python sidecar.
//!
//! ZeroEntropy's zerank-2 is a 4B Qwen3-4B-based cross-encoder with custom
//! modeling code that isn't loadable by ort, candle, or fastembed-rs (yet).
//! The fastest path to running it from Rust is a localhost HTTP sidecar that
//! loads the model with the official `sentence_transformers.CrossEncoder`
//! and exposes one POST /rerank endpoint.
//!
//! Sidecar source: `crates/engram-rerank/python/zerank_server.py`
//! Start with: `uv run --with sentence-transformers --with torch \
//!              crates/engram-rerank/python/zerank_server.py`
//!
//! Why a sidecar:
//! - GPU forward pass dominates inference; Python's HTTP overhead is rounding error.
//! - When an ONNX export of zerank-2 lands upstream we replace this file with
//!   a pure Rust ort/CoreML path. Until then, this is the lean integration.
//!
//! Why this beats Cohere v3.5 for engram:
//! - Biomedical NDCG@10: zerank-2 = 0.7217 vs Cohere = 0.6246 (+9.7 pp)
//! - STEM & Logic NDCG@10: zerank-2 = 0.6521 vs Cohere = 0.5418 (+11.0 pp)
//! - Average across 7 domains: 0.6714 vs 0.5847 (+8.7 pp)
//! Source: https://huggingface.co/zeroentropy/zerank-2 model card

use crate::{RerankCandidate, RerankError, RerankedResult, Reranker};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8765";

pub struct ZerankLocalReranker {
    client: reqwest::Client,
    base_url: String,
}

impl ZerankLocalReranker {
    pub fn new() -> Self {
        let base_url = std::env::var("ENGRAM_ZERANK_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Self {
            client: reqwest::Client::builder()
                // Local model on M-series Metal can be 1-3s for 50 docs;
                // generous timeout to absorb GC pauses or contention spikes.
                .timeout(std::time::Duration::from_secs(300))
                .user_agent(concat!("engram/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("failed to build reqwest client"),
            base_url,
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Hit /health to confirm the sidecar is up.
    pub async fn health_check(&self) -> Result<bool, RerankError> {
        let url = format!("{}/health", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| RerankError::Http {
                provider: "zerank_local",
                source: e,
            })?;
        Ok(resp.status().is_success())
    }
}

impl Default for ZerankLocalReranker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize)]
struct RerankRequest<'a> {
    query: &'a str,
    documents: Vec<&'a str>,
    top_k: usize,
}

#[derive(Deserialize)]
struct RerankResponse {
    results: Vec<RerankResultItem>,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct RerankResultItem {
    index: usize,
    score: f32,
}

#[async_trait]
impl Reranker for ZerankLocalReranker {
    fn name(&self) -> &'static str {
        "zerank_local"
    }

    async fn rerank(
        &self,
        query: &str,
        candidates: &[RerankCandidate],
        top_k: usize,
    ) -> Result<Vec<RerankedResult>, RerankError> {
        if candidates.is_empty() {
            return Ok(vec![]);
        }
        let documents: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();
        let req = RerankRequest {
            query,
            documents,
            top_k: top_k.min(candidates.len()),
        };

        let url = format!("{}/rerank", self.base_url);

        // Full-lifecycle retry: localhost connections drop occasionally
        // when the threading HTTP server's backlog fills up under load.
        // Without retry, one transient kills the whole 1500+ question bench.
        let delays_secs: [u64; 6] = [1, 2, 4, 8, 16, 32];
        let mut attempt: u32 = 0;
        let body: RerankResponse = loop {
            // 1. send
            let send_result = self.client.post(&url).json(&req).send().await;
            let resp = match send_result {
                Ok(r) => r,
                Err(e) => {
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(RerankError::Http {
                        provider: "zerank_local",
                        source: e,
                    });
                }
            };

            // 2. status
            let status = resp.status();
            if status.is_server_error() {
                let body_text = resp.text().await.unwrap_or_default();
                if (attempt as usize) < delays_secs.len() {
                    let wait = delays_secs[attempt as usize];
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(RerankError::Api {
                    provider: "zerank_local",
                    message: format!("status {}: {}", status, body_text),
                });
            }
            if !status.is_success() {
                let body_text = resp.text().await.unwrap_or_default();
                return Err(RerankError::Api {
                    provider: "zerank_local",
                    message: format!(
                        "status {}: {} (is the sidecar running? start with: \
                         uv run --with 'transformers<5.0,>=4.45' \
                         --with 'sentence-transformers>=3.0,<4.0' --with torch \
                         crates/engram-rerank/python/zerank_server.py)",
                        status, body_text
                    ),
                });
            }

            // 3. body read
            let body_text = match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(RerankError::Http {
                        provider: "zerank_local",
                        source: e,
                    });
                }
            };

            // 4. parse
            match serde_json::from_str::<RerankResponse>(&body_text) {
                Ok(parsed) => break parsed,
                Err(e) => {
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(RerankError::Api {
                        provider: "zerank_local",
                        message: format!("parse failed: {}; body: {}", e, body_text),
                    });
                }
            }
        };

        if let Some(err) = body.error {
            return Err(RerankError::Api {
                provider: "zerank_local",
                message: err,
            });
        }

        // Sidecar latency is logged via the elapsed_ms field if needed by callers.
        let _ = body.elapsed_ms;

        Ok(body
            .results
            .into_iter()
            .map(|r| RerankedResult {
                id: candidates[r.index].id.clone(),
                score: r.score,
                original_index: r.index,
            })
            .collect())
    }
}

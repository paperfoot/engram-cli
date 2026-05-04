//! Cohere rerank-v3.5 client.

use crate::{RerankCandidate, RerankError, RerankedResult, Reranker};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// Latest Cohere Rerank model (v3.5). Multilingual, better quality than
// rerank-english-v3.0, same API and same price.
const DEFAULT_MODEL: &str = "rerank-v3.5";
const ENDPOINT: &str = "https://api.cohere.com/v2/rerank";

pub struct CohereReranker {
    client: reqwest::Client,
    api_key: String,
    model: String,
}

impl CohereReranker {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent(concat!("engram/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("failed to build reqwest client"),
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_string(),
        }
    }

    pub fn from_env() -> Result<Self, RerankError> {
        let key = std::env::var("COHERE_API_KEY")
            .map_err(|_| RerankError::MissingKey { provider: "cohere" })?;
        Ok(Self::new(key))
    }

    pub fn with_model(mut self, m: impl Into<String>) -> Self {
        self.model = m.into();
        self
    }
}

#[derive(Serialize)]
struct CohereRequest<'a> {
    model: &'a str,
    query: &'a str,
    documents: Vec<&'a str>,
    top_n: usize,
}

#[derive(Deserialize)]
struct CohereResponse {
    results: Vec<CohereResult>,
}

#[derive(Deserialize)]
struct CohereResult {
    index: usize,
    relevance_score: f32,
}

#[async_trait]
impl Reranker for CohereReranker {
    fn name(&self) -> &'static str {
        "cohere"
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
        let req = CohereRequest {
            model: &self.model,
            query,
            documents,
            top_n: top_k.min(candidates.len()),
        };

        // Full-lifecycle retry: send / 429 / 5xx / body read / parse.
        // Cohere API occasionally returns 502 / 503 from upstream load
        // balancers; without retry these kill long benchmark runs.
        let delays_secs: [u64; 6] = [2, 4, 8, 16, 32, 64];
        let mut attempt: u32 = 0;
        let body: CohereResponse = loop {
            // 1. send
            let send_result = self
                .client
                .post(ENDPOINT)
                .bearer_auth(&self.api_key)
                .json(&req)
                .send()
                .await;
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
                        provider: "cohere",
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
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(RerankError::RateLimited { provider: "cohere" });
            }
            if status.is_server_error() {
                let body_text = resp.text().await.unwrap_or_default();
                if (attempt as usize) < delays_secs.len() {
                    let wait = delays_secs[attempt as usize];
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(RerankError::Api {
                    provider: "cohere",
                    message: format!("status {}: {}", status, body_text),
                });
            }
            if !status.is_success() {
                let body_text = resp.text().await.unwrap_or_default();
                return Err(RerankError::Api {
                    provider: "cohere",
                    message: format!("status {}: {}", status, body_text),
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
                        provider: "cohere",
                        source: e,
                    });
                }
            };

            // 4. parse
            match serde_json::from_str::<CohereResponse>(&body_text) {
                Ok(parsed) => break parsed,
                Err(e) => {
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(RerankError::Api {
                        provider: "cohere",
                        message: format!("parse failed: {}; body: {}", e, body_text),
                    });
                }
            }
        };

        Ok(body
            .results
            .into_iter()
            .map(|r| RerankedResult {
                id: candidates[r.index].id.clone(),
                score: r.relevance_score,
                original_index: r.index,
            })
            .collect())
    }
}

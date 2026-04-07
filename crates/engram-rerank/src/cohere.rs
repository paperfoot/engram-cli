//! Cohere Rerank 4 Pro client.

use crate::{RerankCandidate, RerankError, RerankedResult, Reranker};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

const DEFAULT_MODEL: &str = "rerank-english-v3.0"; // upgrade to rerank-4-pro when GA
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
        let resp = self
            .client
            .post(ENDPOINT)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| RerankError::Http { provider: "cohere", source: e })?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(RerankError::RateLimited { provider: "cohere" });
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RerankError::Api {
                provider: "cohere",
                message: format!("status {}: {}", status, body),
            });
        }

        let body: CohereResponse = resp
            .json()
            .await
            .map_err(|e| RerankError::Http { provider: "cohere", source: e })?;

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

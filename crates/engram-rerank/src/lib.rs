//! Reranking providers behind a trait.

pub mod cohere;
pub mod error;
pub mod passthrough;
pub mod zerank_local;

use async_trait::async_trait;
pub use error::RerankError;

#[derive(Debug, Clone)]
pub struct RerankCandidate {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct RerankedResult {
    pub id: String,
    pub score: f32,
    pub original_index: usize,
}

#[async_trait]
pub trait Reranker: Send + Sync {
    fn name(&self) -> &'static str;

    async fn rerank(
        &self,
        query: &str,
        candidates: &[RerankCandidate],
        top_k: usize,
    ) -> Result<Vec<RerankedResult>, RerankError>;
}

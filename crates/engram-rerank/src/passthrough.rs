//! No-op reranker — preserves order. Used when no reranker is configured.

use crate::{RerankCandidate, RerankError, RerankedResult, Reranker};
use async_trait::async_trait;

pub struct PassthroughReranker;

#[async_trait]
impl Reranker for PassthroughReranker {
    fn name(&self) -> &'static str {
        "passthrough"
    }

    async fn rerank(
        &self,
        _query: &str,
        candidates: &[RerankCandidate],
        top_k: usize,
    ) -> Result<Vec<RerankedResult>, RerankError> {
        Ok(candidates
            .iter()
            .take(top_k)
            .enumerate()
            .map(|(i, c)| RerankedResult {
                id: c.id.clone(),
                score: 1.0 - (i as f32) / (candidates.len().max(1) as f32),
                original_index: i,
            })
            .collect())
    }
}

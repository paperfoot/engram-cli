//! Embedding providers behind a single trait.
//!
//! v0 ships Gemini Embed 2 (cloud) + a deterministic stub for tests.
//! Local fallbacks (candle/ort) land in Phase 4.

pub mod error;
pub mod gemini;
pub mod stub;

use async_trait::async_trait;
pub use error::EmbedError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskMode {
    /// Embed a query for retrieval.
    RetrievalQuery,
    /// Embed a document being indexed.
    RetrievalDocument,
}

#[async_trait]
pub trait Embedder: Send + Sync {
    fn name(&self) -> &'static str;
    fn dimensions(&self) -> usize;

    async fn embed_one(&self, text: &str, mode: TaskMode) -> Result<Vec<f32>, EmbedError>;

    async fn embed_batch(
        &self,
        texts: &[&str],
        mode: TaskMode,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed_one(t, mode).await?);
        }
        Ok(out)
    }
}

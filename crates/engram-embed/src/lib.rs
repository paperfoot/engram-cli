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
    fn model(&self) -> String {
        self.name().to_string()
    }
    fn prompt_format(&self) -> &'static str {
        "legacy"
    }

    /// Longest input (in bytes) this provider accepts before it truncates.
    /// `None` means the provider has no client-side cap.
    ///
    /// Callers use this to detect — and report — a chunk whose tail is about to
    /// be thrown away. Truncation used to be entirely silent: a long note went
    /// in, a vector for its first ~7,800 characters came back, and the rest of
    /// the note was permanently unretrievable with nothing in the output to say
    /// so. Chunking now caps well below this, so a hit here means a knob was
    /// widened past the provider's window and the user needs to know.
    fn max_input_chars(&self) -> Option<usize> {
        None
    }

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

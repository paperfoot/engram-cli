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

/// A batch embed that failed *after* the provider had already served — and
/// will therefore bill for — part of the work.
///
/// One logical `embed_batch` call can be several billed HTTP requests (see
/// `gemini::embed_batch_accounted`). When request 2 of 3 fails, request 1 has
/// been paid for. Dropping that on the floor is how a provider invoice grows
/// while the local ledger and every `engram budget` cap built on it stay flat.
#[derive(Debug)]
pub struct BatchEmbedFailure {
    pub error: EmbedError,
    /// How many of `texts`, counting from the FRONT, were embedded by a
    /// request the provider actually served. Always a prefix: batches are
    /// packed and sent in input order, so the caller can bill
    /// `&texts[..charged_texts]` with the same estimator it uses on success.
    pub charged_texts: usize,
    /// How many billed HTTP requests that prefix cost.
    pub charged_requests: usize,
}

impl BatchEmbedFailure {
    /// A failure that cost nothing — nothing had been sent yet.
    pub fn unpaid(error: EmbedError) -> Self {
        Self {
            error,
            charged_texts: 0,
            charged_requests: 0,
        }
    }
}

impl From<BatchEmbedFailure> for EmbedError {
    fn from(f: BatchEmbedFailure) -> Self {
        f.error
    }
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

    /// A human-readable warning when this embedder is running degraded —
    /// e.g. the primary API key stopped serving and a fallback answered.
    /// `None` means healthy. Callers surface this in command output so a
    /// silent failover never stays silent: a fallback keeps you running,
    /// and this line is what tells you to go fix the primary.
    fn degradation_note(&self) -> Option<String> {
        None
    }

    async fn embed_one(&self, text: &str, mode: TaskMode) -> Result<Vec<f32>, EmbedError>;

    /// Embed a batch. Errors here discard the partial-spend accounting; a
    /// caller that records usage should use [`Embedder::embed_batch_accounted`].
    async fn embed_batch(
        &self,
        texts: &[&str],
        mode: TaskMode,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.embed_batch_accounted(texts, mode)
            .await
            .map_err(|f| f.error)
    }

    /// Embed a batch, and on failure report what the provider was already PAID
    /// for. The default is the one-call-per-text loop, which knows exactly how
    /// many requests it got through; Gemini overrides it because it packs many
    /// texts into a few billed requests.
    ///
    /// OVERRIDE THIS, NOT `embed_batch`: the default `embed_batch` delegates
    /// here, so one override serves both and the two can never disagree.
    async fn embed_batch_accounted(
        &self,
        texts: &[&str],
        mode: TaskMode,
    ) -> Result<Vec<Vec<f32>>, BatchEmbedFailure> {
        let mut out = Vec::with_capacity(texts.len());
        for (i, t) in texts.iter().enumerate() {
            match self.embed_one(t, mode).await {
                Ok(v) => out.push(v),
                // The first `i` texts each cost their own served request.
                Err(error) => {
                    return Err(BatchEmbedFailure {
                        error,
                        charged_texts: i,
                        charged_requests: i,
                    })
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fails on text number `n` (0-based), succeeds on every earlier one.
    struct FailAfter {
        n: usize,
        seen: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl Embedder for FailAfter {
        fn name(&self) -> &'static str {
            "fail-after"
        }
        fn dimensions(&self) -> usize {
            4
        }
        async fn embed_one(&self, _text: &str, _mode: TaskMode) -> Result<Vec<f32>, EmbedError> {
            let i = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if i == self.n {
                return Err(EmbedError::RateLimited { provider: "test" });
            }
            Ok(vec![0.0; 4])
        }
    }

    fn fail_after(n: usize) -> FailAfter {
        FailAfter {
            n,
            seen: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    const FIVE: [&str; 5] = ["a", "b", "c", "d", "e"];

    #[tokio::test]
    async fn the_default_accounting_seam_counts_the_requests_it_got_through() {
        let e = fail_after(2);
        let failure = e
            .embed_batch_accounted(&FIVE, TaskMode::RetrievalDocument)
            .await
            .expect_err("text 3 of 5 fails");
        assert_eq!(failure.charged_texts, 2);
        assert_eq!(failure.charged_requests, 2);
    }

    #[tokio::test]
    async fn a_first_call_failure_is_billed_as_nothing() {
        let e = fail_after(0);
        let failure = e
            .embed_batch_accounted(&FIVE, TaskMode::RetrievalDocument)
            .await
            .expect_err("text 1 of 5 fails");
        assert_eq!(failure.charged_texts, 0);
        assert_eq!(failure.charged_requests, 0);
    }

    #[tokio::test]
    async fn embed_batch_still_returns_the_bare_error() {
        let e = fail_after(1);
        let err = e
            .embed_batch(&FIVE, TaskMode::RetrievalDocument)
            .await
            .expect_err("the delegating default surfaces the same variant");
        assert!(matches!(err, EmbedError::RateLimited { .. }));
    }
}

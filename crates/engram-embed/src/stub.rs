//! Deterministic stub embedder for tests and CI. Hashes text into a fixed-size
//! float vector. Not meaningful semantically but stable and offline.

use crate::{Embedder, EmbedError, TaskMode};
use async_trait::async_trait;

pub struct StubEmbedder {
    dims: usize,
}

impl StubEmbedder {
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }
}

impl Default for StubEmbedder {
    fn default() -> Self {
        Self { dims: 64 }
    }
}

#[async_trait]
impl Embedder for StubEmbedder {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    async fn embed_one(&self, text: &str, _mode: TaskMode) -> Result<Vec<f32>, EmbedError> {
        let mut v = vec![0f32; self.dims];
        for (i, byte) in text.bytes().enumerate() {
            let bucket = (i + byte as usize) % self.dims;
            v[bucket] += (byte as f32) / 255.0;
        }
        // L2 normalize
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_is_deterministic() {
        let e = StubEmbedder::default();
        let a = e.embed_one("hello", TaskMode::RetrievalQuery).await.unwrap();
        let b = e.embed_one("hello", TaskMode::RetrievalQuery).await.unwrap();
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn different_texts_give_different_embeddings() {
        let e = StubEmbedder::default();
        let a = e.embed_one("rapamycin", TaskMode::RetrievalQuery).await.unwrap();
        let b = e.embed_one("metformin", TaskMode::RetrievalQuery).await.unwrap();
        assert_ne!(a, b);
    }
}

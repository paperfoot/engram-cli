//! Standard IR metrics.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Recall {
    pub at_1: f32,
    pub at_5: f32,
    pub at_10: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metrics {
    pub recall: Recall,
    pub mrr: f32,
    pub ndcg_at_10: f32,
    pub mean_latency_ms: f32,
    pub p50_latency_ms: f32,
    pub p95_latency_ms: f32,
    pub questions_evaluated: usize,
}

/// Recall@k: fraction of questions where any of the top-k retrieved items
/// matches a relevant id.
pub fn recall_at_k<T: PartialEq>(retrieved: &[T], relevant: &[T], k: usize) -> f32 {
    if relevant.is_empty() {
        return 0.0;
    }
    let top_k = retrieved.iter().take(k);
    for r in top_k {
        if relevant.contains(r) {
            return 1.0;
        }
    }
    0.0
}

/// Reciprocal rank of the first relevant item; 0 if none in top-k.
pub fn reciprocal_rank<T: PartialEq>(retrieved: &[T], relevant: &[T], k: usize) -> f32 {
    for (i, r) in retrieved.iter().take(k).enumerate() {
        if relevant.contains(r) {
            return 1.0 / (i as f32 + 1.0);
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_at_k_finds_relevant() {
        let retrieved = vec![1, 2, 3, 4, 5];
        let relevant = vec![3];
        assert_eq!(recall_at_k(&retrieved, &relevant, 5), 1.0);
        assert_eq!(recall_at_k(&retrieved, &relevant, 2), 0.0);
    }

    #[test]
    fn mrr_is_one_over_rank() {
        let retrieved = vec![1, 2, 3];
        let relevant = vec![3];
        assert!((reciprocal_rank(&retrieved, &relevant, 5) - 1.0 / 3.0).abs() < 1e-6);
    }
}

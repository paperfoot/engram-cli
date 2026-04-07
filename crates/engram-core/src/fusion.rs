//! Reciprocal Rank Fusion and related ranking primitives.
//!
//! This module is a primary autoresearch target — every retrieval improvement
//! depends on how dense + lexical + entity hits are merged.

use crate::types::{ChunkId, RetrievalSource, Score};
use ordered_float::OrderedFloat;
use std::collections::HashMap;

/// One ranked result from a single retrieval source.
#[derive(Debug, Clone)]
pub struct RankedHit {
    pub chunk_id: ChunkId,
    pub rank: usize, // 1-indexed
    pub raw_score: f32,
    pub source: RetrievalSource,
}

/// Reciprocal Rank Fusion.
///
/// `k` is the smoothing constant (60 is the canonical value from Cormack et al.,
/// but autoresearch can tune it).
///
/// Tie-break order: higher score first; on equal scores, smaller chunk id first.
/// The deterministic tiebreak matters because HashMap iteration order is
/// randomized — without this, the ranking flips between runs whenever two
/// chunks share a fused score.
pub fn reciprocal_rank_fusion(
    runs: &[Vec<RankedHit>],
    k: f32,
) -> Vec<(ChunkId, Score)> {
    let mut scores: HashMap<ChunkId, f32> = HashMap::new();
    for run in runs {
        for hit in run {
            let contribution = 1.0 / (k + hit.rank as f32);
            *scores.entry(hit.chunk_id).or_insert(0.0) += contribution;
        }
    }
    let mut fused: Vec<_> = scores
        .into_iter()
        .map(|(id, s)| (id, OrderedFloat(s)))
        .collect();
    fused.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    fused
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn rrf_promotes_items_appearing_in_multiple_runs() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();

        let dense = vec![
            RankedHit { chunk_id: a, rank: 1, raw_score: 0.9, source: RetrievalSource::Dense },
            RankedHit { chunk_id: b, rank: 2, raw_score: 0.7, source: RetrievalSource::Dense },
        ];
        let lexical = vec![
            RankedHit { chunk_id: b, rank: 1, raw_score: 5.0, source: RetrievalSource::Lexical },
            RankedHit { chunk_id: c, rank: 2, raw_score: 3.0, source: RetrievalSource::Lexical },
        ];

        let fused = reciprocal_rank_fusion(&[dense, lexical], 60.0);
        // b appears in both runs and should be ranked first.
        assert_eq!(fused[0].0, b);
    }

    #[test]
    fn rrf_with_empty_runs_is_empty() {
        let fused = reciprocal_rank_fusion(&[], 60.0);
        assert!(fused.is_empty());
    }
}

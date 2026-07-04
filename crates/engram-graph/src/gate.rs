//! Write-time novelty gate for facts.
//!
//! Before any LLM adjudication, a new fact's embedding is compared against
//! the active facts in the same (diary, kb). Three outcomes:
//!
//! - clear duplicate  → NOOP (the existing fact gains a confirmation)
//! - clearly novel    → insert, no adjudication needed
//! - uncertain band   → candidate conflict; adjudicate (LLM or caller)
//!
//! This eliminates most adjudication calls at zero API cost (SAGE,
//! arXiv 2605.30711 measures 16–18% of write-path LLM calls removed with no
//! quality loss). Thresholds are caller-supplied and tuned on the bench
//! harness — they are config, not truth.

use uuid::Uuid;

/// Default cosine similarity above which a fact is a semantic duplicate.
pub const DEFAULT_DUP_THRESHOLD: f32 = 0.95;
/// Default cosine similarity below which a fact is clearly novel.
pub const DEFAULT_NOVEL_THRESHOLD: f32 = 0.65;
/// Max conflict candidates forwarded to adjudication.
pub const MAX_CANDIDATES: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    /// Semantically identical to an existing active fact.
    Duplicate { fact_id: Uuid, score: f32 },
    /// Unrelated to anything stored; safe to insert without adjudication.
    Novel,
    /// Similar-but-not-identical to existing facts: potential contradiction.
    /// Candidates are (fact_id, cosine), highest first.
    Uncertain { candidates: Vec<(Uuid, f32)> },
}

/// Classify a new fact embedding against existing active-fact embeddings.
/// Embeddings with mismatched dimensions are skipped (mixed-model stores).
pub fn novelty_gate(
    new_embedding: &[f32],
    existing: &[(Uuid, Vec<f32>)],
    dup_threshold: f32,
    novel_threshold: f32,
) -> GateDecision {
    let mut scored: Vec<(Uuid, f32)> = existing
        .iter()
        .filter(|(_, v)| v.len() == new_embedding.len() && !v.is_empty())
        .map(|(id, v)| (*id, cosine(new_embedding, v)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    match scored.first() {
        None => GateDecision::Novel,
        Some(&(id, top)) if top >= dup_threshold => GateDecision::Duplicate { fact_id: id, score: top },
        Some(&(_, top)) if top < novel_threshold => GateDecision::Novel,
        _ => GateDecision::Uncertain {
            candidates: scored
                .into_iter()
                .take_while(|(_, s)| *s >= novel_threshold)
                .take(MAX_CANDIDATES)
                .collect(),
        },
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn empty_store_is_novel() {
        assert_eq!(
            novelty_gate(&[1.0, 0.0], &[], DEFAULT_DUP_THRESHOLD, DEFAULT_NOVEL_THRESHOLD),
            GateDecision::Novel
        );
    }

    #[test]
    fn identical_vector_is_duplicate() {
        let existing = vec![(id(1), vec![1.0, 0.0])];
        match novelty_gate(&[1.0, 0.0], &existing, 0.95, 0.65) {
            GateDecision::Duplicate { fact_id, score } => {
                assert_eq!(fact_id, id(1));
                assert!(score > 0.99);
            }
            other => panic!("expected Duplicate, got {other:?}"),
        }
    }

    #[test]
    fn orthogonal_vector_is_novel() {
        let existing = vec![(id(1), vec![1.0, 0.0])];
        assert_eq!(
            novelty_gate(&[0.0, 1.0], &existing, 0.95, 0.65),
            GateDecision::Novel
        );
    }

    #[test]
    fn mid_band_is_uncertain_with_ranked_candidates() {
        // cos(new, a) ≈ 0.894, cos(new, b) ≈ 0.707 — both in [0.65, 0.95).
        let existing = vec![(id(1), vec![1.0, 0.5]), (id(2), vec![1.0, 1.0])];
        match novelty_gate(&[1.0, 0.0], &existing, 0.95, 0.65) {
            GateDecision::Uncertain { candidates } => {
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0].0, id(1)); // higher cosine first
                assert!(candidates[0].1 > candidates[1].1);
            }
            other => panic!("expected Uncertain, got {other:?}"),
        }
    }

    #[test]
    fn dimension_mismatch_is_skipped_not_compared() {
        let existing = vec![(id(1), vec![1.0, 0.0, 0.0])];
        assert_eq!(
            novelty_gate(&[1.0, 0.0], &existing, 0.95, 0.65),
            GateDecision::Novel
        );
    }

    #[test]
    fn candidates_capped_at_max() {
        let existing: Vec<(Uuid, Vec<f32>)> =
            (0..10).map(|i| (id(i), vec![1.0, 0.5])).collect();
        match novelty_gate(&[1.0, 0.0], &existing, 0.99, 0.5) {
            GateDecision::Uncertain { candidates } => {
                assert_eq!(candidates.len(), MAX_CANDIDATES);
            }
            other => panic!("expected Uncertain, got {other:?}"),
        }
    }
}

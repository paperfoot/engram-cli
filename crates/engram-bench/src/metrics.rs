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

/// True iff ALL relevant items appear in top-k. Stricter than recall_at_k.
pub fn all_gold_present<T: PartialEq>(retrieved: &[T], relevant: &[T], k: usize) -> bool {
    if relevant.is_empty() {
        return true;
    }
    let top_k: Vec<&T> = retrieved.iter().take(k).collect();
    relevant.iter().all(|r| top_k.contains(&r))
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

/// Normalised Discounted Cumulative Gain at cut-off 10.
///
/// Uses graded relevance from `qrels` (doc id -> relevance grade). Documents
/// not present in `qrels` are treated as grade 0. Uses **linear** gain to match
/// `pytrec_eval`'s `ndcg_cut` — the tool the published BEIR leaderboard is
/// computed with — so graded datasets (trec-covid, scidocs) are comparable to
/// the paper. For binary qrels (SciFact: all grade 1) linear and exponential
/// gain are identical, so the headline SciFact nDCG@10 is unchanged.
///
/// ```text
/// DCG  = Σ_{i=0..<10} rel_i / log2(i + 2)
/// IDCG = same, over the qrels grades sorted descending (top 10)
/// nDCG = DCG / IDCG          (0.0 when IDCG == 0)
/// ```
pub fn ndcg_at_10(retrieved_doc_ids: &[String], qrels: &std::collections::HashMap<String, u32>) -> f32 {
    fn dcg(grades: impl Iterator<Item = u32>) -> f32 {
        grades
            .take(10)
            .enumerate()
            .map(|(i, rel)| {
                // Linear gain (pytrec_eval ndcg_cut). A u32 grade -> f32 cannot
                // overflow-panic, so no saturating math is needed.
                (rel as f32) / ((i as f32) + 2.0).log2()
            })
            .sum()
    }

    let dcg_score = dcg(retrieved_doc_ids
        .iter()
        .map(|id| qrels.get(id).copied().unwrap_or(0)));

    let mut ideal: Vec<u32> = qrels.values().copied().collect();
    ideal.sort_unstable_by(|a, b| b.cmp(a));
    let idcg = dcg(ideal.into_iter());

    if idcg == 0.0 {
        0.0
    } else {
        dcg_score / idcg
    }
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
    fn all_gold_present_requires_full_coverage() {
        let retrieved = vec![1, 2, 3, 4, 5];
        assert!(all_gold_present(&retrieved, &vec![1, 3], 5));
        assert!(!all_gold_present(&retrieved, &vec![1, 6], 5));
        assert!(!all_gold_present(&retrieved, &vec![3], 2));
        assert!(all_gold_present(&retrieved, &Vec::<i32>::new(), 5));
    }

    #[test]
    fn mrr_is_one_over_rank() {
        let retrieved = vec![1, 2, 3];
        let relevant = vec![3];
        assert!((reciprocal_rank(&retrieved, &relevant, 5) - 1.0 / 3.0).abs() < 1e-6);
    }

    fn qrels(pairs: &[(&str, u32)]) -> std::collections::HashMap<String, u32> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ndcg_perfect_ranking_is_one() {
        let q = qrels(&[("a", 3), ("b", 2), ("c", 1)]);
        let retrieved = ids(&["a", "b", "c"]);
        assert!((ndcg_at_10(&retrieved, &q) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ndcg_empty_is_zero() {
        let q = qrels(&[("a", 3)]);
        // No retrieved docs -> DCG 0 -> nDCG 0.
        assert_eq!(ndcg_at_10(&[], &q), 0.0);
        // No qrels -> IDCG 0 -> nDCG 0 by the IDCG==0 guard.
        assert_eq!(ndcg_at_10(&ids(&["a"]), &std::collections::HashMap::new()), 0.0);
    }

    #[test]
    fn ndcg_hand_worked_example() {
        // Linear gain (pytrec_eval ndcg_cut). qrels a=3, b=2, c=1.
        // Retrieved = [b, x, a] where x is unjudged (grade 0).
        //   DCG  = 2/log2(2) + 0 + 3/log2(4) = 2 + 1.5                     = 3.5
        //   IDCG = 3/log2(2) + 2/log2(3) + 1/log2(4)
        //        = 3 + 1.2618595071 + 0.5                                  = 4.7618595071
        //   nDCG = 3.5 / 4.7618595071                                      = 0.7350069851
        let q = qrels(&[("a", 3), ("b", 2), ("c", 1)]);
        let retrieved = ids(&["b", "x", "a"]);
        assert!((ndcg_at_10(&retrieved, &q) - 0.735_006_99).abs() < 1e-6);
    }

    #[test]
    fn ndcg_ignores_docs_beyond_rank_10() {
        // 11 relevant docs but only top-10 count; the 11th (grade 5) must be ignored.
        let q = qrels(&[
            ("d0", 1), ("d1", 1), ("d2", 1), ("d3", 1), ("d4", 1), ("d5", 1),
            ("d6", 1), ("d7", 1), ("d8", 1), ("d9", 1), ("d10", 5),
        ]);
        let retrieved = ids(&[
            "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8", "d9", "d10",
        ]);
        // IDCG sorts grades desc -> the grade-5 doc lands at rank 0, but the
        // retrieved list has all the grade-1 docs first, so nDCG < 1.0.
        let score = ndcg_at_10(&retrieved, &q);
        assert!(score > 0.0 && score < 1.0, "got {score}");
    }
}

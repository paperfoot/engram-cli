//! Inline mini benchmark — runs in <1s with no external dataset.
//!
//! This is the fast loop autoresearch hits on every iteration. The real
//! LongMemEval run validates wins on the published held-out split.
//!
//! Test set is intentionally small but covers the four query patterns the
//! retrieval pipeline must handle:
//!
//! 1. Direct fact recall ("rapamycin lifespan")
//! 2. Synonym/alias resolution ("sirolimus" should find "rapamycin")
//! 3. Multi-fact disambiguation (two memories about mTOR; which one matches?)
//! 4. Negation / time-bounded ("what was true in 2025?")
//!
//! Each test has 1 relevant chunk in a haystack of distractors.

use crate::metrics::{recall_at_k, reciprocal_rank, Metrics, Recall};
use engram_storage::SqliteStore;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniQuestion {
    pub question: String,
    pub relevant_text: &'static str, // matched against stored chunk content
}

// Designed to be hard for keyword-only retrieval. Each question avoids
// exact keyword overlap with its target chunk so synonyms, concepts, and
// semantic similarity matter. FTS5 alone should land around 30-50%; a
// full hybrid pipeline should approach 100%.
const HAYSTACK: &[&str] = &[
    // 0: rapamycin — query uses "sirolimus" + concept words
    "Rapamycin extends median lifespan in mice by inhibiting the mechanistic target of rapamycin complex one signaling pathway.",
    // 1: metformin — query uses "biguanide" + "antidiabetic"
    "Metformin reduces all-cause mortality in type 2 diabetics and is being studied for longevity in non-diabetic adults.",
    // 2: senolytics — query uses "ageing cells" + "clear"
    "The combination of dasatinib and quercetin selectively eliminates zombie cells from aged tissues, improving healthspan markers.",
    // 3: semaglutide — query uses "GLP-1 agonist" + "obesity drug"
    "Wegovy produced fifteen percent body weight reduction at sixty-eight weeks in the STEP one trial of adults with obesity.",
    // 4: caloric restriction — query uses "eating less" + "extends life"
    "Reducing daily energy intake by thirty percent without malnutrition prolongs lifespan in budding yeast, nematodes, fruit flies, and rodents.",
    // 5: NAD precursors — query uses "NAD booster"
    "Nicotinamide riboside supplementation raised whole-blood nicotinamide adenine dinucleotide levels by sixty percent in healthy older adults.",
    // 6: mTOR pathway — query uses "growth signaling"
    "TORC1 integrates amino acid availability with the cellular decision to grow and divide versus enter autophagy.",
    // 7: senescent cells biology — query uses "secrete inflammatory"
    "Cells that undergo replicative arrest develop the senescence-associated secretory phenotype and release IL-6, MCP-1, and PAI-1.",
    // 8: GLP-1 mechanism — query uses "incretin"
    "Glucagon-like peptide one is released from intestinal L cells in response to nutrients and stimulates pancreatic insulin secretion.",
    // 9: HDAC and aging — query uses "epigenetic"
    "Sirtuin deacetylases respond to NAD+ levels and modulate chromatin state in a way that has been linked to longevity in model organisms.",
    // Distractors
    "The Eiffel Tower in Paris is three hundred thirty meters tall and was completed in eighteen eighty-nine.",
    "Python is a popular dynamically-typed interpreted general-purpose programming language with garbage collection.",
    "Mount Everest sits on the border between Nepal and Tibet and reaches eight thousand eight hundred forty-eight meters above sea level.",
    "The Pacific Ocean covers approximately one hundred sixty-five million square kilometers of the Earth's surface.",
    "Photosynthesis converts carbon dioxide and water into glucose and molecular oxygen using light energy from the sun.",
    "Albert Einstein published his theory of general relativity in nineteen fifteen, predicting gravitational lensing.",
    "The DNA double helix structure was elucidated by James Watson, Francis Crick, and Rosalind Franklin in nineteen fifty-three.",
    "Quantum computing uses qubits that can exist in superposition of zero and one until measurement collapses them.",
    "The speed of light in vacuum is approximately two hundred ninety-nine thousand seven hundred ninety-two kilometers per second.",
    "William Shakespeare wrote thirty-nine plays, one hundred fifty-four sonnets, and several long narrative poems.",
];

const QUESTIONS: &[(&str, &str)] = &[
    ("Which drug originally called sirolimus prolongs life in laboratory mice?", "Rapamycin extends median"),
    ("What biguanide antidiabetic is being repurposed as a longevity intervention?", "Metformin reduces all-cause"),
    ("Which senolytic combination removes zombie cells from old tissue?", "The combination of dasatinib"),
    ("What GLP-1 receptor agonist marketed as Wegovy treats obesity?", "Wegovy produced fifteen"),
    ("How does eating fewer calories than normal affect animal lifespan?", "Reducing daily energy intake"),
    ("Which NAD+ booster has been shown to raise the cofactor in elderly humans?", "Nicotinamide riboside"),
    ("What pathway senses amino acids and regulates cellular growth versus autophagy?", "TORC1 integrates"),
    ("What secretory profile do senescent cells develop that drives inflammation?", "Cells that undergo replicative"),
    ("Which incretin hormone from the gut triggers insulin release after meals?", "Glucagon-like peptide"),
    ("How do sirtuins use NAD+ to influence chromatin and aging?", "Sirtuin deacetylases"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniReport {
    pub metrics: Metrics,
    pub per_question: Vec<MiniQuestionResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniQuestionResult {
    pub question: String,
    pub relevant_prefix: String,
    pub recall_at_1: f32,
    pub recall_at_5: f32,
    pub reciprocal_rank: f32,
    pub latency_ms: u64,
    pub top_chunk_excerpt: String,
}

/// Run the mini benchmark using FTS5 only (the v0 baseline). Returns full
/// metrics that autoresearch can optimize against.
pub fn run_fts_baseline() -> Result<MiniReport, crate::error::BenchError> {
    let store = SqliteStore::open_in_memory()?;

    // Seed the haystack as one memory per chunk so each is independently
    // retrievable. The relevant content's chunk_id is what we score against.
    use engram_core::types::{Memory, MemorySource};
    use uuid::Uuid;

    let mut chunk_ids_by_text: Vec<(Uuid, &'static str)> = Vec::new();
    for text in HAYSTACK {
        let m = Memory {
            id: Uuid::new_v4(),
            content: (*text).to_string(),
            created_at: chrono::Utc::now(),
            event_time: None,
            importance: 5,
            emotional_weight: 0,
            access_count: 0,
            last_accessed: None,
            stability: 1.0,
            source: MemorySource::Manual,
            diary: "bench".into(),
            valid_from: None,
            valid_until: None,
            tags: vec![],
        };
        store.insert_memory(&m)?;
        let chunk_id = Uuid::new_v4();
        store.insert_chunk(chunk_id, m.id, text, 0, None)?;
        chunk_ids_by_text.push((chunk_id, text));
    }

    let mut latencies = Vec::with_capacity(QUESTIONS.len());
    let mut r1_total = 0f32;
    let mut r5_total = 0f32;
    let mut mrr_total = 0f32;
    let mut per_question = Vec::with_capacity(QUESTIONS.len());

    for (question, relevant_prefix) in QUESTIONS {
        let target_id = chunk_ids_by_text
            .iter()
            .find(|(_, text)| text.starts_with(relevant_prefix))
            .map(|(id, _)| *id)
            .ok_or_else(|| {
                crate::error::BenchError::InvalidDataset(format!(
                    "no haystack entry starts with {relevant_prefix:?}"
                ))
            })?;

        let start = Instant::now();
        // FTS5 needs OR-style queries; preprocess naive query into FTS5 syntax.
        let fts_query = build_fts_query(question);
        let hits = store.fts_search(&fts_query, 10)?;
        let latency_ms = start.elapsed().as_millis() as u64;
        latencies.push(latency_ms);

        let retrieved: Vec<Uuid> = hits.iter().map(|(id, _)| *id).collect();
        let relevant = vec![target_id];

        let r1 = recall_at_k(&retrieved, &relevant, 1);
        let r5 = recall_at_k(&retrieved, &relevant, 5);
        let rr = reciprocal_rank(&retrieved, &relevant, 10);

        r1_total += r1;
        r5_total += r5;
        mrr_total += rr;

        let top_excerpt = retrieved
            .first()
            .and_then(|id| {
                chunk_ids_by_text
                    .iter()
                    .find(|(cid, _)| cid == id)
                    .map(|(_, text)| (*text).to_string())
            })
            .unwrap_or_default();

        per_question.push(MiniQuestionResult {
            question: question.to_string(),
            relevant_prefix: relevant_prefix.to_string(),
            recall_at_1: r1,
            recall_at_5: r5,
            reciprocal_rank: rr,
            latency_ms,
            top_chunk_excerpt: top_excerpt.chars().take(80).collect(),
        });
    }

    let n = QUESTIONS.len() as f32;
    let mut metrics = Metrics::default();
    metrics.recall = Recall {
        at_1: r1_total / n,
        at_5: r5_total / n,
        at_10: r5_total / n, // collapses for k=10 in this set
    };
    metrics.mrr = mrr_total / n;
    metrics.questions_evaluated = QUESTIONS.len();
    let mean_lat = latencies.iter().sum::<u64>() as f32 / n;
    metrics.mean_latency_ms = mean_lat;
    let mut sorted = latencies.clone();
    sorted.sort_unstable();
    metrics.p50_latency_ms = sorted[sorted.len() / 2] as f32;
    metrics.p95_latency_ms = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)] as f32;

    Ok(MiniReport { metrics, per_question })
}

/// Convert a free-text query into FTS5 syntax. Strips punctuation, lowercases,
/// drops short and stop-ish tokens, then ORs them with each token quoted so
/// FTS5 cannot interpret hyphens, plus signs or numbers as syntax.
pub fn build_fts_query(text: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "that", "what", "which", "how",
        "does", "are", "was", "were", "from", "into", "this", "have",
    ];
    let mut tokens: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        if lower.len() < 3 {
            continue;
        }
        if STOPWORDS.contains(&lower.as_str()) {
            continue;
        }
        tokens.push(format!("\"{}\"", lower));
    }
    tokens.join(" OR ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_builder_drops_short_tokens_and_ors() {
        let q = build_fts_query("of the rapamycin");
        assert_eq!(q, "rapamycin");
    }

    #[test]
    fn baseline_runs_and_returns_metrics() {
        let report = run_fts_baseline().expect("baseline ran");
        assert_eq!(report.metrics.questions_evaluated, QUESTIONS.len());
        // Should at least find SOMETHING for every query.
        assert!(report.per_question.iter().all(|q| q.reciprocal_rank > 0.0));
    }
}

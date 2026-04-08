//! LongMemEval dataset loader and runner.
//!
//! Dataset:  https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned
//! Format:   `[{question_id, question_type, question, answer, question_date,
//!             haystack_dates, haystack_session_ids, haystack_sessions,
//!             answer_session_ids}]`
//!
//! `haystack_sessions` is parallel to `haystack_session_ids` — each entry is
//! the list of message turns in that session.
//!
//! Oracle split: 500 questions × ~3 sessions per question. ~15 MB on disk.

use crate::error::BenchError;
use crate::metrics::{recall_at_k, reciprocal_rank, Metrics, Recall};
use engram_core::fusion::{reciprocal_rank_fusion, RankedHit};
use engram_core::types::{Memory, MemorySource, RetrievalSource};
use engram_embed::{Embedder, TaskMode};
use engram_rerank::{RerankCandidate, Reranker};
use engram_storage::SqliteStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongMemEvalTurn {
    pub role: String,
    pub content: String,
    #[serde(default, rename = "has_answer")]
    pub has_answer: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongMemEvalQuestion {
    pub question_id: String,
    pub question_type: String,
    pub question: String,
    /// Some questions have numeric answers (e.g., "how many"), so accept both
    /// strings and JSON numbers.
    #[serde(deserialize_with = "deserialize_answer_flex")]
    pub answer: String,
    #[serde(default)]
    pub question_date: Option<String>,
    #[serde(default)]
    pub haystack_dates: Vec<String>,
    pub haystack_session_ids: Vec<String>,
    pub haystack_sessions: Vec<Vec<LongMemEvalTurn>>,
    pub answer_session_ids: Vec<String>,
}

fn deserialize_answer_flex<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        serde_json::Value::Null => Ok(String::new()),
        other => Ok(other.to_string()),
    }
}

pub struct LongMemEvalDataset {
    pub questions: Vec<LongMemEvalQuestion>,
}

impl LongMemEvalDataset {
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, BenchError> {
        let bytes = std::fs::read(path.as_ref())?;
        let questions: Vec<LongMemEvalQuestion> = serde_json::from_slice(&bytes)?;
        if questions.is_empty() {
            return Err(BenchError::InvalidDataset("0 questions".into()));
        }
        Ok(Self { questions })
    }
}

/// Default disk path for the Oracle split (no distractors — useful for QA
/// validation only, not retrieval).
pub fn default_oracle_path() -> PathBuf {
    PathBuf::from("data/longmemeval/raw/oracle.json")
}

/// Default disk path for the S split (~48 sessions per question, real
/// retrieval benchmark — this is what MemPalace's published numbers use).
pub fn default_s_path() -> PathBuf {
    PathBuf::from("data/longmemeval/raw/s.json")
}

/// Concatenate a session's turns into a single text blob suitable for both
/// FTS5 and embedding.
fn flatten_session(turns: &[LongMemEvalTurn]) -> String {
    let mut s = String::with_capacity(turns.iter().map(|t| t.content.len() + 16).sum::<usize>());
    for t in turns {
        s.push_str(&t.role);
        s.push_str(": ");
        s.push_str(&t.content);
        s.push('\n');
    }
    s
}

fn stable_id(prefix: &str, key: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_DNS, format!("{prefix}:{key}").as_bytes())
}

fn cache_path(embedder_name: &str) -> PathBuf {
    let dir = engram_storage::paths::cache_dir().join("bench-longmemeval");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{embedder_name}.json"))
}

#[derive(Serialize, Deserialize, Default)]
struct EmbedCache {
    /// Keyed by SHA-like content fingerprint.
    blobs: HashMap<String, Vec<f32>>,
}

impl EmbedCache {
    fn load(p: &Path) -> Self {
        std::fs::read(p)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    fn save(&self, p: &Path) {
        if let Ok(b) = serde_json::to_vec(self) {
            let _ = std::fs::write(p, b);
        }
    }
}

fn fingerprint(text: &str) -> String {
    // Cheap deterministic key — content length + first/last bytes are enough
    // to avoid collisions in practice for the bench corpus.
    format!("{}:{}", text.len(), {
        let mut h: u64 = 14695981039346656037; // FNV offset
        for b in text.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        h
    })
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na.sqrt() * nb.sqrt()) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongMemEvalReport {
    pub metrics: Metrics,
    pub mode: String,
    pub questions_evaluated: usize,
    pub r1_count: usize,
    pub r5_count: usize,
    pub r10_count: usize,
}

/// Build an FTS5 query from a free-text question. Mirrors `mini::build_fts_query`.
fn build_fts_query(text: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "that", "what", "which", "how",
        "does", "are", "was", "were", "from", "into", "this", "have",
        "has", "had", "been", "being", "shown", "show", "shows",
        "can", "could", "should", "would", "may", "might",
    ];
    let mut tokens: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() { continue; }
        let lower = raw.to_ascii_lowercase();
        if lower.len() < 3 { continue; }
        if STOPWORDS.contains(&lower.as_str()) { continue; }
        tokens.push(format!("\"{}\"", lower));
    }
    tokens.join(" OR ")
}

/// Run hybrid retrieval over LongMemEval. Each question gets its own
/// in-memory store seeded with that question's haystack only, then we
/// recall and check whether retrieved chunks belong to one of the gold
/// answer sessions.
///
/// If a reranker is provided it reranks the top-50 fused candidates.
pub async fn run_oracle_hybrid<E, R>(
    dataset: &LongMemEvalDataset,
    embedder: &E,
    reranker: Option<&R>,
    rrf_k: f32,
    limit: Option<usize>,
) -> Result<LongMemEvalReport, BenchError>
where
    E: Embedder + ?Sized,
    R: Reranker + ?Sized,
{
    let cache_p = cache_path(embedder.name());
    let mut cache = EmbedCache::load(&cache_p);
    let mut cache_dirty = false;

    let n = limit
        .map(|l| l.min(dataset.questions.len()))
        .unwrap_or(dataset.questions.len());

    let mut latencies = Vec::with_capacity(n);
    let mut r1_total = 0f32;
    let mut r5_total = 0f32;
    let mut r10_total = 0f32;
    let mut mrr_total = 0f32;
    let mut r1_count = 0usize;
    let mut r5_count = 0usize;
    let mut r10_count = 0usize;

    let chrono_epoch = chrono::Utc.timestamp_opt(0, 0).single().unwrap();
    use chrono::TimeZone;

    for q in dataset.questions.iter().take(n) {
        let store = SqliteStore::open_in_memory()?;

        // session_id -> chunk_id
        let mut chunk_to_session: HashMap<Uuid, String> = HashMap::new();
        let mut chunk_embeddings: HashMap<Uuid, Vec<f32>> = HashMap::new();
        let mut seen_sids: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Pass 1: insert into store and figure out which chunks need embedding.
        struct PendingChunk { chunk_id: Uuid, text: String, key: String }
        let mut pending: Vec<PendingChunk> = Vec::new();

        for (sid, turns) in q.haystack_session_ids.iter().zip(q.haystack_sessions.iter()) {
            if !seen_sids.insert(sid.clone()) {
                continue;
            }
            let text = flatten_session(turns);
            let mem_id = stable_id("mem", &format!("{}:{sid}", q.question_id));
            let chunk_id = stable_id("chunk", &format!("{}:{sid}", q.question_id));

            let m = Memory {
                id: mem_id,
                content: text.clone(),
                created_at: chrono_epoch,
                event_time: None,
                importance: 5,
                emotional_weight: 0,
                access_count: 0,
                last_accessed: None,
                stability: 1.0,
                source: MemorySource::Conversation { thread: sid.clone(), turn: 0 },
                diary: "lme".into(),
                valid_from: None,
                valid_until: None,
                tags: vec![],
            };
            store.insert_memory(&m)?;
            store.insert_chunk(chunk_id, mem_id, &text, 0, None)?;
            chunk_to_session.insert(chunk_id, sid.clone());

            let key = fingerprint(&text);
            if let Some(v) = cache.blobs.get(&key) {
                chunk_embeddings.insert(chunk_id, v.clone());
            } else {
                pending.push(PendingChunk { chunk_id, text, key });
            }
        }

        // Pass 2: batch-embed everything that wasn't cached. ONE API call
        // (split into 100-text chunks internally) instead of N sequential calls.
        if !pending.is_empty() {
            let texts: Vec<&str> = pending.iter().map(|p| p.text.as_str()).collect();
            let vecs = embedder.embed_batch(&texts, TaskMode::RetrievalDocument).await?;
            for (p, v) in pending.into_iter().zip(vecs.into_iter()) {
                cache.blobs.insert(p.key, v.clone());
                chunk_embeddings.insert(p.chunk_id, v);
                cache_dirty = true;
            }
        }

        let start = Instant::now();

        // Lexical
        let fts_q = build_fts_query(&q.question);
        let fts_hits = if fts_q.is_empty() {
            Vec::new()
        } else {
            store.fts_search(&fts_q, 50).unwrap_or_default()
        };
        let lexical_run: Vec<RankedHit> = fts_hits
            .iter()
            .enumerate()
            .map(|(i, (id, score))| RankedHit {
                chunk_id: *id,
                rank: i + 1,
                raw_score: *score,
                source: RetrievalSource::Lexical,
            })
            .collect();

        // Dense
        let q_key = fingerprint(&q.question);
        let q_emb = if let Some(v) = cache.blobs.get(&q_key) {
            v.clone()
        } else {
            let fresh = embedder.embed_one(&q.question, TaskMode::RetrievalQuery).await?;
            cache.blobs.insert(q_key, fresh.clone());
            cache_dirty = true;
            fresh
        };
        // Iterate the chunks we actually inserted (post-dedupe).
        let mut dense_scored: Vec<(Uuid, f32)> = chunk_to_session
            .keys()
            .map(|cid| {
                let emb = chunk_embeddings.get(cid).expect("seeded");
                (*cid, cosine_sim(&q_emb, emb))
            })
            .collect();
        dense_scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let dense_run: Vec<RankedHit> = dense_scored
            .iter()
            .take(50)
            .enumerate()
            .map(|(i, (id, score))| RankedHit {
                chunk_id: *id,
                rank: i + 1,
                raw_score: *score,
                source: RetrievalSource::Dense,
            })
            .collect();

        let fused = reciprocal_rank_fusion(&[lexical_run, dense_run], rrf_k);

        // Optional rerank on the top-50 fused candidates. Cohere returns a
        // new ordering; we pipe it back into session ids.
        let retrieved_sessions: Vec<String> = if let Some(r) = reranker {
            let top_candidates: Vec<(Uuid, String)> = fused
                .iter()
                .take(50)
                .filter_map(|(id, _)| {
                    chunk_to_session.get(id).and_then(|_sid| {
                        // Get the chunk text via the store.
                        store
                            .get_chunk_content(*id)
                            .ok()
                            .flatten()
                            .map(|content| (*id, content))
                    })
                })
                .collect();

            if top_candidates.is_empty() {
                Vec::new()
            } else {
                let cands: Vec<RerankCandidate> = top_candidates
                    .iter()
                    .map(|(id, text)| RerankCandidate {
                        id: id.to_string(),
                        text: text.clone(),
                    })
                    .collect();
                let reranked = r.rerank(&q.question, &cands, 10).await?;
                reranked
                    .into_iter()
                    .filter_map(|rr| {
                        let uuid = Uuid::parse_str(&rr.id).ok()?;
                        chunk_to_session.get(&uuid).cloned()
                    })
                    .collect()
            }
        } else {
            fused
                .iter()
                .filter_map(|(id, _)| chunk_to_session.get(id).cloned())
                .collect()
        };

        let latency_ms = start.elapsed().as_millis() as u64;
        latencies.push(latency_ms);

        let r1 = recall_at_k(&retrieved_sessions, &q.answer_session_ids, 1);
        let r5 = recall_at_k(&retrieved_sessions, &q.answer_session_ids, 5);
        let r10 = recall_at_k(&retrieved_sessions, &q.answer_session_ids, 10);
        let rr = reciprocal_rank(&retrieved_sessions, &q.answer_session_ids, 10);

        r1_total += r1; r5_total += r5; r10_total += r10; mrr_total += rr;
        if r1 > 0.0 { r1_count += 1; }
        if r5 > 0.0 { r5_count += 1; }
        if r10 > 0.0 { r10_count += 1; }
    }

    if cache_dirty { cache.save(&cache_p); }

    let nf = n as f32;
    let mut metrics = Metrics::default();
    metrics.recall = Recall {
        at_1: r1_total / nf,
        at_5: r5_total / nf,
        at_10: r10_total / nf,
    };
    metrics.mrr = mrr_total / nf;
    metrics.questions_evaluated = n;
    let mean_lat = latencies.iter().sum::<u64>() as f32 / nf;
    metrics.mean_latency_ms = mean_lat;
    let mut sorted = latencies.clone();
    sorted.sort_unstable();
    metrics.p50_latency_ms = sorted[sorted.len() / 2] as f32;
    metrics.p95_latency_ms = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)] as f32;

    Ok(LongMemEvalReport {
        metrics,
        mode: format!("hybrid_{}", embedder.name()),
        questions_evaluated: n,
        r1_count,
        r5_count,
        r10_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_real_oracle_when_present() {
        let p = default_oracle_path();
        if p.exists() {
            let ds = LongMemEvalDataset::load_from_file(&p).unwrap();
            assert!(ds.questions.len() > 0);
            assert!(!ds.questions[0].haystack_session_ids.is_empty());
        }
    }
}

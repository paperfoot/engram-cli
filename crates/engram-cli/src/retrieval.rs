//! Hybrid retrieval pipeline shared by the `recall` command and the bench.
//!
//! Pipeline:
//!   query → embed (dense) → brute-force cosine over stored vectors
//!        → FTS5 lexical search
//!        → RRF fusion (deterministic tiebreak)
//!        → optional Cohere reranker
//!        → top-k

use chrono::{DateTime, Utc};
use engram_core::fusion::{reciprocal_rank_fusion, RankedHit};
use engram_core::types::RetrievalSource;
use engram_embed::{Embedder, TaskMode};
use engram_rerank::{passthrough::PassthroughReranker, RerankCandidate, Reranker};
use engram_storage::SqliteStore;
use uuid::Uuid;

pub struct Filters {
    pub diary: Option<String>,
    pub valid_at: Option<DateTime<Utc>>,
}

pub struct HybridResult {
    pub chunk_id: Uuid,
    pub content: String,
    pub score: f32,
    pub sources: Vec<RetrievalSource>,
}

pub struct HybridParams<'a> {
    pub query: &'a str,
    pub top_k: usize,
    pub rrf_k: f32,
    pub filters: Filters,
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
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// FTS5 query builder (matches mini bench + longmemeval conventions).
pub fn build_fts_query(text: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "that", "what", "which", "how",
        "does", "are", "was", "were", "from", "into", "this", "have",
        "has", "had", "been", "being", "shown", "show", "shows",
        "can", "could", "should", "would", "may", "might",
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

/// Run the full hybrid pipeline against a persistent SQLite store.
///
/// The reranker is optional — passing `None` uses a passthrough. The Cohere
/// call is only made if we actually have >1 candidate after fusion.
pub async fn hybrid_recall<E, R>(
    store: &SqliteStore,
    embedder: &E,
    reranker: Option<&R>,
    params: HybridParams<'_>,
) -> Result<Vec<HybridResult>, crate::error::CliError>
where
    E: Embedder + ?Sized,
    R: Reranker + ?Sized,
{
    // Dense: embed query, brute-force cosine over stored embeddings.
    let chunks = store.iter_chunks_with_embeddings(params.filters.diary.as_deref())?;

    let q_emb = embedder
        .embed_one(params.query, TaskMode::RetrievalQuery)
        .await?;

    let mut dense_scored: Vec<(Uuid, String, f32)> = chunks
        .iter()
        .map(|(id, content, emb, _diary)| (*id, content.clone(), cosine_sim(&q_emb, emb)))
        .collect();
    dense_scored.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let dense_top = dense_scored.iter().take(50).collect::<Vec<_>>();

    let dense_run: Vec<RankedHit> = dense_top
        .iter()
        .enumerate()
        .map(|(i, (id, _, score))| RankedHit {
            chunk_id: *id,
            rank: i + 1,
            raw_score: *score,
            source: RetrievalSource::Dense,
        })
        .collect();

    // Lexical: FTS5 BM25 — pipe through the same query builder used by the bench.
    let fts_q = build_fts_query(params.query);
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

    // Fuse. RRF with deterministic tiebreak.
    let fused = reciprocal_rank_fusion(&[dense_run, lexical_run], params.rrf_k);

    // Gather candidate content for reranking.
    let mut candidates: Vec<RerankCandidate> = Vec::new();
    let mut fused_scores: std::collections::HashMap<Uuid, f32> = std::collections::HashMap::new();
    for (id, score) in fused.iter().take(50) {
        fused_scores.insert(*id, score.0);
        if let Some(content) = dense_scored.iter().find(|(cid, _, _)| cid == id).map(|(_, c, _)| c.clone()) {
            candidates.push(RerankCandidate {
                id: id.to_string(),
                text: content,
            });
        } else if let Some(content) = store.get_chunk_content(*id)? {
            candidates.push(RerankCandidate {
                id: id.to_string(),
                text: content,
            });
        }
    }

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Optional rerank. If a real reranker is provided, it overrides fusion order.
    let reranked = if let Some(r) = reranker {
        r.rerank(params.query, &candidates, params.top_k).await?
    } else {
        let pt = PassthroughReranker;
        pt.rerank(params.query, &candidates, params.top_k).await?
    };

    let mut out = Vec::with_capacity(reranked.len());
    for r in reranked {
        let chunk_id = Uuid::parse_str(&r.id).unwrap_or(Uuid::nil());
        let content = candidates[r.original_index].text.clone();
        out.push(HybridResult {
            chunk_id,
            content,
            score: r.score,
            sources: sources_for(&chunk_id, &fused),
        });
    }

    Ok(out)
}

fn sources_for(id: &Uuid, fused: &[(Uuid, engram_core::types::Score)]) -> Vec<RetrievalSource> {
    if fused.iter().any(|(cid, _)| cid == id) {
        vec![RetrievalSource::Dense, RetrievalSource::Lexical, RetrievalSource::Reranker]
    } else {
        vec![RetrievalSource::Reranker]
    }
}

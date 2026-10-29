//! [P1] Recall's dense scan reads ids and vectors only; the text of the chunks
//! that survive fusion is fetched by id afterwards, and the query norm is
//! computed once. None of that may move a result: this pins `hybrid_recall`'s
//! ranked ids, scores and text to a straightforward reference (per-pair cosine
//! over every chunk, reciprocal rank fusion, passthrough scores), on a store
//! large enough to hold exact ties and multi-chunk memories.

use std::collections::HashMap;

use engram_cli::retrieval::{
    build_fts_query, hybrid_recall, Filters, HybridParams, RecallMode, RecallOutcome,
};
use engram_core::types::{Memory, MemorySource};
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
use engram_rerank::passthrough::PassthroughReranker;
use engram_storage::SqliteStore;
use uuid::Uuid;

const MEMORIES: usize = 1500;
const TOP_K: usize = 25;
const POOL: usize = 50;
const RRF_K: f32 = 60.0;

struct Seeded {
    store: SqliteStore,
    _dir: tempfile::TempDir,
    /// Every chunk: (id, memory id, text, the vector written for it).
    chunks: Vec<(Uuid, Uuid, String, Vec<f32>)>,
}

fn word(x: u64) -> String {
    const ON: &[&str] = &["b", "d", "f", "g", "k", "l", "m", "n", "p", "r", "s", "t"];
    const NU: &[&str] = &["a", "e", "i", "o", "u", "ai", "ou"];
    (0..3)
        .map(|i| {
            let s = x >> (i * 8);
            format!(
                "{}{}",
                ON[(s % ON.len() as u64) as usize],
                NU[((s >> 4) % NU.len() as u64) as usize]
            )
        })
        .collect()
}

fn text(seed: usize) -> String {
    let mut x = seed as u64 * 2654435761 + 11;
    (0..24)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            word(x % 4096)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn seed() -> Seeded {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("db.sqlite")).unwrap();
    let stub = StubEmbedder::default();
    let mut chunks = Vec::new();
    for i in 0..MEMORIES {
        let memory_id = Uuid::from_u128(0x1000_0000 + i as u128);
        // Every 97th memory repeats an earlier text: identical vectors, so the
        // id tie-break is part of what is compared.
        let first = if i % 97 == 96 { text(i - 50) } else { text(i) };
        let mut texts = vec![first];
        if i % 5 == 0 {
            texts.push(text(i + 100_000));
        }
        for (pos, t) in texts.into_iter().enumerate() {
            let chunk_id = Uuid::from_u128(0x2000_0000 + (i * 2 + pos) as u128);
            // The stub returns unit vectors; scaling them keeps the ranking a
            // cosine one (a dot product alone would now rank by length).
            let scale = 0.25 + (i % 7) as f32 * 0.5;
            let v: Vec<f32> = stub
                .embed_one(&t, TaskMode::RetrievalDocument)
                .await
                .unwrap()
                .into_iter()
                .map(|x| x * scale)
                .collect();
            chunks.push((chunk_id, memory_id, t, v));
        }
    }
    store
        .with_tx(|s| {
            let mut current = None;
            for (chunk_id, memory_id, t, v) in &chunks {
                if current != Some(*memory_id) {
                    current = Some(*memory_id);
                    let m = Memory {
                        id: *memory_id,
                        content: t.clone(),
                        created_at: chrono::Utc::now(),
                        event_time: None,
                        importance: 5,
                        emotional_weight: 0,
                        access_count: 0,
                        last_accessed: None,
                        stability: 1.0,
                        source: MemorySource::Manual,
                        diary: "default".into(),
                        valid_from: None,
                        valid_until: None,
                        tags: vec![],
                    };
                    let kb = if memory_id.as_u128() % 2 == 0 {
                        "personal"
                    } else {
                        "work"
                    };
                    s.insert_memory_with_kb(&m, kb)?;
                }
                s.insert_chunk_with_embedding_meta(
                    *chunk_id,
                    *memory_id,
                    t,
                    0,
                    None,
                    v,
                    &stub.model(),
                    stub.dimensions(),
                    stub.prompt_format(),
                    None,
                )?;
            }
            Ok(())
        })
        .unwrap();
    Seeded {
        store,
        _dir: dir,
        chunks,
    }
}

/// The per-pair cosine the dense scan used before the query norm was hoisted.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
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

fn by_score_then_id(a: &(Uuid, f32), b: &(Uuid, f32)) -> std::cmp::Ordering {
    b.1.partial_cmp(&a.1).unwrap().then_with(|| a.0.cmp(&b.0))
}

/// What recall must return: (id, score) for the top `TOP_K`.
async fn reference(seeded: &Seeded, query: &str) -> Vec<(Uuid, f32)> {
    let q = StubEmbedder::default()
        .embed_one(query, TaskMode::RetrievalQuery)
        .await
        .unwrap();
    let mut dense: Vec<(Uuid, f32)> = seeded
        .chunks
        .iter()
        .map(|(id, _, _, v)| (*id, cosine(&q, v)))
        .collect();
    dense.sort_by(by_score_then_id);
    dense.truncate(POOL);

    // The lexical leg did not change; read it straight from the FTS index.
    let fts = build_fts_query(query);
    let lexical = if fts.is_empty() {
        Vec::new()
    } else {
        let now = chrono::Utc::now().to_rfc3339();
        seeded
            .store
            .fts_search_scoped_filtered(&fts, None, None, POOL, Some(&now))
            .unwrap()
    };

    let mut fused: HashMap<Uuid, f32> = HashMap::new();
    for run in [&dense, &lexical] {
        for (rank, (id, _)) in run.iter().take(POOL).enumerate() {
            *fused.entry(*id).or_insert(0.0) += 1.0 / (RRF_K + (rank + 1) as f32);
        }
    }
    let mut fused: Vec<(Uuid, f32)> = fused.into_iter().collect();
    fused.sort_by(by_score_then_id);
    fused.truncate(POOL);
    let n = fused.len();
    fused
        .iter()
        .take(TOP_K)
        .enumerate()
        .map(|(i, (id, _))| (*id, 1.0 - i as f32 / n as f32))
        .collect()
}

async fn recall(seeded: &Seeded, query: &str) -> RecallOutcome {
    hybrid_recall(
        &seeded.store,
        &StubEmbedder::default(),
        None::<&PassthroughReranker>,
        HybridParams {
            query,
            top_k: TOP_K,
            rrf_k: RRF_K,
            rerank_top_n: POOL,
            filters: Filters {
                diary: None,
                kb: None,
                since: None,
                until: None,
                include_expired: false,
            },
            mode: RecallMode::Evidence,
            allow_mixed_embeddings: false,
            defer_doc_cap: false,
        },
    )
    .await
    .unwrap()
}

async fn assert_matches_reference(seeded: &Seeded, query: &str) {
    let expected = reference(seeded, query).await;
    let got = recall(seeded, query).await;
    assert_eq!(got.stats.chunks_considered, seeded.chunks.len());
    let got_ranked: Vec<(Uuid, f32)> = got.results.iter().map(|r| (r.id, r.score)).collect();
    assert_eq!(got_ranked, expected, "query {query:?}");

    let by_id: HashMap<Uuid, (Uuid, &str)> = seeded
        .chunks
        .iter()
        .map(|(id, m, t, _)| (*id, (*m, t.as_str())))
        .collect();
    for r in &got.results {
        let (memory, text) = by_id[&r.id];
        assert_eq!(r.kind, "chunk");
        assert_eq!(r.content, text, "chunk {} text", r.id);
        assert_eq!(r.provenance.memory_id, Some(memory), "chunk {}", r.id);
    }
}

#[tokio::test]
async fn dense_only_recall_matches_the_per_pair_reference() {
    let seeded = seed().await;
    // Every token is under three characters, so the FTS, claim and entity
    // legs stay empty and the order is the dense scan's alone.
    assert!(build_fts_query("it is on").is_empty());
    assert_matches_reference(&seeded, "it is on").await;
}

#[tokio::test]
async fn hybrid_recall_matches_the_reference_fusion() {
    let seeded = seed().await;
    // Words from three stored texts: the lexical leg has real hits to fuse.
    let words: Vec<String> = [3usize, 480, 1210]
        .iter()
        .map(|i| text(*i).split(' ').nth(2).unwrap().to_string())
        .collect();
    let query = words.join(" ");
    assert!(!build_fts_query(&query).is_empty());
    assert_matches_reference(&seeded, &query).await;
}

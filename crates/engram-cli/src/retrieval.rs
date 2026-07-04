//! Hybrid retrieval pipeline shared by the CLI and daemon.
//!
//! Pipeline:
//!   query -> dense Gemini/stub search -> FTS5 BM25 -> claim/wiki/entity search
//!        -> graph expansion -> RRF fusion -> optional Cohere rerank.

use chrono::{DateTime, Utc};
use engram_core::fusion::{reciprocal_rank_fusion, RankedHit};
use engram_core::types::RetrievalSource;
use engram_embed::{Embedder, TaskMode};
use engram_graph::extract_entities;
use engram_rerank::{passthrough::PassthroughReranker, RerankCandidate, Reranker};
use engram_storage::{SourceCitation, SqliteStore, StoredChunkEmbedding};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Filters {
    pub diary: Option<String>,
    pub kb: Option<String>,
    /// Inclusive lower/upper bounds on each memory's effective event time
    /// (`COALESCE(event_time, created_at)`). `recall --since/--until` map here
    /// and are actually applied to the candidate set (previously discarded).
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallMode {
    Evidence,
    Raw,
    Wiki,
    Explore,
    Agent,
}

impl RecallMode {
    pub fn parse(s: &str) -> Result<Self, crate::error::CliError> {
        match s.to_ascii_lowercase().as_str() {
            "evidence" => Ok(Self::Evidence),
            "raw" => Ok(Self::Raw),
            "wiki" => Ok(Self::Wiki),
            "explore" => Ok(Self::Explore),
            "agent" => Ok(Self::Agent),
            other => Err(crate::error::CliError::BadInput(format!(
                "unknown recall mode: {other} (expected evidence|raw|wiki|explore|agent)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::Raw => "raw",
            Self::Wiki => "wiki",
            Self::Explore => "explore",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalProfile {
    CloudQuality,
    Fast,
    Offline,
}

impl RetrievalProfile {
    pub fn parse(s: &str) -> Result<Self, crate::error::CliError> {
        match s.to_ascii_lowercase().as_str() {
            "cloud_quality" | "cloud-quality" | "cloud" => Ok(Self::CloudQuality),
            "fast" => Ok(Self::Fast),
            "offline" | "local" => Ok(Self::Offline),
            other => Err(crate::error::CliError::BadInput(format!(
                "unknown retrieval profile: {other} (expected cloud_quality|fast|offline)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CloudQuality => "cloud_quality",
            Self::Fast => "fast",
            Self::Offline => "offline",
        }
    }

    pub fn default_rerank_top_n(self) -> usize {
        match self {
            Self::CloudQuality => 200,
            Self::Fast => 20,
            Self::Offline => 50,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HybridResult {
    pub id: Uuid,
    pub kind: String,
    pub content: String,
    pub score: f32,
    pub rerank_score: Option<f32>,
    pub kb: String,
    pub citations: Vec<SourceCitation>,
    pub sources: Vec<RetrievalSource>,
}

#[derive(Debug, Clone)]
pub struct HybridParams<'a> {
    pub query: &'a str,
    pub top_k: usize,
    pub rrf_k: f32,
    pub rerank_top_n: usize,
    pub filters: Filters,
    pub mode: RecallMode,
    pub graph_hops: u8,
    pub allow_mixed_embeddings: bool,
}

#[derive(Debug, Clone)]
struct CandidateRecord {
    kind: String,
    content: String,
    kb: String,
    citations: Vec<SourceCitation>,
    /// For a claim candidate, the chunk it was derived from — used to drop a
    /// claim that merely duplicates a chunk already in the result set.
    parent_chunk: Option<Uuid>,
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
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
        "the", "and", "for", "with", "that", "what", "which", "how", "does", "are", "was", "were",
        "from", "into", "this", "have", "has", "had", "been", "being", "shown", "show", "shows",
        "can", "could", "should", "would", "may", "might",
    ];
    let mut tokens: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        if lower.len() < 3 || STOPWORDS.contains(&lower.as_str()) {
            continue;
        }
        tokens.push(format!("\"{}\"", lower));
    }
    tokens.join(" OR ")
}

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
    let expected_model = embedder.model();
    let expected_dims = embedder.dimensions();
    let expected_prompt = embedder.prompt_format().to_string();
    let kb = params.filters.kb.as_deref();
    let diary = params.filters.diary.as_deref();

    let mut candidate_records: HashMap<Uuid, CandidateRecord> = HashMap::new();
    let mut source_map: HashMap<Uuid, HashSet<RetrievalSource>> = HashMap::new();
    let mut runs: Vec<Vec<RankedHit>> = Vec::new();

    let since = params.filters.since.map(|t| t.to_rfc3339());
    let until = params.filters.until.map(|t| t.to_rfc3339());
    let chunks = store.iter_chunks_with_embedding_records_since(
        diary,
        kb,
        since.as_deref(),
        until.as_deref(),
    )?;
    guard_embedding_compatibility(
        &chunks,
        &expected_model,
        expected_dims,
        &expected_prompt,
        params.allow_mixed_embeddings,
    )?;

    // When a temporal window is active, the lexical (FTS) path — which queries
    // the whole table independently — must be restricted to the same in-window
    // chunk set as the dense path, or `--since/--until` would leak out-of-window
    // hits through FTS.
    let temporal_active = since.is_some() || until.is_some();
    let allowed_chunks: Option<HashSet<Uuid>> =
        temporal_active.then(|| chunks.iter().map(|c| c.chunk_id).collect());

    // Index chunk content/kb by id so the dense path can score all chunks
    // without cloning content or fetching citations for every one — those are
    // materialized lazily for only the post-fusion rerank pool (see below).
    let chunk_index: HashMap<Uuid, &StoredChunkEmbedding> =
        chunks.iter().map(|c| (c.chunk_id, c)).collect();

    if params.mode != RecallMode::Wiki {
        let q_emb = embedder
            .embed_one(params.query, TaskMode::RetrievalQuery)
            .await?;
        let mut dense_scored: Vec<(Uuid, f32)> = Vec::new();
        for chunk in &chunks {
            if chunk.embedding.len() != q_emb.len() {
                continue;
            }
            dense_scored.push((chunk.chunk_id, cosine_sim(&q_emb, &chunk.embedding)));
        }
        dense_scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let dense_run = ranked_run(&dense_scored, RetrievalSource::Dense, params.rerank_top_n);
        mark_sources(&mut source_map, &dense_run);
        runs.push(dense_run);
    }

    let fts_q = build_fts_query(params.query);
    if !fts_q.is_empty() && params.mode != RecallMode::Wiki {
        let mut lexical = match store.fts_search_scoped(&fts_q, kb, diary, params.rerank_top_n) {
            Ok(hits) => hits,
            Err(e) => {
                // Don't silently degrade to dense-only: a malformed FTS query or
                // SQL error is worth surfacing, but must not abort the whole
                // recall, so warn and continue with an empty lexical run.
                tracing::warn!("lexical (FTS) search failed, continuing without it: {e}");
                Vec::new()
            }
        };
        if let Some(allowed) = &allowed_chunks {
            lexical.retain(|(id, _)| allowed.contains(id));
        }
        // Chunk content/citations for lexical-only hits are resolved lazily for
        // the rerank pool (see resolve_candidate), not here — so we don't do a
        // per-hit chunk fetch for candidates that fusion will discard.
        let lexical_run = ranked_run(&lexical, RetrievalSource::Lexical, params.rerank_top_n);
        mark_sources(&mut source_map, &lexical_run);
        runs.push(lexical_run);
    }

    if !fts_q.is_empty()
        && matches!(
            params.mode,
            RecallMode::Evidence | RecallMode::Explore | RecallMode::Agent
        )
    {
        let claims = match store.claim_fts_search(&fts_q, kb, params.rerank_top_n) {
            Ok(hits) => hits,
            Err(e) => {
                tracing::warn!("claim search failed, continuing without it: {e}");
                Vec::new()
            }
        };
        for (id, _) in &claims {
            if let Some(claim) = store.get_claim(*id)? {
                // Claims found via their own FTS index are a distinct
                // provenance from raw-chunk lexical hits.
                let parent_chunk = claim.citations.iter().find_map(|c| c.chunk_id);
                candidate_records.insert(
                    *id,
                    CandidateRecord {
                        kind: "claim".to_string(),
                        content: claim.content,
                        kb: claim.kb,
                        citations: claim.citations,
                        parent_chunk,
                    },
                );
            }
        }
        let claim_run = ranked_run(&claims, RetrievalSource::Claim, params.rerank_top_n);
        mark_sources(&mut source_map, &claim_run);
        runs.push(claim_run);
    }

    if !fts_q.is_empty()
        && matches!(
            params.mode,
            RecallMode::Wiki | RecallMode::Explore | RecallMode::Agent
        )
    {
        let wiki_hits = match store.wiki_fts_search(&fts_q, kb, params.rerank_top_n) {
            Ok(hits) => hits,
            Err(e) => {
                tracing::warn!("wiki search failed, continuing without it: {e}");
                Vec::new()
            }
        };
        for (id, _) in &wiki_hits {
            if let Some(page) = store.get_wiki_page_by_id(*id)? {
                candidate_records.insert(
                    *id,
                    CandidateRecord {
                        kind: "wiki_page".to_string(),
                        content: page.content,
                        kb: page.kb,
                        citations: Vec::new(),
                        parent_chunk: None,
                    },
                );
            }
        }
        let wiki_run = ranked_run(&wiki_hits, RetrievalSource::Wiki, params.rerank_top_n);
        mark_sources(&mut source_map, &wiki_run);
        runs.push(wiki_run);
    }

    add_entity_and_graph_candidates(
        store,
        params.query,
        kb,
        params.mode,
        params.graph_hops,
        &mut candidate_records,
        &mut source_map,
        &mut runs,
    )?;

    if runs.is_empty() {
        return Ok(Vec::new());
    }

    let fused = reciprocal_rank_fusion(&runs, params.rrf_k);

    // Resolve content for ONLY the post-fusion rerank pool. Chunk candidates
    // that were never materialized (dense/lexical-only hits) get their content
    // from the in-memory chunk index — no per-candidate DB round-trip, and
    // citations are fetched lazily below for just the survivors.
    let mut rerank_candidates: Vec<RerankCandidate> = Vec::new();
    let mut candidate_ids: Vec<Uuid> = Vec::new();
    for (id, _) in fused.iter().take(params.rerank_top_n) {
        let text = if let Some(c) = candidate_records.get(id) {
            Some(c.content.clone())
        } else {
            chunk_index.get(id).map(|c| c.content.clone())
        };
        if let Some(text) = text {
            candidate_ids.push(*id);
            rerank_candidates.push(RerankCandidate {
                id: id.to_string(),
                text,
            });
        }
    }

    if rerank_candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Rerank the full candidate pool (not just top_k) so the diversity cap
    // below has enough material to spread results across distinct documents.
    let rerank_limit = rerank_candidates.len();
    let reranked = if let Some(r) = reranker {
        r.rerank(params.query, &rerank_candidates, rerank_limit)
            .await?
    } else {
        let pt = PassthroughReranker;
        pt.rerank(params.query, &rerank_candidates, rerank_limit)
            .await?
    };

    // Diversity cap: keep at most N chunks per source document so a few
    // near-duplicate passages from one paper can't crowd out other papers
    // (this lifts paper-level recall). Tune via ENGRAM_RERANK_MAX_PER_DOC;
    // 0 = unlimited (legacy behaviour).
    const DEFAULT_MAX_PER_DOC: usize = 2;
    let max_per_doc = match std::env::var("ENGRAM_RERANK_MAX_PER_DOC")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        Some(0) => usize::MAX,
        Some(n) => n,
        None => DEFAULT_MAX_PER_DOC,
    };

    let using_reranker = reranker.is_some();
    let mut out = Vec::with_capacity(params.top_k);
    let mut per_doc: HashMap<Uuid, usize> = HashMap::new();
    let mut emitted_chunks: HashSet<Uuid> = HashSet::new();
    for r in reranked {
        if out.len() >= params.top_k {
            break;
        }
        let id = candidate_ids
            .get(r.original_index)
            .copied()
            .unwrap_or_else(Uuid::nil);

        // Resolve the candidate: a materialized record (claim/wiki/entity/
        // relation) or, failing that, a raw chunk from the index. Citations
        // are fetched lazily here — for pool survivors only, not the whole KB.
        let (kind, content, cand_kb, citations, parent_chunk) =
            if let Some(c) = candidate_records.get(&id) {
                let citations = if c.kind == "chunk" && c.citations.is_empty() {
                    store.citations_for_chunk(id).unwrap_or_default()
                } else {
                    c.citations.clone()
                };
                (
                    c.kind.clone(),
                    c.content.clone(),
                    c.kb.clone(),
                    citations,
                    c.parent_chunk,
                )
            } else if let Some(chunk) = chunk_index.get(&id) {
                let citations = store.citations_for_chunk(id).unwrap_or_default();
                (
                    "chunk".to_string(),
                    chunk.content.clone(),
                    chunk.kb.clone(),
                    citations,
                    None,
                )
            } else {
                continue;
            };

        // Drop a claim whose parent chunk is already in the result set — it
        // would be a redundant restatement of a chunk we already returned.
        if kind == "claim" {
            if let Some(parent) = parent_chunk {
                if emitted_chunks.contains(&parent) {
                    continue;
                }
            }
        }

        if let Some(doc) = citations.first().and_then(|cit| cit.document_id) {
            let seen = per_doc.entry(doc).or_insert(0);
            if *seen >= max_per_doc {
                continue;
            }
            *seen += 1;
        }
        if kind == "chunk" {
            emitted_chunks.insert(id);
        }

        let mut sources: Vec<RetrievalSource> = source_map
            .get(&id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        if using_reranker && !sources.contains(&RetrievalSource::Reranker) {
            sources.push(RetrievalSource::Reranker);
        }
        sources.sort_by_key(source_rank);
        out.push(HybridResult {
            id,
            kind,
            content,
            score: r.score,
            rerank_score: using_reranker.then_some(r.score),
            kb: cand_kb,
            citations,
            sources,
        });
    }

    Ok(out)
}

fn source_rank(s: &RetrievalSource) -> u8 {
    match s {
        RetrievalSource::Dense => 0,
        RetrievalSource::Lexical => 1,
        RetrievalSource::Claim => 2,
        RetrievalSource::Wiki => 3,
        RetrievalSource::Entity => 4,
        RetrievalSource::Graph => 5,
        RetrievalSource::Reranker => 6,
    }
}

fn guard_embedding_compatibility(
    chunks: &[StoredChunkEmbedding],
    expected_model: &str,
    expected_dims: usize,
    expected_prompt: &str,
    allow_mixed: bool,
) -> Result<(), crate::error::CliError> {
    if allow_mixed {
        return Ok(());
    }
    let mismatched = chunks
        .iter()
        .filter(|c| {
            c.embed_model.as_deref() != Some(expected_model)
                || c.embed_dimensions != Some(expected_dims)
                || c.embed_prompt_format.as_deref() != Some(expected_prompt)
        })
        .take(5)
        .collect::<Vec<_>>();
    if mismatched.is_empty() {
        return Ok(());
    }
    let examples = mismatched
        .iter()
        .map(|c| {
            format!(
                "{}: model={:?} dims={:?} prompt={:?}",
                c.chunk_id, c.embed_model, c.embed_dimensions, c.embed_prompt_format
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(crate::error::CliError::Config(format!(
        "embedding metadata mismatch; refusing mixed retrieval. Expected model={expected_model} dims={expected_dims} prompt={expected_prompt}. Examples: {examples}. Run `engram reindex --kb <name>` or pass --allow-mixed-embeddings."
    )))
}

fn ranked_run(scored: &[(Uuid, f32)], source: RetrievalSource, limit: usize) -> Vec<RankedHit> {
    scored
        .iter()
        .take(limit)
        .enumerate()
        .map(|(i, (id, score))| RankedHit {
            chunk_id: *id,
            rank: i + 1,
            raw_score: *score,
            source,
        })
        .collect()
}

fn mark_sources(source_map: &mut HashMap<Uuid, HashSet<RetrievalSource>>, run: &[RankedHit]) {
    for hit in run {
        source_map
            .entry(hit.chunk_id)
            .or_default()
            .insert(hit.source);
    }
}

fn add_entity_and_graph_candidates(
    store: &SqliteStore,
    query: &str,
    kb: Option<&str>,
    mode: RecallMode,
    graph_hops: u8,
    candidate_records: &mut HashMap<Uuid, CandidateRecord>,
    source_map: &mut HashMap<Uuid, HashSet<RetrievalSource>>,
    runs: &mut Vec<Vec<RankedHit>>,
) -> Result<(), crate::error::CliError> {
    if mode == RecallMode::Raw || kb.is_none() {
        return Ok(());
    }
    let kb = kb.unwrap();
    // Match entity names from the query. `find_entity` already lowercases both
    // sides, so a lowercased query term matches a Title-Case entity. Also try
    // individual query tokens, so "rapamycin dose" matches the entity
    // "Rapamycin" even though extract_entities (which wants capitalization)
    // finds nothing in a lowercase query.
    let mut entity_names: Vec<String> = extract_entities(query);
    for tok in query.split(|c: char| !c.is_alphanumeric()) {
        if tok.len() >= 3 {
            entity_names.push(tok.to_string());
        }
    }
    entity_names.sort();
    entity_names.dedup();

    let mut entity_scored = Vec::new();
    let mut graph_scored = Vec::new();
    let mut seen_entities: HashSet<Uuid> = HashSet::new();
    for name in entity_names {
        if let Some(entity) = store.find_entity(Some(kb), &name)? {
            if !seen_entities.insert(entity.id) {
                continue;
            }
            candidate_records.insert(
                entity.id,
                CandidateRecord {
                    kind: "entity".to_string(),
                    content: format!(
                        "{} ({}) mentioned {} times in KB {}",
                        entity.canonical_name, entity.kind, entity.mention_count, entity.kb
                    ),
                    kb: entity.kb.clone(),
                    citations: Vec::new(),
                    parent_chunk: None,
                },
            );
            entity_scored.push((entity.id, entity.mention_count as f32));

            // Graph expansion only in Explore mode. co_occurs_with edges are
            // uncited text-adjacency, not evidence — injecting them into the
            // fused ranking of a precision recall (evidence/agent) let a
            // frequently-co-mentioned but irrelevant entity displace a real
            // passage. In Explore (a browsing mode) they're useful context.
            if mode == RecallMode::Explore {
                let hops = graph_hops.clamp(1, 3);
                for rel in store.graph_neighbors(kb, &entity.canonical_name, hops)? {
                    candidate_records.insert(
                        rel.id,
                        CandidateRecord {
                            kind: "relation".to_string(),
                            content: format!(
                                "{} --{}--> {}",
                                rel.from_entity, rel.predicate, rel.to_entity
                            ),
                            kb: rel.kb,
                            citations: Vec::new(),
                            parent_chunk: None,
                        },
                    );
                    graph_scored.push((rel.id, rel.weight));
                }
            }
        }
    }
    let entity_run = ranked_run(&entity_scored, RetrievalSource::Entity, 20);
    mark_sources(source_map, &entity_run);
    if !entity_run.is_empty() {
        runs.push(entity_run);
    }
    let graph_run = ranked_run(&graph_scored, RetrievalSource::Graph, 20);
    mark_sources(source_map, &graph_run);
    if !graph_run.is_empty() {
        runs.push(graph_run);
    }
    Ok(())
}

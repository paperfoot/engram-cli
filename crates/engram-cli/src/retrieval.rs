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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Filters {
    pub diary: Option<String>,
    /// KB to search. `None` means EVERY KB — that is the default scope for
    /// recall, because a memory is only findable if the search reaches the KB
    /// it was written to and callers rarely know that name at read time.
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

/// Config key (and env override) that pins the default KB for reads.
pub const DEFAULT_KB_CONFIG_KEY: &str = "retrieval.default_kb";
pub const DEFAULT_KB_ENV: &str = "ENGRAM_DEFAULT_KB";

/// Where a recall's KB scope came from. Reported in response metadata so an
/// agent can tell an all-KB search from a pinned one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbScopeOrigin {
    /// An explicit `--kb <name>`.
    Flag,
    /// `retrieval.default_kb` (or `ENGRAM_DEFAULT_KB`).
    Config,
    /// Every KB: the default, or an explicit `--all-kbs` / `--kb '*'`.
    AllKbs,
}

impl KbScopeOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Config => "config",
            Self::AllKbs => "all_kbs",
        }
    }
}

/// The KB a read command will search. `kb == None` means every KB.
#[derive(Debug, Clone)]
pub struct KbScope {
    pub kb: Option<String>,
    pub origin: KbScopeOrigin,
}

impl KbScope {
    pub fn as_deref(&self) -> Option<&str> {
        self.kb.as_deref()
    }

    /// Human/JSON label: the KB name, or `*` for an all-KB search.
    pub fn label(&self) -> &str {
        self.kb.as_deref().unwrap_or("*")
    }
}

/// Resolve the KB scope of a read command.
///
/// Precedence: explicit `--kb <name>` > `retrieval.default_kb` config > all KBs.
///
/// Searching every KB is the default because the previous default — the KB
/// literally named "default" — made a plain `engram recall` miss any memory
/// written with `--kb <something>`: the read side has no way to guess which
/// namespace a fact was filed under. `--all-kbs` and `--kb '*'` stay as
/// explicit ways to say "everything" even when a default KB is pinned.
pub fn resolve_kb_scope(explicit: Option<&str>, all_kbs: bool) -> KbScope {
    if all_kbs {
        return KbScope {
            kb: None,
            origin: KbScopeOrigin::AllKbs,
        };
    }
    if let Some(name) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return if name == "*" {
            KbScope {
                kb: None,
                origin: KbScopeOrigin::AllKbs,
            }
        } else {
            KbScope {
                kb: Some(name.to_string()),
                origin: KbScopeOrigin::Flag,
            }
        };
    }
    let configured = std::env::var(DEFAULT_KB_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| crate::commands::config::resolve_config_string(DEFAULT_KB_CONFIG_KEY));
    match configured.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() && name != "*" => KbScope {
            kb: Some(name.to_string()),
            origin: KbScopeOrigin::Config,
        },
        _ => KbScope {
            kb: None,
            origin: KbScopeOrigin::AllKbs,
        },
    }
}

/// Non-result diagnostics from a recall: what the pipeline deliberately
/// withheld. Callers surface this in response metadata so a dropped retrieval
/// source is visible to the agent instead of silently missing.
#[derive(Debug, Clone, Default)]
pub struct RecallNotes {
    /// Diary this recall was confined to. `None` = every diary.
    pub diary_scope: Option<String>,
    /// Candidates withheld by the diary-isolation guard, keyed by source name.
    pub diary_excluded: BTreeMap<&'static str, usize>,
    /// `--since` bound of this recall, as RFC3339.
    pub since: Option<String>,
    /// `--until` bound of this recall, as RFC3339.
    pub until: Option<String>,
    /// Retrieval sources skipped because a temporal window was active.
    pub temporal_excluded: BTreeSet<&'static str>,
}

impl RecallNotes {
    fn exclude(&mut self, source: &'static str, count: usize) {
        if count > 0 {
            *self.diary_excluded.entry(source).or_insert(0) += count;
        }
    }

    fn exclude_temporal(&mut self, source: &'static str) {
        self.temporal_excluded.insert(source);
    }

    /// JSON for response metadata, or `None` when no window was requested.
    ///
    /// Mirrors `diary_isolation_json`: a source the pipeline deliberately did
    /// not consult is reported, never dropped in silence.
    pub fn temporal_scope_json(&self) -> Option<serde_json::Value> {
        if self.since.is_none() && self.until.is_none() {
            return None;
        }
        Some(serde_json::json!({
            "since": self.since,
            "until": self.until,
            "excluded_sources": self.temporal_excluded.iter().collect::<Vec<_>>(),
            "reason": "derived content (claims, wiki pages, entities, relations) has no \
                       event time of its own, so it cannot be placed inside or outside the \
                       requested window; only raw memory chunks are searched",
        }))
    }

    /// JSON for response metadata, or `None` when nothing was withheld.
    pub fn diary_isolation_json(&self) -> Option<serde_json::Value> {
        if self.diary_excluded.is_empty() {
            return None;
        }
        let excluded: serde_json::Map<String, serde_json::Value> = self
            .diary_excluded
            .iter()
            .map(|(k, v)| ((*k).to_string(), serde_json::json!(v)))
            .collect();
        Some(serde_json::json!({
            "diary": self.diary_scope,
            "excluded_candidates": excluded,
            "reason": "derived content (claims, wiki pages, entities, relations) \
                       that could not be proven to come from this diary's memories",
        }))
    }
}

/// A recall's results plus the diagnostics that explain them.
#[derive(Debug, Clone, Default)]
pub struct RecallOutcome {
    pub results: Vec<HybridResult>,
    pub notes: RecallNotes,
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
) -> Result<RecallOutcome, crate::error::CliError>
where
    E: Embedder + ?Sized,
    R: Reranker + ?Sized,
{
    let expected_model = embedder.model();
    let expected_dims = embedder.dimensions();
    let expected_prompt = embedder.prompt_format().to_string();
    let kb = params.filters.kb.as_deref();
    let diary = params.filters.diary.as_deref();

    let since = params.filters.since.map(|t| t.to_rfc3339());
    let until = params.filters.until.map(|t| t.to_rfc3339());

    let mut notes = RecallNotes {
        diary_scope: params.filters.diary.clone(),
        since: since.clone(),
        until: until.clone(),
        ..RecallNotes::default()
    };

    // DIARY ISOLATION INVARIANT: a diary-scoped recall never returns content
    // derived solely from another diary's memories.
    //
    // Raw chunks are filtered by `m.diary` in SQL, but the derived layers are
    // not: claims and wiki pages are LLM/compiler rewrites of chunk text, and
    // entities/relations are aggregates over it. Returning one of those for a
    // recall scoped to diary X could hand back a paraphrase of diary Y's
    // private memory. Claims carry provenance
    // (claim_evidence -> chunks -> memories.diary) so they are filtered
    // exactly; wiki pages, entities and relations record only a `kb`, so the
    // only sound test available is KB-level confinement — a KB whose live
    // memories all belong to X can only have produced X-derived artifacts.
    // Anything withheld is reported in `RecallNotes` rather than dropped
    // silently.
    let mixed_diary_kbs = match diary {
        Some(d) => Some(store.kbs_with_memories_outside_diary(d)?),
        None => None,
    };

    let mut candidate_records: HashMap<Uuid, CandidateRecord> = HashMap::new();
    let mut source_map: HashMap<Uuid, HashSet<RetrievalSource>> = HashMap::new();
    let mut runs: Vec<Vec<RankedHit>> = Vec::new();

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

    // TEMPORAL WINDOW SEMANTICS: `--since/--until` are bounds on a memory's
    // effective event time (`COALESCE(event_time, created_at)`). Only raw
    // chunks have one.
    //
    // The lexical (FTS) path queries the whole table independently, so it is
    // restricted to the same in-window chunk set as the dense path or the
    // window would leak out-of-window hits through FTS.
    //
    // The DERIVED layers have no per-item event time at all: a wiki page is a
    // synthesis over a whole KB, an entity row an aggregate mention count, a
    // relation an edge accumulated across chunks. A claim carries evidence
    // links but is itself a rewrite with no timestamp. Admitting them would
    // let `recall --since 2025-01-01` return a wiki page compiled from a 2010
    // memory (reproduced), and filtering them "by their evidence" is
    // ill-defined the moment evidence straddles the boundary. So a windowed
    // recall is chunks-only: claim/wiki/entity/graph are EXCLUDED, and each
    // exclusion is reported in `RecallNotes` (metadata.temporal_scope) rather
    // than dropped silently — same contract as diary isolation. Drop the
    // window to search the derived layers.
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

    let claims_wanted = !fts_q.is_empty()
        && matches!(
            params.mode,
            RecallMode::Evidence | RecallMode::Explore | RecallMode::Agent
        );
    if claims_wanted && temporal_active {
        notes.exclude_temporal("claim");
    }
    if claims_wanted && !temporal_active {
        let mut claims = match store.claim_fts_search(&fts_q, kb, params.rerank_top_n) {
            Ok(hits) => hits,
            Err(e) => {
                tracing::warn!("claim search failed, continuing without it: {e}");
                Vec::new()
            }
        };
        // Diary isolation (see invariant above): keep only claims whose
        // evidence resolves into the requested diary.
        if let Some(d) = diary {
            let ids: Vec<Uuid> = claims.iter().map(|(id, _)| *id).collect();
            let visible = store.claims_visible_in_diary(&ids, d)?;
            let before = claims.len();
            claims.retain(|(id, _)| visible.contains(id));
            notes.exclude("claim", before - claims.len());
        }
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

    let wiki_wanted = !fts_q.is_empty()
        && matches!(
            params.mode,
            RecallMode::Wiki | RecallMode::Explore | RecallMode::Agent
        );
    if wiki_wanted && temporal_active {
        notes.exclude_temporal("wiki");
    }
    if wiki_wanted && !temporal_active {
        let wiki_hits = match store.wiki_fts_search(&fts_q, kb, params.rerank_top_n) {
            Ok(hits) => hits,
            Err(e) => {
                tracing::warn!("wiki search failed, continuing without it: {e}");
                Vec::new()
            }
        };
        // Diary isolation (see invariant above): a wiki page records only its
        // KB, so it is admissible only when that KB is confined to the
        // requested diary. Filtering happens BEFORE ranking so the surviving
        // pages get contiguous ranks in the fused run.
        let mut kept_wiki: Vec<(Uuid, f32)> = Vec::with_capacity(wiki_hits.len());
        let mut wiki_excluded = 0usize;
        for (id, score) in wiki_hits {
            let Some(page) = store.get_wiki_page_by_id(id)? else {
                continue;
            };
            if mixed_diary_kbs
                .as_ref()
                .is_some_and(|mixed| mixed.contains(&page.kb))
            {
                wiki_excluded += 1;
                continue;
            }
            candidate_records.insert(
                id,
                CandidateRecord {
                    kind: "wiki_page".to_string(),
                    content: page.content,
                    kb: page.kb,
                    citations: Vec::new(),
                    parent_chunk: None,
                },
            );
            kept_wiki.push((id, score));
        }
        notes.exclude("wiki", wiki_excluded);
        let wiki_run = ranked_run(&kept_wiki, RetrievalSource::Wiki, params.rerank_top_n);
        mark_sources(&mut source_map, &wiki_run);
        runs.push(wiki_run);
    }

    add_entity_and_graph_candidates(
        store,
        params.query,
        kb,
        params.mode,
        params.graph_hops,
        temporal_active,
        mixed_diary_kbs.as_ref(),
        &mut CandidateSink {
            records: &mut candidate_records,
            sources: &mut source_map,
            runs: &mut runs,
            notes: &mut notes,
        },
    )?;

    if runs.is_empty() {
        return Ok(RecallOutcome {
            results: Vec::new(),
            notes,
        });
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
        return Ok(RecallOutcome {
            results: Vec::new(),
            notes,
        });
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
    let mut results = Vec::with_capacity(params.top_k);
    let mut per_doc: HashMap<Uuid, usize> = HashMap::new();
    let mut emitted_chunks: HashSet<Uuid> = HashSet::new();
    for r in reranked {
        if results.len() >= params.top_k {
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
        results.push(HybridResult {
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

    Ok(RecallOutcome { results, notes })
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
        "embedding metadata mismatch; refusing mixed retrieval. Expected model={expected_model} dims={expected_dims} prompt={expected_prompt}. Examples: {examples}. Run `engram reindex --all` (recall now searches every KB by default), scope the search with `--kb <name>`, or pass --allow-mixed-embeddings."
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

/// Mutable collectors threaded through candidate generation.
struct CandidateSink<'a> {
    records: &'a mut HashMap<Uuid, CandidateRecord>,
    sources: &'a mut HashMap<Uuid, HashSet<RetrievalSource>>,
    runs: &'a mut Vec<Vec<RankedHit>>,
    notes: &'a mut RecallNotes,
}

/// Maximum entity rows pulled per query term. In cross-KB mode one term can
/// legitimately match one entity per KB; the cap keeps a common word from
/// flooding the fused ranking.
const ENTITY_MATCHES_PER_TERM: usize = 8;

#[allow(clippy::too_many_arguments)]
fn add_entity_and_graph_candidates(
    store: &SqliteStore,
    query: &str,
    kb: Option<&str>,
    mode: RecallMode,
    graph_hops: u8,
    temporal_active: bool,
    mixed_diary_kbs: Option<&HashSet<String>>,
    sink: &mut CandidateSink<'_>,
) -> Result<(), crate::error::CliError> {
    // Raw mode is chunks-only by definition. Every other mode runs entity and
    // graph retrieval — including the unscoped (all-KBs) case, which used to
    // bail out here and silently lose two of the pipeline's five sources.
    if mode == RecallMode::Raw {
        return Ok(());
    }
    // A windowed recall is chunks-only: entity rows are mention-count
    // aggregates and relations are accumulated edges, so neither can be
    // placed inside or outside `--since/--until`. See the temporal window
    // semantics note in `hybrid_recall`.
    if temporal_active {
        sink.notes.exclude_temporal("entity");
        if mode == RecallMode::Explore {
            sink.notes.exclude_temporal("graph");
        }
        return Ok(());
    }
    // Match entity names from the query. `find_entities` already lowercases both
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
    let mut excluded = 0usize;
    for name in entity_names {
        for entity in store.find_entities(kb, &name, ENTITY_MATCHES_PER_TERM)? {
            if !seen_entities.insert(entity.id) {
                continue;
            }
            // Diary isolation: an entity row is an aggregate over a KB's chunks
            // and carries no diary, so it is admissible only when its KB holds
            // nothing but the requested diary's memories.
            if mixed_diary_kbs.is_some_and(|mixed| mixed.contains(&entity.kb)) {
                excluded += 1;
                continue;
            }
            sink.records.insert(
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
            // Expansion runs inside the matched entity's own KB, so an
            // unscoped recall walks each KB's graph without merging them.
            if mode == RecallMode::Explore {
                let hops = graph_hops.clamp(1, 3);
                for rel in store.graph_neighbors(&entity.kb, &entity.canonical_name, hops)? {
                    sink.records.insert(
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
    sink.notes.exclude("entity", excluded);
    // Highest mention count first: `find_entities` orders within a term, but
    // candidates from different terms/KBs still have to be ranked against
    // each other before the fused run is built.
    entity_scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    graph_scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let entity_run = ranked_run(&entity_scored, RetrievalSource::Entity, 20);
    mark_sources(sink.sources, &entity_run);
    if !entity_run.is_empty() {
        sink.runs.push(entity_run);
    }
    let graph_run = ranked_run(&graph_scored, RetrievalSource::Graph, 20);
    mark_sources(sink.sources, &graph_run);
    if !graph_run.is_empty() {
        sink.runs.push(graph_run);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: only the branches that resolve WITHOUT consulting config are unit
    // tested here — reading `retrieval.default_kb` touches the user's real
    // config file, so that precedence step is covered by the CLI integration
    // tests (which run against an isolated HOME).

    #[test]
    fn explicit_kb_flag_wins() {
        let scope = resolve_kb_scope(Some("relationships"), false);
        assert_eq!(scope.kb.as_deref(), Some("relationships"));
        assert_eq!(scope.origin, KbScopeOrigin::Flag);
        assert_eq!(scope.label(), "relationships");
    }

    #[test]
    fn all_kbs_flag_overrides_an_explicit_kb() {
        let scope = resolve_kb_scope(Some("relationships"), true);
        assert!(scope.kb.is_none());
        assert_eq!(scope.origin, KbScopeOrigin::AllKbs);
        assert_eq!(scope.label(), "*");
    }

    #[test]
    fn star_kb_means_every_kb() {
        let scope = resolve_kb_scope(Some("*"), false);
        assert!(scope.kb.is_none());
        assert_eq!(scope.origin, KbScopeOrigin::AllKbs);
    }

    #[test]
    fn blank_kb_is_not_treated_as_a_name() {
        // `--kb ''` must not scope retrieval to a KB literally named "".
        let scope = resolve_kb_scope(Some("   "), true);
        assert!(scope.kb.is_none());
    }

    #[test]
    fn notes_report_nothing_when_no_source_was_withheld() {
        let notes = RecallNotes {
            diary_scope: Some("work".into()),
            ..RecallNotes::default()
        };
        assert!(notes.diary_isolation_json().is_none());
    }

    #[test]
    fn notes_report_each_withheld_source() {
        let mut notes = RecallNotes {
            diary_scope: Some("work".into()),
            ..RecallNotes::default()
        };
        notes.exclude("claim", 2);
        notes.exclude("claim", 1);
        notes.exclude("wiki", 0);
        let json = notes.diary_isolation_json().expect("exclusions reported");
        assert_eq!(json["diary"], "work");
        assert_eq!(json["excluded_candidates"]["claim"], 3);
        assert!(json["excluded_candidates"].get("wiki").is_none());
    }
}

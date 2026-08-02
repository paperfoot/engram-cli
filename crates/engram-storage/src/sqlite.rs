//! SQLite source of truth + FTS5 lexical index.

use crate::decisions::InsertOutcome;
use crate::error::StorageError;
use crate::write_path::{close_authority_for, RetractOutcome};
use engram_core::interval::{CloseAuthority, CloseReason};
use engram_core::types::Memory;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Serialize a slice of f32 into little-endian bytes for SQLite BLOB storage.
fn f32_slice_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Deserialize little-endian bytes back into a Vec<f32>.
fn f32_bytes_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

pub struct SqliteStore {
    /// `pub(crate)` so `journal`, `rebuild`, `migrate_v10`, `backup` and
    /// `export_v3` can prepare statements against the same connection —
    /// invariant 1 requires the event append and the projection write to share
    /// one transaction, which means they must share one connection.
    pub(crate) conn: Connection,
    path: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntegrityReport {
    pub ok: bool,
    pub stale_facts_from_deleted_memories: i64,
    pub orphan_chunks: i64,
    pub empty_kbs_with_derived: Vec<KbDerivedResidue>,
    pub duplicate_source_documents: Vec<DocumentDuplicateRecord>,
    /// KBs that hold content but never got a registry row — invisible to
    /// `kb list`. Healed by `engram kb repair`, not by `engram repair`:
    /// registration only ever adds rows.
    pub unregistered_kbs: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KbDerivedResidue {
    pub kb: String,
    pub active_memories: i64,
    pub documents: i64,
    pub entities: i64,
    pub relations: i64,
    pub claims: i64,
    pub wiki_pages: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocumentDuplicateRecord {
    pub document_id: Uuid,
    pub kb: String,
    pub source_path: Option<String>,
    pub active_memories: i64,
    pub active_chunks: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RepairReport {
    pub dry_run: bool,
    /// How many facts of deleted memories were FOUND. It keeps its published
    /// name and its type, and it is now always a count of rows repair left
    /// alone: nothing hard-deletes them any more.
    pub facts_from_deleted_memories_removed: i64,
    /// How many of those intervals `repair` actually closed. The rename the
    /// honest report wanted is forbidden by the published-key rule, so the new
    /// number ships beside the old one instead of replacing it.
    #[serde(default)]
    pub facts_from_deleted_memories_closed: i64,
    pub derived_kbs_cleared: Vec<String>,
    pub duplicate_source_documents: Vec<DocumentDuplicateRecord>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeBase {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub document_count: i64,
    pub memory_count: i64,
    pub chunk_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocumentRecord {
    pub id: Uuid,
    pub kb: String,
    pub title: String,
    pub source_path: Option<String>,
    pub source_url: Option<String>,
    pub doi: Option<String>,
    pub year: Option<i32>,
    pub mode: String,
    pub created_at: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredChunkEmbedding {
    pub chunk_id: Uuid,
    pub memory_id: Uuid,
    pub content: String,
    pub embedding: Vec<f32>,
    pub diary: String,
    pub kb: String,
    pub section: Option<String>,
    pub embed_model: Option<String>,
    pub embed_dimensions: Option<usize>,
    pub embed_prompt_format: Option<String>,
    pub document_id: Option<Uuid>,
    pub source: Option<String>,
}

/// A memory's two dates: its `--event-time` (when the content happened, if
/// known) and when the store recorded it. Always read together — a caller that
/// takes one without the other reintroduces the fabricated "now" that
/// `MemoryTime` in the write path exists to prevent.
pub type MemoryTimes = (
    Option<chrono::DateTime<chrono::Utc>>,
    chrono::DateTime<chrono::Utc>,
);

/// One chunk as the brute-force dense scan wants it: `(chunk id, content,
/// embedding, diary)`. Deliberately a tuple and not `ChunkRecord` — the scan
/// reads millions of these and pays for every field it does not use.
pub type ChunkEmbeddingRow = (Uuid, String, Vec<f32>, String);

/// One row of the embedding census: `(model, dimensions, prompt format, chunk
/// count)`. The dimensions and format are optional because chunks written
/// before schema v5 recorded neither.
pub type EmbeddingProfileRow = (String, Option<i64>, Option<String>, i64);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChunkRecord {
    pub chunk_id: Uuid,
    pub memory_id: Uuid,
    pub content: String,
    pub position: u32,
    pub section: Option<String>,
    pub diary: String,
    pub kb: String,
    pub document_id: Option<Uuid>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceCitation {
    pub document_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub page: Option<i64>,
    pub section: Option<String>,
    pub source: Option<String>,
    /// Char offsets `[offset_start, offset_end)` into the cited chunk locating
    /// the exact supporting text, when a precise span was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_end: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClaimRecord {
    pub id: Uuid,
    pub kb: String,
    pub content: String,
    pub evidence_level: String,
    pub confidence: f32,
    pub created_at: String,
    pub citations: Vec<SourceCitation>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EntityRecord {
    pub id: Uuid,
    pub kb: String,
    pub canonical_name: String,
    pub kind: String,
    pub mention_count: u32,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelationRecord {
    pub id: Uuid,
    pub kb: String,
    pub from_entity: String,
    pub to_entity: String,
    pub predicate: String,
    pub weight: f32,
    pub provenance: serde_json::Value,
    /// Endpoint ids, so callers can traverse the graph without re-resolving
    /// names (two people can share a display name; ids cannot collide).
    pub from_entity_id: Uuid,
    pub to_entity_id: Uuid,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WikiPageRecord {
    pub id: Uuid,
    pub kb: String,
    pub path: String,
    pub title: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageSummary {
    pub provider: String,
    pub operation: String,
    pub model: Option<String>,
    pub events: i64,
    pub request_count: i64,
    pub item_count: i64,
    pub input_tokens_estimated: i64,
    pub output_tokens_estimated: i64,
    pub search_units: f64,
    pub cost_usd_estimated: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompileJobRecord {
    pub id: Uuid,
    pub kb: String,
    pub status: String,
    pub mode: String,
    pub created_at: String,
    pub updated_at: String,
    pub message: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageBudget {
    pub scope: String,
    pub kb: Option<String>,
    pub daily_usd: Option<f64>,
    pub monthly_usd: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
}

// ----- Full-fidelity export/import rows (source-of-truth tables only;
// ----- claims/entities/relations/wiki are derived caches that `compile`
// ----- regenerates from these).

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KbExportRow {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocumentExportRow {
    pub id: String,
    pub kb: String,
    pub title: String,
    pub source_path: Option<String>,
    pub source_url: Option<String>,
    pub doi: Option<String>,
    pub authors_json: String,
    pub year: Option<i64>,
    pub mode: String,
    pub created_at: String,
    pub metadata_json: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryExportRow {
    pub id: String,
    pub content: String,
    pub created_at: String,
    pub event_time: Option<String>,
    pub importance: i64,
    pub emotional_weight: i64,
    pub access_count: i64,
    pub last_accessed: Option<String>,
    pub stability: f64,
    pub source_json: String,
    pub diary: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub tags_json: String,
    pub kb: String,
    /// WR-11: a retracted memory MUST export — its closed facts still
    /// reference it, and dropping the row makes the file unimportable (FK
    /// death on facts.source_memory_id). Serde defaults keep pre-fix v3
    /// files readable.
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChunkExportRow {
    pub id: String,
    pub memory_id: String,
    pub content: String,
    pub position: i64,
    pub section: Option<String>,
    /// Embedding BLOB (little-endian f32s) as base64, so a backup round-trip
    /// preserves dense search without re-embedding.
    pub embedding_b64: Option<String>,
    pub embed_model: Option<String>,
    pub embed_dimensions: Option<i64>,
    pub embed_prompt_format: Option<String>,
    pub document_id: Option<String>,
    pub source_start: Option<i64>,
    pub source_end: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FactExportRow {
    pub id: String,
    pub source_memory_id: String,
    pub subject: String,
    pub subject_norm: String,
    pub predicate: String,
    pub object: String,
    pub object_norm: String,
    pub confidence: f64,
    pub created_at: String,
    pub superseded_by: Option<String>,
    pub superseded_at: Option<String>,
    pub diary: String,
    pub kb: String,
    #[serde(default = "default_fact_type_str")]
    pub fact_type: String,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub valid_until: Option<String>,
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default = "default_source_kind_str")]
    pub source_kind: String,
    #[serde(default)]
    pub confirmations: i64,
    #[serde(default)]
    pub closed_by: Option<String>,
    #[serde(default)]
    pub close_reason: Option<String>,
    #[serde(default)]
    pub close_authority: Option<String>,
}

/// What [`SqliteStore::import_fact_row`] did with one row. A bare `bool`
/// could only say "landed"; the caller also has to be able to report that a
/// close was refused on the way in [G2].
#[derive(Debug, Clone, Copy, Default)]
pub struct ImportedFactRow {
    pub inserted: bool,
    pub close_rejected: bool,
}

fn default_fact_type_str() -> String {
    "assertion".to_string()
}

fn default_source_kind_str() -> String {
    "agent_extracted".to_string()
}

/// A queued, unresolved contradiction between two active facts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FactConflictRow {
    pub id: Uuid,
    pub new_fact_id: Uuid,
    pub old_fact_id: Uuid,
    pub diary: String,
    pub kb: String,
    pub detector: String,
    pub score: Option<f32>,
    pub suggested: Option<String>,
    pub note: Option<String>,
    pub status: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub resolved_by: Option<String>,
    /// The semantic key this question is about. NULL on a row whose facts are
    /// gone (an orphan the migration deliberately kept) and on any row written
    /// before wave 3 taught the write path to compute one.
    #[serde(default)]
    pub decision_key: Option<String>,
    #[serde(default)]
    pub member_key: Option<String>,
    #[serde(default)]
    pub registry_version: Option<String>,
}

/// The queue projection, in `parse_conflict_row` order.
pub(crate) fn conflict_columns(prefix: &str) -> String {
    const NAMES: [&str; 16] = [
        "id",
        "new_fact_id",
        "old_fact_id",
        "diary",
        "kb",
        "detector",
        "score",
        "suggested",
        "note",
        "status",
        "created_at",
        "resolved_at",
        "resolved_by",
        "decision_key",
        "member_key",
        "registry_version",
    ];
    NAMES
        .iter()
        .map(|n| format!("{prefix}{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) const CONFLICT_COLUMN_COUNT: usize = 16;

fn parse_conflict_row(r: &rusqlite::Row) -> rusqlite::Result<FactConflictRow> {
    let id: String = r.get(0)?;
    let new_id: String = r.get(1)?;
    let old_id: String = r.get(2)?;
    Ok(FactConflictRow {
        id: Uuid::parse_str(&id).unwrap_or(Uuid::nil()),
        new_fact_id: Uuid::parse_str(&new_id).unwrap_or(Uuid::nil()),
        old_fact_id: Uuid::parse_str(&old_id).unwrap_or(Uuid::nil()),
        diary: r.get(3)?,
        kb: r.get(4)?,
        detector: r.get(5)?,
        score: r.get(6)?,
        suggested: r.get(7)?,
        note: r.get(8)?,
        status: r.get(9)?,
        created_at: r.get(10)?,
        resolved_at: r.get(11)?,
        resolved_by: r.get(12)?,
        decision_key: r.get(13)?,
        member_key: r.get(14)?,
        registry_version: r.get(15)?,
    })
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS memories (
    id               TEXT PRIMARY KEY,
    content          TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    event_time       TEXT,
    importance       INTEGER NOT NULL DEFAULT 5,
    emotional_weight INTEGER NOT NULL DEFAULT 0,
    access_count     INTEGER NOT NULL DEFAULT 0,
    last_accessed    TEXT,
    stability        REAL NOT NULL DEFAULT 1.0,
    source_json      TEXT NOT NULL,
    diary            TEXT NOT NULL DEFAULT 'default',
    valid_from       TEXT,
    valid_until      TEXT,
    tags_json        TEXT NOT NULL DEFAULT '[]',
    deleted          INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_memories_diary ON memories(diary) WHERE deleted = 0;
CREATE INDEX IF NOT EXISTS idx_memories_created ON memories(created_at) WHERE deleted = 0;

CREATE TABLE IF NOT EXISTS chunks (
    id           TEXT PRIMARY KEY,
    memory_id    TEXT NOT NULL,
    content      TEXT NOT NULL,
    position     INTEGER NOT NULL,
    section      TEXT,
    token_count  INTEGER,
    embedding    BLOB,    -- f32[] serialized as little-endian bytes
    embed_model  TEXT,
    FOREIGN KEY(memory_id) REFERENCES memories(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chunks_memory ON chunks(memory_id);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    content,
    content='chunks',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS chunks_fts_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS chunks_fts_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES('delete', old.rowid, old.content);
END;

CREATE TRIGGER IF NOT EXISTS chunks_fts_au AFTER UPDATE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES('delete', old.rowid, old.content);
    INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TABLE IF NOT EXISTS entities (
    id              TEXT PRIMARY KEY,
    canonical_name  TEXT NOT NULL UNIQUE,
    aliases_json    TEXT NOT NULL DEFAULT '[]',
    kind            TEXT NOT NULL,
    mention_count   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_entities_kind ON entities(kind);

CREATE TABLE IF NOT EXISTS edges (
    from_id         TEXT NOT NULL,
    to_id           TEXT NOT NULL,
    kind            TEXT NOT NULL,
    weight          REAL NOT NULL DEFAULT 1.0,
    provenance_json TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (from_id, to_id, kind),
    FOREIGN KEY(from_id) REFERENCES entities(id) ON DELETE CASCADE,
    FOREIGN KEY(to_id)   REFERENCES entities(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_edges_from ON edges(from_id);
CREATE INDEX IF NOT EXISTS idx_edges_to ON edges(to_id);

-- Facts table: atomic (subject, predicate, object) triples extracted from
-- memory content. Used for contradiction detection at write time. Old facts
-- are superseded (not deleted) so the history is queryable.
CREATE TABLE IF NOT EXISTS facts (
    id               TEXT PRIMARY KEY,
    source_memory_id TEXT NOT NULL,
    subject          TEXT NOT NULL,
    subject_norm     TEXT NOT NULL,
    predicate        TEXT NOT NULL,
    object           TEXT NOT NULL,
    object_norm      TEXT NOT NULL,
    confidence       REAL NOT NULL DEFAULT 1.0,
    created_at       TEXT NOT NULL,
    superseded_by    TEXT,
    superseded_at    TEXT,
    diary            TEXT NOT NULL DEFAULT 'default',
    FOREIGN KEY(source_memory_id) REFERENCES memories(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_facts_active_lookup
    ON facts(subject_norm, predicate)
    WHERE superseded_by IS NULL;
CREATE INDEX IF NOT EXISTS idx_facts_subject ON facts(subject_norm);
CREATE INDEX IF NOT EXISTS idx_facts_diary ON facts(diary);
CREATE INDEX IF NOT EXISTS idx_facts_superseded ON facts(superseded_by);
"#;

const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS knowledge_bases (
    name        TEXT PRIMARY KEY,
    description TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS documents (
    id            TEXT PRIMARY KEY,
    kb            TEXT NOT NULL DEFAULT 'default',
    title         TEXT NOT NULL,
    source_path   TEXT,
    source_url    TEXT,
    doi           TEXT,
    authors_json  TEXT NOT NULL DEFAULT '[]',
    year          INTEGER,
    mode          TEXT NOT NULL DEFAULT 'general',
    created_at    TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_documents_kb ON documents(kb);
CREATE INDEX IF NOT EXISTS idx_documents_source_path ON documents(source_path);

CREATE TABLE IF NOT EXISTS document_sources (
    id            TEXT PRIMARY KEY,
    document_id   TEXT NOT NULL,
    kind          TEXT NOT NULL,
    uri           TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY(document_id) REFERENCES documents(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS embedding_runs (
    id            TEXT PRIMARY KEY,
    kb            TEXT NOT NULL DEFAULT 'default',
    provider      TEXT NOT NULL,
    model         TEXT NOT NULL,
    dimensions    INTEGER NOT NULL,
    prompt_format TEXT NOT NULL,
    started_at    TEXT NOT NULL,
    finished_at   TEXT,
    chunk_count   INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL DEFAULT 'running',
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS source_spans (
    id           TEXT PRIMARY KEY,
    kb           TEXT NOT NULL DEFAULT 'default',
    document_id  TEXT,
    chunk_id     TEXT,
    page         INTEGER,
    section      TEXT,
    offset_start INTEGER,
    offset_end   INTEGER,
    source       TEXT,
    text_preview TEXT,
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE,
    FOREIGN KEY(document_id) REFERENCES documents(id) ON DELETE SET NULL,
    FOREIGN KEY(chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_source_spans_kb ON source_spans(kb);
CREATE INDEX IF NOT EXISTS idx_source_spans_chunk ON source_spans(chunk_id);

CREATE TABLE IF NOT EXISTS claims (
    id             TEXT PRIMARY KEY,
    kb             TEXT NOT NULL DEFAULT 'default',
    content        TEXT NOT NULL,
    evidence_level TEXT NOT NULL DEFAULT 'unknown',
    confidence     REAL NOT NULL DEFAULT 0.7,
    created_at     TEXT NOT NULL,
    derived_from   TEXT NOT NULL DEFAULT 'compiler',
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_claims_kb ON claims(kb);
CREATE VIRTUAL TABLE IF NOT EXISTS claims_fts USING fts5(
    content,
    content='claims',
    content_rowid='rowid',
    tokenize='porter unicode61'
);
CREATE TRIGGER IF NOT EXISTS claims_fts_ai AFTER INSERT ON claims BEGIN
    INSERT INTO claims_fts(rowid, content) VALUES (new.rowid, new.content);
END;
CREATE TRIGGER IF NOT EXISTS claims_fts_ad AFTER DELETE ON claims BEGIN
    INSERT INTO claims_fts(claims_fts, rowid, content) VALUES('delete', old.rowid, old.content);
END;
CREATE TRIGGER IF NOT EXISTS claims_fts_au AFTER UPDATE ON claims BEGIN
    INSERT INTO claims_fts(claims_fts, rowid, content) VALUES('delete', old.rowid, old.content);
    INSERT INTO claims_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TABLE IF NOT EXISTS claim_evidence (
    claim_id       TEXT NOT NULL,
    source_span_id TEXT,
    chunk_id       TEXT,
    document_id    TEXT,
    PRIMARY KEY (claim_id, source_span_id, chunk_id),
    FOREIGN KEY(claim_id) REFERENCES claims(id) ON DELETE CASCADE,
    FOREIGN KEY(source_span_id) REFERENCES source_spans(id) ON DELETE CASCADE,
    FOREIGN KEY(chunk_id) REFERENCES chunks(id) ON DELETE CASCADE,
    FOREIGN KEY(document_id) REFERENCES documents(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS entity_aliases (
    id        TEXT PRIMARY KEY,
    kb        TEXT NOT NULL DEFAULT 'default',
    entity_id TEXT NOT NULL,
    alias     TEXT NOT NULL,
    alias_norm TEXT NOT NULL,
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE,
    FOREIGN KEY(entity_id) REFERENCES entities(id) ON DELETE CASCADE,
    UNIQUE(kb, alias_norm)
);
CREATE INDEX IF NOT EXISTS idx_entity_aliases_kb_norm ON entity_aliases(kb, alias_norm);

CREATE TABLE IF NOT EXISTS relations (
    id              TEXT PRIMARY KEY,
    kb              TEXT NOT NULL DEFAULT 'default',
    from_entity_id  TEXT NOT NULL,
    to_entity_id    TEXT NOT NULL,
    predicate       TEXT NOT NULL,
    weight          REAL NOT NULL DEFAULT 1.0,
    created_at      TEXT NOT NULL,
    provenance_json TEXT NOT NULL DEFAULT '[]',
    source_span_id  TEXT,
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE,
    FOREIGN KEY(from_entity_id) REFERENCES entities(id) ON DELETE CASCADE,
    FOREIGN KEY(to_entity_id) REFERENCES entities(id) ON DELETE CASCADE,
    FOREIGN KEY(source_span_id) REFERENCES source_spans(id) ON DELETE SET NULL,
    UNIQUE(kb, from_entity_id, to_entity_id, predicate)
);
CREATE INDEX IF NOT EXISTS idx_relations_kb_from ON relations(kb, from_entity_id);
CREATE INDEX IF NOT EXISTS idx_relations_kb_to ON relations(kb, to_entity_id);

CREATE TABLE IF NOT EXISTS takeaways (
    id             TEXT PRIMARY KEY,
    kb             TEXT NOT NULL DEFAULT 'default',
    document_id    TEXT,
    content        TEXT NOT NULL,
    evidence_level TEXT NOT NULL DEFAULT 'unknown',
    created_at     TEXT NOT NULL,
    citations_json TEXT NOT NULL DEFAULT '[]',
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE,
    FOREIGN KEY(document_id) REFERENCES documents(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_takeaways_kb ON takeaways(kb);

CREATE TABLE IF NOT EXISTS wiki_pages (
    id         TEXT PRIMARY KEY,
    kb         TEXT NOT NULL DEFAULT 'default',
    path       TEXT NOT NULL,
    title      TEXT NOT NULL,
    content    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE,
    UNIQUE(kb, path)
);
CREATE INDEX IF NOT EXISTS idx_wiki_pages_kb_path ON wiki_pages(kb, path);
CREATE VIRTUAL TABLE IF NOT EXISTS wiki_pages_fts USING fts5(
    title,
    content,
    content='wiki_pages',
    content_rowid='rowid',
    tokenize='porter unicode61'
);
CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_ai AFTER INSERT ON wiki_pages BEGIN
    INSERT INTO wiki_pages_fts(rowid, title, content) VALUES (new.rowid, new.title, new.content);
END;
CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_ad AFTER DELETE ON wiki_pages BEGIN
    INSERT INTO wiki_pages_fts(wiki_pages_fts, rowid, title, content) VALUES('delete', old.rowid, old.title, old.content);
END;
CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_au AFTER UPDATE ON wiki_pages BEGIN
    INSERT INTO wiki_pages_fts(wiki_pages_fts, rowid, title, content) VALUES('delete', old.rowid, old.title, old.content);
    INSERT INTO wiki_pages_fts(rowid, title, content) VALUES (new.rowid, new.title, new.content);
END;

CREATE TABLE IF NOT EXISTS compile_jobs (
    id            TEXT PRIMARY KEY,
    kb            TEXT NOT NULL DEFAULT 'default',
    status        TEXT NOT NULL DEFAULT 'queued',
    mode          TEXT NOT NULL DEFAULT 'evidence',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    message       TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_compile_jobs_kb ON compile_jobs(kb);
"#;

const SCHEMA_V3: &str = r#"
CREATE TABLE IF NOT EXISTS usage_events (
    id                       TEXT PRIMARY KEY,
    created_at               TEXT NOT NULL,
    provider                 TEXT NOT NULL,
    operation                TEXT NOT NULL,
    model                    TEXT,
    kb                       TEXT,
    diary                    TEXT,
    request_count            INTEGER NOT NULL DEFAULT 1,
    item_count               INTEGER NOT NULL DEFAULT 0,
    input_tokens_estimated   INTEGER NOT NULL DEFAULT 0,
    output_tokens_estimated  INTEGER NOT NULL DEFAULT 0,
    search_units             REAL NOT NULL DEFAULT 0,
    cost_usd_estimated       REAL NOT NULL DEFAULT 0,
    metadata_json            TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_usage_events_created ON usage_events(created_at);
CREATE INDEX IF NOT EXISTS idx_usage_events_provider ON usage_events(provider, operation, model);
CREATE INDEX IF NOT EXISTS idx_usage_events_kb ON usage_events(kb);
"#;

const SCHEMA_V4: &str = r#"
CREATE TABLE IF NOT EXISTS usage_budgets (
    scope       TEXT PRIMARY KEY,
    kb          TEXT,
    daily_usd   REAL,
    monthly_usd REAL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_usage_budgets_kb ON usage_budgets(kb);
"#;

/// A memory the fact extractor has never processed: no extraction MARKER.
/// The marker is the one source of truth. The predicate used to also count
/// "has any fact" as processed, which contradicted the write path's own
/// semantics: a caller-supplied `--facts-json` batch commits with
/// `marker: None` precisely BECAUSE the LLM extractor has not seen the memory
/// yet — and one committed caller fact then hid the memory from every future
/// backfill, so extraction the caller was promised could never happen. A
/// memory with facts but no marker is pending on purpose: backfill will run
/// extraction once, dedup absorbs the re-supplied triples, and the marker
/// retires it. Written once and shared by the backfill work queue and its
/// coverage counter so the two can never disagree about what "pending"
/// means. Requires the memories table to be aliased `m`.
const PENDING_EXTRACTION_PREDICATE: &str =
    "NOT EXISTS (SELECT 1 FROM fact_extraction_log l WHERE l.memory_id = m.id)";

const SCHEMA_V7: &str = r#"
-- Which memories the fact extractor has already looked at. Facts alone are
-- not a sufficient record: a memory that legitimately yields zero facts would
-- otherwise be re-sent to the LLM by every `facts backfill` run, and an
-- oldest-first backfill with --limit would stall on it forever.
CREATE TABLE IF NOT EXISTS fact_extraction_log (
    memory_id    TEXT PRIMARY KEY,
    extracted_at TEXT NOT NULL,
    model        TEXT,
    fact_count   INTEGER NOT NULL DEFAULT 0,
    source       TEXT NOT NULL DEFAULT 'remember',
    FOREIGN KEY(memory_id) REFERENCES memories(id) ON DELETE CASCADE
);
"#;

/// Whether a memory row (which MUST be aliased `m`) still constrains what its
/// KB's derived layer is allowed to expose. True for every live memory, and
/// ALSO for a soft-deleted one whose content has not actually been purged yet.
///
/// `forget` is a soft delete: it hard-deletes the memory's claims and source
/// spans, but the KB's wiki pages and entity/relation aggregates are rewrites
/// and roll-ups that keep the deleted text's substance until the next full
/// `compile` rebuilds them from the surviving chunks. Treating a soft-deleted
/// memory as gone therefore *widened* what a diary-scoped recall could see:
/// deleting the only memory of diary B made a mixed KB look pure to diary A,
/// and A's recall started returning wiki/entity text paraphrasing B's private,
/// deleted memory. Deleting content must never publish it.
///
/// A completed `compile` job for the KB that STARTED after the deletion is
/// proof the derived layer was rebuilt without that memory (compile reads only
/// live chunks and clears the KB's derived rows in the same transaction), so
/// such a memory stops constraining anything and the KB un-gates. `created_at`
/// — not `updated_at` — is the safe side of the comparison: the job row is
/// created before the compiler reads its chunk snapshot, so "job started after
/// the delete" implies the snapshot excluded the deleted memory.
///
/// The timestamps are compared as strings, which is exact here: every writer
/// of `compile_jobs.created_at` and `memories.deleted_at` is
/// `chrono::Utc::now().to_rfc3339()` — UTC (`+00:00`) with a variable number
/// of fractional-second digits (AutoSi: 0/3/6/9), which still sorts
/// chronologically because `+` (0x2B) is below every ASCII digit, so a
/// shorter timestamp truncated at the offset sign orders correctly against a
/// longer one. A NULL `deleted_at` (deletion time unknown) compares against a
/// far-future sentinel, so it can never be proven purged — the v8 migration
/// backfills it precisely to avoid gating legacy stores forever.
const MEMORY_CONSTRAINS_DERIVED: &str = "(
    m.deleted = 0
    OR NOT EXISTS (
        SELECT 1 FROM compile_jobs j
        WHERE j.kb = m.kb
          AND j.status = 'completed'
          AND j.created_at > COALESCE(m.deleted_at, '9999-12-31T23:59:59.999999999+00:00')
    )
)";

pub const CURRENT_VERSION: i32 = 11;

/// A fact still asserts something only while BOTH are true: no successor took
/// its slot, and its own interval has not been closed. `close_reason='stopped'`
/// sets `valid_until` and leaves `superseded_by` NULL, so the legacy
/// `superseded_by IS NULL` predicate reads a stopped medication as current —
/// the exact failure move 2 exists to prevent, introduced by move 2. Mirrors
/// `engram_core::interval::is_open_fact`; the two are one rule in two
/// languages and a test pins them equal.
const OPEN_INTERVAL_PREDICATE: &str = "(valid_until IS NULL AND superseded_by IS NULL)";

/// The fact projection, in `parse_fact_row` order. Written once so a `SELECT`
/// and its parser cannot drift: a wrong column offset mis-parses silently,
/// which is how a fact's `object` ends up in its `subject_norm`.
pub(crate) fn fact_columns(prefix: &str) -> String {
    const NAMES: [&str; 22] = [
        "id",
        "source_memory_id",
        "subject",
        "subject_norm",
        "predicate",
        "object",
        "object_norm",
        "confidence",
        "created_at",
        "superseded_by",
        "superseded_at",
        "diary",
        "fact_type",
        "valid_from",
        "valid_until",
        "resolution",
        "source_kind",
        "confirmations",
        "closed_by",
        "close_reason",
        "close_authority",
        "kb",
    ];
    NAMES
        .iter()
        .map(|n| format!("{prefix}{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// How many columns [`fact_columns`] emits — the stride between two fact
/// projections in one joined row.
pub(crate) const FACT_COLUMN_COUNT: usize = 22;

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let conn = Connection::open(path.as_ref())?;
        // Standard performance pragmas.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "cache_size", -65536)?; // 64 MB
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "mmap_size", 268435456_i64)?; // 256 MB
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Concurrent CLI + daemon access: wait for locks instead of failing
        // instantly with SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;

        let store = Self {
            conn,
            path: path.as_ref().to_path_buf(),
        };
        // The pre-v10 backup, BEFORE the migration transaction opens: VACUUM
        // INTO cannot run inside one. Only for a store that already exists and
        // is behind — a fresh store has nothing to back up, and an up-to-date
        // one must not pay for a copy on every CLI invocation.
        let mut backup_path = None;
        if let Some(v) = store.try_schema_version()? {
            if v < 10 && store.path != Path::new(":memory:") {
                // Captured, not discarded: the migration log records where
                // the rollback file went (final-review finding 5) — a backup
                // an operator cannot find is a backup that does not exist.
                backup_path = Some(crate::backup::vacuum_into_backup(&store.conn, &store.path)?);
            }
        }
        store.migrate(backup_path)?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self {
            conn,
            path: PathBuf::from(":memory:"),
        };
        store.migrate(None)?;
        Ok(store)
    }

    /// Run `f` inside a single SQLite transaction. If a transaction is
    /// already open on this connection, `f` joins it instead of nesting.
    /// On error the transaction is rolled back, so multi-step writes are
    /// all-or-nothing.
    pub fn with_tx<T>(
        &self,
        f: impl FnOnce(&Self) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        if !self.conn.is_autocommit() {
            return f(self);
        }
        // Busy is translated at the boundary so a caller can retry without
        // string-matching an error message (R11).
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(crate::error::tx_error)?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Explicit transaction control for async callers that cannot use the
    /// `with_tx` closure (e.g. compile, which awaits LLM calls between
    /// writes). Under WAL, readers keep seeing the pre-transaction snapshot
    /// until `commit`, so a long-running build-then-swap is safe.
    pub fn begin_immediate(&self) -> Result<(), StorageError> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(crate::error::tx_error)?;
        Ok(())
    }

    pub fn commit(&self) -> Result<(), StorageError> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn rollback(&self) -> Result<(), StorageError> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    fn migrate(&self, backup_path: Option<PathBuf>) -> Result<(), StorageError> {
        // Fast read-only path: an up-to-date database needs no DDL and no
        // version stamp. This keeps `open()` write-free for concurrent
        // daemon + CLI access.
        if let Ok(Some(version)) = self.try_schema_version() {
            if version == CURRENT_VERSION {
                return Ok(());
            }
            if version > CURRENT_VERSION {
                return Err(StorageError::Migration(format!(
                    "database schema version {version} is newer than this binary supports \
                     ({CURRENT_VERSION}); upgrade engram instead of downgrading the database"
                )));
            }
        }
        self.with_tx(|s| {
            s.conn.execute_batch(SCHEMA_V1)?;
            let mut stmt = s
                .conn
                .prepare("SELECT version FROM schema_version LIMIT 1")?;
            let current: i32 = stmt.query_row([], |r| r.get(0)).optional()?.unwrap_or(0);
            drop(stmt);
            if current > CURRENT_VERSION {
                return Err(StorageError::Migration(format!(
                    "database schema version {current} is newer than this binary supports \
                     ({CURRENT_VERSION}); upgrade engram instead of downgrading the database"
                )));
            }
            if current == 0 {
                s.conn.execute(
                    "INSERT INTO schema_version(version) VALUES (?1)",
                    params![1],
                )?;
            }
            if current < 2 {
                s.migrate_v2()?;
            }
            if current < 3 {
                s.conn.execute_batch(SCHEMA_V3)?;
            }
            if current < 4 {
                s.conn.execute_batch(SCHEMA_V4)?;
            }
            if current < 5 {
                s.migrate_v5()?;
            }
            if current < 6 {
                s.migrate_v6()?;
            }
            if current < 7 {
                s.conn.execute_batch(SCHEMA_V7)?;
            }
            if current < 8 {
                s.migrate_v8()?;
            }
            if current < 9 {
                s.migrate_v9()?;
            }
            if current < 10 {
                s.migrate_v10(backup_path.clone())?;
            }
            // v11 is additive-only (D25): a new table and two indexes, no
            // backfill and no row rewrite, so it takes no `VACUUM INTO`
            // backup — that rule exists for migrations that rewrite rows.
            if current < 11 {
                s.migrate_v11()?;
            }
            if current != CURRENT_VERSION {
                s.conn.execute(
                    "UPDATE schema_version SET version = ?1",
                    params![CURRENT_VERSION],
                )?;
            }
            Ok(())
        })
    }

    /// Read the schema version without creating any tables. Returns
    /// `Ok(None)` when the version table doesn't exist yet (fresh DB).
    pub(crate) fn try_schema_version(&self) -> Result<Option<i32>, StorageError> {
        let table_exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !table_exists {
            return Ok(None);
        }
        let version: Option<i32> = self
            .conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(version)
    }

    /// v5: deduplicate documents sharing (kb, source_path), then enforce
    /// uniqueness with a partial index so concurrent ingests cannot create
    /// duplicates again. The keeper is the document with the most active
    /// chunks (ties: earliest created_at); references from other rows are
    /// repointed before the duplicates are removed.
    fn migrate_v5(&self) -> Result<(), StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT kb, source_path FROM documents
             WHERE source_path IS NOT NULL
             GROUP BY kb, source_path HAVING COUNT(*) > 1",
        )?;
        let groups: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        for (kb, source_path) in groups {
            let mut stmt = self.conn.prepare(
                "SELECT d.id,
                        (SELECT COUNT(*) FROM chunks c JOIN memories m ON m.id = c.memory_id
                          WHERE c.document_id = d.id AND m.deleted = 0) AS active_chunks,
                        d.created_at
                 FROM documents d
                 WHERE d.kb = ?1 AND d.source_path = ?2
                 ORDER BY active_chunks DESC, d.created_at ASC",
            )?;
            let ids: Vec<String> = stmt
                .query_map(params![kb, source_path], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            let Some((keeper, dupes)) = ids.split_first() else {
                continue;
            };
            for dupe in dupes {
                for sql in [
                    "UPDATE chunks SET document_id = ?1 WHERE document_id = ?2",
                    "UPDATE source_spans SET document_id = ?1 WHERE document_id = ?2",
                    "UPDATE takeaways SET document_id = ?1 WHERE document_id = ?2",
                    "UPDATE document_sources SET document_id = ?1 WHERE document_id = ?2",
                ] {
                    self.conn.execute(sql, params![keeper, dupe])?;
                }
                // claim_evidence has a composite PK; repointing can collide
                // with an existing row for the keeper, so drop-then-keep.
                self.conn.execute(
                    "UPDATE OR IGNORE claim_evidence SET document_id = ?1 WHERE document_id = ?2",
                    params![keeper, dupe],
                )?;
                self.conn.execute(
                    "DELETE FROM claim_evidence WHERE document_id = ?1",
                    params![dupe],
                )?;
                self.conn
                    .execute("DELETE FROM documents WHERE id = ?1", params![dupe])?;
            }
        }
        self.conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_documents_kb_source_unique
                 ON documents(kb, source_path) WHERE source_path IS NOT NULL;",
        )?;
        Ok(())
    }

    /// v8: record WHEN a memory was soft-deleted.
    ///
    /// Without it the diary-isolation gate cannot tell a memory deleted before
    /// the last compile (genuinely purged from the derived layer) from one
    /// deleted after it (still paraphrased by live wiki pages and entity
    /// aggregates) — see `MEMORY_CONSTRAINS_DERIVED`. Rows already soft-deleted
    /// when this migration runs are stamped with the migration time: they are
    /// deleted as of now, so any compile that starts later legitimately clears
    /// them, and any compile before now cannot be credited with having done so.
    /// Indexes the entity graph's hot paths. Every statement here services a
    /// predicate that could previously only scan: the canonical lookup needs a
    /// NOCASE index because the query compares NOCASE and the default index is
    /// BINARY; the merge path filters relations and aliases by a bare entity id,
    /// which the existing `(kb, ...)` indexes cannot serve, and which foreign-key
    /// enforcement also probes on delete.
    fn migrate_v9(&self) -> Result<(), StorageError> {
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_entities_kb_name_nocase
                 ON entities(kb, canonical_name COLLATE NOCASE);
             CREATE INDEX IF NOT EXISTS idx_entity_aliases_entity_alias
                 ON entity_aliases(entity_id, alias);
             CREATE INDEX IF NOT EXISTS idx_relations_from_entity
                 ON relations(from_entity_id);
             CREATE INDEX IF NOT EXISTS idx_relations_to_entity
                 ON relations(to_entity_id);",
        )?;
        Ok(())
    }

    fn migrate_v8(&self) -> Result<(), StorageError> {
        self.add_column_if_missing("memories", "deleted_at", "TEXT")?;
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE memories SET deleted_at = ?1 WHERE deleted = 1 AND deleted_at IS NULL",
            params![now],
        )?;
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_memories_kb_diary_deleted
                 ON memories(kb, diary, deleted);
             CREATE INDEX IF NOT EXISTS idx_compile_jobs_kb_status_created
                 ON compile_jobs(kb, status, created_at);",
        )?;
        Ok(())
    }

    /// v6: write-path v2. Facts gain embeddings, a semantic type, bi-temporal
    /// validity (event time alongside transaction time), provenance kind,
    /// confirmation counts, and a resolution audit field. A conflict queue
    /// holds unresolved contradictions for cooperative adjudication: the
    /// calling agent (which has conversation context) resolves them via
    /// `facts resolve`, or a later consolidation sweep does.
    fn migrate_v6(&self) -> Result<(), StorageError> {
        self.add_column_if_missing("facts", "embedding", "BLOB")?;
        self.add_column_if_missing("facts", "embed_model", "TEXT")?;
        self.add_column_if_missing("facts", "fact_type", "TEXT NOT NULL DEFAULT 'assertion'")?;
        self.add_column_if_missing("facts", "valid_from", "TEXT")?;
        self.add_column_if_missing("facts", "valid_until", "TEXT")?;
        self.add_column_if_missing("facts", "resolution", "TEXT")?;
        self.add_column_if_missing(
            "facts",
            "source_kind",
            "TEXT NOT NULL DEFAULT 'agent_extracted'",
        )?;
        self.add_column_if_missing("facts", "confirmations", "INTEGER NOT NULL DEFAULT 0")?;
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_facts_source_memory
                 ON facts(source_memory_id);
             CREATE TABLE IF NOT EXISTS fact_conflict_queue (
                 id           TEXT PRIMARY KEY,
                 new_fact_id  TEXT NOT NULL,
                 old_fact_id  TEXT NOT NULL,
                 diary        TEXT NOT NULL DEFAULT 'default',
                 kb           TEXT NOT NULL DEFAULT 'default',
                 detector     TEXT NOT NULL,
                 score        REAL,
                 suggested    TEXT,
                 note         TEXT,
                 status       TEXT NOT NULL DEFAULT 'open',
                 created_at   TEXT NOT NULL,
                 resolved_at  TEXT,
                 resolved_by  TEXT,
                 UNIQUE(new_fact_id, old_fact_id),
                 FOREIGN KEY(new_fact_id) REFERENCES facts(id) ON DELETE CASCADE,
                 FOREIGN KEY(old_fact_id) REFERENCES facts(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_fcq_open
                 ON fact_conflict_queue(status, created_at)
                 WHERE status = 'open';",
        )?;
        Ok(())
    }

    fn migrate_v2(&self) -> Result<(), StorageError> {
        self.rebuild_entities_for_kb_scope()?;
        self.conn.execute_batch(SCHEMA_V2)?;
        self.add_column_if_missing("memories", "kb", "TEXT NOT NULL DEFAULT 'default'")?;
        self.add_column_if_missing("chunks", "embed_dimensions", "INTEGER")?;
        self.add_column_if_missing("chunks", "embed_prompt_format", "TEXT")?;
        self.add_column_if_missing("chunks", "document_id", "TEXT")?;
        self.add_column_if_missing("chunks", "source_start", "INTEGER")?;
        self.add_column_if_missing("chunks", "source_end", "INTEGER")?;
        self.add_column_if_missing("facts", "kb", "TEXT NOT NULL DEFAULT 'default'")?;
        self.add_column_if_missing("entities", "kb", "TEXT NOT NULL DEFAULT 'default'")?;
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_memories_kb ON memories(kb) WHERE deleted = 0;
             CREATE INDEX IF NOT EXISTS idx_chunks_document ON chunks(document_id);
             CREATE INDEX IF NOT EXISTS idx_chunks_embed_meta ON chunks(embed_model, embed_dimensions, embed_prompt_format);
             CREATE INDEX IF NOT EXISTS idx_facts_kb ON facts(kb);
             CREATE INDEX IF NOT EXISTS idx_entities_kb_name ON entities(kb, canonical_name);",
        )?;
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR IGNORE INTO knowledge_bases(name, description, created_at, updated_at)
             VALUES ('default', 'Default migrated memory and knowledge base', ?1, ?1)",
            params![now],
        )?;
        self.conn.execute(
            "UPDATE memories SET kb = 'default' WHERE kb IS NULL OR kb = ''",
            [],
        )?;
        self.conn.execute(
            "UPDATE facts SET kb = 'default' WHERE kb IS NULL OR kb = ''",
            [],
        )?;
        self.conn.execute(
            "UPDATE entities SET kb = 'default' WHERE kb IS NULL OR kb = ''",
            [],
        )?;
        Ok(())
    }

    fn rebuild_entities_for_kb_scope(&self) -> Result<(), StorageError> {
        if self.column_exists("entities", "kb")? {
            return Ok(());
        }
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS entities_v1_global;
             ALTER TABLE entities RENAME TO entities_v1_global;
             CREATE TABLE entities (
                id              TEXT PRIMARY KEY,
                kb              TEXT NOT NULL DEFAULT 'default',
                canonical_name  TEXT NOT NULL,
                aliases_json    TEXT NOT NULL DEFAULT '[]',
                kind            TEXT NOT NULL,
                mention_count   INTEGER NOT NULL DEFAULT 0,
                UNIQUE(kb, canonical_name)
             );
             INSERT OR IGNORE INTO entities(id, kb, canonical_name, aliases_json, kind, mention_count)
                SELECT id, 'default', canonical_name, aliases_json, kind, mention_count
                FROM entities_v1_global;
             CREATE INDEX IF NOT EXISTS idx_entities_kind ON entities(kind);
             CREATE INDEX IF NOT EXISTS idx_entities_kb_name ON entities(kb, canonical_name);",
        )?;
        Ok(())
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool, StorageError> {
        let pragma = format!("PRAGMA table_info({table})");
        let mut stmt = self.conn.prepare(&pragma)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn add_column_if_missing(
        &self,
        table: &str,
        column: &str,
        definition: &str,
    ) -> Result<(), StorageError> {
        if self.column_exists(table, column)? {
            return Ok(());
        }
        let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {definition}");
        self.conn.execute_batch(&sql)?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<i32, StorageError> {
        let version = self
            .conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        Ok(version)
    }

    pub fn ensure_kb(&self, name: &str, description: Option<&str>) -> Result<(), StorageError> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO knowledge_bases(name, description, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(name) DO UPDATE SET
                description = COALESCE(excluded.description, knowledge_bases.description),
                updated_at = excluded.updated_at",
            params![name, description, now],
        )?;
        Ok(())
    }

    /// KB names that hold live memories but have no row in `knowledge_bases`.
    ///
    /// Stores written by builds that predated write-time registration have
    /// memories in KBs that `kb list` cannot see — invisible to anyone
    /// browsing the store, and silently absent from any workflow that
    /// enumerates KBs.
    ///
    /// Deliberately keyed on non-deleted memories only. `delete_kb` drops the
    /// registry row and soft-deletes the memories but leaves `documents` rows
    /// in place, so scanning documents (or any table `delete_kb` does not
    /// clear) would make repair silently resurrect a KB the user deleted.
    /// Chunks need no separate scan: they hang off memories.
    pub fn unregistered_kbs(&self) -> Result<Vec<String>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT kb FROM memories
             WHERE deleted = 0 AND kb IS NOT NULL AND kb <> ''
               AND kb NOT IN (SELECT name FROM knowledge_bases)
             ORDER BY kb ASC",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Register every KB that owns content but is missing from the registry.
    /// Idempotent: a healed store reports an empty list on the next run.
    pub fn repair_kb_registry(&self) -> Result<Vec<String>, StorageError> {
        let missing = self.unregistered_kbs()?;
        if missing.is_empty() {
            return Ok(missing);
        }
        self.with_tx(|s| {
            for name in &missing {
                s.ensure_kb(name, Some("Recovered by `engram kb repair`"))?;
            }
            Ok(())
        })?;
        Ok(missing)
    }

    pub fn list_kbs(&self) -> Result<Vec<KnowledgeBase>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT kb.name, kb.description, kb.created_at, kb.updated_at,
                    (SELECT COUNT(*) FROM documents d WHERE d.kb = kb.name),
                    (SELECT COUNT(*) FROM memories m WHERE m.kb = kb.name AND m.deleted = 0),
                    (SELECT COUNT(*) FROM chunks c JOIN memories m ON m.id = c.memory_id
                        WHERE m.kb = kb.name AND m.deleted = 0)
             FROM knowledge_bases kb
             ORDER BY kb.name ASC",
        )?;
        let rows = stmt.query_map([], parse_kb_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn get_kb(&self, name: &str) -> Result<Option<KnowledgeBase>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT kb.name, kb.description, kb.created_at, kb.updated_at,
                    (SELECT COUNT(*) FROM documents d WHERE d.kb = kb.name),
                    (SELECT COUNT(*) FROM memories m WHERE m.kb = kb.name AND m.deleted = 0),
                    (SELECT COUNT(*) FROM chunks c JOIN memories m ON m.id = c.memory_id
                        WHERE m.kb = kb.name AND m.deleted = 0)
             FROM knowledge_bases kb
             WHERE kb.name = ?1",
        )?;
        stmt.query_row(params![name], parse_kb_row)
            .optional()
            .map_err(StorageError::from)
    }

    pub fn delete_kb(&self, name: &str) -> Result<bool, StorageError> {
        if name == "default" {
            return Err(StorageError::Migration(
                "refusing to delete default KB".into(),
            ));
        }
        if self.get_kb(name)?.is_none() {
            return Ok(false);
        }
        self.with_tx(|s| {
            s.conn.execute(
                "UPDATE memories SET deleted = 1, deleted_at = ?2 WHERE kb = ?1 AND deleted = 0",
                params![name, chrono::Utc::now().to_rfc3339()],
            )?;
            s.conn
                .execute("DELETE FROM facts WHERE kb = ?1", params![name])?;
            s.clear_derived_for_kb(name)?;
            let n = s
                .conn
                .execute("DELETE FROM knowledge_bases WHERE name = ?1", params![name])?;
            Ok(n > 0)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_usage_event(
        &self,
        provider: &str,
        operation: &str,
        model: Option<&str>,
        kb: Option<&str>,
        diary: Option<&str>,
        request_count: i64,
        item_count: i64,
        input_tokens_estimated: i64,
        output_tokens_estimated: i64,
        search_units: f64,
        cost_usd_estimated: f64,
        metadata: serde_json::Value,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO usage_events(
                id, created_at, provider, operation, model, kb, diary,
                request_count, item_count, input_tokens_estimated,
                output_tokens_estimated, search_units, cost_usd_estimated, metadata_json
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                id.to_string(),
                now,
                provider,
                operation,
                model,
                kb,
                diary,
                request_count,
                item_count,
                input_tokens_estimated,
                output_tokens_estimated,
                search_units,
                cost_usd_estimated,
                serde_json::to_string(&metadata)?,
            ],
        )?;
        Ok(id)
    }

    pub fn usage_summary(
        &self,
        kb: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<UsageSummary>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT provider, operation, model,
                    COUNT(*) AS events,
                    COALESCE(SUM(request_count), 0),
                    COALESCE(SUM(item_count), 0),
                    COALESCE(SUM(input_tokens_estimated), 0),
                    COALESCE(SUM(output_tokens_estimated), 0),
                    COALESCE(SUM(search_units), 0.0),
                    COALESCE(SUM(cost_usd_estimated), 0.0)
             FROM usage_events
             WHERE (?1 IS NULL OR kb = ?1)
               AND (?2 IS NULL OR created_at >= ?2)
             GROUP BY provider, operation, model
             ORDER BY provider ASC, operation ASC, model ASC",
        )?;
        let rows = stmt.query_map(params![kb, since], |r| {
            Ok(UsageSummary {
                provider: r.get(0)?,
                operation: r.get(1)?,
                model: r.get(2)?,
                events: r.get(3)?,
                request_count: r.get(4)?,
                item_count: r.get(5)?,
                input_tokens_estimated: r.get(6)?,
                output_tokens_estimated: r.get(7)?,
                search_units: r.get(8)?,
                cost_usd_estimated: r.get(9)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn upsert_usage_budget(
        &self,
        scope: &str,
        kb: Option<&str>,
        daily_usd: Option<f64>,
        monthly_usd: Option<f64>,
    ) -> Result<(), StorageError> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO usage_budgets(scope, kb, daily_usd, monthly_usd, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(scope) DO UPDATE SET
                kb = excluded.kb,
                daily_usd = excluded.daily_usd,
                monthly_usd = excluded.monthly_usd,
                updated_at = excluded.updated_at",
            params![scope, kb, daily_usd, monthly_usd, now],
        )?;
        Ok(())
    }

    pub fn get_usage_budget(&self, scope: &str) -> Result<Option<UsageBudget>, StorageError> {
        self.conn
            .query_row(
                "SELECT scope, kb, daily_usd, monthly_usd, created_at, updated_at
                 FROM usage_budgets WHERE scope = ?1",
                params![scope],
                parse_usage_budget_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn delete_usage_budget(&self, scope: &str) -> Result<bool, StorageError> {
        let n = self
            .conn
            .execute("DELETE FROM usage_budgets WHERE scope = ?1", params![scope])?;
        Ok(n > 0)
    }

    pub fn insert_memory(&self, m: &Memory) -> Result<(), StorageError> {
        self.insert_memory_with_kb(m, "default")
    }

    pub fn insert_memory_with_kb(&self, m: &Memory, kb: &str) -> Result<(), StorageError> {
        self.ensure_kb(kb, None)?;
        // The row and its event are one transaction (invariant 1). `with_tx`
        // joins an already-open one rather than nesting, so a caller that
        // wraps a whole write in its own transaction still gets exactly one.
        self.with_tx(|s| s.insert_memory_row(m, kb))
    }

    fn insert_memory_row(&self, m: &Memory, kb: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO memories(id, content, created_at, event_time, importance,
                emotional_weight, access_count, last_accessed, stability, source_json,
                diary, valid_from, valid_until, tags_json, kb)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                m.id.to_string(),
                m.content,
                m.created_at.to_rfc3339(),
                m.event_time.map(|t| t.to_rfc3339()),
                m.importance,
                m.emotional_weight,
                m.access_count,
                m.last_accessed.map(|t| t.to_rfc3339()),
                m.stability,
                serde_json::to_string(&m.source)?,
                m.diary,
                m.valid_from.map(|t| t.to_rfc3339()),
                m.valid_until.map(|t| t.to_rfc3339()),
                serde_json::to_string(&m.tags)?,
                kb,
            ],
        )?;
        self.append_event(
            &crate::journal::EventDraft::new(
                engram_core::events::EventKind::MemoryWritten,
                crate::journal::cli_actor(&m.diary),
                crate::journal::payload::memory_written(
                    &engram_core::keys::content_hash(&m.content),
                    m.importance as i64,
                    &m.tags,
                    m.event_time.map(|t| t.to_rfc3339()).as_deref(),
                    "remember",
                    false,
                ),
            )
            .scope(Some(kb), Some(&m.diary))
            .subject(m.id),
        )?;
        Ok(())
    }

    /// Find an existing non-deleted memory with identical content in the same
    /// (kb, diary). Used by `remember` to dedup: saving the same fact twice
    /// should not create a second row (and a second embedding charge).
    pub fn find_duplicate_memory(
        &self,
        kb: &str,
        diary: &str,
        content: &str,
    ) -> Result<Option<Uuid>, StorageError> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM memories
                 WHERE deleted = 0 AND kb = ?1 AND diary = ?2 AND content = ?3
                 ORDER BY created_at ASC LIMIT 1",
                params![kb, diary, content],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    pub fn count_memories(&self) -> Result<i64, StorageError> {
        let n: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM memories WHERE deleted = 0", [], |r| {
                    r.get(0)
                })?;
        Ok(n)
    }

    /// Insert a chunk. Caller must ensure the parent memory exists.
    pub fn insert_chunk(
        &self,
        chunk_id: Uuid,
        memory_id: Uuid,
        content: &str,
        position: u32,
        section: Option<&str>,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO chunks(id, memory_id, content, position, section)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                chunk_id.to_string(),
                memory_id.to_string(),
                content,
                position,
                section
            ],
        )?;
        Ok(())
    }

    /// Insert a chunk together with its embedding. The embedding is stored
    /// as a BLOB of little-endian f32s.
    // Every argument is one non-defaultable column of `chunks`; a struct here
    // would only move the same list one call frame further out.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_chunk_with_embedding(
        &self,
        chunk_id: Uuid,
        memory_id: Uuid,
        content: &str,
        position: u32,
        section: Option<&str>,
        embedding: &[f32],
        embed_model: &str,
    ) -> Result<(), StorageError> {
        self.insert_chunk_with_embedding_meta(
            chunk_id,
            memory_id,
            content,
            position,
            section,
            embedding,
            embed_model,
            embedding.len(),
            "legacy",
            None,
        )
    }

    // As above, plus the three embedding-provenance columns that make a
    // mixed-embedding store diagnosable.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_chunk_with_embedding_meta(
        &self,
        chunk_id: Uuid,
        memory_id: Uuid,
        content: &str,
        position: u32,
        section: Option<&str>,
        embedding: &[f32],
        embed_model: &str,
        embed_dimensions: usize,
        embed_prompt_format: &str,
        document_id: Option<Uuid>,
    ) -> Result<(), StorageError> {
        let blob = f32_slice_to_bytes(embedding);
        self.conn.execute(
            "INSERT INTO chunks(id, memory_id, content, position, section, embedding, embed_model,
                embed_dimensions, embed_prompt_format, document_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                chunk_id.to_string(),
                memory_id.to_string(),
                content,
                position,
                section,
                blob,
                embed_model,
                embed_dimensions as i64,
                embed_prompt_format,
                document_id.map(|id| id.to_string()),
            ],
        )?;
        Ok(())
    }

    /// Attach an embedding to an already-inserted chunk.
    pub fn set_chunk_embedding(
        &self,
        chunk_id: Uuid,
        embedding: &[f32],
        embed_model: &str,
    ) -> Result<(), StorageError> {
        self.set_chunk_embedding_meta(chunk_id, embedding, embed_model, embedding.len(), "legacy")
    }

    pub fn set_chunk_embedding_meta(
        &self,
        chunk_id: Uuid,
        embedding: &[f32],
        embed_model: &str,
        embed_dimensions: usize,
        embed_prompt_format: &str,
    ) -> Result<(), StorageError> {
        let blob = f32_slice_to_bytes(embedding);
        self.conn.execute(
            "UPDATE chunks
             SET embedding = ?1, embed_model = ?2, embed_dimensions = ?3, embed_prompt_format = ?4
             WHERE id = ?5",
            params![
                blob,
                embed_model,
                embed_dimensions as i64,
                embed_prompt_format,
                chunk_id.to_string()
            ],
        )?;
        Ok(())
    }

    /// List all chunks (id, content, embedding, diary) matching filters.
    /// Used by `dense_search` which does brute-force cosine in Rust.
    pub fn iter_chunks_with_embeddings(
        &self,
        diary: Option<&str>,
    ) -> Result<Vec<ChunkEmbeddingRow>, StorageError> {
        let sql = match diary {
            Some(_) => {
                "SELECT c.id, c.content, c.embedding, m.diary
                 FROM chunks c
                 JOIN memories m ON m.id = c.memory_id
                 WHERE c.embedding IS NOT NULL AND m.deleted = 0 AND m.diary = ?1"
            }
            None => {
                "SELECT c.id, c.content, c.embedding, m.diary
                 FROM chunks c
                 JOIN memories m ON m.id = c.memory_id
                 WHERE c.embedding IS NOT NULL AND m.deleted = 0"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mut out = Vec::new();
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<(Uuid, String, Vec<f32>, String)> {
            let id_str: String = r.get(0)?;
            let content: String = r.get(1)?;
            let blob: Vec<u8> = r.get(2)?;
            let diary: String = r.get(3)?;
            Ok((
                Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
                content,
                f32_bytes_to_vec(&blob),
                diary,
            ))
        };
        let rows_iter: Box<dyn Iterator<Item = rusqlite::Result<ChunkEmbeddingRow>>> = match diary {
            Some(d) => {
                let iter = stmt.query_map(params![d], mapper)?;
                Box::new(iter.collect::<Vec<_>>().into_iter())
            }
            None => {
                let iter = stmt.query_map([], mapper)?;
                Box::new(iter.collect::<Vec<_>>().into_iter())
            }
        };
        for row in rows_iter {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn iter_chunks_with_embedding_records(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
    ) -> Result<Vec<StoredChunkEmbedding>, StorageError> {
        self.iter_chunks_with_embedding_records_since(diary, kb, None, None)
    }

    /// Like `iter_chunks_with_embedding_records` but with optional temporal
    /// bounds. `since`/`until` are RFC3339 timestamps compared against each
    /// memory's effective event time `COALESCE(event_time, created_at)`, so a
    /// `recall --since/--until` actually scopes results to that window instead
    /// of silently ignoring the flag.
    pub fn iter_chunks_with_embedding_records_since(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<StoredChunkEmbedding>, StorageError> {
        let mut sql = String::from(
            "SELECT c.id, c.memory_id, c.content, c.embedding, m.diary, m.kb, c.section,
                    c.embed_model, c.embed_dimensions, c.embed_prompt_format,
                    c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE c.embedding IS NOT NULL AND m.deleted = 0",
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(d) = diary {
            binds.push(Box::new(d.to_string()));
            sql.push_str(&format!(" AND m.diary = ?{}", binds.len()));
        }
        if let Some(k) = kb {
            binds.push(Box::new(k.to_string()));
            sql.push_str(&format!(" AND m.kb = ?{}", binds.len()));
        }
        if let Some(s) = since {
            binds.push(Box::new(s.to_string()));
            sql.push_str(&format!(
                " AND COALESCE(m.event_time, m.created_at) >= ?{}",
                binds.len()
            ));
        }
        if let Some(u) = until {
            binds.push(Box::new(u.to_string()));
            sql.push_str(&format!(
                " AND COALESCE(m.event_time, m.created_at) <= ?{}",
                binds.len()
            ));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
                parse_chunk_embedding_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_chunks_for_kb(&self, kb: &str) -> Result<Vec<ChunkRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.memory_id, c.content, c.position, c.section, m.diary, m.kb,
                    c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE m.deleted = 0 AND m.kb = ?1
             ORDER BY c.position ASC, c.id ASC",
        )?;
        let rows = stmt.query_map(params![kb], parse_chunk_record_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn list_chunks_for_reindex(
        &self,
        kb: Option<&str>,
    ) -> Result<Vec<ChunkRecord>, StorageError> {
        let sql = if kb.is_some() {
            "SELECT c.id, c.memory_id, c.content, c.position, c.section, m.diary, m.kb,
                    c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE m.deleted = 0 AND m.kb = ?1
             ORDER BY m.kb ASC, c.position ASC"
        } else {
            "SELECT c.id, c.memory_id, c.content, c.position, c.section, m.diary, m.kb,
                    c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE m.deleted = 0
             ORDER BY m.kb ASC, c.position ASC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(params![k], parse_chunk_record_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], parse_chunk_record_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Every live chunk, grouped by parent memory and ordered by position.
    ///
    /// `list_chunks_for_kb` orders by `position` globally, which interleaves
    /// chunks from different memories — useless to anything that needs a
    /// memory's chunk SEQUENCE, such as `chunks repair` merging a degenerate
    /// fragment into the neighbour it actually belongs next to. `None` spans
    /// every KB.
    pub fn list_chunks_by_memory(
        &self,
        kb: Option<&str>,
    ) -> Result<Vec<ChunkRecord>, StorageError> {
        let sql = if kb.is_some() {
            "SELECT c.id, c.memory_id, c.content, c.position, c.section, m.diary, m.kb,
                    c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE m.deleted = 0 AND m.kb = ?1
             ORDER BY c.memory_id ASC, c.position ASC, c.id ASC"
        } else {
            "SELECT c.id, c.memory_id, c.content, c.position, c.section, m.diary, m.kb,
                    c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE m.deleted = 0
             ORDER BY c.memory_id ASC, c.position ASC, c.id ASC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(params![k], parse_chunk_record_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], parse_chunk_record_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Every chunk of one memory, in position order. `list_chunks_by_memory`
    /// spans a whole KB and filters deleted memories out; this answers "what
    /// text does THIS memory consist of", which a provenance surface needs
    /// even for a memory that was later retracted.
    pub fn chunks_for_memory(&self, memory_id: Uuid) -> Result<Vec<ChunkRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.memory_id, c.content, c.position, c.section, m.diary, m.kb,
                    c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE c.memory_id = ?1
             ORDER BY c.position ASC, c.id ASC",
        )?;
        let rows = stmt.query_map(params![memory_id.to_string()], parse_chunk_record_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Fetch chunk content by id.
    pub fn get_chunk_content(&self, chunk_id: Uuid) -> Result<Option<String>, StorageError> {
        let r = self
            .conn
            .query_row(
                "SELECT content FROM chunks WHERE id = ?1",
                params![chunk_id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(r)
    }

    /// Mark a memory as deleted (soft delete), CLOSING the intervals it
    /// derived instead of deleting them.
    ///
    /// This is the headline defect of v0.3: `forget` hard-deleted the memory's
    /// facts, and the FK cascade on `fact_conflict_queue` then erased every
    /// open and resolved adjudication row that touched them. A retraction is
    /// information — "this stopped being a source" — not an erasure, and the
    /// questions a person already answered about those facts must survive it.
    /// Facts therefore remain visible in history listings as closed intervals;
    /// that is a deliberate value change on a published surface.
    ///
    /// `deleted_at` is stamped so the diary-isolation gate can tell whether a
    /// later compile has actually purged this memory from the KB's derived
    /// layer — see `MEMORY_CONSTRAINS_DERIVED`.
    pub fn soft_delete_memory(
        &self,
        memory_id: Uuid,
    ) -> Result<Option<RetractOutcome>, StorageError> {
        self.with_tx(|s| {
            let Some((kb, diary, already_deleted)) = s.memory_scope_including_deleted(memory_id)?
            else {
                return Ok(None);
            };
            if already_deleted {
                return Ok(Some(RetractOutcome::default()));
            }
            s.conn.execute(
                "UPDATE memories SET deleted = 1, deleted_at = ?2 WHERE id = ?1 AND deleted = 0",
                params![memory_id.to_string(), chrono::Utc::now().to_rfc3339()],
            )?;
            // AFTER the soft delete, in the same transaction — which is why
            // `close_facts_for_memory` must not filter on `deleted = 0`.
            let closed = s.close_facts_for_memory(
                memory_id,
                chrono::Utc::now(),
                CloseReason::RetractedSource,
                &CloseAuthority::Caller {
                    actor: crate::journal::cli_actor(&diary),
                },
                &crate::journal::cli_actor(&diary),
            )?;
            s.delete_derived_for_memory_chunks(memory_id)?;
            s.append_event(
                &crate::journal::EventDraft::new(
                    engram_core::events::EventKind::MemoryRetracted,
                    crate::journal::cli_actor(&diary),
                    crate::journal::payload::memory_retracted(memory_id, Some("forget")),
                )
                .scope(Some(&kb), Some(&diary))
                .subject(memory_id),
            )?;
            Ok(Some(RetractOutcome {
                retracted: true,
                closed,
            }))
        })
    }

    /// Remove derived claims (and their evidence links) whose evidence points
    /// at this memory's chunks, so a deleted or edited memory cannot keep
    /// feeding stale claims into recall. Scoped to the memory — unlike the
    /// old behaviour, this does not wipe the whole KB's compiled knowledge.
    /// Entities/relations/wiki keep their (recomputable) aggregates until the
    /// next `compile`.
    pub fn delete_derived_for_memory_chunks(&self, memory_id: Uuid) -> Result<usize, StorageError> {
        let n = self.conn.execute(
            "DELETE FROM claims WHERE id IN (
                 SELECT ce.claim_id FROM claim_evidence ce
                 JOIN chunks c ON c.id = ce.chunk_id
                 WHERE c.memory_id = ?1
             )",
            params![memory_id.to_string()],
        )?;
        self.conn.execute(
            "DELETE FROM source_spans WHERE chunk_id IN (
                 SELECT id FROM chunks WHERE memory_id = ?1
             )",
            params![memory_id.to_string()],
        )?;
        Ok(n)
    }

    /// Update memory content / importance.
    ///
    /// A content change appends `memory.revised`, an importance-only change
    /// `memory.meta_changed`. The distinction is load-bearing rather than
    /// tidy: `memory.revised` is what tells a rebuild that the memory's
    /// extraction marker must be cleared, so a pending re-extraction survives
    /// `admin rebuild` instead of being silently un-pended (D6, option 2).
    pub fn update_memory(
        &self,
        memory_id: Uuid,
        content: Option<&str>,
        importance: Option<u8>,
    ) -> Result<bool, StorageError> {
        if content.is_none() && importance.is_none() {
            return Ok(false);
        }
        self.with_tx(|s| {
            let previous: Option<(String, String)> = s
                .conn
                .query_row(
                    "SELECT content, diary FROM memories WHERE id = ?1 AND deleted = 0",
                    params![memory_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((previous_content, diary)) = previous else {
                return Ok(false);
            };
            let changed = match (content, importance) {
                (Some(c), Some(i)) => s.conn.execute(
                    "UPDATE memories SET content = ?1, importance = ?2 WHERE id = ?3 AND deleted = 0",
                    params![c, i, memory_id.to_string()],
                )?,
                (Some(c), None) => s.conn.execute(
                    "UPDATE memories SET content = ?1 WHERE id = ?2 AND deleted = 0",
                    params![c, memory_id.to_string()],
                )?,
                (None, Some(i)) => s.conn.execute(
                    "UPDATE memories SET importance = ?1 WHERE id = ?2 AND deleted = 0",
                    params![i, memory_id.to_string()],
                )?,
                (None, None) => 0,
            };
            if changed == 0 {
                return Ok(false);
            }
            let kb = s.kb_for_memory(memory_id)?;
            let (kind, payload) = match content {
                Some(c) => (
                    engram_core::events::EventKind::MemoryRevised,
                    crate::journal::payload::memory_revised(
                        memory_id,
                        &engram_core::keys::content_hash(c),
                        &engram_core::keys::content_hash(&previous_content),
                    ),
                ),
                None => (
                    engram_core::events::EventKind::MemoryMetaChanged,
                    crate::journal::payload::memory_meta_changed(
                        memory_id,
                        importance.map(|i| i as i64),
                    ),
                ),
            };
            s.append_event(
                &crate::journal::EventDraft::new(kind, crate::journal::cli_actor(&diary), payload)
                    .scope(kb.as_deref(), Some(&diary))
                    .subject(memory_id),
            )?;
            Ok(true)
        })
    }

    /// Retire everything the fact layer derived from one memory's content and
    /// re-open it for extraction. Despite the name it NO LONGER DELETES: it
    /// closes each open interval with `retracted_source` and clears the
    /// "already extracted" marker, so rewritten content is eligible for
    /// extraction again without the FK cascade taking the adjudication ledger
    /// with it. Returns how many intervals it closed.
    ///
    /// New callers use `close_facts_for_memory` + `clear_extraction_marker`
    /// separately: `edit` needs to close facts WITHOUT clearing the marker in
    /// some paths, and welding the two together is what made an honest
    /// "re-extraction: replayed" impossible to report. No `#[deprecated]`
    /// attribute — the warning would fail `clippy -D warnings` in the lane
    /// that still calls this.
    pub fn delete_facts_for_memory(&self, memory_id: Uuid) -> Result<usize, StorageError> {
        self.with_tx(|s| {
            let diary = s
                .memory_scope_including_deleted(memory_id)?
                .map(|(_, d, _)| d)
                .unwrap_or_else(|| "default".to_string());
            let closed = s.close_facts_for_memory(
                memory_id,
                chrono::Utc::now(),
                CloseReason::RetractedSource,
                &CloseAuthority::Caller {
                    actor: crate::journal::cli_actor(&diary),
                },
                &crate::journal::cli_actor(&diary),
            )?;
            s.clear_extraction_marker(memory_id)?;
            Ok(closed.len())
        })
    }

    /// KB of each of `ids`, as a lookup map.
    ///
    /// `Fact` carries no `kb` field, but an unscoped `facts list` has to label
    /// every row with the namespace it came from — without it a cross-KB
    /// listing is a pile of facts with no way to tell which store answered.
    pub fn kbs_for_facts(
        &self,
        ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>, StorageError> {
        let mut out = std::collections::HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let placeholders = (1..=ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT id, kb FROM facts WHERE id IN ({placeholders})");
        let binds: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, kb) = row?;
            if let Ok(id) = Uuid::parse_str(&id) {
                out.insert(id, kb);
            }
        }
        Ok(out)
    }

    /// Record that the fact extractor has processed this memory's content.
    /// Idempotent: a later run overwrites the previous marker.
    ///
    /// UNWELDED, and therefore not a write path anyone may reach for [G6]:
    /// it writes the row and appends nothing, so `admin rebuild` — which
    /// restores `fact_extraction_log` from `derivation.ran` alone — cannot
    /// put the marker back, and the memory silently rejoins the backfill
    /// queue. `record_derivation` is the welded path and calls this inside
    /// its own transaction; the migration's genesis pass is the only other
    /// legitimate caller (it reads the markers a v9 store already had).
    ///
    /// Becomes `pub(crate)` the moment `write_fact_batch` stops calling it —
    /// see the lane report. The proof of what happens meanwhile is
    /// `write_path::tests::only_the_welded_derivation_restores_the_extraction_marker`.
    pub(crate) fn record_fact_extraction(
        &self,
        memory_id: Uuid,
        model: Option<&str>,
        fact_count: i64,
        source: &str,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO fact_extraction_log(memory_id, extracted_at, model, fact_count, source)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(memory_id) DO UPDATE SET
                 extracted_at = excluded.extracted_at,
                 model        = excluded.model,
                 fact_count   = excluded.fact_count,
                 source       = excluded.source",
            params![
                memory_id.to_string(),
                chrono::Utc::now().to_rfc3339(),
                model,
                fact_count,
                source,
            ],
        )?;
        Ok(())
    }

    /// Memories in scope the fact extractor has never seen: no facts and no
    /// extraction marker. Oldest first, so a `--limit`ed backfill always
    /// advances chronologically and a re-run resumes where it stopped.
    ///
    /// `kb = None` sweeps EVERY KB, and the ordering is global across the
    /// union rather than per-KB, so a bounded sweep still processes the
    /// oldest unprocessed memories in the store first. Each row carries its
    /// own KB, because a cross-KB sweep must write each memory's facts back
    /// into the KB the memory actually lives in.
    pub fn list_memories_pending_fact_extraction(
        &self,
        kb: Option<&str>,
        diary: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<(String, Memory)>, StorageError> {
        let mut sql = format!(
            "SELECT m.kb, {} FROM memories m
             WHERE m.deleted = 0 AND {}",
            memory_columns("m."),
            PENDING_EXTRACTION_PREDICATE
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(k) = kb {
            binds.push(Box::new(k.to_string()));
            sql.push_str(&format!(" AND m.kb = ?{}", binds.len()));
        }
        if let Some(d) = diary {
            binds.push(Box::new(d.to_string()));
            sql.push_str(&format!(" AND m.diary = ?{}", binds.len()));
        }
        sql.push_str(" ORDER BY m.created_at ASC, m.id ASC");
        if let Some(n) = limit {
            binds.push(Box::new(n as i64));
            sql.push_str(&format!(" LIMIT ?{}", binds.len()));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
                |r| Ok((r.get::<_, String>(0)?, parse_memory_from_offset(r, 1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `(total, pending)` memory counts for a backfill scope, so a dry run can
    /// report coverage without loading every row. `kb = None` counts every KB.
    pub fn fact_extraction_coverage(
        &self,
        kb: Option<&str>,
        diary: Option<&str>,
    ) -> Result<(i64, i64), StorageError> {
        let mut sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN {} THEN 1 ELSE 0 END), 0)
             FROM memories m
             WHERE m.deleted = 0",
            PENDING_EXTRACTION_PREDICATE
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(k) = kb {
            binds.push(Box::new(k.to_string()));
            sql.push_str(&format!(" AND m.kb = ?{}", binds.len()));
        }
        if let Some(d) = diary {
            binds.push(Box::new(d.to_string()));
            sql.push_str(&format!(" AND m.diary = ?{}", binds.len()));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt.query_row(
            rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )?;
        Ok(row)
    }

    /// Delete all chunks belonging to a memory. Used by `edit` before
    /// re-chunking + re-embedding so recall sees the updated content.
    pub fn delete_chunks_for_memory(&self, memory_id: Uuid) -> Result<usize, StorageError> {
        let n = self.conn.execute(
            "DELETE FROM chunks WHERE memory_id = ?1",
            params![memory_id.to_string()],
        )?;
        Ok(n)
    }

    /// Hard-delete a memory and all its chunks (used by ingest rollback
    /// when embedding fails mid-batch and we want to clean up the orphan).
    pub fn hard_delete_memory(&self, memory_id: Uuid) -> Result<(), StorageError> {
        self.purge_facts_for_memory(memory_id)?;
        // Chunks are removed by ON DELETE CASCADE on the foreign key.
        self.conn.execute(
            "DELETE FROM memories WHERE id = ?1",
            params![memory_id.to_string()],
        )?;
        Ok(())
    }

    /// List all memories with optional filters. Returns newest first.
    pub fn list_memories(
        &self,
        diary: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Memory>, StorageError> {
        let sql = match diary {
            Some(_) => {
                "SELECT id, content, created_at, event_time, importance, emotional_weight,
                        access_count, last_accessed, stability, source_json, diary,
                        valid_from, valid_until, tags_json
                 FROM memories WHERE deleted = 0 AND diary = ?1
                 ORDER BY created_at DESC LIMIT ?2"
            }
            None => {
                "SELECT id, content, created_at, event_time, importance, emotional_weight,
                        access_count, last_accessed, stability, source_json, diary,
                        valid_from, valid_until, tags_json
                 FROM memories WHERE deleted = 0
                 ORDER BY created_at DESC LIMIT ?1"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<Memory> {
            use chrono::{DateTime, Utc};
            let id_str: String = r.get(0)?;
            let created: String = r.get(2)?;
            let event: Option<String> = r.get(3)?;
            let last_ac: Option<String> = r.get(7)?;
            let src_json: String = r.get(9)?;
            let vf: Option<String> = r.get(11)?;
            let vu: Option<String> = r.get(12)?;
            let tags_json: String = r.get(13)?;
            Ok(Memory {
                id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
                content: r.get(1)?,
                created_at: DateTime::parse_from_rfc3339(&created)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                event_time: event
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                importance: r.get(4)?,
                emotional_weight: r.get(5)?,
                access_count: r.get(6)?,
                last_accessed: last_ac
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                stability: r.get(8)?,
                source: serde_json::from_str(&src_json)
                    .unwrap_or(engram_core::types::MemorySource::Manual),
                diary: r.get(10)?,
                valid_from: vf
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                valid_until: vu
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            })
        };
        let rows = match diary {
            Some(d) => stmt
                .query_map(params![d, limit as i64], mapper)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![limit as i64], mapper)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    pub fn document_id_for_source_path(
        &self,
        kb: &str,
        source_path: &str,
    ) -> Result<Option<Uuid>, StorageError> {
        let id = self
            .conn
            .query_row(
                "SELECT id FROM documents WHERE kb = ?1 AND source_path = ?2 LIMIT 1",
                params![kb, source_path],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(id.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    pub fn active_document_counts(&self, document_id: Uuid) -> Result<(i64, i64), StorageError> {
        let document_id = document_id.to_string();
        let memories = self.conn.query_row(
            "SELECT COUNT(DISTINCT m.id)
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             WHERE c.document_id = ?1 AND m.deleted = 0",
            params![document_id],
            |r| r.get::<_, i64>(0),
        )?;
        let chunks = self.conn.query_row(
            "SELECT COUNT(*)
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             WHERE c.document_id = ?1 AND m.deleted = 0",
            params![document_id],
            |r| r.get::<_, i64>(0),
        )?;
        Ok((memories, chunks))
    }

    pub fn list_memories_scoped(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Memory>, StorageError> {
        if kb.is_none() {
            return self.list_memories(diary, limit);
        }
        let k = kb.unwrap();
        let sql = match diary {
            Some(_) => {
                "SELECT id, content, created_at, event_time, importance, emotional_weight,
                        access_count, last_accessed, stability, source_json, diary,
                        valid_from, valid_until, tags_json
                 FROM memories WHERE deleted = 0 AND kb = ?1 AND diary = ?2
                 ORDER BY created_at DESC LIMIT ?3"
            }
            None => {
                "SELECT id, content, created_at, event_time, importance, emotional_weight,
                        access_count, last_accessed, stability, source_json, diary,
                        valid_from, valid_until, tags_json
                 FROM memories WHERE deleted = 0 AND kb = ?1
                 ORDER BY created_at DESC LIMIT ?2"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<Memory> {
            use chrono::{DateTime, Utc};
            let id_str: String = r.get(0)?;
            let created: String = r.get(2)?;
            let event: Option<String> = r.get(3)?;
            let last_ac: Option<String> = r.get(7)?;
            let src_json: String = r.get(9)?;
            let vf: Option<String> = r.get(11)?;
            let vu: Option<String> = r.get(12)?;
            let tags_json: String = r.get(13)?;
            Ok(Memory {
                id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
                content: r.get(1)?,
                created_at: DateTime::parse_from_rfc3339(&created)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                event_time: event
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                importance: r.get(4)?,
                emotional_weight: r.get(5)?,
                access_count: r.get(6)?,
                last_accessed: last_ac
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                stability: r.get(8)?,
                source: serde_json::from_str(&src_json)
                    .unwrap_or(engram_core::types::MemorySource::Manual),
                diary: r.get(10)?,
                valid_from: vf
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                valid_until: vu
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            })
        };
        let rows = match diary {
            Some(d) => stmt
                .query_map(params![k, d, limit as i64], mapper)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![k, limit as i64], mapper)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    /// Lexical (FTS5/BM25) search. Returns chunk_id + bm25 score.
    /// Unscoped across all diaries — used by the bench which opens a fresh
    /// store per question.
    pub fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<(Uuid, f32)>, StorageError> {
        // bm25() returns lower = better; we negate so higher = better.
        let mut stmt = self.conn.prepare(
            "SELECT c.id, -bm25(chunks_fts)
             FROM chunks_fts
             JOIN chunks c ON c.rowid = chunks_fts.rowid
             JOIN memories m ON m.id = c.memory_id
             WHERE chunks_fts MATCH ?1 AND m.deleted = 0
             ORDER BY bm25(chunks_fts)
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |r| {
            let id_str: String = r.get(0)?;
            let score: f64 = r.get(1)?;
            Ok((id_str, score as f32))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id_str, score) = row?;
            if let Ok(id) = Uuid::parse_str(&id_str) {
                out.push((id, score));
            }
        }
        Ok(out)
    }

    /// Diary-scoped lexical search. Same as `fts_search` but filters to one
    /// diary (namespace per specialist agent).
    pub fn fts_search_in_diary(
        &self,
        query: &str,
        diary: &str,
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, -bm25(chunks_fts)
             FROM chunks_fts
             JOIN chunks c ON c.rowid = chunks_fts.rowid
             JOIN memories m ON m.id = c.memory_id
             WHERE chunks_fts MATCH ?1 AND m.deleted = 0 AND m.diary = ?2
             ORDER BY bm25(chunks_fts)
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![query, diary, limit as i64], |r| {
            let id_str: String = r.get(0)?;
            let score: f64 = r.get(1)?;
            Ok((id_str, score as f32))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id_str, score) = row?;
            if let Ok(id) = Uuid::parse_str(&id_str) {
                out.push((id, score));
            }
        }
        Ok(out)
    }

    pub fn fts_search_scoped(
        &self,
        query: &str,
        kb: Option<&str>,
        diary: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>, StorageError> {
        let sql = match (kb, diary) {
            (Some(_), Some(_)) => {
                "SELECT c.id, -bm25(chunks_fts)
                 FROM chunks_fts
                 JOIN chunks c ON c.rowid = chunks_fts.rowid
                 JOIN memories m ON m.id = c.memory_id
                 WHERE chunks_fts MATCH ?1 AND m.deleted = 0 AND m.kb = ?2 AND m.diary = ?3
                 ORDER BY bm25(chunks_fts)
                 LIMIT ?4"
            }
            (Some(_), None) => {
                "SELECT c.id, -bm25(chunks_fts)
                 FROM chunks_fts
                 JOIN chunks c ON c.rowid = chunks_fts.rowid
                 JOIN memories m ON m.id = c.memory_id
                 WHERE chunks_fts MATCH ?1 AND m.deleted = 0 AND m.kb = ?2
                 ORDER BY bm25(chunks_fts)
                 LIMIT ?3"
            }
            (None, Some(_)) => {
                "SELECT c.id, -bm25(chunks_fts)
                 FROM chunks_fts
                 JOIN chunks c ON c.rowid = chunks_fts.rowid
                 JOIN memories m ON m.id = c.memory_id
                 WHERE chunks_fts MATCH ?1 AND m.deleted = 0 AND m.diary = ?2
                 ORDER BY bm25(chunks_fts)
                 LIMIT ?3"
            }
            (None, None) => {
                "SELECT c.id, -bm25(chunks_fts)
                 FROM chunks_fts
                 JOIN chunks c ON c.rowid = chunks_fts.rowid
                 JOIN memories m ON m.id = c.memory_id
                 WHERE chunks_fts MATCH ?1 AND m.deleted = 0
                 ORDER BY bm25(chunks_fts)
                 LIMIT ?2"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = match (kb, diary) {
            (Some(k), Some(d)) => stmt
                .query_map(params![query, k, d, limit as i64], parse_id_score_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (Some(k), None) => stmt
                .query_map(params![query, k, limit as i64], parse_id_score_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (None, Some(d)) => stmt
                .query_map(params![query, d, limit as i64], parse_id_score_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (None, None) => stmt
                .query_map(params![query, limit as i64], parse_id_score_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows
            .into_iter()
            .filter_map(|(id_str, score)| Uuid::parse_str(&id_str).ok().map(|id| (id, score)))
            .collect())
    }

    pub fn claim_fts_search(
        &self,
        query: &str,
        kb: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>, StorageError> {
        let sql = if kb.is_some() {
            "SELECT claims.id, -bm25(claims_fts)
             FROM claims_fts
             JOIN claims ON claims.rowid = claims_fts.rowid
             WHERE claims_fts MATCH ?1 AND claims.kb = ?2
             ORDER BY bm25(claims_fts)
             LIMIT ?3"
        } else {
            "SELECT claims.id, -bm25(claims_fts)
             FROM claims_fts
             JOIN claims ON claims.rowid = claims_fts.rowid
             WHERE claims_fts MATCH ?1
             ORDER BY bm25(claims_fts)
             LIMIT ?2"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(params![query, k, limit as i64], parse_id_score_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![query, limit as i64], parse_id_score_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows
            .into_iter()
            .filter_map(|(id_str, score)| Uuid::parse_str(&id_str).ok().map(|id| (id, score)))
            .collect())
    }

    pub fn insert_document(
        &self,
        kb: &str,
        title: &str,
        source_path: Option<&str>,
        mode: &str,
        metadata: serde_json::Value,
    ) -> Result<Uuid, StorageError> {
        self.ensure_kb(kb, None)?;
        if let Some(path) = source_path {
            if let Some(existing) = self
                .conn
                .query_row(
                    "SELECT id FROM documents WHERE kb = ?1 AND source_path = ?2 LIMIT 1",
                    params![kb, path],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            {
                return Ok(Uuid::parse_str(&existing).unwrap_or(Uuid::nil()));
            }
        }
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().to_rfc3339();
        let inserted = self.conn.execute(
            "INSERT INTO documents(id, kb, title, source_path, mode, created_at, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(kb, source_path) WHERE source_path IS NOT NULL DO NOTHING",
            params![
                id.to_string(),
                kb,
                title,
                source_path,
                mode,
                now,
                serde_json::to_string(&metadata)?
            ],
        )?;
        if inserted == 0 {
            // Lost a race with a concurrent ingest of the same file; return
            // the row that won.
            if let Some(path) = source_path {
                if let Some(existing) = self
                    .conn
                    .query_row(
                        "SELECT id FROM documents WHERE kb = ?1 AND source_path = ?2 LIMIT 1",
                        params![kb, path],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                {
                    return Ok(Uuid::parse_str(&existing).unwrap_or(Uuid::nil()));
                }
            }
        }
        Ok(id)
    }

    pub fn get_document(&self, id: Uuid) -> Result<Option<DocumentRecord>, StorageError> {
        self.conn
            .query_row(
                "SELECT id, kb, title, source_path, source_url, doi, year, mode, created_at, metadata_json
                 FROM documents WHERE id = ?1",
                params![id.to_string()],
                parse_document_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn list_documents(
        &self,
        kb: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DocumentRecord>, StorageError> {
        let sql = if kb.is_some() {
            "SELECT id, kb, title, source_path, source_url, doi, year, mode, created_at, metadata_json
             FROM documents WHERE kb = ?1 ORDER BY created_at DESC LIMIT ?2"
        } else {
            "SELECT id, kb, title, source_path, source_url, doi, year, mode, created_at, metadata_json
             FROM documents ORDER BY created_at DESC LIMIT ?1"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(params![k, limit as i64], parse_document_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![limit as i64], parse_document_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    pub fn delete_document(&self, id: Uuid) -> Result<bool, StorageError> {
        let id_s = id.to_string();
        if self.get_document(id)?.is_none() {
            return Ok(false);
        }
        self.with_tx(|s| {
            // Route the retraction through `soft_delete_memory` so each memory
            // gets its `memory.retracted` event and its facts are closed rather
            // than deleted. The bare `UPDATE memories SET deleted = 1` this
            // replaces was the fourth hard-delete path, and the only one that
            // retracted memories with no journal record whatsoever.
            let memory_ids = s.memory_ids_for_document(id)?;
            for memory_id in &memory_ids {
                s.soft_delete_memory(*memory_id)?;
            }
            s.conn.execute(
                "DELETE FROM claims
                 WHERE id IN (SELECT claim_id FROM claim_evidence WHERE document_id = ?1)",
                params![id_s],
            )?;
            s.conn.execute(
                "DELETE FROM source_spans WHERE document_id = ?1",
                params![id_s],
            )?;
            s.conn.execute(
                "DELETE FROM takeaways WHERE document_id = ?1",
                params![id_s],
            )?;
            s.conn.execute(
                "UPDATE chunks SET document_id = NULL WHERE document_id = ?1",
                params![id_s],
            )?;
            let n = s
                .conn
                .execute("DELETE FROM documents WHERE id = ?1", params![id_s])?;
            // Entities/relations/wiki aggregates go stale until the next
            // `compile`; deleting one document no longer wipes the whole
            // KB's compiled knowledge.
            Ok(n > 0)
        })
    }

    /// Does the document's stored source text match `content`? Compares against
    /// the concatenated content of its active memories (ingest stores the whole
    /// file text as the memory content). Used to decide skip-vs-replace on
    /// re-ingest of the same path.
    pub fn document_content_matches(
        &self,
        document_id: Uuid,
        content: &str,
    ) -> Result<bool, StorageError> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT m.content FROM memories m
                 JOIN chunks c ON c.memory_id = m.id
                 WHERE c.document_id = ?1 AND m.deleted = 0
                 ORDER BY m.created_at ASC LIMIT 1",
                params![document_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(stored.as_deref() == Some(content))
    }

    pub fn memory_ids_for_document(&self, id: Uuid) -> Result<Vec<Uuid>, StorageError> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT memory_id FROM chunks WHERE document_id = ?1")?;
        let rows = stmt.query_map(params![id.to_string()], |r| r.get::<_, String>(0))?;
        let ids = rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|s| Uuid::parse_str(&s).ok())
            .collect();
        Ok(ids)
    }

    pub fn create_compile_job(
        &self,
        kb: &str,
        mode: &str,
        metadata: serde_json::Value,
    ) -> Result<Uuid, StorageError> {
        self.ensure_kb(kb, None)?;
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().to_rfc3339();
        // A crash leaves a job stuck at 'running' forever (there is no
        // heartbeat); reconcile by abandoning any stale running job for
        // this KB before starting a new one, so `jobs list` stays honest.
        self.conn.execute(
            "UPDATE compile_jobs
             SET status = 'abandoned',
                 message = 'superseded by a newer compile run (previous run crashed or was killed)',
                 updated_at = ?1
             WHERE kb = ?2 AND status = 'running'",
            params![now, kb],
        )?;
        self.conn.execute(
            "INSERT INTO compile_jobs(id, kb, status, mode, created_at, updated_at, metadata_json)
             VALUES (?1, ?2, 'running', ?3, ?4, ?4, ?5)",
            params![
                id.to_string(),
                kb,
                mode,
                now,
                serde_json::to_string(&metadata)?
            ],
        )?;
        Ok(id)
    }

    /// Move a running compile job to its terminal state.
    ///
    /// The `status = 'running'` guard means the update can legitimately match
    /// NOTHING: the job was already finished, or a newer compile of the same
    /// KB abandoned it (see [`Self::create_compile_job`]). Reporting that as
    /// success used to hide it — the caller printed a completed compile whose
    /// job row still said `abandoned`, and `jobs list` disagreed with the
    /// command that had just run. A no-op transition is now an error, so the
    /// caller decides whether it matters.
    pub fn finish_compile_job(
        &self,
        id: Uuid,
        status: &str,
        message: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), StorageError> {
        let now = chrono::Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE compile_jobs
             SET status = ?1, message = ?2, metadata_json = ?3, updated_at = ?4
             WHERE id = ?5 AND status = 'running'",
            params![
                status,
                message,
                serde_json::to_string(&metadata)?,
                now,
                id.to_string()
            ],
        )?;
        if changed == 0 {
            let current: Option<String> = self
                .conn
                .query_row(
                    "SELECT status FROM compile_jobs WHERE id = ?1",
                    params![id.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            return Err(StorageError::NotFound(match current {
                Some(current) => format!(
                    "compile job {id} is '{current}', not 'running' — cannot finish it as '{status}'"
                ),
                None => format!("compile job {id} no longer exists"),
            }));
        }
        Ok(())
    }

    pub fn list_compile_jobs(
        &self,
        kb: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CompileJobRecord>, StorageError> {
        let sql = if kb.is_some() {
            "SELECT id, kb, status, mode, created_at, updated_at, message, metadata_json
             FROM compile_jobs WHERE kb = ?1 ORDER BY created_at DESC LIMIT ?2"
        } else {
            "SELECT id, kb, status, mode, created_at, updated_at, message, metadata_json
             FROM compile_jobs ORDER BY created_at DESC LIMIT ?1"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(params![k, limit as i64], parse_compile_job_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![limit as i64], parse_compile_job_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    pub fn get_compile_job(&self, id: Uuid) -> Result<Option<CompileJobRecord>, StorageError> {
        self.conn
            .query_row(
                "SELECT id, kb, status, mode, created_at, updated_at, message, metadata_json
                 FROM compile_jobs WHERE id = ?1",
                params![id.to_string()],
                parse_compile_job_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn document_for_memory(&self, memory_id: Uuid) -> Result<Option<Uuid>, StorageError> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT document_id FROM chunks WHERE memory_id = ?1 AND document_id IS NOT NULL LIMIT 1",
                params![memory_id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(id.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    pub fn kb_for_memory(&self, memory_id: Uuid) -> Result<Option<String>, StorageError> {
        self.conn
            .query_row(
                "SELECT kb FROM memories WHERE id = ?1",
                params![memory_id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// The `(kb, diary)` a LIVE memory belongs to, or `None` when the id names
    /// no live memory.
    ///
    /// `edit` needs all three answers in one query — does this row exist, which
    /// KB budget governs the edit, and which (kb, diary) the usage row must be
    /// attributed to — BEFORE it makes a paid embedding call. `kb_for_memory`
    /// answers neither the last one nor the `deleted = 0` question, so it would
    /// let an edit of a forgotten memory buy an embedding and then fail.
    pub fn memory_scope(&self, memory_id: Uuid) -> Result<Option<(String, String)>, StorageError> {
        self.conn
            .query_row(
                "SELECT kb, diary FROM memories WHERE id = ?1 AND deleted = 0",
                params![memory_id.to_string()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// A memory's own dates, `(event_time, created_at)`. `None` when the
    /// memory is gone or soft-deleted.
    ///
    /// A fact derived from a chunk is dated by the MEMORY the chunk came from,
    /// never by the clock of the pass that read it. A fabricated "now" is not a
    /// missing date: it is the NEWEST date in the store, so it wins recency
    /// supersession against the older true fact — the same defect the anchored
    /// extractor dates closed on the `remember` path. Exposed as a single-row
    /// lookup rather than widened onto `ChunkRecord`, whose row parser is
    /// shared by five queries and whose JSON shape is public.
    pub fn memory_times(&self, memory_id: Uuid) -> Result<Option<MemoryTimes>, StorageError> {
        let row: Option<(Option<String>, String)> = self
            .conn
            .query_row(
                "SELECT event_time, created_at FROM memories WHERE id = ?1 AND deleted = 0",
                params![memory_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let parse = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.with_timezone(&chrono::Utc))
        };
        Ok(row.map(|(event_time, created_at)| {
            (
                event_time.as_deref().and_then(parse),
                // Every writer of `created_at` is `Utc::now().to_rfc3339()`,
                // so an unparseable one means hand-edited or imported data.
                // Fall back to the EPOCH, never to now: a fabricated "now" is
                // the newest date in the store and would WIN recency
                // supersession against older true facts, while the epoch
                // always loses it — an unreadable date cannot outrank a real
                // one.
                parse(&created_at).unwrap_or(chrono::DateTime::UNIX_EPOCH),
            )
        }))
    }

    pub fn clear_derived_for_kb(&self, kb: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM claim_evidence WHERE claim_id IN (SELECT id FROM claims WHERE kb = ?1)",
            params![kb],
        )?;
        self.conn
            .execute("DELETE FROM claims WHERE kb = ?1", params![kb])?;
        self.conn
            .execute("DELETE FROM source_spans WHERE kb = ?1", params![kb])?;
        self.conn
            .execute("DELETE FROM takeaways WHERE kb = ?1", params![kb])?;
        self.conn
            .execute("DELETE FROM wiki_pages WHERE kb = ?1", params![kb])?;
        self.conn
            .execute("DELETE FROM relations WHERE kb = ?1", params![kb])?;
        self.conn
            .execute("DELETE FROM entity_aliases WHERE kb = ?1", params![kb])?;
        self.conn
            .execute("DELETE FROM entities WHERE kb = ?1", params![kb])?;
        Ok(())
    }

    pub fn integrity_report(&self) -> Result<IntegrityReport, StorageError> {
        let stale_facts_from_deleted_memories = self.conn.query_row(
            "SELECT COUNT(*)
             FROM facts f
             JOIN memories m ON m.id = f.source_memory_id
             WHERE m.deleted = 1",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        let orphan_chunks = self.conn.query_row(
            "SELECT COUNT(*)
             FROM chunks c
             LEFT JOIN memories m ON m.id = c.memory_id
             WHERE m.id IS NULL",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        let empty_kbs_with_derived = self.empty_kbs_with_derived()?;
        let duplicate_source_documents = self.duplicate_source_documents()?;
        let unregistered_kbs = self.unregistered_kbs()?;
        let ok = stale_facts_from_deleted_memories == 0
            && orphan_chunks == 0
            && empty_kbs_with_derived.is_empty()
            && duplicate_source_documents.is_empty()
            && unregistered_kbs.is_empty();
        Ok(IntegrityReport {
            ok,
            stale_facts_from_deleted_memories,
            orphan_chunks,
            empty_kbs_with_derived,
            duplicate_source_documents,
            unregistered_kbs,
        })
    }

    pub fn repair_integrity(&self, dry_run: bool) -> Result<RepairReport, StorageError> {
        let report = self.integrity_report()?;
        let facts_from_deleted_memories_removed = report.stale_facts_from_deleted_memories;
        let derived_kbs_cleared = report
            .empty_kbs_with_derived
            .iter()
            .map(|r| r.kb.clone())
            .collect::<Vec<_>>();
        let mut facts_from_deleted_memories_closed = 0usize;
        if !dry_run {
            // Facts of a deleted memory are CLOSED, not deleted. Shipping the
            // soft-delete rewrite without this one means the next
            // `engram repair` quietly undoes it — and takes the adjudication
            // rows with it through the queue's FK cascade.
            let stale: Vec<Uuid> = {
                let mut stmt = self.conn.prepare(
                    "SELECT f.id FROM facts f
                       JOIN memories m ON m.id = f.source_memory_id
                      WHERE m.deleted = 1
                        AND f.valid_until IS NULL AND f.superseded_by IS NULL",
                )?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .filter_map(|v| Uuid::parse_str(v).ok())
                    .collect()
            };
            for id in stale {
                // `caller`, not `explicit_end` (final-review finding 2): this
                // sweep completes a retraction a person already ordered — it
                // holds the forget-caller's authority, not text evidence. An
                // `explicit_end` stamp here would let a machine sweep close
                // safety intervals with an authority it never had.
                if self.close_fact_interval(
                    id,
                    chrono::Utc::now(),
                    CloseReason::RetractedSource.as_str(),
                    CloseAuthority::Caller {
                        actor: "repair".to_string(),
                    }
                    .as_str(),
                    None,
                    None,
                )? {
                    facts_from_deleted_memories_closed += 1;
                }
            }
            for kb in &derived_kbs_cleared {
                self.clear_derived_for_kb(kb)?;
            }
        }
        Ok(RepairReport {
            dry_run,
            facts_from_deleted_memories_removed,
            facts_from_deleted_memories_closed: facts_from_deleted_memories_closed as i64,
            derived_kbs_cleared,
            duplicate_source_documents: report.duplicate_source_documents,
        })
    }

    fn empty_kbs_with_derived(&self) -> Result<Vec<KbDerivedResidue>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT kb.name,
                    (SELECT COUNT(*) FROM memories m WHERE m.kb = kb.name AND m.deleted = 0) AS active_memories,
                    (SELECT COUNT(*) FROM documents d WHERE d.kb = kb.name) AS documents,
                    (SELECT COUNT(*) FROM entities e WHERE e.kb = kb.name) AS entities,
                    (SELECT COUNT(*) FROM relations r WHERE r.kb = kb.name) AS relations,
                    (SELECT COUNT(*) FROM claims c WHERE c.kb = kb.name) AS claims,
                    (SELECT COUNT(*) FROM wiki_pages w WHERE w.kb = kb.name) AS wiki_pages
             FROM knowledge_bases kb",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(KbDerivedResidue {
                kb: r.get(0)?,
                active_memories: r.get(1)?,
                documents: r.get(2)?,
                entities: r.get(3)?,
                relations: r.get(4)?,
                claims: r.get(5)?,
                wiki_pages: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            let row = row?;
            let derived = row.entities + row.relations + row.claims + row.wiki_pages;
            if row.active_memories == 0 && row.documents == 0 && derived > 0 {
                out.push(row);
            }
        }
        Ok(out)
    }

    fn duplicate_source_documents(&self) -> Result<Vec<DocumentDuplicateRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.kb, d.source_path,
                    COUNT(DISTINCT m.id) AS active_memories,
                    COUNT(c.id) AS active_chunks
             FROM documents d
             JOIN chunks c ON c.document_id = d.id
             JOIN memories m ON m.id = c.memory_id AND m.deleted = 0
             GROUP BY d.id, d.kb, d.source_path
             HAVING COUNT(DISTINCT m.id) > 1
             ORDER BY active_memories DESC, d.kb ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            Ok(DocumentDuplicateRecord {
                document_id: Uuid::parse_str(&id).unwrap_or(Uuid::nil()),
                kb: r.get(1)?,
                source_path: r.get(2)?,
                active_memories: r.get(3)?,
                active_chunks: r.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Insert a source span. `offset_start`/`offset_end` are CHAR offsets into
    /// the parent chunk's content locating the exact supporting text (e.g. the
    /// claim sentence), so a citation resolves to a precise span rather than
    /// the whole chunk. `page` is the 1-based page when known.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_source_span(
        &self,
        kb: &str,
        document_id: Option<Uuid>,
        chunk_id: Option<Uuid>,
        section: Option<&str>,
        source: Option<&str>,
        text_preview: &str,
        offset_start: Option<usize>,
        offset_end: Option<usize>,
        page: Option<i64>,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        self.conn.execute(
            "INSERT INTO source_spans(id, kb, document_id, chunk_id, section, source,
                text_preview, offset_start, offset_end, page)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id.to_string(),
                kb,
                document_id.map(|id| id.to_string()),
                chunk_id.map(|id| id.to_string()),
                section,
                source,
                text_preview,
                offset_start.map(|v| v as i64),
                offset_end.map(|v| v as i64),
                page,
            ],
        )?;
        Ok(id)
    }

    // One argument per column of `claims`; grouping them would hide which of
    // the three provenance ids a caller left unset.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_claim(
        &self,
        kb: &str,
        content: &str,
        evidence_level: &str,
        confidence: f32,
        source_span_id: Option<Uuid>,
        chunk_id: Option<Uuid>,
        document_id: Option<Uuid>,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO claims(id, kb, content, evidence_level, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id.to_string(), kb, content, evidence_level, confidence, now],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO claim_evidence(claim_id, source_span_id, chunk_id, document_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id.to_string(),
                source_span_id.map(|sid| sid.to_string()),
                chunk_id.map(|cid| cid.to_string()),
                document_id.map(|did| did.to_string())
            ],
        )?;
        Ok(id)
    }

    pub fn insert_takeaway(
        &self,
        kb: &str,
        document_id: Option<Uuid>,
        content: &str,
        evidence_level: &str,
        citations: serde_json::Value,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO takeaways(id, kb, document_id, content, evidence_level, created_at, citations_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.to_string(),
                kb,
                document_id.map(|id| id.to_string()),
                content,
                evidence_level,
                now,
                serde_json::to_string(&citations)?,
            ],
        )?;
        Ok(id)
    }

    pub fn get_claims_by_ids(&self, ids: &[Uuid]) -> Result<Vec<ClaimRecord>, StorageError> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(claim) = self.get_claim(*id)? {
                out.push(claim);
            }
        }
        Ok(out)
    }

    pub fn get_claim(&self, id: Uuid) -> Result<Option<ClaimRecord>, StorageError> {
        let claim = self
            .conn
            .query_row(
                "SELECT id, kb, content, evidence_level, confidence, created_at
                 FROM claims WHERE id = ?1",
                params![id.to_string()],
                |r| {
                    let id_str: String = r.get(0)?;
                    Ok(ClaimRecord {
                        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
                        kb: r.get(1)?,
                        content: r.get(2)?,
                        evidence_level: r.get(3)?,
                        confidence: r.get(4)?,
                        created_at: r.get(5)?,
                        citations: Vec::new(),
                    })
                },
            )
            .optional()?;
        if let Some(mut c) = claim {
            c.citations = self.citations_for_claim(c.id)?;
            Ok(Some(c))
        } else {
            Ok(None)
        }
    }

    pub fn citations_for_claim(&self, claim_id: Uuid) -> Result<Vec<SourceCitation>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT ce.document_id, ce.chunk_id, ss.page, ss.section, ss.source,
                    ss.offset_start, ss.offset_end
             FROM claim_evidence ce
             LEFT JOIN source_spans ss ON ss.id = ce.source_span_id
             WHERE ce.claim_id = ?1",
        )?;
        let rows = stmt.query_map(params![claim_id.to_string()], parse_citation_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn citations_for_chunk(&self, chunk_id: Uuid) -> Result<Vec<SourceCitation>, StorageError> {
        // A chunk cites its document/section/source as a whole. Precise char
        // spans belong to individual claims (see citations_for_claim), not to
        // the chunk, so we don't attach one arbitrary claim's offset here — and
        // we return a single deduped citation row even when the chunk has many
        // claim spans.
        let mut stmt = self.conn.prepare(
            "SELECT c.document_id, c.id, NULL, c.section, d.source_path
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE c.id = ?1 AND m.deleted = 0",
        )?;
        let rows = stmt.query_map(params![chunk_id.to_string()], parse_citation_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn upsert_entity(
        &self,
        kb: &str,
        canonical_name: &str,
        kind: &str,
        increment: u32,
    ) -> Result<Uuid, StorageError> {
        self.upsert_entity_with_aliases(kb, canonical_name, kind, increment, &[])
    }

    /// Upsert an entity together with the alternate spellings that resolve to
    /// it ("Elias's", "Dr Jane Doe", a bare first name).
    ///
    /// Resolution is by canonical name OR by ANY spelling this call carries —
    /// canonical name and aliases alike, all case-folded — so a recompile
    /// converges on ONE row per real-world entity instead of accumulating
    /// near-duplicates. The stored `kind` only ever gets more specific
    /// (`concept` → `person`), never less: whichever chunk carried the
    /// evidence wins regardless of chunk order.
    ///
    /// RESOLVING ON THE CANONICAL NAME ALONE was the bug that left the real
    /// store with "Noemi" (119 mentions) beside "Noemi Varkoni" (54), and
    /// "Maria" (432) beside "Maria Marenova" (21). The chunk that only knew
    /// the bare first name compiled first and created its row; the chunk that
    /// knew the full name arrived later carrying "Noemi" as an ALIAS, but the
    /// alias insert is `OR IGNORE` under `UNIQUE(kb, alias_norm)` — the bare
    /// row already owned that spelling, so the alias was silently dropped and
    /// a second row appeared. Two entities for one woman, with the bare form
    /// usually winning `people list` because it had more mentions.
    ///
    /// So every incoming spelling now resolves, every row it reaches is merged
    /// into one, and a bare first name gives up its canonical slot to the full
    /// name that contains it (see [`promotes_canonical`]).
    pub fn upsert_entity_with_aliases(
        &self,
        kb: &str,
        canonical_name: &str,
        kind: &str,
        increment: u32,
        aliases: &[String],
    ) -> Result<Uuid, StorageError> {
        let canonical_norm = canonical_name.trim().to_ascii_lowercase();
        // The canonical name is looked up FIRST, so when it already names a
        // row that row survives and keeps its id (and therefore its edges).
        let mut keys: Vec<String> = vec![canonical_norm.clone()];
        for alias in aliases {
            let norm = alias.trim().to_ascii_lowercase();
            if !norm.is_empty() && !keys.contains(&norm) {
                keys.push(norm);
            }
        }
        let mut matched: Vec<String> = Vec::new();
        for key in &keys {
            // Two indexed seeks, deliberately NOT one OR-joined query. An `OR`
            // spanning `entities` and `entity_aliases` across a LEFT JOIN forces
            // SQLite to scan every entity in the KB and cross-join the KB's
            // aliases (it can only seek the `kb` prefix), and `lower()` on the
            // column defeats idx_entities_kb_name outright. On a 2,300-entity KB
            // that measured 904ms per lookup against 0.09ms for these two seeks,
            // once per spelling per mention — minutes of compile became hours.
            let mut by_name = self.conn.prepare_cached(
                "SELECT id FROM entities
                 WHERE kb = ?1 AND canonical_name = ?2 COLLATE NOCASE",
            )?;
            let mut by_alias = self.conn.prepare_cached(
                "SELECT entity_id FROM entity_aliases WHERE kb = ?1 AND alias_norm = ?2",
            )?;
            let found = by_name
                .query_map(params![kb, key], |r| r.get::<_, String>(0))?
                .chain(by_alias.query_map(params![kb, key], |r| r.get::<_, String>(0))?);
            for row in found {
                let found = row?;
                if !matched.contains(&found) {
                    matched.push(found);
                }
            }
        }
        let id = match matched.split_first() {
            Some((survivor, duplicates)) => {
                let survivor = survivor.clone();
                // One human, one row: everything the rival rows accumulated —
                // mentions, edges, spellings — moves onto the survivor.
                for dead in duplicates {
                    self.merge_entity_rows(&survivor, dead)?;
                }
                let (current_name, current_kind): (String, String) = self.conn.query_row(
                    "SELECT canonical_name, kind FROM entities WHERE id = ?1",
                    params![survivor],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let merged_kind = if kind_rank(kind) > kind_rank(&current_kind) {
                    kind
                } else {
                    current_kind.as_str()
                };
                self.conn.execute(
                    "UPDATE entities SET mention_count = mention_count + ?1, kind = ?2
                     WHERE id = ?3",
                    params![increment as i64, merged_kind, survivor],
                )?;
                if promotes_canonical(&current_name, canonical_name) {
                    self.conn.execute(
                        "UPDATE entities SET canonical_name = ?1 WHERE id = ?2",
                        params![canonical_name.trim(), survivor],
                    )?;
                }
                Uuid::parse_str(&survivor).unwrap_or(Uuid::nil())
            }
            None => {
                let id = Uuid::new_v4();
                self.conn.execute(
                    "INSERT INTO entities(id, kb, canonical_name, aliases_json, kind, mention_count)
                     VALUES (?1, ?2, ?3, '[]', ?4, ?5)",
                    params![id.to_string(), kb, canonical_name, kind, increment as i64],
                )?;
                id
            }
        };

        for alias in std::iter::once(canonical_name).chain(aliases.iter().map(|s| s.as_str())) {
            let alias = alias.trim();
            if alias.is_empty() {
                continue;
            }
            self.conn.execute(
                "INSERT OR IGNORE INTO entity_aliases(id, kb, entity_id, alias, alias_norm)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    Uuid::new_v4().to_string(),
                    kb,
                    id.to_string(),
                    alias,
                    alias.to_ascii_lowercase()
                ],
            )?;
        }
        // Mirror the alias set onto the entity row so `entities show` and the
        // JSON envelope carry it without a second query. The exclusion is on
        // the entity's OWN canonical name, not the name this call arrived
        // with — an upsert that came in under an alias must not drop that
        // alias from the list or promote the canonical name into it.
        let stored_canonical: String = self.conn.query_row(
            "SELECT canonical_name FROM entities WHERE id = ?1",
            params![id.to_string()],
            |r| r.get(0),
        )?;
        let mut stored: Vec<String> = self
            .conn
            .prepare(
                "SELECT alias FROM entity_aliases
                 WHERE kb = ?1 AND entity_id = ?2 AND lower(alias) != lower(?3)
                 ORDER BY alias ASC",
            )?
            .query_map(params![kb, id.to_string(), stored_canonical], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        stored.dedup();
        self.conn.execute(
            "UPDATE entities SET aliases_json = ?1 WHERE id = ?2",
            params![serde_json::to_string(&stored)?, id.to_string()],
        )?;
        Ok(id)
    }

    /// Fold one entity row into another: mentions summed, kind sharpened,
    /// edges and spellings carried over, then the rival row deleted.
    ///
    /// Edges keep their `provenance_json`, so every receipt `people show`
    /// prints survives the merge. `UPDATE OR IGNORE` drops only an edge the
    /// survivor already has under the same predicate — `UNIQUE(kb, from, to,
    /// predicate)` — and those leftovers are deleted with the row.
    fn merge_entity_rows(&self, survivor: &str, dead: &str) -> Result<(), StorageError> {
        if survivor == dead {
            return Ok(());
        }
        let dead_kind: Option<String> = self
            .conn
            .query_row(
                "SELECT kind FROM entities WHERE id = ?1",
                params![dead],
                |r| r.get(0),
            )
            .optional()?;
        let Some(dead_kind) = dead_kind else {
            return Ok(());
        };
        self.conn.execute(
            "UPDATE entities SET mention_count = mention_count
                 + COALESCE((SELECT mention_count FROM entities WHERE id = ?2), 0)
             WHERE id = ?1",
            params![survivor, dead],
        )?;
        let survivor_kind: String = self.conn.query_row(
            "SELECT kind FROM entities WHERE id = ?1",
            params![survivor],
            |r| r.get(0),
        )?;
        if kind_rank(&dead_kind) > kind_rank(&survivor_kind) {
            self.conn.execute(
                "UPDATE entities SET kind = ?1 WHERE id = ?2",
                params![dead_kind, survivor],
            )?;
        }
        self.conn.execute(
            "UPDATE OR IGNORE relations SET from_entity_id = ?1 WHERE from_entity_id = ?2",
            params![survivor, dead],
        )?;
        self.conn.execute(
            "UPDATE OR IGNORE relations SET to_entity_id = ?1 WHERE to_entity_id = ?2",
            params![survivor, dead],
        )?;
        // An edge between the two merged rows has become a self-loop.
        self.conn.execute(
            "DELETE FROM relations WHERE from_entity_id = ?1 AND to_entity_id = ?1",
            params![survivor],
        )?;
        self.conn.execute(
            "DELETE FROM relations WHERE from_entity_id = ?1 OR to_entity_id = ?1",
            params![dead],
        )?;
        // Its spellings — including its own canonical name, which every upsert
        // registers — become spellings of the survivor.
        self.conn.execute(
            "UPDATE OR IGNORE entity_aliases SET entity_id = ?1 WHERE entity_id = ?2",
            params![survivor, dead],
        )?;
        self.conn
            .execute("DELETE FROM entities WHERE id = ?1", params![dead])?;
        Ok(())
    }

    /// Insert or reinforce a relation, ACCUMULATING provenance instead of
    /// overwriting it.
    ///
    /// Every typed person edge must stay verifiable: if three memories say
    /// "Avi introduced me to Noemi", all three sources stay attached to the
    /// single `introduced_by` edge, so `people show` can print each receipt.
    /// Sources are deduplicated on `chunk_id` and capped so one chatty KB
    /// cannot grow an unbounded JSON blob.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_relation_with_source(
        &self,
        kb: &str,
        from_entity_id: Uuid,
        to_entity_id: Uuid,
        predicate: &str,
        weight: f32,
        source: serde_json::Value,
        source_span_id: Option<Uuid>,
    ) -> Result<Uuid, StorageError> {
        const MAX_SOURCES: usize = 25;
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT provenance_json FROM relations
                 WHERE kb = ?1 AND from_entity_id = ?2 AND to_entity_id = ?3 AND predicate = ?4",
                params![
                    kb,
                    from_entity_id.to_string(),
                    to_entity_id.to_string(),
                    predicate
                ],
                |r| r.get(0),
            )
            .optional()?;
        let mut sources: Vec<serde_json::Value> = existing
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|v| {
                v.get("sources")
                    .and_then(|s| s.as_array())
                    .map(|a| a.to_vec())
            })
            .unwrap_or_default();
        let already = sources.iter().any(|existing| {
            existing.get("chunk_id") == source.get("chunk_id")
                && existing.get("sentence") == source.get("sentence")
        });
        if !already && sources.len() < MAX_SOURCES {
            sources.push(source);
        }
        let provenance = serde_json::json!({
            "extracted_by": "person_graph",
            "sources": sources,
        });
        self.insert_relation(
            kb,
            from_entity_id,
            to_entity_id,
            predicate,
            weight,
            provenance,
            source_span_id,
        )
    }

    // One argument per column of `relations`.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_relation(
        &self,
        kb: &str,
        from_entity_id: Uuid,
        to_entity_id: Uuid,
        predicate: &str,
        weight: f32,
        provenance: serde_json::Value,
        source_span_id: Option<Uuid>,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO relations(id, kb, from_entity_id, to_entity_id, predicate, weight,
                created_at, provenance_json, source_span_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(kb, from_entity_id, to_entity_id, predicate)
             DO UPDATE SET weight = relations.weight + excluded.weight,
                           provenance_json = excluded.provenance_json",
            params![
                id.to_string(),
                kb,
                from_entity_id.to_string(),
                to_entity_id.to_string(),
                predicate,
                weight,
                now,
                serde_json::to_string(&provenance)?,
                source_span_id.map(|id| id.to_string()),
            ],
        )?;
        Ok(id)
    }

    pub fn list_entities(
        &self,
        kb: Option<&str>,
        limit: usize,
        min_mentions: u32,
    ) -> Result<Vec<EntityRecord>, StorageError> {
        let sql = if kb.is_some() {
            "SELECT id, kb, canonical_name, aliases_json, kind, mention_count
             FROM entities WHERE kb = ?1 AND mention_count >= ?2
             ORDER BY mention_count DESC, canonical_name ASC LIMIT ?3"
        } else {
            "SELECT id, kb, canonical_name, aliases_json, kind, mention_count
             FROM entities WHERE mention_count >= ?1
             ORDER BY mention_count DESC, canonical_name ASC LIMIT ?2"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(
                params![k, min_mentions as i64, limit as i64],
                parse_entity_row,
            )?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![min_mentions as i64, limit as i64], parse_entity_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Entities of one kind (e.g. `person`), most-mentioned first.
    pub fn list_entities_by_kind(
        &self,
        kb: Option<&str>,
        kind: &str,
        limit: usize,
        min_mentions: u32,
    ) -> Result<Vec<EntityRecord>, StorageError> {
        let sql = if kb.is_some() {
            "SELECT id, kb, canonical_name, aliases_json, kind, mention_count
             FROM entities WHERE kb = ?1 AND kind = ?2 AND mention_count >= ?3
             ORDER BY mention_count DESC, canonical_name ASC LIMIT ?4"
        } else {
            "SELECT id, kb, canonical_name, aliases_json, kind, mention_count
             FROM entities WHERE kind = ?1 AND mention_count >= ?2
             ORDER BY mention_count DESC, canonical_name ASC LIMIT ?3"
        };
        // `usize::MAX as i64` would wrap to -1; clamp so an "unbounded" call
        // asks for everything explicitly rather than by accident.
        let limit = limit.min(i64::MAX as usize) as i64;
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(
                params![k, kind, min_mentions as i64, limit],
                parse_entity_row,
            )?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![kind, min_mentions as i64, limit], parse_entity_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Direct (one-hop) relations touching `entity_id`, heaviest first.
    pub fn relations_for_entity(
        &self,
        kb: &str,
        entity_id: Uuid,
        limit: usize,
    ) -> Result<Vec<RelationRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.kb, f.canonical_name, t.canonical_name, r.predicate,
                    r.weight, r.provenance_json, r.from_entity_id, r.to_entity_id
             FROM relations r
             JOIN entities f ON f.id = r.from_entity_id
             JOIN entities t ON t.id = r.to_entity_id
             WHERE r.kb = ?1 AND (r.from_entity_id = ?2 OR r.to_entity_id = ?2)
             ORDER BY r.weight DESC, r.predicate ASC LIMIT ?3",
        )?;
        let id = entity_id.to_string();
        let rows = stmt.query_map(params![kb, id, limit as i64], parse_relation_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Live chunks in `kb` whose text contains `needle` (case-insensitive).
    ///
    /// Used to attach readable memory snippets to a person without an
    /// embedding call, so `people show` works with no API key at all.
    pub fn chunks_containing(
        &self,
        kb: &str,
        needle: &str,
        limit: usize,
    ) -> Result<Vec<ChunkRecord>, StorageError> {
        let escaped = needle
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.memory_id, c.content, c.position, c.section, m.diary, m.kb,
                    c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE m.deleted = 0 AND m.kb = ?1
               AND lower(c.content) LIKE lower(?2) ESCAPE '\\'
             ORDER BY c.position ASC, c.id ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![kb, pattern, limit as i64], parse_chunk_record_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Fetch one chunk by id, if its memory is still live.
    pub fn get_chunk_record(&self, chunk_id: Uuid) -> Result<Option<ChunkRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.memory_id, c.content, c.position, c.section, m.diary, m.kb,
                    c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             WHERE c.id = ?1 AND m.deleted = 0",
        )?;
        stmt.query_row(params![chunk_id.to_string()], parse_chunk_record_row)
            .optional()
            .map_err(StorageError::from)
    }

    pub fn find_entity(
        &self,
        kb: Option<&str>,
        name: &str,
    ) -> Result<Option<EntityRecord>, StorageError> {
        let norm = name.to_ascii_lowercase();
        let sql = if kb.is_some() {
            "SELECT e.id, e.kb, e.canonical_name, e.aliases_json, e.kind, e.mention_count
             FROM entities e
             LEFT JOIN entity_aliases a ON a.entity_id = e.id
             WHERE e.kb = ?1 AND (lower(e.canonical_name) = ?2 OR a.alias_norm = ?2)
             LIMIT 1"
        } else {
            "SELECT e.id, e.kb, e.canonical_name, e.aliases_json, e.kind, e.mention_count
             FROM entities e
             LEFT JOIN entity_aliases a ON a.entity_id = e.id
             WHERE lower(e.canonical_name) = ?1 OR a.alias_norm = ?1
             LIMIT 1"
        };
        let mut stmt = self.conn.prepare(sql)?;
        if let Some(k) = kb {
            stmt.query_row(params![k, norm], parse_entity_row)
                .optional()
                .map_err(StorageError::from)
        } else {
            stmt.query_row(params![norm], parse_entity_row)
                .optional()
                .map_err(StorageError::from)
        }
    }

    /// Every entity matching `name` (canonical name or alias, case-insensitive).
    ///
    /// `find_entity` returns at most one row, which is wrong for an unscoped
    /// (all-KBs) lookup: the same person or concept usually exists once per KB,
    /// and picking an arbitrary one silently drops the others from retrieval.
    /// With `kb = None` this returns one entity per KB, best-mentioned first.
    pub fn find_entities(
        &self,
        kb: Option<&str>,
        name: &str,
        limit: usize,
    ) -> Result<Vec<EntityRecord>, StorageError> {
        let norm = name.to_ascii_lowercase();
        let sql = if kb.is_some() {
            "SELECT e.id, e.kb, e.canonical_name, e.aliases_json, e.kind, e.mention_count
             FROM entities e
             LEFT JOIN entity_aliases a ON a.entity_id = e.id
             WHERE e.kb = ?1 AND (lower(e.canonical_name) = ?2 OR a.alias_norm = ?2)
             GROUP BY e.id
             ORDER BY e.mention_count DESC, e.canonical_name ASC
             LIMIT ?3"
        } else {
            "SELECT e.id, e.kb, e.canonical_name, e.aliases_json, e.kind, e.mention_count
             FROM entities e
             LEFT JOIN entity_aliases a ON a.entity_id = e.id
             WHERE lower(e.canonical_name) = ?1 OR a.alias_norm = ?1
             GROUP BY e.id
             ORDER BY e.mention_count DESC, e.canonical_name ASC
             LIMIT ?2"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(params![k, norm, limit as i64], parse_entity_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![norm, limit as i64], parse_entity_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// KBs whose derived layer could still carry content from OUTSIDE `diary`.
    ///
    /// DIARY ISOLATION INVARIANT: a diary-scoped recall never returns content
    /// derived solely from another diary's memories. Compiler artifacts (wiki
    /// pages, entities, relations) record only a `kb`, never a diary, so the
    /// only sound test is at KB granularity: if a KB contains nothing but
    /// `diary`'s memories, anything derived from it is necessarily derived from
    /// `diary`. Any KB in this set is mixed, so its diary-less derived rows must
    /// be withheld from a recall scoped to `diary`.
    ///
    /// "Contains" spans soft-deleted memories that no compile has purged yet
    /// (`MEMORY_CONSTRAINS_DERIVED`). Counting only live rows meant `forget`
    /// could UN-gate a KB: delete the last memory of diary B and a recall
    /// scoped to diary A started seeing wiki pages and entity aggregates
    /// paraphrasing B's just-deleted private memory. Over-blocking (a KB stays
    /// gated until it is recompiled) is the acceptable failure here; leaking a
    /// deleted memory into another diary is not.
    pub fn kbs_with_memories_outside_diary(
        &self,
        diary: &str,
    ) -> Result<std::collections::HashSet<String>, StorageError> {
        let sql = format!(
            "SELECT DISTINCT m.kb FROM memories m
             WHERE m.diary <> ?1 AND {MEMORY_CONSTRAINS_DERIVED}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![diary], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// Filter `ids` down to the claims a recall scoped to `diary` may return.
    ///
    /// DIARY ISOLATION INVARIANT: a diary-scoped recall never returns content
    /// derived solely from another diary's memories. A claim is a compiler/LLM
    /// paraphrase of chunk text, so returning one whose evidence lives in
    /// another diary leaks that diary's content in rewritten form. Claims carry
    /// no diary column, but they do carry provenance
    /// (claim_evidence -> chunks -> memories.diary), so the check is exact:
    ///   - visible when any evidence resolves to a live memory in `diary`;
    ///   - a claim with no resolvable evidence is visible only when its whole KB
    ///     is confined to `diary` (nothing else could have produced it).
    ///
    /// DELIBERATE PARTIAL EXPOSURE — read this before "tightening" the first
    /// rule. `ANY evidence chunk in `diary`" is an OR, not an ALL: a claim
    /// synthesised from evidence spanning several diaries is admitted as soon
    /// as ONE of its evidence chunks belongs to the requested diary, even
    /// though the claim's wording may also reflect the other diaries' chunks.
    /// The alternative (require EVERY evidence chunk to be in `diary`) would
    /// suppress the claims a multi-diary KB exists to produce — a cross-cutting
    /// claim would become invisible from every diary, including the ones that
    /// genuinely contributed to it — and would make the layer useless in the
    /// exact case it is built for. So the trade is: a diary-scoped recall may
    /// see a claim it part-owns, but never one it has no stake in at all.
    /// Callers who need strict single-diary provenance must scope by KB (one
    /// KB per diary), which the KB-confinement rule above enforces exactly.
    ///
    /// The "no resolvable evidence" fallback uses the same
    /// `MEMORY_CONSTRAINS_DERIVED` rule as `kbs_with_memories_outside_diary`,
    /// so a soft-deleted memory keeps gating the KB until a compile actually
    /// purges it.
    pub fn claims_visible_in_diary(
        &self,
        ids: &[Uuid],
        diary: &str,
    ) -> Result<std::collections::HashSet<Uuid>, StorageError> {
        if ids.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let placeholders = (2..2 + ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT c.id FROM claims c
             WHERE c.id IN ({placeholders})
               AND (
                 EXISTS (
                   SELECT 1 FROM claim_evidence ce
                   JOIN chunks ch ON ch.id = ce.chunk_id
                   JOIN memories m ON m.id = ch.memory_id
                   WHERE ce.claim_id = c.id AND m.deleted = 0 AND m.diary = ?1
                 )
                 OR (
                   NOT EXISTS (
                     SELECT 1 FROM claim_evidence ce
                     JOIN chunks ch ON ch.id = ce.chunk_id
                     JOIN memories m ON m.id = ch.memory_id
                     WHERE ce.claim_id = c.id AND m.deleted = 0
                   )
                   AND NOT EXISTS (
                     SELECT 1 FROM memories m
                     WHERE m.kb = c.kb AND m.diary <> ?1
                       AND {MEMORY_CONSTRAINS_DERIVED}
                   )
                 )
               )"
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(ids.len() + 1);
        binds.push(Box::new(diary.to_string()));
        for id in ids {
            binds.push(Box::new(id.to_string()));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
            |r| r.get::<_, String>(0),
        )?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            if let Ok(id) = Uuid::parse_str(&row?) {
                out.insert(id);
            }
        }
        Ok(out)
    }

    pub fn graph_neighbors(
        &self,
        kb: &str,
        entity_name: &str,
        hops: u8,
    ) -> Result<Vec<RelationRecord>, StorageError> {
        let Some(seed) = self.find_entity(Some(kb), entity_name)? else {
            return Ok(Vec::new());
        };
        let mut frontier = vec![seed.id];
        let mut seen = std::collections::HashSet::new();
        let mut seen_relations = std::collections::HashSet::new();
        let mut out = Vec::new();
        for _ in 0..hops.max(1) {
            let mut next = Vec::new();
            for entity_id in &frontier {
                if !seen.insert(*entity_id) {
                    continue;
                }
                let mut stmt = self.conn.prepare(
                    "SELECT r.id, r.kb, f.canonical_name, t.canonical_name, r.predicate,
                            r.weight, r.provenance_json, r.from_entity_id, r.to_entity_id
                     FROM relations r
                     JOIN entities f ON f.id = r.from_entity_id
                     JOIN entities t ON t.id = r.to_entity_id
                     WHERE r.kb = ?1 AND (r.from_entity_id = ?2 OR r.to_entity_id = ?2)
                     ORDER BY r.weight DESC LIMIT 50",
                )?;
                let current = entity_id.to_string();
                let rows = stmt.query_map(params![kb, current.clone()], |row| {
                    let rel = parse_relation_row(row)?;
                    let from_id: String = row.get(7)?;
                    let to_id: String = row.get(8)?;
                    let neighbor_id = if from_id == current { to_id } else { from_id };
                    Ok((rel, Uuid::parse_str(&neighbor_id).unwrap_or(Uuid::nil())))
                })?;
                for row in rows {
                    let (rel, neighbor_id) = row?;
                    if neighbor_id != Uuid::nil() {
                        next.push(neighbor_id);
                    }
                    if seen_relations.insert(rel.id) {
                        out.push(rel);
                    }
                }
            }
            frontier = next;
        }
        Ok(out)
    }

    pub fn upsert_wiki_page(
        &self,
        kb: &str,
        path: &str,
        title: &str,
        content: &str,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO wiki_pages(id, kb, path, title, content, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(kb, path) DO UPDATE SET
                title = excluded.title,
                content = excluded.content,
                updated_at = excluded.updated_at",
            params![id.to_string(), kb, path, title, content, now],
        )?;
        let stored: String = self.conn.query_row(
            "SELECT id FROM wiki_pages WHERE kb = ?1 AND path = ?2",
            params![kb, path],
            |r| r.get(0),
        )?;
        Ok(Uuid::parse_str(&stored).unwrap_or(id))
    }

    pub fn list_wiki_pages(&self, kb: &str) -> Result<Vec<WikiPageRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kb, path, title, content, created_at, updated_at
             FROM wiki_pages WHERE kb = ?1 ORDER BY path ASC",
        )?;
        let rows = stmt.query_map(params![kb], parse_wiki_page_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn get_wiki_page(
        &self,
        kb: &str,
        path: &str,
    ) -> Result<Option<WikiPageRecord>, StorageError> {
        self.conn
            .query_row(
                "SELECT id, kb, path, title, content, created_at, updated_at
                 FROM wiki_pages WHERE kb = ?1 AND path = ?2",
                params![kb, path],
                parse_wiki_page_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn get_wiki_page_by_id(&self, id: Uuid) -> Result<Option<WikiPageRecord>, StorageError> {
        self.conn
            .query_row(
                "SELECT id, kb, path, title, content, created_at, updated_at
                 FROM wiki_pages WHERE id = ?1",
                params![id.to_string()],
                parse_wiki_page_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn wiki_fts_search(
        &self,
        query: &str,
        kb: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>, StorageError> {
        let sql = if kb.is_some() {
            "SELECT wiki_pages.id, -bm25(wiki_pages_fts)
             FROM wiki_pages_fts
             JOIN wiki_pages ON wiki_pages.rowid = wiki_pages_fts.rowid
             WHERE wiki_pages_fts MATCH ?1 AND wiki_pages.kb = ?2
             ORDER BY bm25(wiki_pages_fts)
             LIMIT ?3"
        } else {
            "SELECT wiki_pages.id, -bm25(wiki_pages_fts)
             FROM wiki_pages_fts
             JOIN wiki_pages ON wiki_pages.rowid = wiki_pages_fts.rowid
             WHERE wiki_pages_fts MATCH ?1
             ORDER BY bm25(wiki_pages_fts)
             LIMIT ?2"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(params![query, k, limit as i64], parse_id_score_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![query, limit as i64], parse_id_score_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows
            .into_iter()
            .filter_map(|(id_str, score)| Uuid::parse_str(&id_str).ok().map(|id| (id, score)))
            .collect())
    }

    // ========================================================================
    // Facts — atomic (subject, predicate, object) triples for contradiction
    // detection. Lean MVP: insert, lookup by (subject_norm, predicate),
    // supersede, browse. No bi-temporal windows, no entity resolution beyond
    // lowercase.
    // ========================================================================

    /// Insert a new fact and append its `fact.observed`, in one transaction.
    /// Caller is responsible for first checking whether it conflicts with an
    /// existing active fact and superseding the old one.
    pub fn insert_fact(&self, f: &engram_core::types::Fact) -> Result<(), StorageError> {
        self.with_tx(|s| s.insert_fact_row(f))
    }

    fn insert_fact_row(&self, f: &engram_core::types::Fact) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO facts(id, source_memory_id, subject, subject_norm,
                predicate, object, object_norm, confidence, created_at,
                superseded_by, superseded_at, diary,
                fact_type, valid_from, valid_until, resolution, source_kind,
                confirmations, closed_by, close_reason, close_authority, kb)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21,
                CASE WHEN ?22 = '' THEN
                    COALESCE((SELECT kb FROM memories WHERE id = ?2), 'default')
                ELSE ?22 END)",
            params![
                f.id.to_string(),
                f.source_memory_id.to_string(),
                f.subject,
                f.subject_norm,
                f.predicate,
                f.object,
                f.object_norm,
                f.confidence,
                f.created_at.to_rfc3339(),
                f.superseded_by.map(|u| u.to_string()),
                f.superseded_at.map(|t| t.to_rfc3339()),
                f.diary,
                f.fact_type.as_str(),
                f.valid_from.map(|t| t.to_rfc3339()),
                f.valid_until.map(|t| t.to_rfc3339()),
                f.resolution,
                f.source_kind.as_str(),
                f.confirmations as i64,
                f.closed_by.map(|u| u.to_string()),
                f.close_reason,
                f.close_authority,
                f.kb,
            ],
        )?;
        // The stored kb is read BACK rather than assumed: when the caller
        // leaves it empty the column falls back to the source memory's KB, and
        // the event has to carry what was actually written or a replay re-runs
        // the fallback against a memory that may have moved since.
        let stored_kb: String = self.conn.query_row(
            "SELECT kb FROM facts WHERE id = ?1",
            params![f.id.to_string()],
            |r| r.get(0),
        )?;
        self.append_event(
            &crate::journal::EventDraft::new(
                engram_core::events::EventKind::FactObserved,
                crate::journal::cli_actor(&f.diary),
                crate::journal::payload::fact_observed(f, false),
            )
            .scope(Some(&stored_kb), Some(&f.diary))
            .subject(f.id),
        )?;
        Ok(())
    }

    /// Get currently-active facts (not superseded) about a subject + predicate.
    /// Used at write time to check whether a new fact contradicts existing ones.
    /// Filters by diary so specialist agents don't pollute each other's facts.
    pub fn get_active_facts(
        &self,
        subject_norm: &str,
        predicate: &str,
        diary: &str,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {facts}
             FROM facts
             WHERE subject_norm = ?1 AND predicate = ?2 AND diary = ?3
                   AND {open}
             ORDER BY created_at DESC",
            facts = fact_columns(""),
            open = OPEN_INTERVAL_PREDICATE
        ))?;
        let rows = stmt.query_map(params![subject_norm, predicate, diary], parse_fact_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Like `get_active_facts` but ALSO scoped to a KB. A drug's dosage in the
    /// `ageing-biology` KB must not be seen to contradict the same subject in an
    /// unrelated KB — contradiction detection is only meaningful within one
    /// source corpus. Used by the KB-aware write path.
    pub fn get_active_facts_in_kb(
        &self,
        subject_norm: &str,
        predicate: &str,
        diary: &str,
        kb: &str,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {facts}
             FROM facts
             WHERE subject_norm = ?1 AND predicate = ?2 AND diary = ?3 AND kb = ?4
                   AND {open}
             ORDER BY created_at DESC",
            facts = fact_columns(""),
            open = OPEN_INTERVAL_PREDICATE
        ))?;
        let rows = stmt.query_map(params![subject_norm, predicate, diary, kb], parse_fact_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Facts (active + their superseded history) in a KB whose subject or object
    /// contains any of the given lowercase terms. Used by `recall` to surface
    /// contradictions relevant to the query ("this drug's dose was updated"),
    /// so the read path can actually deliver the "resolves contradictions"
    /// promise instead of silently returning stale facts.
    pub fn facts_matching_terms(
        &self,
        kb: Option<&str>,
        diary: Option<&str>,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = format!("SELECT {} FROM facts WHERE ", fact_columns(""));
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(k) = kb {
            binds.push(Box::new(k.to_string()));
            sql.push_str(&format!("kb = ?{} AND ", binds.len()));
        }
        // Without this, `recall` surfaces another diary's facts alongside its
        // own — the cross-diary leak the diary namespace exists to prevent.
        if let Some(d) = diary {
            binds.push(Box::new(d.to_string()));
            sql.push_str(&format!("diary = ?{} AND ", binds.len()));
        }
        let mut clauses = Vec::new();
        for term in terms {
            binds.push(Box::new(format!("%{}%", term.to_ascii_lowercase())));
            let a = binds.len();
            binds.push(Box::new(format!("%{}%", term.to_ascii_lowercase())));
            let b = binds.len();
            clauses.push(format!("subject_norm LIKE ?{a} OR object_norm LIKE ?{b}"));
        }
        sql.push('(');
        sql.push_str(&clauses.join(" OR "));
        sql.push(')');
        binds.push(Box::new(limit as i64));
        sql.push_str(&format!(
            " ORDER BY (superseded_by IS NOT NULL) DESC, created_at DESC LIMIT ?{}",
            binds.len()
        ));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
                parse_fact_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Look up a single fact by id (for pairing superseded → superseding).
    pub fn get_fact(&self, id: Uuid) -> Result<Option<engram_core::types::Fact>, StorageError> {
        self.conn
            .query_row(
                &format!("SELECT {} FROM facts WHERE id = ?1", fact_columns("")),
                params![id.to_string()],
                parse_fact_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Mark `old_id` as superseded by `new_id`. Sets `superseded_at = now`.
    /// Returns true if a row was updated.
    pub fn supersede_fact(&self, old_id: Uuid, new_id: Uuid) -> Result<bool, StorageError> {
        self.supersede_fact_with(old_id, new_id, "exact_match", None)
    }

    /// Supersede with an explicit resolution audit tag and (optionally) an
    /// event-time close of the old fact's validity window. `valid_until`
    /// should be the new fact's `valid_from` when known so the bi-temporal
    /// history reads "true until the replacement became true".
    ///
    /// Re-expressed over [`SqliteStore::close_fact_interval`] so every close
    /// lands in the journal, with the authority DERIVED from the `resolution`
    /// tag the caller already passes. Stamping `heuristic` unconditionally —
    /// which is what the signature invited — makes the safety audit report the
    /// exact opposite of the truth for `facts resolve`, whose whole point is
    /// that a person decided.
    pub fn supersede_fact_with(
        &self,
        old_id: Uuid,
        new_id: Uuid,
        resolution: &str,
        valid_until: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<bool, StorageError> {
        let at = valid_until.unwrap_or_else(chrono::Utc::now);
        let authority = close_authority_for(resolution);
        self.close_fact_interval(
            old_id,
            at,
            engram_core::interval::CloseReason::Superseded.as_str(),
            authority,
            Some(new_id),
            Some(resolution),
        )
    }

    /// Store the embedding of a fact's "{subject} {predicate} {object}" string.
    /// Used by the write-time novelty gate to catch paraphrased contradictions
    /// that exact (subject_norm, predicate) matching misses.
    pub fn set_fact_embedding(
        &self,
        id: Uuid,
        embedding: &[f32],
        model: &str,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE facts SET embedding = ?1, embed_model = ?2 WHERE id = ?3",
            params![f32_slice_to_bytes(embedding), model, id.to_string()],
        )?;
        Ok(())
    }

    /// Active facts in (diary, kb) that carry an embedding from `model`.
    /// Returns (fact_id, embedding). Fact stores are small (thousands, not
    /// millions) so a full scan per write is fine.
    pub fn active_fact_embeddings(
        &self,
        diary: &str,
        kb: &str,
        model: &str,
    ) -> Result<Vec<(Uuid, Vec<f32>)>, StorageError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, embedding FROM facts
             WHERE diary = ?1 AND kb = ?2 AND embed_model = ?3
                   AND embedding IS NOT NULL AND {OPEN_INTERVAL_PREDICATE}"
        ))?;
        let rows = stmt.query_map(params![diary, kb, model], |r| {
            let id_str: String = r.get(0)?;
            let blob: Vec<u8> = r.get(1)?;
            Ok((
                Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
                f32_bytes_to_vec(&blob),
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// A re-asserted duplicate bumps the existing fact's confirmation count
    /// instead of inserting a second row. Confirmations feed trust weighting.
    pub fn increment_fact_confirmations(&self, id: Uuid) -> Result<(), StorageError> {
        self.with_tx(|s| {
            let n = s.conn.execute(
                "UPDATE facts SET confirmations = confirmations + 1 WHERE id = ?1",
                params![id.to_string()],
            )?;
            if n == 0 {
                return Ok(());
            }
            let scope: Option<(String, String)> = s
                .conn
                .query_row(
                    "SELECT kb, diary FROM facts WHERE id = ?1",
                    params![id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (kb, diary) = scope.unwrap_or_else(|| ("default".into(), "default".into()));
            s.append_event(
                &crate::journal::EventDraft::new(
                    engram_core::events::EventKind::FactConfirmed,
                    crate::journal::cli_actor(&diary),
                    crate::journal::payload::fact_confirmed(id),
                )
                .scope(Some(&kb), Some(&diary))
                .subject(id),
            )?;
            Ok(())
        })
    }

    /// Resolve a name against the compile-time entity alias table: returns
    /// the canonical entity name when `alias_norm` is a recorded alias in
    /// this KB. Part of the subject-canonicalization cascade.
    pub fn resolve_entity_alias(
        &self,
        kb: &str,
        alias_norm: &str,
    ) -> Result<Option<String>, StorageError> {
        self.conn
            .query_row(
                "SELECT e.canonical_name
                 FROM entity_aliases a JOIN entities e ON e.id = a.entity_id
                 WHERE a.kb = ?1 AND a.alias_norm = ?2",
                params![kb, alias_norm],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Distinct (display, norm) subjects of active facts in (diary, kb).
    /// Small by construction; used by fuzzy subject canonicalization.
    /// `None` spans every diary / every KB, matching
    /// `list_facts_by_subject_scoped`. The (display, norm) PAIR is kept rather
    /// than narrowed to the norm: fuzzy subject canonicalization compares
    /// against what a person actually typed.
    pub fn distinct_fact_subjects(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
    ) -> Result<Vec<(String, String)>, StorageError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT subject, subject_norm FROM facts
             WHERE (?1 IS NULL OR diary = ?1) AND (?2 IS NULL OR kb = ?2)
                   AND {OPEN_INTERVAL_PREDICATE}
             GROUP BY subject_norm"
        ))?;
        let rows = stmt.query_map(params![diary, kb], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    // ----- Fact conflict queue (cooperative adjudication) -----

    /// Queue an unresolved contradiction between two active facts. Both facts
    /// stay active ("contested") until the caller or a consolidation sweep
    /// resolves it — the store never pretends to know an answer it doesn't
    /// have.
    ///
    /// Returns WHICH negative result happened, because `INSERT OR IGNORE`
    /// returning `false` conflated "this exact ordered pair is already queued"
    /// with "a different pair asking the same question is already queued", and
    /// only the second one honestly means `deduplicated`. An empty
    /// `decision_key` is stored as SQL NULL, precisely so a caller that has no
    /// key yet cannot collide every row under `idx_fcq_open_key`.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_fact_conflict(
        &self,
        id: Uuid,
        new_fact_id: Uuid,
        old_fact_id: Uuid,
        diary: &str,
        kb: &str,
        detector: &str,
        score: Option<f32>,
        suggested: &str,
        decision_key: &str,
        member_key: &str,
        registry_version: &str,
    ) -> Result<InsertOutcome, StorageError> {
        let key: Option<&str> = Some(decision_key).filter(|k| !k.is_empty());
        let member: Option<&str> = Some(member_key).filter(|k| !k.is_empty());
        self.with_tx(|s| {
            // Both pre-checks run inside the caller's BEGIN IMMEDIATE, so they
            // cannot race the insert that follows them.
            let same_pair: Option<String> = s
                .conn
                .query_row(
                    "SELECT id FROM fact_conflict_queue
                      WHERE new_fact_id = ?1 AND old_fact_id = ?2",
                    params![new_fact_id.to_string(), old_fact_id.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(existing) = same_pair {
                return Ok(InsertOutcome::DuplicatePair {
                    existing: Uuid::parse_str(&existing).unwrap_or(Uuid::nil()),
                });
            }
            if let Some(k) = key {
                let same_key: Option<String> = s
                    .conn
                    .query_row(
                        "SELECT id FROM fact_conflict_queue
                          WHERE kb = ?1 AND decision_key = ?2 AND status = 'open'",
                        params![kb, k],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = same_key {
                    return Ok(InsertOutcome::DuplicateKey {
                        existing: Uuid::parse_str(&existing).unwrap_or(Uuid::nil()),
                    });
                }
            }
            let created_at = chrono::Utc::now().to_rfc3339();
            // A plain INSERT, not INSERT OR IGNORE: an ignored insert is
            // exactly the ambiguity this signature exists to remove.
            let inserted = s.conn.execute(
                "INSERT INTO fact_conflict_queue
                    (id, new_fact_id, old_fact_id, diary, kb, detector, score,
                     suggested, status, created_at, decision_key, member_key, registry_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9, ?10, ?11, ?12)",
                params![
                    id.to_string(),
                    new_fact_id.to_string(),
                    old_fact_id.to_string(),
                    diary,
                    kb,
                    detector,
                    score,
                    suggested,
                    created_at,
                    key,
                    member,
                    registry_version,
                ],
            );
            match inserted {
                Ok(_) => {}
                Err(e) => {
                    // A constraint violation surviving both pre-checks is
                    // re-queried once and mapped to whichever duplicate it was.
                    // If neither query finds a row the error propagates: never
                    // report `Inserted` for a row that is not there.
                    return s.classify_conflict_insert_failure(
                        new_fact_id,
                        old_fact_id,
                        kb,
                        key,
                        e,
                    );
                }
            }
            s.append_event(
                &crate::journal::EventDraft::new(
                    engram_core::events::EventKind::ConflictOpened,
                    crate::journal::cli_actor(diary),
                    crate::journal::payload::conflict_opened(
                        id,
                        new_fact_id,
                        old_fact_id,
                        detector,
                        score,
                        Some(suggested),
                        None,
                        key,
                        member,
                        Some(registry_version),
                        &created_at,
                        false,
                    ),
                )
                .scope(Some(kb), Some(diary))
                .subject(id),
            )?;
            Ok(InsertOutcome::Inserted)
        })
    }

    fn classify_conflict_insert_failure(
        &self,
        new_fact_id: Uuid,
        old_fact_id: Uuid,
        kb: &str,
        key: Option<&str>,
        original: rusqlite::Error,
    ) -> Result<InsertOutcome, StorageError> {
        if let Some(existing) = self
            .conn
            .query_row(
                "SELECT id FROM fact_conflict_queue WHERE new_fact_id = ?1 AND old_fact_id = ?2",
                params![new_fact_id.to_string(), old_fact_id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(InsertOutcome::DuplicatePair {
                existing: Uuid::parse_str(&existing).unwrap_or(Uuid::nil()),
            });
        }
        if let Some(k) = key {
            if let Some(existing) = self
                .conn
                .query_row(
                    "SELECT id FROM fact_conflict_queue
                      WHERE kb = ?1 AND decision_key = ?2 AND status = 'open'",
                    params![kb, k],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            {
                return Ok(InsertOutcome::DuplicateKey {
                    existing: Uuid::parse_str(&existing).unwrap_or(Uuid::nil()),
                });
            }
        }
        Err(StorageError::from(original))
    }

    pub fn get_fact_conflict(&self, id: Uuid) -> Result<Option<FactConflictRow>, StorageError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {} FROM fact_conflict_queue WHERE id = ?1",
                    conflict_columns("")
                ),
                params![id.to_string()],
                parse_conflict_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn list_open_fact_conflicts(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FactConflictRow>, StorageError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {conflict}
             FROM fact_conflict_queue
             WHERE status = 'open'
                   AND (?1 IS NULL OR diary = ?1)
                   AND (?2 IS NULL OR kb = ?2)
             ORDER BY created_at DESC LIMIT ?3",
            conflict = conflict_columns("")
        ))?;
        let rows = stmt.query_map(params![diary, kb, limit as i64], parse_conflict_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// How many conflicts are still awaiting a decision. Cheap enough to
    /// report on every conflict listing, which is the only way a caller
    /// learns that a backlog exists at all.
    pub fn count_open_fact_conflicts(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
    ) -> Result<i64, StorageError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM fact_conflict_queue
             WHERE status = 'open'
                   AND (?1 IS NULL OR diary = ?1)
                   AND (?2 IS NULL OR kb = ?2)",
            params![diary, kb],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Open conflicts joined to BOTH facts in one pass, newest first.
    ///
    /// `facts triage` re-classifies the whole backlog; fetching each side
    /// with a separate `get_fact` turned a 9,510-row queue into ~19,000 point
    /// queries. `subject` (optional) matches either side's `subject_norm` as
    /// a substring, so a person's backlog can be inspected on its own.
    /// Rows whose facts were deleted are absent by construction — see
    /// `resolve_orphan_fact_conflicts`.
    pub fn list_open_fact_conflicts_with_facts(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
        subject: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<
        Vec<(
            FactConflictRow,
            engram_core::types::Fact,
            engram_core::types::Fact,
        )>,
        StorageError,
    > {
        let subject_like = subject
            .map(|s| format!("%{}%", s.trim().to_lowercase()))
            .filter(|s| s.len() > 2);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {conflict}, {new_fact}, {old_fact}
             FROM fact_conflict_queue q
             JOIN facts nf ON nf.id = q.new_fact_id
             JOIN facts of ON of.id = q.old_fact_id
             WHERE q.status = 'open'
                   AND (?1 IS NULL OR q.diary = ?1)
                   AND (?2 IS NULL OR q.kb = ?2)
                   AND (?3 IS NULL OR nf.subject_norm LIKE ?3 OR of.subject_norm LIKE ?3)
             ORDER BY q.created_at DESC, q.id LIMIT ?4 OFFSET ?5",
            conflict = conflict_columns("q."),
            new_fact = fact_columns("nf."),
            old_fact = fact_columns("of.")
        ))?;
        // `facts triage` asks for the whole backlog by passing usize::MAX;
        // casting that straight to i64 wraps to -1, which SQLite happens to
        // read as "no limit". Saturating is the same behaviour on purpose.
        let capped = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(
            params![diary, kb, subject_like, capped, offset as i64],
            |r| {
                let row = parse_conflict_row(r)?;
                let new_fact = parse_fact_from_offset(r, CONFLICT_COLUMN_COUNT)?;
                let old_fact =
                    parse_fact_from_offset(r, CONFLICT_COLUMN_COUNT + FACT_COLUMN_COUNT)?;
                Ok((row, new_fact, old_fact))
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Close open conflicts whose facts no longer exist. A hard-deleted
    /// memory takes its facts with it; without this the queue keeps rows that
    /// can never be adjudicated and that inflate every backlog count.
    /// Returns the number of rows closed.
    pub fn resolve_orphan_fact_conflicts(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
        resolved_by: &str,
    ) -> Result<usize, StorageError> {
        self.with_tx(|s| {
            let doomed: Vec<(String, String, String, Option<String>)> = {
                let mut stmt = s.conn.prepare(
                    "SELECT id, kb, diary, decision_key FROM fact_conflict_queue
                      WHERE status = 'open'
                        AND (?1 IS NULL OR diary = ?1)
                        AND (?2 IS NULL OR kb = ?2)
                        AND (new_fact_id NOT IN (SELECT id FROM facts)
                             OR old_fact_id NOT IN (SELECT id FROM facts))",
                )?;
                let rows = stmt.query_map(params![diary, kb], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let n = s.conn.execute(
                "UPDATE fact_conflict_queue
                 SET status = 'resolved', resolved_at = ?1, resolved_by = ?2,
                     note = COALESCE(note, 'auto: referenced facts no longer exist')
                 WHERE status = 'open'
                       AND (?3 IS NULL OR diary = ?3)
                       AND (?4 IS NULL OR kb = ?4)
                       AND (new_fact_id NOT IN (SELECT id FROM facts)
                            OR old_fact_id NOT IN (SELECT id FROM facts))",
                params![chrono::Utc::now().to_rfc3339(), resolved_by, diary, kb],
            )?;
            for (id, row_kb, row_diary, decision_key) in doomed {
                let conflict_id = Uuid::parse_str(&id).ok();
                // A row whose facts are gone may have no key at all. The
                // synthetic one is shaped so it can never collide with a real
                // SemanticKey encoding, and therefore can never replay onto an
                // unrelated pair.
                let key = decision_key.unwrap_or_else(|| {
                    engram_core::keys::semantic_key_encode("orphan", "orphan", &id)
                });
                s.record_decision(
                    &row_kb,
                    &row_diary,
                    &key,
                    "void",
                    "machine",
                    resolved_by,
                    Some("auto: referenced facts no longer exist"),
                    conflict_id,
                    engram_core::keys::SEMANTIC_KEY_VERSION,
                )?;
            }
            Ok(n)
        })
    }

    /// Source-memory preview for each fact id: `(memory_id, first line)`.
    /// A queued conflict is unreadable without the sentences the two values
    /// came from — "Lisbon vs Porto" says nothing on its own.
    pub fn fact_source_previews(
        &self,
        fact_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, (Uuid, String)>, StorageError> {
        let mut out = std::collections::HashMap::new();
        if fact_ids.is_empty() {
            return Ok(out);
        }
        let placeholders = (1..=fact_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT f.id, m.id, m.content FROM facts f
             JOIN memories m ON m.id = f.source_memory_id
             WHERE f.id IN ({placeholders})"
        );
        let binds: Vec<String> = fact_ids.iter().map(|i| i.to_string()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (fid, mid, content) = row?;
            let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut preview: String = flat.chars().take(120).collect();
            if flat.chars().count() > 120 {
                preview.push('…');
            }
            if let (Ok(fid), Ok(mid)) = (Uuid::parse_str(&fid), Uuid::parse_str(&mid)) {
                out.insert(fid, (mid, preview));
            }
        }
        Ok(out)
    }

    // `resolve_fact_conflict` used to live here: a bare
    // `UPDATE fact_conflict_queue SET status='resolved'` with no event, kept
    // "for compatibility with the paths that already call it". By the time
    // wave 6 swept for it, no path did — and a rebuild replayed the row's
    // `conflict.opened` and reopened every question it had answered, note and
    // all. `resolve_conflict_with_decision` is the only way to answer a
    // queued question, because it is the only one that also records the
    // ruling (invariant 1, and blocker 6's defect class).

    /// Open conflicts in a KB whose facts mention any of the query terms —
    /// used by `recall` to surface contested values instead of silently
    /// returning one side of an unresolved contradiction.
    pub fn open_conflicts_matching_terms(
        &self,
        kb: Option<&str>,
        diary: Option<&str>,
        terms: &[String],
        limit: usize,
    ) -> Result<
        Vec<(
            FactConflictRow,
            engram_core::types::Fact,
            engram_core::types::Fact,
        )>,
        StorageError,
    > {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = format!(
            "SELECT {}
             FROM fact_conflict_queue q
             JOIN facts nf ON nf.id = q.new_fact_id
             JOIN facts of ON of.id = q.old_fact_id
             WHERE q.status = 'open' AND ",
            conflict_columns("q.")
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(k) = kb {
            binds.push(Box::new(k.to_string()));
            sql.push_str(&format!("q.kb = ?{} AND ", binds.len()));
        }
        // Same leak, on the contradictions half of the same response.
        if let Some(d) = diary {
            binds.push(Box::new(d.to_string()));
            sql.push_str(&format!("q.diary = ?{} AND ", binds.len()));
        }
        let mut clauses = Vec::new();
        for term in terms {
            let like = format!("%{}%", term.to_ascii_lowercase());
            binds.push(Box::new(like.clone()));
            let a = binds.len();
            binds.push(Box::new(like.clone()));
            let b = binds.len();
            binds.push(Box::new(like.clone()));
            let c = binds.len();
            binds.push(Box::new(like));
            let d = binds.len();
            clauses.push(format!(
                "nf.subject_norm LIKE ?{a} OR nf.object_norm LIKE ?{b}                  OR of.subject_norm LIKE ?{c} OR of.object_norm LIKE ?{d}"
            ));
        }
        sql.push('(');
        sql.push_str(&clauses.join(" OR "));
        sql.push(')');
        binds.push(Box::new(limit as i64));
        sql.push_str(&format!(
            " ORDER BY q.created_at DESC LIMIT ?{}",
            binds.len()
        ));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
                parse_conflict_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for row in rows {
            let (Some(nf), Some(of)) = (
                self.get_fact(row.new_fact_id)?,
                self.get_fact(row.old_fact_id)?,
            ) else {
                continue;
            };
            out.push((row, nf, of));
        }
        Ok(out)
    }

    /// All facts about a subject (active + superseded), newest first.
    /// `diary = None` means search across all diaries.
    pub fn list_facts_by_subject(
        &self,
        subject_norm: &str,
        diary: Option<&str>,
        include_superseded: bool,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        let sql = match (diary, include_superseded) {
            (Some(_), true) => {
                "SELECT {facts}
                 FROM facts WHERE subject_norm = ?1 AND diary = ?2
                 ORDER BY created_at DESC"
            }
            (Some(_), false) => {
                "SELECT {facts}
                 FROM facts WHERE subject_norm = ?1 AND diary = ?2
                       AND {open}
                 ORDER BY created_at DESC"
            }
            (None, true) => {
                "SELECT {facts}
                 FROM facts WHERE subject_norm = ?1
                 ORDER BY created_at DESC"
            }
            (None, false) => {
                "SELECT {facts}
                 FROM facts WHERE subject_norm = ?1 AND {open}
                 ORDER BY created_at DESC"
            }
        };
        let sql = sql
            .replace("{facts}", &fact_columns(""))
            .replace("{open}", OPEN_INTERVAL_PREDICATE);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<engram_core::types::Fact> = match diary {
            Some(d) => stmt
                .query_map(params![subject_norm, d], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![subject_norm], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    /// All facts (active by default), newest first. For `engram facts list`.
    pub fn list_facts(
        &self,
        diary: Option<&str>,
        include_superseded: bool,
        limit: usize,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        self.list_facts_scoped(diary, None, include_superseded, limit, 0)
    }

    fn list_facts_unscoped(
        &self,
        diary: Option<&str>,
        include_superseded: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        let sql = match (diary, include_superseded) {
            (Some(_), true) => {
                "SELECT {facts}
                 FROM facts WHERE diary = ?1 ORDER BY created_at DESC LIMIT ?2 OFFSET ?3"
            }
            (Some(_), false) => {
                "SELECT {facts}
                 FROM facts WHERE diary = ?1 AND {open}
                 ORDER BY created_at DESC LIMIT ?2 OFFSET ?3"
            }
            (None, true) => {
                "SELECT {facts}
                 FROM facts ORDER BY created_at DESC LIMIT ?1 OFFSET ?2"
            }
            (None, false) => {
                "SELECT {facts}
                 FROM facts WHERE {open}
                 ORDER BY created_at DESC LIMIT ?1 OFFSET ?2"
            }
        };
        let sql = sql
            .replace("{facts}", &fact_columns(""))
            .replace("{open}", OPEN_INTERVAL_PREDICATE);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<engram_core::types::Fact> = match diary {
            Some(d) => stmt
                .query_map(params![d, limit as i64, offset as i64], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![limit as i64, offset as i64], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    pub fn list_facts_scoped(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
        include_superseded: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        if kb.is_none() {
            return self.list_facts_unscoped(diary, include_superseded, limit, offset);
        }
        let k = kb.unwrap();
        let sql = match (diary, include_superseded) {
            (Some(_), true) => {
                "SELECT {facts}
                 FROM facts WHERE kb = ?1 AND diary = ?2
                 ORDER BY created_at DESC LIMIT ?3 OFFSET ?4"
            }
            (Some(_), false) => {
                "SELECT {facts}
                 FROM facts WHERE kb = ?1 AND diary = ?2 AND {open}
                 ORDER BY created_at DESC LIMIT ?3 OFFSET ?4"
            }
            (None, true) => {
                "SELECT {facts}
                 FROM facts WHERE kb = ?1 ORDER BY created_at DESC LIMIT ?2 OFFSET ?3"
            }
            (None, false) => {
                "SELECT {facts}
                 FROM facts WHERE kb = ?1 AND {open}
                 ORDER BY created_at DESC LIMIT ?2 OFFSET ?3"
            }
        };
        let sql = sql
            .replace("{facts}", &fact_columns(""))
            .replace("{open}", OPEN_INTERVAL_PREDICATE);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<engram_core::types::Fact> = match diary {
            Some(d) => stmt
                .query_map(params![k, d, limit as i64, offset as i64], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![k, limit as i64, offset as i64], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    pub fn list_facts_by_subject_scoped(
        &self,
        subject_norm: &str,
        diary: Option<&str>,
        kb: Option<&str>,
        include_superseded: bool,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        if kb.is_none() {
            return self.list_facts_by_subject(subject_norm, diary, include_superseded);
        }
        let k = kb.unwrap();
        let sql = match (diary, include_superseded) {
            (Some(_), true) => {
                "SELECT {facts}
                 FROM facts WHERE kb = ?1 AND subject_norm = ?2 AND diary = ?3
                 ORDER BY created_at DESC"
            }
            (Some(_), false) => {
                "SELECT {facts}
                 FROM facts WHERE kb = ?1 AND subject_norm = ?2 AND diary = ?3
                       AND {open}
                 ORDER BY created_at DESC"
            }
            (None, true) => {
                "SELECT {facts}
                 FROM facts WHERE kb = ?1 AND subject_norm = ?2
                 ORDER BY created_at DESC"
            }
            (None, false) => {
                "SELECT {facts}
                 FROM facts WHERE kb = ?1 AND subject_norm = ?2 AND {open}
                 ORDER BY created_at DESC"
            }
        };
        let sql = sql
            .replace("{facts}", &fact_columns(""))
            .replace("{open}", OPEN_INTERVAL_PREDICATE);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<engram_core::types::Fact> = match diary {
            Some(d) => stmt
                .query_map(params![k, subject_norm, d], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![k, subject_norm], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    /// All recent contradictions: facts that were superseded.
    /// Returns the OLD fact alongside the new one that replaced it.
    pub fn list_recent_conflicts(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<(engram_core::types::Fact, engram_core::types::Fact)>, StorageError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {old_fact}, {new_fact}
             FROM facts old
             JOIN facts new ON new.id = old.superseded_by
             WHERE old.superseded_by IS NOT NULL
                   AND (?1 IS NULL OR old.diary = ?1)
                   AND (?2 IS NULL OR old.kb = ?2)
             ORDER BY old.superseded_at DESC
             LIMIT ?3 OFFSET ?4",
            old_fact = fact_columns("old."),
            new_fact = fact_columns("new.")
        ))?;
        let rows = stmt.query_map(params![diary, kb, limit as i64, offset as i64], |r| {
            let old = parse_fact_from_offset(r, 0)?;
            let new = parse_fact_from_offset(r, FACT_COLUMN_COUNT)?;
            Ok((old, new))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn count_facts(&self) -> Result<i64, StorageError> {
        let n: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM facts WHERE {OPEN_INTERVAL_PREDICATE}"),
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    // ----- Full-fidelity export/import -----

    pub fn export_kbs(&self, kb: Option<&str>) -> Result<Vec<KbExportRow>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT name, description, created_at, updated_at FROM knowledge_bases
             WHERE (?1 IS NULL OR name = ?1) ORDER BY name",
        )?;
        let rows = stmt.query_map(params![kb], |r| {
            Ok(KbExportRow {
                name: r.get(0)?,
                description: r.get(1)?,
                created_at: r.get(2)?,
                updated_at: r.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn export_documents(
        &self,
        kb: Option<&str>,
    ) -> Result<Vec<DocumentExportRow>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kb, title, source_path, source_url, doi, authors_json, year, mode,
                    created_at, metadata_json
             FROM documents WHERE (?1 IS NULL OR kb = ?1) ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![kb], |r| {
            Ok(DocumentExportRow {
                id: r.get(0)?,
                kb: r.get(1)?,
                title: r.get(2)?,
                source_path: r.get(3)?,
                source_url: r.get(4)?,
                doi: r.get(5)?,
                authors_json: r.get(6)?,
                year: r.get(7)?,
                mode: r.get(8)?,
                created_at: r.get(9)?,
                metadata_json: r.get(10)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn export_memories(&self, kb: Option<&str>) -> Result<Vec<MemoryExportRow>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, created_at, event_time, importance, emotional_weight,
                    access_count, last_accessed, stability, source_json, diary,
                    valid_from, valid_until, tags_json, kb, deleted, deleted_at
             FROM memories WHERE (?1 IS NULL OR kb = ?1)
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![kb], |r| {
            Ok(MemoryExportRow {
                id: r.get(0)?,
                content: r.get(1)?,
                created_at: r.get(2)?,
                event_time: r.get(3)?,
                importance: r.get(4)?,
                emotional_weight: r.get(5)?,
                access_count: r.get(6)?,
                last_accessed: r.get(7)?,
                stability: r.get(8)?,
                source_json: r.get(9)?,
                diary: r.get(10)?,
                valid_from: r.get(11)?,
                valid_until: r.get(12)?,
                tags_json: r.get(13)?,
                kb: r.get(14)?,
                deleted: r.get::<_, i64>(15)? != 0,
                deleted_at: r.get(16)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn export_chunks(&self, kb: Option<&str>) -> Result<Vec<ChunkExportRow>, StorageError> {
        use base64::Engine as _;
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.memory_id, c.content, c.position, c.section, c.embedding,
                    c.embed_model, c.embed_dimensions, c.embed_prompt_format, c.document_id,
                    c.source_start, c.source_end
             FROM chunks c JOIN memories m ON m.id = c.memory_id
             WHERE m.deleted = 0 AND (?1 IS NULL OR m.kb = ?1)
             ORDER BY c.memory_id, c.position",
        )?;
        let rows = stmt.query_map(params![kb], |r| {
            let blob: Option<Vec<u8>> = r.get(5)?;
            Ok(ChunkExportRow {
                id: r.get(0)?,
                memory_id: r.get(1)?,
                content: r.get(2)?,
                position: r.get(3)?,
                section: r.get(4)?,
                embedding_b64: blob.map(|b| base64::engine::general_purpose::STANDARD.encode(b)),
                embed_model: r.get(6)?,
                embed_dimensions: r.get(7)?,
                embed_prompt_format: r.get(8)?,
                document_id: r.get(9)?,
                source_start: r.get(10)?,
                source_end: r.get(11)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn export_facts(&self, kb: Option<&str>) -> Result<Vec<FactExportRow>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_memory_id, subject, subject_norm, predicate, object,
                    object_norm, confidence, created_at, superseded_by, superseded_at,
                    diary, kb, fact_type, valid_from, valid_until, resolution,
                    source_kind, confirmations, closed_by, close_reason, close_authority
             FROM facts WHERE (?1 IS NULL OR kb = ?1) ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![kb], |r| {
            Ok(FactExportRow {
                id: r.get(0)?,
                source_memory_id: r.get(1)?,
                subject: r.get(2)?,
                subject_norm: r.get(3)?,
                predicate: r.get(4)?,
                object: r.get(5)?,
                object_norm: r.get(6)?,
                confidence: r.get(7)?,
                created_at: r.get(8)?,
                superseded_by: r.get(9)?,
                superseded_at: r.get(10)?,
                diary: r.get(11)?,
                kb: r.get(12)?,
                fact_type: r
                    .get::<_, Option<String>>(13)?
                    .unwrap_or_else(default_fact_type_str),
                valid_from: r.get(14)?,
                valid_until: r.get(15)?,
                resolution: r.get(16)?,
                source_kind: r
                    .get::<_, Option<String>>(17)?
                    .unwrap_or_else(default_source_kind_str),
                confirmations: r.get::<_, Option<i64>>(18)?.unwrap_or(0),
                closed_by: r.get(19)?,
                close_reason: r.get(20)?,
                close_authority: r.get(21)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Import one exported KB row. Existing KBs keep their metadata.
    pub fn import_kb_row(&self, row: &KbExportRow) -> Result<bool, StorageError> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO knowledge_bases(name, description, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![row.name, row.description, row.created_at, row.updated_at],
        )?;
        Ok(n > 0)
    }

    pub fn import_document_row(&self, row: &DocumentExportRow) -> Result<bool, StorageError> {
        self.ensure_kb(&row.kb, None)?;
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO documents(id, kb, title, source_path, source_url, doi,
                authors_json, year, mode, created_at, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                row.id,
                row.kb,
                row.title,
                row.source_path,
                row.source_url,
                row.doi,
                row.authors_json,
                row.year,
                row.mode,
                row.created_at,
                row.metadata_json,
            ],
        )?;
        Ok(n > 0)
    }

    pub fn import_memory_row(&self, row: &MemoryExportRow) -> Result<bool, StorageError> {
        self.ensure_kb(&row.kb, None)?;
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO memories(id, content, created_at, event_time, importance,
                emotional_weight, access_count, last_accessed, stability, source_json,
                diary, valid_from, valid_until, tags_json, kb, deleted, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                row.id,
                row.content,
                row.created_at,
                row.event_time,
                row.importance,
                row.emotional_weight,
                row.access_count,
                row.last_accessed,
                row.stability,
                row.source_json,
                row.diary,
                row.valid_from,
                row.valid_until,
                row.tags_json,
                row.kb,
                row.deleted as i64,
                row.deleted_at,
            ],
        )?;
        Ok(n > 0)
    }

    pub fn import_chunk_row(&self, row: &ChunkExportRow) -> Result<bool, StorageError> {
        use base64::Engine as _;
        let blob: Option<Vec<u8>> = match &row.embedding_b64 {
            Some(b64) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| {
                        StorageError::Migration(format!(
                            "invalid embedding_b64 for chunk {}: {e}",
                            row.id
                        ))
                    })?,
            ),
            None => None,
        };
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO chunks(id, memory_id, content, position, section, embedding,
                embed_model, embed_dimensions, embed_prompt_format, document_id,
                source_start, source_end)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                row.id,
                row.memory_id,
                row.content,
                row.position,
                row.section,
                blob,
                row.embed_model,
                row.embed_dimensions,
                row.embed_prompt_format,
                row.document_id,
                row.source_start,
                row.source_end,
            ],
        )?;
        Ok(n > 0)
    }

    /// Import one fact row from an export file.
    ///
    /// [G2] This is the ONLY write path whose bytes a caller edits by hand,
    /// so it is the only one that cannot trust its own input. Two things it
    /// cannot re-derive get checked instead: an interval that ends before it
    /// begins is refused outright, and a SAFETY relation closed by anything
    /// but a person is imported OPEN with the close dropped and counted.
    /// Everything else about the row is written as given.
    pub fn import_fact_row(&self, row: &FactExportRow) -> Result<ImportedFactRow, StorageError> {
        if crate::export_v3::interval_is_inverted(
            row.valid_from.as_deref(),
            row.valid_until.as_deref(),
        ) {
            return Err(crate::export_v3::inverted_interval_error(
                &row.id,
                row.valid_from.as_deref().unwrap_or(""),
                row.valid_until.as_deref().unwrap_or(""),
            ));
        }
        let closed = row.valid_until.is_some() || row.superseded_by.is_some();
        // A point interval is exempt: re-opening it would manufacture an
        // ongoing prescription out of a sentence that said it ended.
        let close_rejected = closed
            && !crate::export_v3::close_is_point(
                row.valid_from.as_deref(),
                row.valid_until.as_deref(),
            )
            && crate::export_v3::close_needs_person(&row.predicate, row.close_authority.as_deref());
        // Dropping the close means dropping ALL of it: a row left with
        // `superseded_by` set is not open under `is_open_fact`, and half a
        // close is worse than either whole.
        let keep = |v: &Option<String>| if close_rejected { None } else { v.clone() };
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO facts(id, source_memory_id, subject, subject_norm,
                predicate, object, object_norm, confidence, created_at,
                superseded_by, superseded_at, diary, kb, fact_type, valid_from,
                valid_until, resolution, source_kind, confirmations,
                closed_by, close_reason, close_authority)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                row.id,
                row.source_memory_id,
                row.subject,
                row.subject_norm,
                row.predicate,
                row.object,
                row.object_norm,
                row.confidence,
                row.created_at,
                keep(&row.superseded_by),
                keep(&row.superseded_at),
                row.diary,
                row.kb,
                row.fact_type,
                row.valid_from,
                keep(&row.valid_until),
                row.resolution,
                row.source_kind,
                row.confirmations,
                keep(&row.closed_by),
                keep(&row.close_reason),
                keep(&row.close_authority),
            ],
        )?;
        Ok(ImportedFactRow {
            inserted: n > 0,
            close_rejected,
        })
    }

    pub fn embedding_profiles(
        &self,
        kb: Option<&str>,
    ) -> Result<Vec<EmbeddingProfileRow>, StorageError> {
        let sql = if kb.is_some() {
            "SELECT COALESCE(embed_model, 'missing'), embed_dimensions, embed_prompt_format, COUNT(*)
             FROM chunks c JOIN memories m ON m.id = c.memory_id
             WHERE c.embedding IS NOT NULL AND m.deleted = 0 AND m.kb = ?1
             GROUP BY COALESCE(embed_model, 'missing'), embed_dimensions, embed_prompt_format
             ORDER BY COUNT(*) DESC"
        } else {
            "SELECT COALESCE(embed_model, 'missing'), embed_dimensions, embed_prompt_format, COUNT(*)
             FROM chunks c JOIN memories m ON m.id = c.memory_id
             WHERE c.embedding IS NOT NULL AND m.deleted = 0
             GROUP BY COALESCE(embed_model, 'missing'), embed_dimensions, embed_prompt_format
             ORDER BY COUNT(*) DESC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(k) = kb {
            stmt.query_map(params![k], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }
}

fn parse_kb_row(r: &rusqlite::Row) -> rusqlite::Result<KnowledgeBase> {
    Ok(KnowledgeBase {
        name: r.get(0)?,
        description: r.get(1)?,
        created_at: r.get(2)?,
        updated_at: r.get(3)?,
        document_count: r.get(4)?,
        memory_count: r.get(5)?,
        chunk_count: r.get(6)?,
    })
}

fn parse_id_score_row(r: &rusqlite::Row) -> rusqlite::Result<(String, f32)> {
    let id: String = r.get(0)?;
    let score: f64 = r.get(1)?;
    Ok((id, score as f32))
}

/// Column list every `Memory` query selects, in the order
/// [`parse_memory_from_offset`] expects. `{p}` is the table alias prefix.
fn memory_columns(prefix: &str) -> String {
    [
        "id",
        "content",
        "created_at",
        "event_time",
        "importance",
        "emotional_weight",
        "access_count",
        "last_accessed",
        "stability",
        "source_json",
        "diary",
        "valid_from",
        "valid_until",
        "tags_json",
    ]
    .iter()
    .map(|c| format!("{prefix}{c}"))
    .collect::<Vec<_>>()
    .join(", ")
}

/// Build a `Memory` from a row whose memory columns start at index `o` — a
/// cross-KB sweep selects `m.kb` ahead of them, everything else uses `o = 0`.
fn parse_memory_from_offset(r: &rusqlite::Row, o: usize) -> rusqlite::Result<Memory> {
    use chrono::{DateTime, Utc};
    let parse_ts = |s: Option<String>| {
        s.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|d| d.with_timezone(&Utc))
    };
    let id_str: String = r.get(o)?;
    let created: String = r.get(o + 2)?;
    let src_json: String = r.get(o + 9)?;
    let tags_json: String = r.get(o + 13)?;
    Ok(Memory {
        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
        content: r.get(o + 1)?,
        created_at: DateTime::parse_from_rfc3339(&created)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        event_time: parse_ts(r.get(o + 3)?),
        importance: r.get(o + 4)?,
        emotional_weight: r.get(o + 5)?,
        access_count: r.get(o + 6)?,
        last_accessed: parse_ts(r.get(o + 7)?),
        stability: r.get(o + 8)?,
        source: serde_json::from_str(&src_json).unwrap_or(engram_core::types::MemorySource::Manual),
        diary: r.get(o + 10)?,
        valid_from: parse_ts(r.get(o + 11)?),
        valid_until: parse_ts(r.get(o + 12)?),
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
    })
}

fn parse_chunk_embedding_row(r: &rusqlite::Row) -> rusqlite::Result<StoredChunkEmbedding> {
    let chunk_id: String = r.get(0)?;
    let memory_id: String = r.get(1)?;
    let blob: Vec<u8> = r.get(3)?;
    let document_id: Option<String> = r.get(10)?;
    Ok(StoredChunkEmbedding {
        chunk_id: Uuid::parse_str(&chunk_id).unwrap_or(Uuid::nil()),
        memory_id: Uuid::parse_str(&memory_id).unwrap_or(Uuid::nil()),
        content: r.get(2)?,
        embedding: f32_bytes_to_vec(&blob),
        diary: r.get(4)?,
        kb: r.get(5)?,
        section: r.get(6)?,
        embed_model: r.get(7)?,
        embed_dimensions: r.get::<_, Option<i64>>(8)?.map(|v| v as usize),
        embed_prompt_format: r.get(9)?,
        document_id: document_id.and_then(|s| Uuid::parse_str(&s).ok()),
        source: r.get(11)?,
    })
}

fn parse_chunk_record_row(r: &rusqlite::Row) -> rusqlite::Result<ChunkRecord> {
    let chunk_id: String = r.get(0)?;
    let memory_id: String = r.get(1)?;
    let document_id: Option<String> = r.get(7)?;
    let position: i64 = r.get(3)?;
    Ok(ChunkRecord {
        chunk_id: Uuid::parse_str(&chunk_id).unwrap_or(Uuid::nil()),
        memory_id: Uuid::parse_str(&memory_id).unwrap_or(Uuid::nil()),
        content: r.get(2)?,
        position: position as u32,
        section: r.get(4)?,
        diary: r.get(5)?,
        kb: r.get(6)?,
        document_id: document_id.and_then(|s| Uuid::parse_str(&s).ok()),
        source: r.get(8)?,
    })
}

fn parse_document_row(r: &rusqlite::Row) -> rusqlite::Result<DocumentRecord> {
    let id_str: String = r.get(0)?;
    let metadata_json: String = r.get(9)?;
    Ok(DocumentRecord {
        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
        kb: r.get(1)?,
        title: r.get(2)?,
        source_path: r.get(3)?,
        source_url: r.get(4)?,
        doi: r.get(5)?,
        year: r.get(6)?,
        mode: r.get(7)?,
        created_at: r.get(8)?,
        metadata: serde_json::from_str(&metadata_json).unwrap_or(serde_json::Value::Null),
    })
}

fn parse_compile_job_row(r: &rusqlite::Row) -> rusqlite::Result<CompileJobRecord> {
    let id_str: String = r.get(0)?;
    let metadata_json: String = r.get(7)?;
    Ok(CompileJobRecord {
        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
        kb: r.get(1)?,
        status: r.get(2)?,
        mode: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        message: r.get(6)?,
        metadata: serde_json::from_str(&metadata_json).unwrap_or(serde_json::Value::Null),
    })
}

fn parse_usage_budget_row(r: &rusqlite::Row) -> rusqlite::Result<UsageBudget> {
    Ok(UsageBudget {
        scope: r.get(0)?,
        kb: r.get(1)?,
        daily_usd: r.get(2)?,
        monthly_usd: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

fn parse_citation_row(r: &rusqlite::Row) -> rusqlite::Result<SourceCitation> {
    let document_id: Option<String> = r.get(0)?;
    let chunk_id: Option<String> = r.get(1)?;
    // Columns 5/6 (offset_start/offset_end) are optional: older callers select
    // only 5 columns, so tolerate their absence.
    let offset_start = r.get::<_, Option<i64>>(5).unwrap_or(None);
    let offset_end = r.get::<_, Option<i64>>(6).unwrap_or(None);
    Ok(SourceCitation {
        document_id: document_id.and_then(|s| Uuid::parse_str(&s).ok()),
        chunk_id: chunk_id.and_then(|s| Uuid::parse_str(&s).ok()),
        page: r.get(2)?,
        section: r.get(3)?,
        source: r.get(4)?,
        offset_start,
        offset_end,
    })
}

/// Specificity of an entity kind. A recompile may see the same name in two
/// chunks — one with the evidence that it names a human, one without — so the
/// stored kind keeps the most specific reading rather than the last one.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "person" => 3,
        "organization" => 2,
        "gene_or_protein" => 1,
        _ => 0,
    }
}

/// Should `incoming` take over as the canonical name of a row currently called
/// `existing`?
///
/// Only in the one direction that identifies a human better than before: every
/// token of the stored name appears in the incoming one IN ORDER, and the
/// incoming one says more. So "Noemi" yields to "Noemi Varkoni", "Maria" to
/// "Maria Marenova", "Patrick Pruett" to "Patrick Ashcomb Pruett" (a middle
/// name inserted), and "Francisco Ferro" to "Francisco Ferró Oliver" — the
/// comparison ignores accents, because a man who writes his name with one is
/// not two people.
///
/// Containment is what keeps it honest: a messaging-app label ("Maria
/// Microsoft London Tech") is longer but does not contain "Marenova", so it
/// can never displace the real name, and two unrelated names never swap
/// places. ORDER is what keeps a surname-first convention ("Pruett Patrick")
/// from passing as a fuller name.
fn promotes_canonical(existing: &str, incoming: &str) -> bool {
    engram_core::names::is_fuller_name(existing, incoming)
}

fn parse_entity_row(r: &rusqlite::Row) -> rusqlite::Result<EntityRecord> {
    let id_str: String = r.get(0)?;
    let aliases_json: String = r.get(3)?;
    let mention_count: i64 = r.get(5)?;
    Ok(EntityRecord {
        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
        kb: r.get(1)?,
        canonical_name: r.get(2)?,
        aliases: serde_json::from_str(&aliases_json).unwrap_or_default(),
        kind: r.get(4)?,
        mention_count: mention_count as u32,
    })
}

/// Parse a relation row selected as
/// `(id, kb, from_name, to_name, predicate, weight, provenance_json,
///   from_entity_id, to_entity_id)`.
fn parse_relation_row(r: &rusqlite::Row) -> rusqlite::Result<RelationRecord> {
    let id_str: String = r.get(0)?;
    let provenance_json: String = r.get(6)?;
    let from_id: String = r.get(7)?;
    let to_id: String = r.get(8)?;
    Ok(RelationRecord {
        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
        kb: r.get(1)?,
        from_entity: r.get(2)?,
        to_entity: r.get(3)?,
        predicate: r.get(4)?,
        weight: r.get(5)?,
        provenance: serde_json::from_str(&provenance_json).unwrap_or(serde_json::Value::Null),
        from_entity_id: Uuid::parse_str(&from_id).unwrap_or(Uuid::nil()),
        to_entity_id: Uuid::parse_str(&to_id).unwrap_or(Uuid::nil()),
    })
}

fn parse_wiki_page_row(r: &rusqlite::Row) -> rusqlite::Result<WikiPageRecord> {
    let id_str: String = r.get(0)?;
    Ok(WikiPageRecord {
        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
        kb: r.get(1)?,
        path: r.get(2)?,
        title: r.get(3)?,
        content: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

/// Parse a row of the standard 12-column fact projection into a Fact.
fn parse_fact_row(r: &rusqlite::Row) -> rusqlite::Result<engram_core::types::Fact> {
    parse_fact_from_offset(r, 0)
}

/// Same parser, reachable from `write_path` — one column order, one reader.
pub(crate) fn parse_fact_row_pub(r: &rusqlite::Row) -> rusqlite::Result<engram_core::types::Fact> {
    parse_fact_from_offset(r, 0)
}

fn parse_fact_from_offset(
    r: &rusqlite::Row,
    o: usize,
) -> rusqlite::Result<engram_core::types::Fact> {
    use chrono::{DateTime, Utc};
    let id_str: String = r.get(o)?;
    let src_str: String = r.get(o + 1)?;
    let subject: String = r.get(o + 2)?;
    let subject_norm: String = r.get(o + 3)?;
    let predicate: String = r.get(o + 4)?;
    let object: String = r.get(o + 5)?;
    let object_norm: String = r.get(o + 6)?;
    let confidence: f32 = r.get(o + 7)?;
    let created_at_str: String = r.get(o + 8)?;
    let superseded_by_str: Option<String> = r.get(o + 9)?;
    let superseded_at_str: Option<String> = r.get(o + 10)?;
    let diary: String = r.get(o + 11)?;
    let fact_type_str: String = r.get::<_, Option<String>>(o + 12)?.unwrap_or_default();
    let valid_from_str: Option<String> = r.get(o + 13)?;
    let valid_until_str: Option<String> = r.get(o + 14)?;
    let resolution: Option<String> = r.get(o + 15)?;
    let source_kind_str: String = r.get::<_, Option<String>>(o + 16)?.unwrap_or_default();
    let confirmations: i64 = r.get::<_, Option<i64>>(o + 17)?.unwrap_or(0);
    let closed_by_str: Option<String> = r.get(o + 18)?;
    let close_reason: Option<String> = r.get(o + 19)?;
    let close_authority: Option<String> = r.get(o + 20)?;
    let kb: String = r.get::<_, Option<String>>(o + 21)?.unwrap_or_default();
    Ok(engram_core::types::Fact {
        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
        source_memory_id: Uuid::parse_str(&src_str).unwrap_or(Uuid::nil()),
        subject,
        subject_norm,
        predicate,
        object,
        object_norm,
        confidence,
        // Epoch, never now: a fabricated "now" is the newest date in the
        // store and would WIN recency supersession; the epoch always loses.
        // Same rule as memory_times.
        created_at: DateTime::parse_from_rfc3339(&created_at_str)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(DateTime::UNIX_EPOCH),
        superseded_by: superseded_by_str
            .as_deref()
            .and_then(|s| Uuid::parse_str(s).ok()),
        superseded_at: superseded_at_str
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        diary,
        fact_type: engram_core::types::FactType::parse(&fact_type_str),
        valid_from: valid_from_str
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        valid_until: valid_until_str
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        resolution,
        source_kind: engram_core::types::SourceKind::parse(&source_kind_str),
        confirmations: confirmations.max(0) as u32,
        closed_by: closed_by_str
            .as_deref()
            .and_then(|s| Uuid::parse_str(s).ok()),
        close_reason,
        close_authority,
        kb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use engram_core::types::MemorySource;

    fn fact(content: &str) -> Memory {
        Memory {
            id: Uuid::new_v4(),
            content: content.into(),
            created_at: Utc::now(),
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
            tags: vec!["test".into()],
        }
    }

    #[test]
    fn open_in_memory_creates_schema() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.count_memories().unwrap(), 0);
    }

    /// Writing to a KB must register it, or the memory is stored somewhere
    /// `kb list` cannot see. This is the invariant that older builds broke.
    #[test]
    fn writing_a_memory_registers_its_kb() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .insert_memory_with_kb(&fact("a song lyric worth keeping"), "songs")
            .unwrap();
        assert!(store.list_kbs().unwrap().iter().any(|k| k.name == "songs"));
        assert!(store.unregistered_kbs().unwrap().is_empty());
    }

    #[test]
    fn kb_repair_registers_kbs_orphaned_by_older_builds() {
        let store = SqliteStore::open_in_memory().unwrap();
        for (kb, content) in [
            ("songs", "a lyric"),
            ("personal", "a birthday"),
            ("business", "a decision"),
        ] {
            store.insert_memory_with_kb(&fact(content), kb).unwrap();
        }
        // Recreate the legacy state: content present, registry row missing.
        store
            .conn
            .execute(
                "DELETE FROM knowledge_bases WHERE name IN ('songs','personal','business')",
                [],
            )
            .unwrap();
        assert_eq!(
            store.unregistered_kbs().unwrap(),
            vec!["business", "personal", "songs"]
        );
        assert!(!store.integrity_report().unwrap().ok);

        let repaired = store.repair_kb_registry().unwrap();
        assert_eq!(repaired, vec!["business", "personal", "songs"]);
        let names: Vec<String> = store
            .list_kbs()
            .unwrap()
            .into_iter()
            .map(|k| k.name)
            .collect();
        for kb in ["songs", "personal", "business"] {
            assert!(names.contains(&kb.to_string()), "{kb} still missing");
        }
        // Memory counts survive the repair — registration is additive.
        let songs = store.get_kb("songs").unwrap().unwrap();
        assert_eq!(songs.memory_count, 1);

        // Idempotent: a second pass finds nothing.
        assert!(store.repair_kb_registry().unwrap().is_empty());
        assert!(store.integrity_report().unwrap().ok);
    }

    /// `kb delete` drops the registry row on purpose. Repair must not undo it,
    /// or deleting a KB would be silently reversed by the next repair run.
    #[test]
    fn kb_repair_does_not_resurrect_a_deleted_kb() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .insert_memory_with_kb(&fact("throwaway note"), "throwaway")
            .unwrap();
        store
            .insert_document(
                "throwaway",
                "doc",
                Some("/tmp/doc.md"),
                "general",
                serde_json::json!({}),
            )
            .unwrap();
        assert!(store.delete_kb("throwaway").unwrap());

        assert!(
            store.unregistered_kbs().unwrap().is_empty(),
            "deleted KB reappeared as unregistered"
        );
        assert!(store.repair_kb_registry().unwrap().is_empty());
        assert!(store.get_kb("throwaway").unwrap().is_none());
    }

    fn test_fact(
        store: &SqliteStore,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> engram_core::types::Fact {
        let m = fact(&format!("{subject} {predicate} {object}"));
        store.insert_memory(&m).unwrap();
        engram_core::types::Fact {
            id: Uuid::new_v4(),
            source_memory_id: m.id,
            subject: subject.into(),
            subject_norm: subject.to_lowercase(),
            predicate: predicate.into(),
            object: object.into(),
            object_norm: object.to_lowercase(),
            confidence: 1.0,
            created_at: Utc::now(),
            superseded_by: None,
            superseded_at: None,
            diary: "default".into(),
            fact_type: engram_core::types::FactType::ProfileSlot,
            valid_from: Some(Utc::now()),
            valid_until: None,
            resolution: None,
            source_kind: engram_core::types::SourceKind::UserStated,
            confirmations: 0,
            closed_by: None,
            close_reason: None,
            close_authority: None,
            kb: "default".into(),
        }
    }

    #[test]
    fn fact_v6_fields_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let f = test_fact(&store, "Ana", "lives_in", "Lisbon");
        store.insert_fact(&f).unwrap();
        let got = store.get_fact(f.id).unwrap().unwrap();
        assert_eq!(got.fact_type, engram_core::types::FactType::ProfileSlot);
        assert_eq!(got.source_kind, engram_core::types::SourceKind::UserStated);
        assert!(got.valid_from.is_some());
        assert!(got.valid_until.is_none());
        assert_eq!(got.confirmations, 0);
        store.increment_fact_confirmations(f.id).unwrap();
        assert_eq!(store.get_fact(f.id).unwrap().unwrap().confirmations, 1);
    }

    #[test]
    fn supersede_with_resolution_closes_validity() {
        let store = SqliteStore::open_in_memory().unwrap();
        let old_f = test_fact(&store, "Ana", "lives_in", "Porto");
        let new_f = test_fact(&store, "Ana", "lives_in", "Lisbon");
        store.insert_fact(&old_f).unwrap();
        store.insert_fact(&new_f).unwrap();
        let moved = Utc::now();
        assert!(store
            .supersede_fact_with(old_f.id, new_f.id, "slot_lww", Some(moved))
            .unwrap());
        let got = store.get_fact(old_f.id).unwrap().unwrap();
        assert_eq!(got.superseded_by, Some(new_f.id));
        assert_eq!(got.resolution.as_deref(), Some("slot_lww"));
        assert!(got.valid_until.is_some());
        // Second supersede on an already-superseded fact is a no-op.
        assert!(!store
            .supersede_fact_with(old_f.id, new_f.id, "slot_lww", None)
            .unwrap());
    }

    #[test]
    fn fact_embeddings_store_and_filter_by_model() {
        let store = SqliteStore::open_in_memory().unwrap();
        let f = test_fact(&store, "Ana", "works_at", "Acme");
        store.insert_fact(&f).unwrap();
        store
            .set_fact_embedding(f.id, &[0.1, 0.2, 0.3], "test-model")
            .unwrap();
        let got = store
            .active_fact_embeddings("default", "default", "test-model")
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, f.id);
        assert_eq!(got[0].1, vec![0.1, 0.2, 0.3]);
        // Different model sees nothing (mixed-model safety).
        assert!(store
            .active_fact_embeddings("default", "default", "other-model")
            .unwrap()
            .is_empty());
        // Superseded facts leave the gate's candidate set.
        let g = test_fact(&store, "Ana", "works_at", "Beta Corp");
        store.insert_fact(&g).unwrap();
        store.supersede_fact(f.id, g.id).unwrap();
        assert!(store
            .active_fact_embeddings("default", "default", "test-model")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn conflict_queue_roundtrip_and_idempotency() {
        let store = SqliteStore::open_in_memory().unwrap();
        let old_f = test_fact(&store, "Ana", "allergic_to", "penicillin");
        let new_f = test_fact(&store, "Ana", "allergic_to", "amoxicillin");
        store.insert_fact(&old_f).unwrap();
        store.insert_fact(&new_f).unwrap();
        let qid = Uuid::new_v4();
        assert_eq!(
            store
                .insert_fact_conflict(
                    qid,
                    new_f.id,
                    old_f.id,
                    "default",
                    "default",
                    "slot",
                    Some(0.9),
                    "await_confirmation",
                    "",
                    "",
                    engram_core::relations::REGISTRY_VERSION
                )
                .unwrap(),
            InsertOutcome::Inserted
        );
        // Same (new, old) ordered pair queues once, and says WHICH duplicate
        // it was — the caller's `deduplicated` response is only honest for the
        // same-key case, not this one.
        assert_eq!(
            store
                .insert_fact_conflict(
                    Uuid::new_v4(),
                    new_f.id,
                    old_f.id,
                    "default",
                    "default",
                    "slot",
                    None,
                    "await_confirmation",
                    "",
                    "",
                    engram_core::relations::REGISTRY_VERSION
                )
                .unwrap(),
            InsertOutcome::DuplicatePair { existing: qid }
        );
        let open = store.list_open_fact_conflicts(None, None, 10).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].detector, "slot");

        // Term matching surfaces the contested pair from EITHER side: a query
        // about the old value must reveal that the value is contested.
        let hits = store
            .open_conflicts_matching_terms(Some("default"), None, &["penicillin".into()], 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        let hits = store
            .open_conflicts_matching_terms(Some("default"), None, &["amoxicillin".into()], 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.id, new_f.id);
        assert_eq!(hits[0].2.id, old_f.id);

        // Resolve closes it; a second resolve is a no-op. Through the welded
        // path, which is now the only one: the answer has to reach the
        // journal or a rebuild reopens the question.
        let key = engram_core::keys::semantic_key_encode(
            "ana",
            "allergic_to",
            &engram_core::keys::object_pair_hash("amoxicillin", "penicillin"),
        );
        let resolve = |note: Option<&str>| {
            store.resolve_conflict_with_decision(
                qid,
                "default",
                "default",
                &key,
                "close_old",
                "caller",
                "caller",
                note,
                engram_core::keys::SEMANTIC_KEY_VERSION,
            )
        };
        assert!(resolve(Some("user confirmed")).unwrap().is_some());
        assert!(resolve(None).unwrap().is_none());
        assert!(store
            .list_open_fact_conflicts(None, None, 10)
            .unwrap()
            .is_empty());
        let row = store.get_fact_conflict(qid).unwrap().unwrap();
        assert_eq!(row.status, "resolved");
        assert_eq!(row.note.as_deref(), Some("user confirmed"));
    }

    /// `facts triage` re-classifies a whole backlog, so it needs both facts
    /// of every open row in ONE query, a total to report against, and a way
    /// to close rows whose facts are gone.
    #[test]
    fn open_conflicts_join_both_facts_and_count_and_survive_deleted_facts() {
        let store = SqliteStore::open_in_memory().unwrap();
        let old_f = test_fact(&store, "Ana", "allergic_to", "penicillin");
        let new_f = test_fact(&store, "Ana", "allergic_to", "amoxicillin");
        let unrelated = test_fact(&store, "Bob", "lives_in", "Porto");
        let other = test_fact(&store, "Bob", "lives_in", "Berlin");
        for f in [&old_f, &new_f, &unrelated, &other] {
            store.insert_fact(f).unwrap();
        }
        let ana = Uuid::new_v4();
        let bob = Uuid::new_v4();
        store
            .insert_fact_conflict(
                ana,
                new_f.id,
                old_f.id,
                "default",
                "default",
                "slot",
                None,
                "supersede-old",
                "",
                "",
                engram_core::relations::REGISTRY_VERSION,
            )
            .unwrap();
        store
            .insert_fact_conflict(
                bob,
                other.id,
                unrelated.id,
                "default",
                "personal",
                "semantic_cosine",
                Some(0.8),
                "keep-both",
                "",
                "",
                engram_core::relations::REGISTRY_VERSION,
            )
            .unwrap();

        assert_eq!(store.count_open_fact_conflicts(None, None).unwrap(), 2);
        assert_eq!(
            store
                .count_open_fact_conflicts(None, Some("personal"))
                .unwrap(),
            1
        );

        let joined = store
            .list_open_fact_conflicts_with_facts(None, None, None, 10, 0)
            .unwrap();
        assert_eq!(joined.len(), 2);
        for (row, nf, of) in &joined {
            assert_eq!(row.status, "open");
            assert_eq!(nf.id, if row.id == ana { new_f.id } else { other.id });
            assert_eq!(
                of.id,
                if row.id == ana {
                    old_f.id
                } else {
                    unrelated.id
                }
            );
            // The join must hydrate the fields the classifier reads.
            assert!(nf.valid_from.is_some());
            assert!(of.superseded_by.is_none());
        }

        // Subject filter narrows to one person's backlog.
        let ana_only = store
            .list_open_fact_conflicts_with_facts(None, None, Some("ana"), 10, 0)
            .unwrap();
        assert_eq!(ana_only.len(), 1);
        assert_eq!(ana_only[0].0.id, ana);

        // Both values' source sentences are available for review.
        let previews = store.fact_source_previews(&[new_f.id, old_f.id]).unwrap();
        assert_eq!(previews.len(), 2);
        assert!(previews[&new_f.id].1.contains("amoxicillin"));

        // A fact deleted without the cascade (an older schema, a manual
        // repair) leaves a row nobody can ever adjudicate.
        store
            .conn
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        store
            .conn
            .execute(
                "DELETE FROM facts WHERE id = ?1",
                params![unrelated.id.to_string()],
            )
            .unwrap();
        store
            .conn
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        assert_eq!(store.count_open_fact_conflicts(None, None).unwrap(), 2);
        assert_eq!(
            store
                .list_open_fact_conflicts_with_facts(None, None, None, 10, 0)
                .unwrap()
                .len(),
            1,
            "a row whose fact is gone must not be handed to the classifier"
        );
        assert_eq!(
            store
                .resolve_orphan_fact_conflicts(None, None, "triage")
                .unwrap(),
            1
        );
        assert_eq!(store.count_open_fact_conflicts(None, None).unwrap(), 1);
        // Idempotent: nothing left to close.
        assert_eq!(
            store
                .resolve_orphan_fact_conflicts(None, None, "triage")
                .unwrap(),
            0
        );
    }

    #[test]
    fn insert_and_count_memory() {
        let store = SqliteStore::open_in_memory().unwrap();
        let m = fact("Rapamycin extends mouse lifespan.");
        store.insert_memory(&m).unwrap();
        assert_eq!(store.count_memories().unwrap(), 1);
    }

    #[test]
    fn fts_search_finds_inserted_chunk() {
        let store = SqliteStore::open_in_memory().unwrap();
        let m = fact("parent");
        store.insert_memory(&m).unwrap();
        let chunk_id = Uuid::new_v4();
        store
            .insert_chunk(
                chunk_id,
                m.id,
                "Rapamycin inhibits mTORC1 signaling.",
                0,
                None,
            )
            .unwrap();
        let hits = store.fts_search("rapamycin", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, chunk_id);
    }

    #[test]
    fn pending_fact_extraction_queue_is_oldest_first_and_skips_processed() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut oldest = fact("Ana moved to Lisbon.");
        oldest.created_at = Utc::now() - chrono::Duration::days(3);
        let mut middle = fact("Ana works at Acme.");
        middle.created_at = Utc::now() - chrono::Duration::days(2);
        let mut newest = fact("Ana prefers Rust.");
        newest.created_at = Utc::now() - chrono::Duration::days(1);
        for m in [&oldest, &middle, &newest] {
            store.insert_memory_with_kb(m, "personal").unwrap();
        }
        // A memory in another KB must never enter this queue.
        store
            .insert_memory_with_kb(&fact("Unrelated kb memory."), "other")
            .unwrap();

        let pending = store
            .list_memories_pending_fact_extraction(Some("personal"), None, None)
            .unwrap();
        assert_eq!(
            pending.iter().map(|(_, m)| m.id).collect::<Vec<_>>(),
            vec![oldest.id, middle.id, newest.id]
        );
        assert_eq!(
            store
                .fact_extraction_coverage(Some("personal"), None)
                .unwrap(),
            (3, 3)
        );

        // A fact alone does NOT retire a memory: a caller-supplied batch
        // commits facts with no marker precisely because the extractor has
        // not seen the memory, and it must stay on the queue until one has.
        let f = engram_core::types::Fact {
            source_memory_id: oldest.id,
            ..test_fact(&store, "Ana", "lives_in", "Lisbon")
        };
        store.insert_fact(&f).unwrap();
        // A memory that produced NO facts is processed once its MARKER lands.
        store
            .record_fact_extraction(middle.id, Some("test-model"), 0, "backfill")
            .unwrap();

        let pending = store
            .list_memories_pending_fact_extraction(Some("personal"), None, None)
            .unwrap();
        assert_eq!(
            pending.iter().map(|(_, m)| m.id).collect::<Vec<_>>(),
            vec![oldest.id, newest.id],
            "the marker is the one source of truth; a stray fact hides nothing"
        );
        assert_eq!(
            store
                .fact_extraction_coverage(Some("personal"), None)
                .unwrap(),
            (3, 2)
        );

        // The marker is what retires it.
        store
            .record_fact_extraction(oldest.id, Some("test-model"), 1, "remember")
            .unwrap();
        let pending = store
            .list_memories_pending_fact_extraction(Some("personal"), None, None)
            .unwrap();
        assert_eq!(
            pending.iter().map(|(_, m)| m.id).collect::<Vec<_>>(),
            vec![newest.id]
        );
        assert_eq!(
            store
                .fact_extraction_coverage(Some("personal"), None)
                .unwrap(),
            (3, 1)
        );

        // --limit takes the oldest slice.
        let limited = store
            .list_memories_pending_fact_extraction(Some("personal"), None, Some(1))
            .unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].1.id, newest.id);

        // Re-editing content clears the marker so new content is re-examined.
        store.delete_facts_for_memory(middle.id).unwrap();
        assert_eq!(
            store
                .list_memories_pending_fact_extraction(Some("personal"), None, None)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn pending_fact_extraction_queue_filters_by_diary() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut work = fact("Work note.");
        work.diary = "work".into();
        store.insert_memory_with_kb(&work, "personal").unwrap();
        store
            .insert_memory_with_kb(&fact("Default note."), "personal")
            .unwrap();

        let pending = store
            .list_memories_pending_fact_extraction(Some("personal"), Some("work"), None)
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1.id, work.id);
        assert_eq!(
            store
                .fact_extraction_coverage(Some("personal"), Some("work"))
                .unwrap(),
            (1, 1)
        );
    }

    #[test]
    fn event_time_roundtrips_through_the_memory_row() {
        let store = SqliteStore::open_in_memory().unwrap();
        let when = chrono::DateTime::parse_from_rfc3339("1990-04-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut m = fact("Ana was born in April 1990.");
        m.event_time = Some(when);
        store.insert_memory_with_kb(&m, "personal").unwrap();
        let got = store
            .list_memories_pending_fact_extraction(Some("personal"), None, None)
            .unwrap();
        assert_eq!(got[0].1.event_time, Some(when));
    }

    #[test]
    fn memory_scope_answers_existence_kb_and_diary_and_ignores_deleted_rows() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut m = fact("A note filed under work.");
        m.diary = "work".into();
        store.insert_memory_with_kb(&m, "business").unwrap();
        assert_eq!(
            store.memory_scope(m.id).unwrap(),
            Some(("business".to_string(), "work".to_string()))
        );
        assert!(store.memory_scope(Uuid::new_v4()).unwrap().is_none());
        store.soft_delete_memory(m.id).unwrap();
        assert!(
            store.memory_scope(m.id).unwrap().is_none(),
            "a forgotten memory must not be editable, and must not be billable"
        );
    }

    #[test]
    fn memory_times_report_the_event_time_and_vanish_with_the_memory() {
        let store = SqliteStore::open_in_memory().unwrap();
        let when = chrono::DateTime::parse_from_rfc3339("2015-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut m = fact("Ana lived in London.");
        m.event_time = Some(when);
        store.insert_memory_with_kb(&m, "personal").unwrap();
        let (event, created) = store.memory_times(m.id).unwrap().expect("live memory");
        assert_eq!(event, Some(when));
        assert!(created <= Utc::now());
        // A plain memory has only a record time.
        let n = fact("No event time here.");
        store.insert_memory_with_kb(&n, "personal").unwrap();
        assert_eq!(store.memory_times(n.id).unwrap().unwrap().0, None);
        // A soft-deleted memory can date nothing.
        store.soft_delete_memory(m.id).unwrap();
        assert!(store.memory_times(m.id).unwrap().is_none());
        assert!(store.memory_times(Uuid::new_v4()).unwrap().is_none());
    }

    #[test]
    fn graph_neighbors_traverses_reverse_edges() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.ensure_kb("kb", None).unwrap();
        let a = store.upsert_entity("kb", "Alpha", "Concept", 1).unwrap();
        let b = store.upsert_entity("kb", "Beta", "Concept", 1).unwrap();
        let c = store.upsert_entity("kb", "Gamma", "Concept", 1).unwrap();
        store
            .insert_relation("kb", a, b, "related_to", 1.0, serde_json::json!({}), None)
            .unwrap();
        store
            .insert_relation("kb", c, a, "supports", 0.8, serde_json::json!({}), None)
            .unwrap();

        let rels = store.graph_neighbors("kb", "Beta", 2).unwrap();
        assert!(rels
            .iter()
            .any(|rel| rel.from_entity == "Alpha" && rel.to_entity == "Beta"));
        assert!(rels
            .iter()
            .any(|rel| rel.from_entity == "Gamma" && rel.to_entity == "Alpha"));
        // Endpoint ids travel with the record so callers can traverse without
        // re-resolving display names.
        let edge = rels
            .iter()
            .find(|rel| rel.from_entity == "Alpha")
            .expect("edge");
        assert_eq!(edge.from_entity_id, a);
        assert_eq!(edge.to_entity_id, b);
    }

    #[test]
    fn aliases_resolve_to_one_entity_and_kind_only_sharpens() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.ensure_kb("kb", None).unwrap();
        let first = store
            .upsert_entity_with_aliases("kb", "Elias", "concept", 1, &["Elias's".to_string()])
            .unwrap();
        // Same human, seen later with the evidence that they are a person and
        // written in another case: one row, not three.
        let second = store
            .upsert_entity_with_aliases("kb", "elias", "person", 2, &[])
            .unwrap();
        // …and via an alias spelling only.
        let third = store
            .upsert_entity_with_aliases("kb", "Elias's", "concept", 1, &[])
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first, third);

        let entity = store.find_entity(Some("kb"), "ELIAS'S").unwrap().unwrap();
        assert_eq!(entity.canonical_name, "Elias");
        assert_eq!(entity.mention_count, 4);
        // A later "concept" reading must never downgrade a proven person.
        assert_eq!(entity.kind, "person");
        assert!(entity.aliases.contains(&"Elias's".to_string()));

        let people = store
            .list_entities_by_kind(Some("kb"), "person", 10, 1)
            .unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].canonical_name, "Elias");
    }

    /// The real store's twin rows: the chunk that only said "Noemi" compiled
    /// before the chunk that knew "Noemi Varkoni", so the bare name owned the
    /// row — and the alias the full name brought was silently dropped.
    #[test]
    fn a_full_name_absorbs_the_bare_row_that_was_created_first() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.ensure_kb("kb", None).unwrap();
        let bare = store
            .upsert_entity_with_aliases("kb", "Noemi", "person", 119, &[])
            .unwrap();
        let avi = store.upsert_entity("kb", "Avi Roy", "person", 3).unwrap();
        store
            .insert_relation(
                "kb",
                bare,
                avi,
                "knows",
                1.0,
                serde_json::json!({ "sentence": "Noemi knows Avi Roy." }),
                None,
            )
            .unwrap();

        let full = store
            .upsert_entity_with_aliases("kb", "Noemi Varkoni", "person", 54, &["Noemi".to_string()])
            .unwrap();
        assert_eq!(full, bare, "one woman, one row");

        let entity = store.find_entity(Some("kb"), "Noemi").unwrap().unwrap();
        assert_eq!(
            entity.canonical_name, "Noemi Varkoni",
            "the full name identifies her, not the bare first name"
        );
        assert_eq!(entity.mention_count, 173, "both mention counts survive");
        assert!(entity.aliases.contains(&"Noemi".to_string()));
        // Looking her up either way lands on the same row.
        assert_eq!(
            store
                .find_entity(Some("kb"), "noemi varkoni")
                .unwrap()
                .unwrap()
                .id,
            entity.id
        );
        let people = store
            .list_entities_by_kind(Some("kb"), "person", 10, 1)
            .unwrap();
        assert_eq!(
            people
                .iter()
                .filter(|p| p.canonical_name.to_lowercase().contains("noemi"))
                .count(),
            1,
            "no rival node: {people:?}"
        );
        // The edge follows her, provenance intact.
        let rels = store.graph_neighbors("kb", "Noemi Varkoni", 2).unwrap();
        let edge = rels
            .iter()
            .find(|r| r.predicate == "knows")
            .expect("the merged row keeps its edge");
        assert_eq!(edge.from_entity, "Noemi Varkoni");
        assert_eq!(edge.to_entity, "Avi Roy");
        assert!(edge.provenance.to_string().contains("Noemi knows Avi Roy."));
    }

    /// The merge is driven by an ALIAS, never by a shared first name — so two
    /// different humans who happen to share one stay two rows.
    #[test]
    fn same_first_name_without_an_alias_never_merges() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.ensure_kb("kb", None).unwrap();
        let bare = store.upsert_entity("kb", "Laura", "person", 5).unwrap();
        let corvo = store
            .upsert_entity_with_aliases("kb", "Laura Corvo", "person", 2, &[])
            .unwrap();
        let moreno = store
            .upsert_entity_with_aliases("kb", "Laura Quiroz", "person", 1, &[])
            .unwrap();
        assert_ne!(bare, corvo);
        assert_ne!(bare, moreno);
        assert_ne!(corvo, moreno);
        assert_eq!(
            store
                .list_entities_by_kind(Some("kb"), "person", 10, 1)
                .unwrap()
                .len(),
            3
        );
        // The bare row keeps its own name: nothing proved which Laura it is.
        assert_eq!(
            store
                .find_entity(Some("kb"), "Laura")
                .unwrap()
                .unwrap()
                .canonical_name,
            "Laura"
        );
    }

    #[test]
    fn a_contact_label_never_displaces_the_real_name() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.ensure_kb("kb", None).unwrap();
        let id = store
            .upsert_entity_with_aliases("kb", "Maria", "person", 400, &[])
            .unwrap();
        store
            .upsert_entity_with_aliases("kb", "Maria Marenova", "person", 21, &["Maria".into()])
            .unwrap();
        // A phone-contact label is longer, but it is not her name.
        store
            .upsert_entity_with_aliases(
                "kb",
                "Maria Marenova",
                "person",
                4,
                &["Maria Microsoft London Tech".into()],
            )
            .unwrap();
        let entity = store
            .find_entity(Some("kb"), "Maria Microsoft London Tech")
            .unwrap()
            .unwrap();
        assert_eq!(entity.id, id);
        assert_eq!(entity.canonical_name, "Maria Marenova");
        assert_eq!(entity.mention_count, 425);
    }

    #[test]
    fn canonical_promotion_only_goes_bare_to_full() {
        assert!(promotes_canonical("Noemi", "Noemi Varkoni"));
        assert!(promotes_canonical("maria", "Maria Marenova"));
        assert!(promotes_canonical("Avi Roy", "Avi Roy Sharma"));
        // Never the other way, never sideways, never onto a label.
        assert!(!promotes_canonical("Noemi Varkoni", "Noemi"));
        assert!(!promotes_canonical(
            "Maria Marenova",
            "Maria Microsoft London Tech"
        ));
        assert!(!promotes_canonical("Laura Corvo", "Laura Quiroz"));
        assert!(!promotes_canonical("Elias", "Elias"));
        assert!(!promotes_canonical("", "Anything At All"));
    }

    /// A middle name and a missing accent are not a different man.
    ///
    /// The set-containment rule this replaced let "Patrick Pruett" yield to
    /// "Patrick Ashcomb Pruett" but not to anything spelled with a diacritic —
    /// so "Francisco Ferro" kept the canonical slot from "Francisco Ferró
    /// Oliver" and the store showed one man twice. It also accepted a
    /// surname-first reshuffle as an improvement, which is not a fuller name.
    #[test]
    fn canonical_promotion_ignores_accents_but_not_word_order() {
        assert!(promotes_canonical(
            "Patrick Pruett",
            "Patrick Ashcomb Pruett"
        ));
        assert!(promotes_canonical("Ming Su", "Ming Su Lim"));
        assert!(promotes_canonical(
            "Francisco Ferro",
            "Francisco Ferró Oliver"
        ));
        assert!(promotes_canonical("Rosalie", "Rosalie Brandtke-Wendel"));
        // Same tokens in another order is a convention, not more of the name.
        assert!(!promotes_canonical(
            "Patrick Pruett",
            "Pruett Ashcomb Patrick"
        ));
    }

    /// The whole point of the promotion, end to end: the fullest spelling owns
    /// the row, the shorter ones stay resolvable, and the mentions are summed.
    #[test]
    fn the_fullest_name_takes_the_canonical_slot_from_the_shorter_one() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.ensure_kb("kb", None).unwrap();
        // The shorter spelling compiles first and creates the row.
        let id = store
            .upsert_entity("kb", "Francisco Ferro", "person", 40)
            .unwrap();
        // The fuller one arrives later carrying the shorter as an alias.
        store
            .upsert_entity_with_aliases(
                "kb",
                "Francisco Ferró Oliver",
                "person",
                1,
                &["Francisco Ferro".into(), "Francisco".into()],
            )
            .unwrap();
        for spelling in ["Francisco", "Francisco Ferro", "Francisco Ferró Oliver"] {
            let entity = store
                .find_entity(Some("kb"), spelling)
                .unwrap()
                .unwrap_or_else(|| panic!("{spelling} resolved nowhere"));
            assert_eq!(entity.id, id, "{spelling} stood up a second row");
            assert_eq!(entity.canonical_name, "Francisco Ferró Oliver");
            assert_eq!(entity.mention_count, 41);
        }
    }

    #[test]
    fn relation_sources_accumulate_and_dedup() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.ensure_kb("kb", None).unwrap();
        let noemi = store.upsert_entity("kb", "Noemi", "person", 1).unwrap();
        let avi = store.upsert_entity("kb", "Avi Roy", "person", 1).unwrap();

        let source = |chunk: &str, sentence: &str| serde_json::json!({ "chunk_id": chunk, "sentence": sentence, "memory_id": "m1" });
        store
            .insert_relation_with_source(
                "kb",
                noemi,
                avi,
                "introduced_by",
                1.0,
                source("c1", "Avi Roy introduced me to Noemi."),
                None,
            )
            .unwrap();
        // Same evidence again (a recompile): no duplicate receipt.
        store
            .insert_relation_with_source(
                "kb",
                noemi,
                avi,
                "introduced_by",
                1.0,
                source("c1", "Avi Roy introduced me to Noemi."),
                None,
            )
            .unwrap();
        // A second memory saying the same thing: a second receipt.
        store
            .insert_relation_with_source(
                "kb",
                noemi,
                avi,
                "introduced_by",
                1.0,
                source("c2", "Avi introduced Noemi to me last year."),
                None,
            )
            .unwrap();

        let rels = store.relations_for_entity("kb", noemi, 10).unwrap();
        assert_eq!(rels.len(), 1, "one edge, many sources");
        let sources = rels[0].provenance["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["chunk_id"], "c1");
        assert_eq!(sources[1]["chunk_id"], "c2");
    }

    #[test]
    fn chunks_containing_matches_case_insensitively() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.ensure_kb("default", None).unwrap();
        let m = fact("parent");
        store.insert_memory(&m).unwrap();
        let chunk_id = Uuid::new_v4();
        store
            .insert_chunk(chunk_id, m.id, "Noemi Kostova joined the call.", 0, None)
            .unwrap();

        let hits = store
            .chunks_containing("default", "noemi kostova", 5)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk_id, chunk_id);
        assert_eq!(hits[0].memory_id, m.id);
        // LIKE wildcards in the needle must not turn into a match-everything.
        assert!(store
            .chunks_containing("default", "%", 5)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn find_entities_unscoped_returns_every_kbs_match() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.ensure_kb("work", None).unwrap();
        store.ensure_kb("home", None).unwrap();
        store.upsert_entity("work", "Noemi", "Person", 3).unwrap();
        store.upsert_entity("home", "Noemi", "Person", 7).unwrap();

        // Scoped lookup stays scoped.
        let scoped = store.find_entities(Some("work"), "noemi", 8).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].kb, "work");

        // Unscoped lookup must not collapse to one arbitrary KB (the old
        // `find_entity` LIMIT 1 behaviour silently dropped the other KBs).
        let all = store.find_entities(None, "noemi", 8).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].kb, "home", "highest mention count first");
        let kbs: std::collections::HashSet<&str> = all.iter().map(|e| e.kb.as_str()).collect();
        assert!(kbs.contains("work") && kbs.contains("home"));
    }

    /// Memory in `kb`/`diary` with one chunk; returns (memory_id, chunk_id).
    fn seeded_memory(store: &SqliteStore, kb: &str, diary: &str, content: &str) -> (Uuid, Uuid) {
        let mut m = fact(content);
        m.diary = diary.to_string();
        store.insert_memory_with_kb(&m, kb).unwrap();
        let chunk_id = Uuid::new_v4();
        store
            .insert_chunk(chunk_id, m.id, content, 0, None)
            .unwrap();
        (m.id, chunk_id)
    }

    #[test]
    fn kbs_with_memories_outside_diary_flags_only_mixed_kbs() {
        let store = SqliteStore::open_in_memory().unwrap();
        seeded_memory(&store, "shared", "work", "Quarterly numbers.");
        seeded_memory(&store, "shared", "private", "Therapy appointment.");
        seeded_memory(&store, "solo", "work", "Standup notes.");

        let mixed = store.kbs_with_memories_outside_diary("work").unwrap();
        assert!(mixed.contains("shared"));
        assert!(!mixed.contains("solo"), "single-diary KB cannot leak");
    }

    #[test]
    fn claims_visible_in_diary_follows_evidence_provenance() {
        let store = SqliteStore::open_in_memory().unwrap();
        let (_, private_chunk) = seeded_memory(
            &store,
            "shared",
            "private",
            "Therapy appointment on Tuesday.",
        );
        seeded_memory(&store, "shared", "work", "Quarterly numbers are up.");

        let private_claim = store
            .insert_claim(
                "shared",
                "Therapy appointment on Tuesday.",
                "unknown",
                0.7,
                None,
                Some(private_chunk),
                None,
            )
            .unwrap();
        // A claim with no resolvable evidence: admissible only when its whole
        // KB is confined to the requesting diary.
        let orphan_shared = store
            .insert_claim(
                "shared",
                "Something summarised.",
                "unknown",
                0.7,
                None,
                None,
                None,
            )
            .unwrap();
        seeded_memory(&store, "solo", "work", "Standup notes.");
        let orphan_solo = store
            .insert_claim("solo", "Standup summary.", "unknown", 0.7, None, None, None)
            .unwrap();

        let ids = [private_claim, orphan_shared, orphan_solo];

        // The private diary's own recall still sees its claim.
        let seen = store.claims_visible_in_diary(&ids, "private").unwrap();
        assert!(seen.contains(&private_claim));

        // A recall scoped to "work" must NOT see a paraphrase of the private
        // diary's memory, nor an unprovenanced claim from the mixed KB.
        let seen = store.claims_visible_in_diary(&ids, "work").unwrap();
        assert!(!seen.contains(&private_claim));
        assert!(!seen.contains(&orphan_shared));
        // ...but a KB that only ever held "work" memories is safe.
        assert!(seen.contains(&orphan_solo));
    }

    #[test]
    fn claims_visible_in_diary_ignores_deleted_memory_evidence() {
        let store = SqliteStore::open_in_memory().unwrap();
        let (memory_id, chunk_id) = seeded_memory(
            &store,
            "shared",
            "private",
            "Therapy appointment on Tuesday.",
        );
        seeded_memory(&store, "shared", "work", "Quarterly numbers are up.");
        let claim = store
            .insert_claim(
                "shared",
                "Therapy appointment on Tuesday.",
                "unknown",
                0.7,
                None,
                Some(chunk_id),
                None,
            )
            .unwrap();
        store.soft_delete_memory(memory_id).unwrap();

        // The evidence is gone, so provenance can no longer be proven and the
        // KB is mixed — the claim stays out of every diary-scoped recall.
        assert!(store
            .claims_visible_in_diary(&[claim], "work")
            .unwrap()
            .is_empty());
        assert!(store
            .claims_visible_in_diary(&[claim], "private")
            .unwrap()
            .is_empty());
    }

    /// `finish_compile_job` guards on `status = 'running'`, so the update can
    /// match nothing. Reporting that as success made `jobs list` disagree with
    /// the command that had just run: the job row said `abandoned` while the
    /// caller printed a completed compile.
    #[test]
    fn finishing_a_job_that_is_not_running_is_an_error() {
        let store = SqliteStore::open_in_memory().unwrap();
        let job = store
            .create_compile_job("kb", "evidence", serde_json::json!({}))
            .unwrap();
        store
            .finish_compile_job(job, "completed", None, serde_json::json!({}))
            .expect("a running job finishes");

        // Second transition: nothing to update.
        let err = store
            .finish_compile_job(job, "completed", None, serde_json::json!({}))
            .expect_err("a finished job cannot be finished again");
        assert!(
            matches!(&err, StorageError::NotFound(m) if m.contains("completed")),
            "unexpected error: {err}"
        );

        // …and the same for a job a newer compile abandoned underneath us.
        let stale = store
            .create_compile_job("kb", "evidence", serde_json::json!({}))
            .unwrap();
        let _newer = store
            .create_compile_job("kb", "evidence", serde_json::json!({}))
            .unwrap();
        let err = store
            .finish_compile_job(stale, "completed", None, serde_json::json!({}))
            .expect_err("an abandoned job cannot be completed");
        assert!(
            matches!(&err, StorageError::NotFound(m) if m.contains("abandoned")),
            "unexpected error: {err}"
        );

        // An unknown id says so rather than passing silently.
        let err = store
            .finish_compile_job(Uuid::new_v4(), "completed", None, serde_json::json!({}))
            .expect_err("an unknown job cannot be finished");
        assert!(
            matches!(&err, StorageError::NotFound(m) if m.contains("no longer exists")),
            "unexpected error: {err}"
        );
    }
}

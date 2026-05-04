//! SQLite source of truth + FTS5 lexical index.

use crate::error::StorageError;
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
    conn: Connection,
    #[allow(dead_code)]
    path: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntegrityReport {
    pub ok: bool,
    pub stale_facts_from_deleted_memories: i64,
    pub orphan_chunks: i64,
    pub empty_kbs_with_derived: Vec<KbDerivedResidue>,
    pub duplicate_source_documents: Vec<DocumentDuplicateRecord>,
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
    pub facts_from_deleted_memories_removed: i64,
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

const CURRENT_VERSION: i32 = 4;

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let conn = Connection::open(path.as_ref())?;
        // Performance pragmas — same as engram v1.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "cache_size", -65536)?; // 64 MB
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "mmap_size", 268435456_i64)?; // 256 MB
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let store = Self {
            conn,
            path: path.as_ref().to_path_buf(),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self {
            conn,
            path: PathBuf::from(":memory:"),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), StorageError> {
        self.conn.execute_batch(SCHEMA_V1)?;
        let mut stmt = self
            .conn
            .prepare("SELECT version FROM schema_version LIMIT 1")?;
        let current: i32 = stmt.query_row([], |r| r.get(0)).optional()?.unwrap_or(0);
        drop(stmt);
        if current == 0 {
            self.conn.execute(
                "INSERT INTO schema_version(version) VALUES (?1)",
                params![1],
            )?;
        }
        if current < 2 {
            self.migrate_v2()?;
        }
        if current < 3 {
            self.conn.execute_batch(SCHEMA_V3)?;
        }
        if current < 4 {
            self.conn.execute_batch(SCHEMA_V4)?;
        }
        self.conn.execute(
            "UPDATE schema_version SET version = ?1",
            params![CURRENT_VERSION],
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

    fn add_column_if_missing(
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
        self.conn.execute(
            "UPDATE memories SET deleted = 1 WHERE kb = ?1 AND deleted = 0",
            params![name],
        )?;
        self.conn
            .execute("DELETE FROM facts WHERE kb = ?1", params![name])?;
        self.clear_derived_for_kb(name)?;
        let n = self
            .conn
            .execute("DELETE FROM knowledge_bases WHERE name = ?1", params![name])?;
        Ok(n > 0)
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
        Ok(())
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
    ) -> Result<Vec<(Uuid, String, Vec<f32>, String)>, StorageError> {
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
        let rows_iter: Box<
            dyn Iterator<Item = rusqlite::Result<(Uuid, String, Vec<f32>, String)>>,
        > = match diary {
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
        let sql = match (diary, kb) {
            (Some(_), Some(_)) =>
                "SELECT c.id, c.memory_id, c.content, c.embedding, m.diary, m.kb, c.section,
                        c.embed_model, c.embed_dimensions, c.embed_prompt_format,
                        c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
                 FROM chunks c
                 JOIN memories m ON m.id = c.memory_id
                 LEFT JOIN documents d ON d.id = c.document_id
                 WHERE c.embedding IS NOT NULL AND m.deleted = 0 AND m.diary = ?1 AND m.kb = ?2",
            (Some(_), None) =>
                "SELECT c.id, c.memory_id, c.content, c.embedding, m.diary, m.kb, c.section,
                        c.embed_model, c.embed_dimensions, c.embed_prompt_format,
                        c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
                 FROM chunks c
                 JOIN memories m ON m.id = c.memory_id
                 LEFT JOIN documents d ON d.id = c.document_id
                 WHERE c.embedding IS NOT NULL AND m.deleted = 0 AND m.diary = ?1",
            (None, Some(_)) =>
                "SELECT c.id, c.memory_id, c.content, c.embedding, m.diary, m.kb, c.section,
                        c.embed_model, c.embed_dimensions, c.embed_prompt_format,
                        c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
                 FROM chunks c
                 JOIN memories m ON m.id = c.memory_id
                 LEFT JOIN documents d ON d.id = c.document_id
                 WHERE c.embedding IS NOT NULL AND m.deleted = 0 AND m.kb = ?1",
            (None, None) =>
                "SELECT c.id, c.memory_id, c.content, c.embedding, m.diary, m.kb, c.section,
                        c.embed_model, c.embed_dimensions, c.embed_prompt_format,
                        c.document_id, COALESCE(d.source_path, json_extract(m.source_json, '$.path'))
                 FROM chunks c
                 JOIN memories m ON m.id = c.memory_id
                 LEFT JOIN documents d ON d.id = c.document_id
                 WHERE c.embedding IS NOT NULL AND m.deleted = 0",
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = match (diary, kb) {
            (Some(d), Some(k)) => stmt
                .query_map(params![d, k], parse_chunk_embedding_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (Some(d), None) => stmt
                .query_map(params![d], parse_chunk_embedding_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (None, Some(k)) => stmt
                .query_map(params![k], parse_chunk_embedding_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (None, None) => stmt
                .query_map([], parse_chunk_embedding_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
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

    /// Mark a memory as deleted (soft delete).
    pub fn soft_delete_memory(&self, memory_id: Uuid) -> Result<bool, StorageError> {
        let kb = self.kb_for_memory(memory_id)?;
        let document_id = self.document_for_memory(memory_id)?;
        let n = self.conn.execute(
            "UPDATE memories SET deleted = 1 WHERE id = ?1 AND deleted = 0",
            params![memory_id.to_string()],
        )?;
        if n > 0 {
            self.conn.execute(
                "DELETE FROM facts WHERE source_memory_id = ?1",
                params![memory_id.to_string()],
            )?;
            if document_id.is_some() {
                if let Some(kb) = kb {
                    self.clear_derived_for_kb(&kb)?;
                }
            }
        }
        Ok(n > 0)
    }

    /// Update memory content / importance.
    pub fn update_memory(
        &self,
        memory_id: Uuid,
        content: Option<&str>,
        importance: Option<u8>,
    ) -> Result<bool, StorageError> {
        let changed = match (content, importance) {
            (Some(c), Some(i)) => self.conn.execute(
                "UPDATE memories SET content = ?1, importance = ?2 WHERE id = ?3 AND deleted = 0",
                params![c, i, memory_id.to_string()],
            )?,
            (Some(c), None) => self.conn.execute(
                "UPDATE memories SET content = ?1 WHERE id = ?2 AND deleted = 0",
                params![c, memory_id.to_string()],
            )?,
            (None, Some(i)) => self.conn.execute(
                "UPDATE memories SET importance = ?1 WHERE id = ?2 AND deleted = 0",
                params![i, memory_id.to_string()],
            )?,
            (None, None) => 0,
        };
        Ok(changed > 0)
    }

    pub fn delete_facts_for_memory(&self, memory_id: Uuid) -> Result<usize, StorageError> {
        let n = self.conn.execute(
            "DELETE FROM facts WHERE source_memory_id = ?1",
            params![memory_id.to_string()],
        )?;
        Ok(n)
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
        self.delete_facts_for_memory(memory_id)?;
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
        self.conn.execute(
            "INSERT INTO documents(id, kb, title, source_path, mode, created_at, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
        let Some(existing) = self.get_document(id)? else {
            return Ok(false);
        };
        let memory_ids = self.memory_ids_for_document(id)?;
        for memory_id in &memory_ids {
            self.delete_facts_for_memory(*memory_id)?;
        }
        self.conn.execute(
            "UPDATE memories SET deleted = 1
             WHERE id IN (SELECT memory_id FROM chunks WHERE document_id = ?1)",
            params![id_s],
        )?;
        self.conn.execute(
            "DELETE FROM claims
             WHERE id IN (SELECT claim_id FROM claim_evidence WHERE document_id = ?1)",
            params![id.to_string()],
        )?;
        self.conn.execute(
            "DELETE FROM source_spans WHERE document_id = ?1",
            params![id.to_string()],
        )?;
        self.conn.execute(
            "DELETE FROM takeaways WHERE document_id = ?1",
            params![id.to_string()],
        )?;
        self.conn.execute(
            "UPDATE chunks SET document_id = NULL WHERE document_id = ?1",
            params![id.to_string()],
        )?;
        let n = self.conn.execute(
            "DELETE FROM documents WHERE id = ?1",
            params![id.to_string()],
        )?;
        self.clear_derived_for_kb(&existing.kb)?;
        Ok(n > 0)
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

    pub fn finish_compile_job(
        &self,
        id: Uuid,
        status: &str,
        message: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), StorageError> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE compile_jobs
             SET status = ?1, message = ?2, metadata_json = ?3, updated_at = ?4
             WHERE id = ?5",
            params![
                status,
                message,
                serde_json::to_string(&metadata)?,
                now,
                id.to_string()
            ],
        )?;
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
        let ok = stale_facts_from_deleted_memories == 0
            && orphan_chunks == 0
            && empty_kbs_with_derived.is_empty()
            && duplicate_source_documents.is_empty();
        Ok(IntegrityReport {
            ok,
            stale_facts_from_deleted_memories,
            orphan_chunks,
            empty_kbs_with_derived,
            duplicate_source_documents,
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
        if !dry_run {
            self.conn.execute(
                "DELETE FROM facts
                 WHERE source_memory_id IN (SELECT id FROM memories WHERE deleted = 1)",
                [],
            )?;
            for kb in &derived_kbs_cleared {
                self.clear_derived_for_kb(kb)?;
            }
        }
        Ok(RepairReport {
            dry_run,
            facts_from_deleted_memories_removed,
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

    pub fn insert_source_span(
        &self,
        kb: &str,
        document_id: Option<Uuid>,
        chunk_id: Option<Uuid>,
        section: Option<&str>,
        source: Option<&str>,
        text_preview: &str,
    ) -> Result<Uuid, StorageError> {
        let id = Uuid::new_v4();
        self.conn.execute(
            "INSERT INTO source_spans(id, kb, document_id, chunk_id, section, source, text_preview)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.to_string(),
                kb,
                document_id.map(|id| id.to_string()),
                chunk_id.map(|id| id.to_string()),
                section,
                source,
                text_preview
            ],
        )?;
        Ok(id)
    }

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
            "SELECT ce.document_id, ce.chunk_id, ss.page, ss.section, ss.source
             FROM claim_evidence ce
             LEFT JOIN source_spans ss ON ss.id = ce.source_span_id
             WHERE ce.claim_id = ?1",
        )?;
        let rows = stmt.query_map(params![claim_id.to_string()], parse_citation_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn citations_for_chunk(&self, chunk_id: Uuid) -> Result<Vec<SourceCitation>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(ss.document_id, c.document_id), c.id, ss.page,
                    COALESCE(ss.section, c.section), COALESCE(ss.source, d.source_path)
             FROM chunks c
             JOIN memories m ON m.id = c.memory_id
             LEFT JOIN documents d ON d.id = c.document_id
             LEFT JOIN source_spans ss ON ss.chunk_id = c.id
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
        let existing = self
            .conn
            .query_row(
                "SELECT id FROM entities WHERE kb = ?1 AND lower(canonical_name) = lower(?2) LIMIT 1",
                params![kb, canonical_name],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(id_str) = existing {
            self.conn.execute(
                "UPDATE entities SET mention_count = mention_count + ?1 WHERE id = ?2",
                params![increment as i64, id_str],
            )?;
            return Ok(Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()));
        }
        let id = Uuid::new_v4();
        self.conn.execute(
            "INSERT INTO entities(id, kb, canonical_name, aliases_json, kind, mention_count)
             VALUES (?1, ?2, ?3, '[]', ?4, ?5)",
            params![id.to_string(), kb, canonical_name, kind, increment as i64],
        )?;
        let alias_norm = canonical_name.to_ascii_lowercase();
        self.conn.execute(
            "INSERT OR IGNORE INTO entity_aliases(id, kb, entity_id, alias, alias_norm)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                Uuid::new_v4().to_string(),
                kb,
                id.to_string(),
                canonical_name,
                alias_norm
            ],
        )?;
        Ok(id)
    }

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
        let mut out = Vec::new();
        for _ in 0..hops.max(1) {
            let mut next = Vec::new();
            for entity_id in &frontier {
                if !seen.insert(*entity_id) {
                    continue;
                }
                let mut stmt = self.conn.prepare(
                    "SELECT r.id, r.kb, f.canonical_name, t.canonical_name, r.predicate,
                            r.weight, r.provenance_json
                     FROM relations r
                     JOIN entities f ON f.id = r.from_entity_id
                     JOIN entities t ON t.id = r.to_entity_id
                     WHERE r.kb = ?1 AND (r.from_entity_id = ?2 OR r.to_entity_id = ?2)
                     ORDER BY r.weight DESC LIMIT 50",
                )?;
                let rows =
                    stmt.query_map(params![kb, entity_id.to_string()], parse_relation_row)?;
                for row in rows {
                    let rel = row?;
                    if let Some(neighbor_id) =
                        self.find_entity(Some(kb), &rel.to_entity)?.map(|e| e.id)
                    {
                        next.push(neighbor_id);
                    }
                    out.push(rel);
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

    /// Insert a new fact. Caller is responsible for first checking whether
    /// it conflicts with an existing active fact and superseding the old one.
    pub fn insert_fact(&self, f: &engram_core::types::Fact) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO facts(id, source_memory_id, subject, subject_norm,
                predicate, object, object_norm, confidence, created_at,
                superseded_by, superseded_at, diary, kb)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                COALESCE((SELECT kb FROM memories WHERE id = ?2), 'default'))",
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
            ],
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
        let mut stmt = self.conn.prepare(
            "SELECT id, source_memory_id, subject, subject_norm, predicate,
                    object, object_norm, confidence, created_at,
                    superseded_by, superseded_at, diary
             FROM facts
             WHERE subject_norm = ?1 AND predicate = ?2 AND diary = ?3
                   AND superseded_by IS NULL
             ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![subject_norm, predicate, diary], parse_fact_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Mark `old_id` as superseded by `new_id`. Sets `superseded_at = now`.
    /// Returns true if a row was updated.
    pub fn supersede_fact(&self, old_id: Uuid, new_id: Uuid) -> Result<bool, StorageError> {
        let now = chrono::Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE facts SET superseded_by = ?1, superseded_at = ?2
             WHERE id = ?3 AND superseded_by IS NULL",
            params![new_id.to_string(), now, old_id.to_string()],
        )?;
        Ok(n > 0)
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
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE subject_norm = ?1 AND diary = ?2
                 ORDER BY created_at DESC"
            }
            (Some(_), false) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE subject_norm = ?1 AND diary = ?2
                       AND superseded_by IS NULL
                 ORDER BY created_at DESC"
            }
            (None, true) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE subject_norm = ?1
                 ORDER BY created_at DESC"
            }
            (None, false) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE subject_norm = ?1 AND superseded_by IS NULL
                 ORDER BY created_at DESC"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
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
        let sql = match (diary, include_superseded) {
            (Some(_), true) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE diary = ?1 ORDER BY created_at DESC LIMIT ?2"
            }
            (Some(_), false) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE diary = ?1 AND superseded_by IS NULL
                 ORDER BY created_at DESC LIMIT ?2"
            }
            (None, true) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts ORDER BY created_at DESC LIMIT ?1"
            }
            (None, false) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE superseded_by IS NULL
                 ORDER BY created_at DESC LIMIT ?1"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows: Vec<engram_core::types::Fact> = match diary {
            Some(d) => stmt
                .query_map(params![d, limit as i64], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![limit as i64], parse_fact_row)?
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
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        if kb.is_none() {
            return self.list_facts(diary, include_superseded, limit);
        }
        let k = kb.unwrap();
        let sql = match (diary, include_superseded) {
            (Some(_), true) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE kb = ?1 AND diary = ?2 ORDER BY created_at DESC LIMIT ?3"
            }
            (Some(_), false) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE kb = ?1 AND diary = ?2 AND superseded_by IS NULL
                 ORDER BY created_at DESC LIMIT ?3"
            }
            (None, true) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE kb = ?1 ORDER BY created_at DESC LIMIT ?2"
            }
            (None, false) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE kb = ?1 AND superseded_by IS NULL
                 ORDER BY created_at DESC LIMIT ?2"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows: Vec<engram_core::types::Fact> = match diary {
            Some(d) => stmt
                .query_map(params![k, d, limit as i64], parse_fact_row)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![k, limit as i64], parse_fact_row)?
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
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE kb = ?1 AND subject_norm = ?2 AND diary = ?3
                 ORDER BY created_at DESC"
            }
            (Some(_), false) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE kb = ?1 AND subject_norm = ?2 AND diary = ?3
                       AND superseded_by IS NULL
                 ORDER BY created_at DESC"
            }
            (None, true) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE kb = ?1 AND subject_norm = ?2
                 ORDER BY created_at DESC"
            }
            (None, false) => {
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE kb = ?1 AND subject_norm = ?2 AND superseded_by IS NULL
                 ORDER BY created_at DESC"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
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
        limit: usize,
    ) -> Result<Vec<(engram_core::types::Fact, engram_core::types::Fact)>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT old.id, old.source_memory_id, old.subject, old.subject_norm,
                    old.predicate, old.object, old.object_norm, old.confidence,
                    old.created_at, old.superseded_by, old.superseded_at, old.diary,
                    new.id, new.source_memory_id, new.subject, new.subject_norm,
                    new.predicate, new.object, new.object_norm, new.confidence,
                    new.created_at, new.superseded_by, new.superseded_at, new.diary
             FROM facts old
             JOIN facts new ON new.id = old.superseded_by
             WHERE old.superseded_by IS NOT NULL
             ORDER BY old.superseded_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            let old = parse_fact_from_offset(r, 0)?;
            let new = parse_fact_from_offset(r, 12)?;
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
            "SELECT COUNT(*) FROM facts WHERE superseded_by IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    pub fn embedding_profiles(
        &self,
        kb: Option<&str>,
    ) -> Result<Vec<(String, Option<i64>, Option<String>, i64)>, StorageError> {
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
    Ok(SourceCitation {
        document_id: document_id.and_then(|s| Uuid::parse_str(&s).ok()),
        chunk_id: chunk_id.and_then(|s| Uuid::parse_str(&s).ok()),
        page: r.get(2)?,
        section: r.get(3)?,
        source: r.get(4)?,
    })
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

fn parse_relation_row(r: &rusqlite::Row) -> rusqlite::Result<RelationRecord> {
    let id_str: String = r.get(0)?;
    let provenance_json: String = r.get(6)?;
    Ok(RelationRecord {
        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
        kb: r.get(1)?,
        from_entity: r.get(2)?,
        to_entity: r.get(3)?,
        predicate: r.get(4)?,
        weight: r.get(5)?,
        provenance: serde_json::from_str(&provenance_json).unwrap_or(serde_json::Value::Null),
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
    Ok(engram_core::types::Fact {
        id: Uuid::parse_str(&id_str).unwrap_or(Uuid::nil()),
        source_memory_id: Uuid::parse_str(&src_str).unwrap_or(Uuid::nil()),
        subject,
        subject_norm,
        predicate,
        object,
        object_norm,
        confidence,
        created_at: DateTime::parse_from_rfc3339(&created_at_str)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        superseded_by: superseded_by_str
            .as_deref()
            .and_then(|s| Uuid::parse_str(s).ok()),
        superseded_at: superseded_at_str
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        diary,
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
}

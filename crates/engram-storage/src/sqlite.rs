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

const CURRENT_VERSION: i32 = 1;

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
        let mut stmt = self.conn.prepare("SELECT version FROM schema_version LIMIT 1")?;
        let current: Option<i32> = stmt.query_row([], |r| r.get(0)).optional()?;
        if current.is_none() {
            self.conn.execute(
                "INSERT INTO schema_version(version) VALUES (?1)",
                params![CURRENT_VERSION],
            )?;
        }
        Ok(())
    }

    pub fn insert_memory(&self, m: &Memory) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO memories(id, content, created_at, event_time, importance,
                emotional_weight, access_count, last_accessed, stability, source_json,
                diary, valid_from, valid_until, tags_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
            ],
        )?;
        Ok(())
    }

    pub fn count_memories(&self) -> Result<i64, StorageError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories WHERE deleted = 0", [], |r| r.get(0))?;
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
            params![chunk_id.to_string(), memory_id.to_string(), content, position, section],
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
        let blob = f32_slice_to_bytes(embedding);
        self.conn.execute(
            "INSERT INTO chunks(id, memory_id, content, position, section, embedding, embed_model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                chunk_id.to_string(),
                memory_id.to_string(),
                content,
                position,
                section,
                blob,
                embed_model,
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
        let blob = f32_slice_to_bytes(embedding);
        self.conn.execute(
            "UPDATE chunks SET embedding = ?1, embed_model = ?2 WHERE id = ?3",
            params![blob, embed_model, chunk_id.to_string()],
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
        let rows_iter: Box<dyn Iterator<Item = rusqlite::Result<(Uuid, String, Vec<f32>, String)>>> =
            match diary {
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
        let n = self.conn.execute(
            "UPDATE memories SET deleted = 1 WHERE id = ?1 AND deleted = 0",
            params![memory_id.to_string()],
        )?;
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
            Some(_) =>
                "SELECT id, content, created_at, event_time, importance, emotional_weight,
                        access_count, last_accessed, stability, source_json, diary,
                        valid_from, valid_until, tags_json
                 FROM memories WHERE deleted = 0 AND diary = ?1
                 ORDER BY created_at DESC LIMIT ?2",
            None =>
                "SELECT id, content, created_at, event_time, importance, emotional_weight,
                        access_count, last_accessed, stability, source_json, diary,
                        valid_from, valid_until, tags_json
                 FROM memories WHERE deleted = 0
                 ORDER BY created_at DESC LIMIT ?1",
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
                source: serde_json::from_str(&src_json).unwrap_or(
                    engram_core::types::MemorySource::Manual,
                ),
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

    /// Lexical (FTS5/BM25) search. Returns chunk_id + bm25 score.
    /// Unscoped across all diaries — used by the bench which opens a fresh
    /// store per question.
    pub fn fts_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>, StorageError> {
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
                superseded_by, superseded_at, diary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
            (Some(_), true) =>
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE subject_norm = ?1 AND diary = ?2
                 ORDER BY created_at DESC",
            (Some(_), false) =>
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE subject_norm = ?1 AND diary = ?2
                       AND superseded_by IS NULL
                 ORDER BY created_at DESC",
            (None, true) =>
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE subject_norm = ?1
                 ORDER BY created_at DESC",
            (None, false) =>
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE subject_norm = ?1 AND superseded_by IS NULL
                 ORDER BY created_at DESC",
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
            (Some(_), true) =>
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE diary = ?1 ORDER BY created_at DESC LIMIT ?2",
            (Some(_), false) =>
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE diary = ?1 AND superseded_by IS NULL
                 ORDER BY created_at DESC LIMIT ?2",
            (None, true) =>
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts ORDER BY created_at DESC LIMIT ?1",
            (None, false) =>
                "SELECT id, source_memory_id, subject, subject_norm, predicate,
                        object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary
                 FROM facts WHERE superseded_by IS NULL
                 ORDER BY created_at DESC LIMIT ?1",
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
            .insert_chunk(chunk_id, m.id, "Rapamycin inhibits mTORC1 signaling.", 0, None)
            .unwrap();
        let hits = store.fts_search("rapamycin", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, chunk_id);
    }
}

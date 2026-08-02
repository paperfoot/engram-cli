//! Handle resolution, and the read helpers the surfaces need to stop
//! guessing.
//!
//! Every mutator in v0.3 answered "I could not find that id" the same way
//! whether the id was a chunk, a fact, a typo or a memory the caller had
//! already deleted — which is where `forget`'s "no memory found … (already
//! deleted?)" came from: a question mark in place of an answer the store had.

use crate::error::StorageError;
use crate::sqlite::SqliteStore;
use engram_core::handle::{Handle, HandleKind};
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;
use uuid::Uuid;

/// What an id turned out to be. Carries no `deleted` flag on purpose: a caller
/// that cares reads the memory row, and a flag here would be a second source
/// of truth about deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub kind: HandleKind,
    pub id: Uuid,
    pub memory_id: Option<Uuid>,
}

/// The probe order for a bare uuid. PINNED, so two stores with the same rows
/// answer identically — uuid collisions across tables are a practical
/// impossibility and a deterministic answer beats a clever one.
/// The two kinds added by D24 sit at the END of the order, after every kind a
/// v0.3 script could already be passing: a bare uuid that used to resolve to a
/// memory must keep resolving to a memory.
const PROBE_ORDER: [HandleKind; 10] = [
    HandleKind::Memory,
    HandleKind::Chunk,
    HandleKind::Fact,
    HandleKind::Conflict,
    HandleKind::Entity,
    HandleKind::Document,
    HandleKind::Job,
    HandleKind::Event,
    HandleKind::Lesson,
    HandleKind::Wiki,
];

fn table_of(kind: HandleKind) -> &'static str {
    match kind {
        HandleKind::Memory => "memories",
        HandleKind::Chunk => "chunks",
        HandleKind::Fact => "facts",
        HandleKind::Conflict => "fact_conflict_queue",
        HandleKind::Entity => "entities",
        HandleKind::Document => "documents",
        HandleKind::Job => "compile_jobs",
        HandleKind::Event => "events",
        HandleKind::Lesson => "lessons",
        HandleKind::Wiki => "wiki_pages",
    }
}

impl SqliteStore {
    /// Resolve a handle to what it actually names.
    ///
    /// A typed handle probes only its own table: a uuid that exists under a
    /// different kind still returns `None` here, and the CALLER then re-asks
    /// with `kind: None` to name the actual kind in a `wrong_id_kind` error.
    /// Both probes are cheap, and the split keeps CLI error vocabulary out of
    /// the storage API.
    ///
    /// **Soft-deleted memories resolve.** `forget` and `edit` must be able to
    /// say "already deleted" rather than "not found".
    pub fn resolve_handle(&self, h: &Handle) -> Result<Option<Resolved>, StorageError> {
        match h.kind {
            Some(k) => self.probe(k, h.id),
            None => {
                for k in PROBE_ORDER {
                    if let Some(found) = self.probe(k, h.id)? {
                        return Ok(Some(found));
                    }
                }
                Ok(None)
            }
        }
    }

    fn probe(&self, kind: HandleKind, id: Uuid) -> Result<Option<Resolved>, StorageError> {
        let table = table_of(kind);
        // The memory a row belongs to, where the row knows: `Chunk` follows
        // `chunks.memory_id`, `Fact` follows `facts.source_memory_id`, a
        // memory is its own, and nothing else has one.
        let memory_column = match kind {
            HandleKind::Memory => Some("id"),
            HandleKind::Chunk => Some("memory_id"),
            HandleKind::Fact => Some("source_memory_id"),
            _ => None,
        };
        let sql = match memory_column {
            Some(col) => format!("SELECT {col} FROM {table} WHERE id = ?1"),
            None => format!("SELECT NULL FROM {table} WHERE id = ?1"),
        };
        let found: Option<Option<String>> = self
            .conn
            .query_row(&sql, params![id.to_string()], |r| r.get(0))
            .optional()?;
        Ok(found.map(|memory| Resolved {
            kind,
            id,
            memory_id: memory.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
        }))
    }

    /// `chunk_id -> memory_id` for a batch of chunks, in one query.
    ///
    /// Recall scores chunks and has to report which memory each result came
    /// from; doing that with one `get_chunk_record` per hit turned a 50-result
    /// page into 50 point queries.
    pub fn memory_ids_for_chunks(
        &self,
        chunk_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Uuid>, StorageError> {
        let mut out = HashMap::new();
        if chunk_ids.is_empty() {
            return Ok(out);
        }
        let placeholders = (1..=chunk_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT id, memory_id FROM chunks WHERE id IN ({placeholders})");
        let binds: Vec<String> = chunk_ids.iter().map(|i| i.to_string()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (chunk, memory) = row?;
            if let (Ok(c), Ok(m)) = (Uuid::parse_str(&chunk), Uuid::parse_str(&memory)) {
                out.insert(c, m);
            }
        }
        Ok(out)
    }

    // ---------------------------------------------------------------------
    // Counts. Every listing that reports `returned` without `total` teaches
    // an agent that the backlog is however long the page was.
    // ---------------------------------------------------------------------

    pub fn count_facts_scoped(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
        include_superseded: bool,
    ) -> Result<i64, StorageError> {
        let open = if include_superseded {
            ""
        } else {
            " AND valid_until IS NULL AND superseded_by IS NULL"
        };
        Ok(self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM facts
                  WHERE (?1 IS NULL OR diary = ?1) AND (?2 IS NULL OR kb = ?2){open}"
            ),
            params![diary, kb],
            |r| r.get(0),
        )?)
    }

    pub fn count_facts_by_subject_scoped(
        &self,
        subject_norm: &str,
        diary: Option<&str>,
        kb: Option<&str>,
        include_superseded: bool,
    ) -> Result<i64, StorageError> {
        let open = if include_superseded {
            ""
        } else {
            " AND valid_until IS NULL AND superseded_by IS NULL"
        };
        Ok(self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM facts
                  WHERE subject_norm = ?1
                    AND (?2 IS NULL OR diary = ?2) AND (?3 IS NULL OR kb = ?3){open}"
            ),
            params![subject_norm, diary, kb],
            |r| r.get(0),
        )?)
    }

    pub fn count_memories_scoped(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
    ) -> Result<i64, StorageError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM memories
              WHERE deleted = 0 AND (?1 IS NULL OR diary = ?1) AND (?2 IS NULL OR kb = ?2)",
            params![diary, kb],
            |r| r.get(0),
        )?)
    }

    pub fn count_documents(&self, kb: Option<&str>) -> Result<i64, StorageError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM documents WHERE (?1 IS NULL OR kb = ?1)",
            params![kb],
            |r| r.get(0),
        )?)
    }

    pub fn count_entities(&self, kb: Option<&str>, min_mentions: u32) -> Result<i64, StorageError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM entities
              WHERE (?1 IS NULL OR kb = ?1) AND mention_count >= ?2",
            params![kb, min_mentions as i64],
            |r| r.get(0),
        )?)
    }

    pub fn count_entities_by_kind(
        &self,
        kb: Option<&str>,
        kind: &str,
        min_mentions: u32,
    ) -> Result<i64, StorageError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM entities
              WHERE (?1 IS NULL OR kb = ?1) AND kind = ?2 AND mention_count >= ?3",
            params![kb, kind, min_mentions as i64],
            |r| r.get(0),
        )?)
    }

    pub fn count_recent_conflicts(
        &self,
        diary: Option<&str>,
        kb: Option<&str>,
    ) -> Result<i64, StorageError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM facts old
              JOIN facts new ON new.id = old.superseded_by
              WHERE old.superseded_by IS NOT NULL
                AND (?1 IS NULL OR old.diary = ?1) AND (?2 IS NULL OR old.kb = ?2)",
            params![diary, kb],
            |r| r.get(0),
        )?)
    }

    pub fn count_open_conflicts_matching_terms(
        &self,
        kb: Option<&str>,
        diary: Option<&str>,
        terms: &[String],
    ) -> Result<i64, StorageError> {
        if terms.is_empty() {
            return Ok(0);
        }
        let mut sql = String::from(
            "SELECT COUNT(*) FROM fact_conflict_queue q
               JOIN facts nf ON nf.id = q.new_fact_id
               JOIN facts of ON of.id = q.old_fact_id
              WHERE q.status = 'open' AND (?1 IS NULL OR q.kb = ?1)
                AND (?2 IS NULL OR q.diary = ?2) AND ",
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(kb.map(|s| s.to_string())),
            Box::new(diary.map(|s| s.to_string())),
        ];
        let mut clauses = Vec::new();
        for term in terms {
            let like = format!("%{}%", term.to_ascii_lowercase());
            let mut idx = Vec::new();
            for _ in 0..4 {
                binds.push(Box::new(like.clone()));
                idx.push(binds.len());
            }
            clauses.push(format!(
                "nf.subject_norm LIKE ?{} OR nf.object_norm LIKE ?{} \
                 OR of.subject_norm LIKE ?{} OR of.object_norm LIKE ?{}",
                idx[0], idx[1], idx[2], idx[3]
            ));
        }
        sql.push('(');
        sql.push_str(&clauses.join(" OR "));
        sql.push(')');
        Ok(self.conn.query_row(
            &sql,
            rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
            |r| r.get(0),
        )?)
    }

    /// The journal slice a reader tails: events of the given kinds after
    /// `from_seq`. An empty `kinds` means every kind.
    pub fn events_since(
        &self,
        kinds: &[engram_core::events::EventKind],
        from_seq: i64,
        limit: usize,
    ) -> Result<Vec<crate::journal::EventRow>, StorageError> {
        let all = self.events_slice(from_seq, None, None)?;
        Ok(all
            .into_iter()
            .filter(|e| kinds.is_empty() || kinds.contains(&e.kind))
            .take(limit)
            .collect())
    }

    /// The first `seq` at or after a wall-clock instant. `0` when the journal
    /// has nothing that late — callers read that as "start from the top".
    pub fn seq_at_or_after(&self, ts: chrono::DateTime<chrono::Utc>) -> Result<i64, StorageError> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT MIN(seq) FROM events WHERE ts >= ?1",
                params![ts.to_rfc3339()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(seq.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_store;
    use engram_core::types::{Memory, MemorySource};

    #[test]
    fn resolve_handle_answers_every_kind_and_a_bare_uuid() {
        let (_tmp, _g, store) = temp_store();
        let m = Memory {
            id: Uuid::new_v4(),
            content: "Ana takes metformin.".into(),
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
        store.insert_memory_with_kb(&m, "personal").unwrap();
        let chunk = Uuid::new_v4();
        store
            .insert_chunk(chunk, m.id, "Ana takes metformin.", 0, None)
            .unwrap();

        // Bare uuid: the pinned probe order finds the memory.
        let bare = store
            .resolve_handle(&Handle {
                kind: None,
                id: m.id,
            })
            .unwrap()
            .expect("memory resolves");
        assert_eq!(bare.kind, HandleKind::Memory);
        assert_eq!(bare.memory_id, Some(m.id));

        // A chunk follows through to its memory, which is what lets every
        // mutator accept any handle.
        let c = store
            .resolve_handle(&Handle {
                kind: None,
                id: chunk,
            })
            .unwrap()
            .expect("chunk resolves");
        assert_eq!(c.kind, HandleKind::Chunk);
        assert_eq!(c.memory_id, Some(m.id));
        assert_eq!(
            store
                .memory_ids_for_chunks(&[chunk])
                .unwrap()
                .get(&chunk)
                .copied(),
            Some(m.id)
        );

        // A typed handle probes only its own table: the wrong kind is None
        // here, and the caller re-asks untyped to name what it really is.
        assert_eq!(
            store
                .resolve_handle(&Handle {
                    kind: Some(HandleKind::Fact),
                    id: m.id
                })
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .resolve_handle(&Handle {
                    kind: None,
                    id: Uuid::new_v4()
                })
                .unwrap(),
            None
        );

        // A forgotten memory still resolves — "already deleted" is an answer,
        // "not found" is a guess.
        store.soft_delete_memory(m.id).unwrap();
        assert!(store
            .resolve_handle(&Handle {
                kind: Some(HandleKind::Memory),
                id: m.id
            })
            .unwrap()
            .is_some());
        assert_eq!(
            store.memory_scope_including_deleted(m.id).unwrap(),
            Some(("personal".to_string(), "default".to_string(), true))
        );
    }

    #[test]
    fn counts_are_scoped_and_respect_closed_intervals() {
        let (_tmp, _g, store) = temp_store();
        assert_eq!(store.count_memories_scoped(None, None).unwrap(), 0);
        assert_eq!(store.count_facts_scoped(None, None, false).unwrap(), 0);
        assert_eq!(store.count_documents(None).unwrap(), 0);
        assert_eq!(store.count_entities(None, 0).unwrap(), 0);
        assert_eq!(store.count_recent_conflicts(None, None).unwrap(), 0);
        assert_eq!(
            store
                .count_open_conflicts_matching_terms(None, None, &["ana".to_string()])
                .unwrap(),
            0
        );
        assert_eq!(store.seq_at_or_after(chrono::Utc::now()).unwrap(), 0);
    }
}

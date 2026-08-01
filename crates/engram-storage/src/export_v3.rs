//! Export v3 — a journal slice, and an import that replays it.
//!
//! Invariant 4. A v2 export was a pile of rows: it restored WHAT the store
//! believed and nothing about why, so an imported store had no history, no
//! decisions, and no way to rebuild its own projections. v3 carries the events
//! as the source of truth and keeps the v2 sections for the things the journal
//! deliberately does not hold — chunk embeddings, access telemetry, documents.
//!
//! This module owns the format, the writer and the replayer as library
//! functions; wiring `engram export` / `engram import` to them is
//! write-surfaces' work.

use crate::error::StorageError;
use crate::sqlite::{
    ChunkExportRow, DocumentExportRow, FactExportRow, KbExportRow, MemoryExportRow, SqliteStore,
    CURRENT_VERSION,
};
use serde::{Deserialize, Serialize};

pub const EXPORT_VERSION: i64 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportV3 {
    pub version: i64,
    pub schema_version: i32,
    pub kb: Option<String>,
    pub exported_at: String,
    pub counts: ExportCounts,
    /// The journal slice, in `seq` order. Lossless: this is the source of
    /// truth for facts, conflicts and decisions.
    pub events: Vec<ExportEvent>,
    /// v2 sections, retained verbatim so a v3 file also restores what the
    /// journal deliberately does not carry.
    pub kbs: Vec<KbExportRow>,
    pub documents: Vec<DocumentExportRow>,
    pub memories: Vec<MemoryExportRow>,
    pub chunks: Vec<ChunkExportRow>,
    pub facts: Vec<FactExportRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportCounts {
    pub events: usize,
    pub kbs: usize,
    pub documents: usize,
    pub memories: usize,
    pub chunks: usize,
    pub facts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEvent {
    /// Local append order at export time. NOT preserved on import — it is this
    /// store's sequence, and two merged slices cannot share one — but it IS
    /// the replay order, which is why it travels.
    pub seq: i64,
    pub id: String,
    pub ts: String,
    pub actor: String,
    pub kind: String,
    pub subject_id: Option<String>,
    pub kb: Option<String>,
    pub diary: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportV3Report {
    pub kbs: usize,
    pub documents: usize,
    pub memories: usize,
    pub chunks: usize,
    pub facts: usize,
    pub events: usize,
    pub events_skipped_duplicate: usize,
    pub genesis_synthesized: usize,
    pub conflicts_skipped_orphan: usize,
}

impl SqliteStore {
    pub fn export_v3(&self, kb: Option<&str>) -> Result<ExportV3, StorageError> {
        // A per-KB export takes that KB's events PLUS the un-scoped events
        // whose subject lives in it, or the slice is silently missing its own
        // genesis.
        let events: Vec<ExportEvent> = {
            let mut stmt = self.conn.prepare(
                "SELECT seq, id, ts, actor, kind, subject_id, kb, diary, payload
                   FROM events
                  WHERE ?1 IS NULL OR kb = ?1
                     OR (kb IS NULL AND subject_id IN (
                            SELECT id FROM memories WHERE kb = ?1
                            UNION ALL SELECT id FROM facts WHERE kb = ?1))
                  ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(rusqlite::params![kb], |r| {
                Ok(ExportEvent {
                    seq: r.get(0)?,
                    id: r.get(1)?,
                    ts: r.get(2)?,
                    actor: r.get(3)?,
                    kind: r.get(4)?,
                    subject_id: r.get(5)?,
                    kb: r.get(6)?,
                    diary: r.get(7)?,
                    payload: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let kbs = self.export_kbs(kb)?;
        let documents = self.export_documents(kb)?;
        let memories = self.export_memories(kb)?;
        let chunks = self.export_chunks(kb)?;
        let facts = self.export_facts(kb)?;
        Ok(ExportV3 {
            version: EXPORT_VERSION,
            schema_version: CURRENT_VERSION,
            kb: kb.map(|s| s.to_string()),
            exported_at: chrono::Utc::now().to_rfc3339(),
            counts: ExportCounts {
                events: events.len(),
                kbs: kbs.len(),
                documents: documents.len(),
                memories: memories.len(),
                chunks: chunks.len(),
                facts: facts.len(),
            },
            events,
            kbs,
            documents,
            memories,
            chunks,
            facts,
        })
    }

    /// Replay an import: v2 rows first (the things the journal does not
    /// carry), then the event slice through the same `rebuild::apply`
    /// dispatcher, so import and `admin rebuild` cannot drift.
    ///
    /// Facts appear in BOTH sections and the precedence is pinned: the rows
    /// land first with `INSERT OR IGNORE` — they exist solely to carry
    /// `embedding`/`embed_model`, which no event holds — and the replay is
    /// then authoritative for everything else. Replay-then-rows loses the
    /// interval closes; rows-then-replay without `OR IGNORE` aborts on
    /// `facts.id`.
    pub fn import_v3(&self, doc: &ExportV3) -> Result<ImportV3Report, StorageError> {
        if doc.schema_version > CURRENT_VERSION {
            return Err(StorageError::Migration(format!(
                "export was written by schema version {} and this binary supports {}; \
                 upgrade engram instead of downgrading the file",
                doc.schema_version, CURRENT_VERSION
            )));
        }
        self.with_tx(|s| {
            let mut report = ImportV3Report::default();
            s.import_v2_sections(
                &doc.kbs,
                &doc.documents,
                &doc.memories,
                &doc.chunks,
                &doc.facts,
                &mut report,
            )?;
            for e in &doc.events {
                // Event ids are preserved, so re-importing the same file is a
                // no-op rather than a duplicated journal. `seq` is not: it is
                // the exporting store's append order.
                let kind = engram_core::events::EventKind::parse(&e.kind).ok_or_else(|| {
                    StorageError::MalformedRow(format!("unknown event kind `{}`", e.kind))
                })?;
                let id = uuid::Uuid::parse_str(&e.id).map_err(|err| {
                    StorageError::MalformedRow(format!("event id `{}`: {err}", e.id))
                })?;
                let ts = chrono::DateTime::parse_from_rfc3339(&e.ts)
                    .map(|d| d.with_timezone(&chrono::Utc))
                    .map_err(|err| {
                        StorageError::MalformedRow(format!("event ts `{}`: {err}", e.ts))
                    })?;
                let draft = crate::journal::EventDraft::new(kind, &e.actor, e.payload.clone())
                    .scope(e.kb.as_deref(), e.diary.as_deref());
                let draft = match e
                    .subject_id
                    .as_deref()
                    .and_then(|v| uuid::Uuid::parse_str(v).ok())
                {
                    Some(u) => draft.subject(u),
                    None => draft,
                };
                match s.append_event_with_identity(id, ts, &draft)? {
                    Some(_) => report.events += 1,
                    None => {
                        report.events_skipped_duplicate += 1;
                        continue;
                    }
                }
                let row = s
                    .event_by_id(id)?
                    .expect("just appended, inside this transaction");
                match crate::rebuild::apply(s, &row) {
                    Ok(()) => {}
                    Err(StorageError::NotFound(_))
                        if kind == engram_core::events::EventKind::ConflictOpened =>
                    {
                        report.conflicts_skipped_orphan += 1;
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(report)
        })
    }

    /// A v2 file stays importable forever, and its missing journal is
    /// SYNTHESIZED from the rows it does carry — the same generator the v9→v10
    /// migration uses, so a v2 import and a migration produce identical
    /// journals for identical rows. Without this an imported store would be
    /// un-rebuildable and invariant 3 would fail on it.
    pub fn import_v2_with_genesis(
        &self,
        kbs: &[KbExportRow],
        documents: &[DocumentExportRow],
        memories: &[MemoryExportRow],
        chunks: &[ChunkExportRow],
        facts: &[FactExportRow],
        actor: &str,
    ) -> Result<ImportV3Report, StorageError> {
        self.with_tx(|s| {
            let mut report = ImportV3Report::default();
            s.import_v2_sections(kbs, documents, memories, chunks, facts, &mut report)?;
            let counts = s.synthesize_genesis(actor)?;
            report.genesis_synthesized = counts.events;
            report.events = counts.events;
            Ok(report)
        })
    }

    fn import_v2_sections(
        &self,
        kbs: &[KbExportRow],
        documents: &[DocumentExportRow],
        memories: &[MemoryExportRow],
        chunks: &[ChunkExportRow],
        facts: &[FactExportRow],
        report: &mut ImportV3Report,
    ) -> Result<(), StorageError> {
        for row in kbs {
            report.kbs += self.import_kb_row(row)? as usize;
        }
        for row in documents {
            report.documents += self.import_document_row(row)? as usize;
        }
        for row in memories {
            report.memories += self.import_memory_row(row)? as usize;
        }
        for row in chunks {
            report.chunks += self.import_chunk_row(row)? as usize;
        }
        for row in facts {
            report.facts += self.import_fact_row(row)? as usize;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{extra_store, temp_store};
    use engram_core::types::{Fact, FactType, Memory, MemorySource, SourceKind};
    use uuid::Uuid;

    fn seed(store: &SqliteStore) -> (Uuid, Uuid, Uuid) {
        let m = Memory {
            id: Uuid::new_v4(),
            content: "Ana lived in Lisbon, then Porto.".into(),
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
        let mk = |object: &str| Fact {
            id: Uuid::new_v4(),
            source_memory_id: m.id,
            subject: "Ana".into(),
            subject_norm: "ana".into(),
            predicate: "lives_in".into(),
            object: object.into(),
            object_norm: object.to_lowercase(),
            confidence: 0.9,
            created_at: chrono::Utc::now(),
            superseded_by: None,
            superseded_at: None,
            diary: "default".into(),
            fact_type: FactType::ProfileSlot,
            valid_from: Some(chrono::Utc::now()),
            valid_until: None,
            resolution: None,
            source_kind: SourceKind::AgentExtracted,
            confirmations: 0,
            closed_by: None,
            close_reason: None,
            close_authority: None,
            kb: "personal".into(),
        };
        let old = mk("Lisbon");
        let new = mk("Porto");
        store.insert_fact(&old).unwrap();
        store.insert_fact(&new).unwrap();
        store
            .supersede_fact_with(old.id, new.id, "caller", None)
            .unwrap();
        (m.id, new.id, old.id)
    }

    fn facts_digest(store: &SqliteStore) -> Vec<String> {
        let mut rows: Vec<String> = store
            .list_facts(None, true, 100)
            .unwrap()
            .iter()
            .map(|f| {
                format!(
                    "{}|{}|{}|{:?}|{:?}|{:?}|{}",
                    f.id,
                    f.object_norm,
                    f.kb,
                    f.valid_until,
                    f.close_reason,
                    f.superseded_by,
                    f.fact_type.as_str()
                )
            })
            .collect();
        rows.sort();
        rows
    }

    #[test]
    fn export_v3_round_trips_into_a_fresh_store() {
        let (_tmp, _g, source) = temp_store();
        seed(&source);
        let doc = source.export_v3(None).unwrap();
        assert_eq!(doc.version, 3);
        assert!(doc.counts.events >= 4, "memory + 2 facts + 1 close");

        let target = extra_store(&_tmp, "target");
        let report = target.import_v3(&doc).unwrap();
        assert_eq!(report.events, doc.counts.events);
        assert_eq!(
            facts_digest(&target),
            facts_digest(&source),
            "a v3 file must restore the fact projection exactly"
        );
    }

    #[test]
    fn importing_the_same_v3_file_twice_is_idempotent() {
        let (_tmp, _g, source) = temp_store();
        seed(&source);
        let doc = source.export_v3(None).unwrap();
        let target = extra_store(&_tmp, "target");
        target.import_v3(&doc).unwrap();
        let before = facts_digest(&target);
        let events_before = target.count_events().unwrap();

        let second = target.import_v3(&doc).unwrap();
        assert_eq!(second.events, 0);
        assert_eq!(second.events_skipped_duplicate, doc.counts.events);
        assert_eq!(target.count_events().unwrap(), events_before);
        assert_eq!(facts_digest(&target), before);
    }

    #[test]
    fn a_v2_file_imports_with_a_synthesized_journal() {
        let (_tmp, _g, source) = temp_store();
        seed(&source);
        // A v2 file is the row sections and nothing else.
        let kbs = source.export_kbs(None).unwrap();
        let documents = source.export_documents(None).unwrap();
        let memories = source.export_memories(None).unwrap();
        let chunks = source.export_chunks(None).unwrap();
        let facts = source.export_facts(None).unwrap();

        let target = extra_store(&_tmp, "target");
        let report = target
            .import_v2_with_genesis(&kbs, &documents, &memories, &chunks, &facts, "import:v2")
            .unwrap();
        assert!(report.genesis_synthesized > 0);
        assert_eq!(facts_digest(&target), facts_digest(&source));

        // …and the resulting store passes the rebuild-identity property, which
        // is the whole point: a v2 import that produced no journal would leave
        // a store `admin rebuild` could only empty.
        let before = facts_digest(&target);
        target.rebuild_projections(None, false).unwrap();
        assert_eq!(facts_digest(&target), before);
    }

    #[test]
    fn a_future_schema_version_is_refused() {
        let (_tmp, _g, store) = temp_store();
        let mut doc = store.export_v3(None).unwrap();
        doc.schema_version = CURRENT_VERSION + 1;
        let err = store.import_v3(&doc).unwrap_err();
        assert!(matches!(err, StorageError::Migration(_)), "{err:?}");
        assert!(
            err.to_string().contains("upgrade engram"),
            "the message must point up, not down: {err}"
        );
    }
}

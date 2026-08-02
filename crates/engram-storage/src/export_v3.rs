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
    /// [G2] Closes the file asked for and the store refused: a safety
    /// relation retired by something other than a person. The row still
    /// imports — OPEN.
    pub close_authority_rejected: usize,
}

/// [G2] The close authorities a PERSON can be behind. `may_close` allows
/// exactly these on a safety relation; an import cannot re-derive the
/// classification that produced the row, so it checks the authority instead.
const PERSON_SUPPLIED_AUTHORITIES: [&str; 3] = ["explicit_end", "caller", "replayed_caller"];

/// [G2] Does this imported close have to be dropped?
///
/// True when a SAFETY relation arrives closed by anything but a person —
/// including by nothing at all, since a close no one can name an authority
/// for is not a close this store will honour on an allergy or a
/// prescription. Non-safety relations are unaffected: "Bruno moved out of
/// Lisbon" still imports closed, whoever decided it.
///
/// The direction of the error is deliberate. Dropping a close leaves the
/// interval OPEN, which is the same direction `may_close` already errs in:
/// still-allergic, still-prescribed, and visible to the caller rather than
/// silently retired by a file.
pub(crate) fn close_needs_person(predicate: &str, authority: Option<&str>) -> bool {
    if !engram_core::relations::is_safety_slot(&engram_core::relations::canonical_relation(
        predicate,
    )) {
        return false;
    }
    !authority
        .map(|a| PERSON_SUPPLIED_AUTHORITIES.contains(&a))
        .unwrap_or(false)
}

/// [G2] A negative-length interval. Unreadable to `Interval::overlaps` and
/// to every consumer downstream of it; the live path and the migration both
/// clamp it away, and an import is the only door it can still walk through.
/// Unparsable timestamps are NOT rejected here — they are the v2 files'
/// problem, and the parsers downstream already tolerate them.
pub(crate) fn interval_is_inverted(from: Option<&str>, until: Option<&str>) -> bool {
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    match (from.and_then(parse), until.and_then(parse)) {
        (Some(f), Some(u)) => u < f,
        _ => false,
    }
}

pub(crate) fn inverted_interval_error(id: &str, from: &str, until: &str) -> StorageError {
    StorageError::MalformedRow(format!(
        "fact {id}: valid_until {until} precedes valid_from {from}; an interval that ends \
         before it begins is not something this store can import"
    ))
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
                match crate::rebuild::apply_from(s, &row, crate::rebuild::Origin::Imported) {
                    Ok(refused) => report.close_authority_rejected += refused,
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
            let outcome = self.import_fact_row(row)?;
            report.facts += outcome.inserted as usize;
            report.close_authority_rejected += outcome.close_rejected as usize;
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

    /// One OPEN allergy — a safety relation, the case `may_close` exists for.
    fn seed_allergy(store: &SqliteStore) -> (Uuid, Uuid) {
        let m = Memory {
            id: Uuid::new_v4(),
            content: "Ana is allergic to penicillin.".into(),
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
        let f = Fact {
            id: Uuid::new_v4(),
            source_memory_id: m.id,
            subject: "Ana".into(),
            subject_norm: "ana".into(),
            predicate: "allergic_to".into(),
            object: "penicillin".into(),
            object_norm: "penicillin".into(),
            confidence: 0.95,
            created_at: chrono::Utc::now(),
            superseded_by: None,
            superseded_at: None,
            diary: "default".into(),
            fact_type: FactType::Assertion,
            valid_from: Some(chrono::Utc::now()),
            valid_until: None,
            resolution: None,
            source_kind: SourceKind::UserStated,
            confirmations: 0,
            closed_by: None,
            close_reason: None,
            close_authority: None,
            kb: "personal".into(),
        };
        store.insert_fact(&f).unwrap();
        (m.id, f.id)
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

    // -----------------------------------------------------------------
    // [G2] An import is the one write path whose bytes a caller edits by
    // hand. Everything else re-derives what it stores through `may_close`;
    // an import writes columns straight through, so the gate has to live
    // here too or `jq` is a privilege escalation.
    // -----------------------------------------------------------------

    /// The release gate's own reproduction, in Rust: export a store, empty
    /// the journal so nothing can contradict the rows, and hand-edit the
    /// allergy into a CLOSED interval with `heuristic` authority — which
    /// `may_close` refuses on every safety relation.
    #[test]
    fn a_forged_close_on_a_safety_relation_imports_open() {
        let (_tmp, _g, source) = temp_store();
        let (_m, allergy) = seed_allergy(&source);
        let mut doc = source.export_v3(None).unwrap();
        doc.events.clear();
        doc.counts.events = 0;
        let row = doc
            .facts
            .iter_mut()
            .find(|f| f.predicate == "allergic_to")
            .expect("the export carries the allergy");
        // `valid_from` is forged back too, so this row fails ONE rule and the
        // test cannot pass on the interval-order check by accident.
        row.valid_from = Some("2026-01-01T00:00:00Z".to_string());
        row.valid_until = Some("2026-08-01T00:00:00Z".to_string());
        row.close_reason = Some("superseded".to_string());
        row.close_authority = Some("heuristic".to_string());

        let target = extra_store(&_tmp, "target");
        let report = target.import_v3(&doc).unwrap();
        assert_eq!(
            report.close_authority_rejected, 1,
            "the refusal has to be COUNTED; a silent one is a store that \
             disagrees with the file it was handed"
        );
        let f = target
            .get_fact(allergy)
            .unwrap()
            .expect("the fact itself still imports — only its close is dropped");
        assert_eq!(f.valid_until, None);
        assert_eq!(f.close_reason, None);
        assert_eq!(f.close_authority, None);
        assert_eq!(f.superseded_by, None);
        assert!(engram_core::interval::is_open_fact(&f));
    }

    /// The other half of the same rule: a close a PERSON is entitled to make
    /// travels through an import intact. A gate that refused these would make
    /// export/import lossy for every legitimately retired prescription.
    #[test]
    fn an_imported_caller_close_on_a_safety_relation_is_honoured() {
        let (_tmp, _g, source) = temp_store();
        let (_m, allergy) = seed_allergy(&source);
        source
            .close_fact_interval(
                allergy,
                chrono::Utc::now(),
                "corrected",
                "caller",
                None,
                Some("caller"),
            )
            .unwrap();
        let doc = source.export_v3(None).unwrap();

        let target = extra_store(&_tmp, "target");
        let report = target.import_v3(&doc).unwrap();
        assert_eq!(report.close_authority_rejected, 0);
        let f = target.get_fact(allergy).unwrap().unwrap();
        assert!(f.valid_until.is_some());
        assert_eq!(f.close_authority.as_deref(), Some("caller"));
    }

    /// The journal half of the same file. `import_v3` treats the events as
    /// the source of truth, so a forger who edits only the events wins
    /// unless the replay applies the same gate the rows do.
    #[test]
    fn a_forged_close_event_on_a_safety_relation_is_refused_too() {
        let (_tmp, _g, source) = temp_store();
        let (_m, allergy) = seed_allergy(&source);
        source
            .close_fact_interval(
                allergy,
                chrono::Utc::now(),
                "corrected",
                "caller",
                None,
                Some("caller"),
            )
            .unwrap();
        let mut doc = source.export_v3(None).unwrap();
        // The row section is left honest — only the journal is edited, so
        // this test cannot pass on the row gate alone.
        for f in doc.facts.iter_mut() {
            f.valid_until = None;
            f.close_reason = None;
            f.close_authority = None;
            f.closed_by = None;
            f.superseded_by = None;
            f.superseded_at = None;
        }
        for e in doc.events.iter_mut() {
            if e.kind == "fact.interval_closed" {
                e.payload["authority"] = serde_json::json!("heuristic");
            }
        }

        let target = extra_store(&_tmp, "target");
        let report = target.import_v3(&doc).unwrap();
        assert_eq!(report.close_authority_rejected, 1);
        let f = target.get_fact(allergy).unwrap().unwrap();
        assert!(
            engram_core::interval::is_open_fact(&f),
            "a replayed close is still a close, and the gate does not care \
             which half of the file it arrived in"
        );
    }

    /// A negative-length interval is not a policy question — it is nonsense
    /// that `overlaps` and every reader downstream cannot answer. The file is
    /// refused whole rather than half-imported.
    #[test]
    fn an_import_refuses_an_interval_that_ends_before_it_begins() {
        let (_tmp, _g, source) = temp_store();
        let (_m, allergy) = seed_allergy(&source);
        let mut doc = source.export_v3(None).unwrap();
        doc.events.clear();
        doc.counts.events = 0;
        let row = doc
            .facts
            .iter_mut()
            .find(|f| f.predicate == "allergic_to")
            .unwrap();
        row.valid_from = Some("2026-01-01T00:00:00Z".to_string());
        row.valid_until = Some("2020-01-01T00:00:00Z".to_string());

        let target = extra_store(&_tmp, "target");
        let err = target.import_v3(&doc).unwrap_err();
        assert!(
            err.to_string().contains("valid_until"),
            "the message must name the field: {err}"
        );
        assert!(
            target.get_fact(allergy).unwrap().is_none(),
            "a refused import rolls back whole"
        );
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

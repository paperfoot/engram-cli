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
    /// [G8] Closes the file asked for and the store refused, on rows and on
    /// replayed events alike: a safety interval never arrives closed from
    /// outside. The fact still imports — OPEN — and `engram import` prints
    /// this number, because a store that silently declines to do what a file
    /// asked is a store whose backups quietly disagree with themselves.
    pub close_authority_rejected: usize,
}

/// [G8] Does an imported close have to be dropped?
///
/// The whole test, and it takes ONE argument for a reason. Three gate passes
/// each answered a forged import by consulting one more field of the file —
/// first `close_authority`, then the interval shape, then `close_reason` —
/// and each time the next forger simply wrote that field too. Every one of
/// them is written by whoever wrote the file, so none of them is evidence.
/// The relation is the only thing here the store decides for itself, through
/// its own registry.
///
/// So: a fact on a SAFETY relation lands OPEN, whatever the file says about
/// who closed it, why, or over what interval. Non-safety relations are
/// untouched — "Bruno moved out of Lisbon" still imports closed, whoever
/// decided it.
///
/// The direction of the error is the whole argument. Over-reporting an
/// allergy or a prescription is survivable and visible: the reopened claim
/// sits beside the real one and is already queued for adjudication. Under-
/// reporting one is a medical incident. A store that imports "she stopped
/// taking it" as "she is still taking it" is wrong in the direction a human
/// notices and corrects; the reverse is wrong in the direction that kills
/// someone. Fidelity loses to safety at a trust boundary.
pub(crate) fn import_close_is_refused(predicate: &str) -> bool {
    engram_core::relations::is_safety_slot(&engram_core::relations::canonical_relation(predicate))
}

/// The five row sections of an export, travelling together because they always
/// do. Their insertion order is a dependency order — kb, document, memory,
/// chunk, fact — and a signature that takes them one by one is a signature a
/// caller can get wrong.
struct RowSections<'a> {
    kbs: &'a [KbExportRow],
    documents: &'a [DocumentExportRow],
    memories: &'a [MemoryExportRow],
    chunks: &'a [ChunkExportRow],
    facts: &'a [FactExportRow],
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
    /// Facts appear in BOTH sections and the precedence is now absolute: any
    /// fact the JOURNAL describes is built from the journal, and its row is
    /// skipped. A `FactExportRow` carries no `embedding` — the reason the rows
    /// were once said to be here — so for a v3 file it duplicates
    /// `fact.observed` and nothing more.
    ///
    /// Landing both, rows first, under `INSERT OR IGNORE` meant the ROW won
    /// every column and the event became a no-op. That is how a forged row
    /// shadowed the authentic open observation sitting right beside it in the
    /// same file, and it also broke invariant 3 in a quieter way: a store
    /// imported that way disagreed with its own journal until the next
    /// `admin rebuild` silently changed the facts back. Rows still land for
    /// ids no event mentions — a truncated journal is not a reason to drop
    /// data — and still land under `OR IGNORE`, since without it the store's
    /// own pre-existing facts would abort the import on `facts.id`.
    pub fn import_v3(&self, doc: &ExportV3) -> Result<ImportV3Report, StorageError> {
        if doc.schema_version > CURRENT_VERSION {
            return Err(StorageError::Migration(format!(
                "export was written by schema version {} and this binary supports {}; \
                 upgrade engram instead of downgrading the file",
                doc.schema_version, CURRENT_VERSION
            )));
        }
        let journalled: std::collections::HashSet<String> = doc
            .events
            .iter()
            .filter(|e| e.kind == engram_core::events::EventKind::FactObserved.as_str())
            .filter_map(|e| e.payload.get("fact_id").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .collect();
        self.with_tx(|s| {
            let mut report = ImportV3Report::default();
            s.import_v2_sections(
                RowSections {
                    kbs: &doc.kbs,
                    documents: &doc.documents,
                    memories: &doc.memories,
                    chunks: &doc.chunks,
                    facts: &doc.facts,
                },
                &journalled,
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
                // [G9] Stamped `Imported` HERE, by the code path doing the
                // importing. `ExportEvent` has no origin field for the file to
                // set, so no amount of editing makes a foreign event claim to
                // be this store's own — which is what makes the refusal below
                // survive every later `admin rebuild` instead of lasting until
                // the next one.
                let draft = crate::journal::EventDraft::new(kind, &e.actor, e.payload.clone())
                    .scope(e.kb.as_deref(), e.diary.as_deref())
                    .origin(crate::journal::Origin::Imported);
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
                    Ok(applied) => {
                        report.close_authority_rejected += applied.closes_refused;
                        // The journal builds the facts now, so this is where
                        // `imported.facts` comes from for a v3 file.
                        report.facts += applied.facts_inserted;
                    }
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
    ///
    /// [G8] The rows land FIRST and the generator then reads them back, so a
    /// safety close the file asked for has already been dropped by the time
    /// genesis sees it: no `fact.interval_closed` is synthesized for a fact
    /// this store decided to hold open. That ordering is what stops a v2 file
    /// laundering a forged close into trusted history — the previous version
    /// exempted the row, synthesized a close event from it, and the store then
    /// held the forgery as something it had witnessed itself.
    ///
    /// [G9] Those synthesized events are stamped `Imported` all the same. They
    /// describe rows that came out of a file; a rebuild re-gating them costs
    /// nothing, because they already say what the gate would make them say.
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
            // A v2 file carries no journal, so no row is described by one.
            let journalled = std::collections::HashSet::new();
            s.import_v2_sections(
                RowSections {
                    kbs,
                    documents,
                    memories,
                    chunks,
                    facts,
                },
                &journalled,
                &mut report,
            )?;
            let counts = s.synthesize_genesis(actor, crate::journal::Origin::Imported)?;
            report.genesis_synthesized = counts.events;
            report.events = counts.events;
            Ok(report)
        })
    }

    /// `journalled` names the fact ids the caller's event slice will observe;
    /// those rows are skipped so the journal stays the single source of truth
    /// for them. A v2 file has no journal, so it passes an empty set and every
    /// row lands.
    fn import_v2_sections(
        &self,
        rows: RowSections<'_>,
        journalled: &std::collections::HashSet<String>,
        report: &mut ImportV3Report,
    ) -> Result<(), StorageError> {
        for row in rows.kbs {
            report.kbs += self.import_kb_row(row)? as usize;
        }
        for row in rows.documents {
            report.documents += self.import_document_row(row)? as usize;
        }
        for row in rows.memories {
            report.memories += self.import_memory_row(row)? as usize;
        }
        for row in rows.chunks {
            report.chunks += self.import_chunk_row(row)? as usize;
        }
        for row in rows.facts {
            if journalled.contains(&row.id) {
                continue;
            }
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

    /// What the planner writes for "Ana stopped taking metformin" when there
    /// is nothing open to close: a fact BORN closed at a single instant,
    /// reason `stopped`, authority `heuristic`, and the pair queued for a
    /// person. The write path still produces this shape; [G8] is about what
    /// an IMPORT is allowed to conclude from a file wearing it.
    fn seed_extracted_cessation(store: &SqliteStore) -> (Uuid, Uuid) {
        let m = Memory {
            id: Uuid::new_v4(),
            content: "Ana stopped taking metformin.".into(),
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
        let at = chrono::Utc::now();
        let f = Fact {
            id: Uuid::new_v4(),
            source_memory_id: m.id,
            subject: "Ana".into(),
            subject_norm: "ana".into(),
            predicate: "takes_medication".into(),
            object: "metformin".into(),
            object_norm: "metformin".into(),
            confidence: 0.9,
            created_at: at,
            superseded_by: None,
            superseded_at: None,
            diary: "default".into(),
            fact_type: FactType::Assertion,
            valid_from: Some(at),
            valid_until: None,
            resolution: None,
            source_kind: SourceKind::AgentExtracted,
            confirmations: 0,
            closed_by: None,
            close_reason: None,
            close_authority: None,
            kb: "personal".into(),
        };
        store.insert_fact(&f).unwrap();
        store
            .close_fact_interval(f.id, at, "stopped", "heuristic", None, Some("cessation"))
            .unwrap();
        (m.id, f.id)
    }

    fn facts_digest(store: &SqliteStore) -> Vec<String> {
        let mut rows: Vec<String> = store
            .list_facts(None, true, 100)
            .unwrap()
            .iter()
            .map(|f| {
                format!(
                    "{}|{}|{}|{:?}|{:?}|{:?}|{}|{:?}|{:?}",
                    f.id,
                    f.object_norm,
                    f.kb,
                    f.valid_until,
                    f.close_reason,
                    f.superseded_by,
                    f.fact_type.as_str(),
                    f.close_authority,
                    // [V5] Which rule or judge closed it travels in the close
                    // event now, so a round trip must bring it back.
                    f.resolution
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

    /// [G8] The close that used to be the exception, now the rule. `caller` is
    /// the strongest authority a close can carry — and it is still a string in
    /// a file that anyone can type. The import drops it like any other, and
    /// the store says so.
    ///
    /// This inverts a v2.2 test on purpose. The cost is real: a legitimately
    /// retired prescription comes back OPEN in the restored store. That is the
    /// trade the ruling makes explicitly, and it only bites on the direction
    /// that a human notices and corrects.
    #[test]
    fn even_an_imported_caller_close_on_a_safety_relation_is_dropped() {
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
        assert_eq!(
            report.close_authority_rejected, 1,
            "refusing quietly is what the standing advisory was about"
        );
        let f = target.get_fact(allergy).unwrap().unwrap();
        assert!(engram_core::interval::is_open_fact(&f));
        assert_eq!(f.valid_until, None);
        assert_eq!(f.close_authority, None);
    }

    /// A non-safety relation is untouched by all of this, which is the other
    /// half of the ruling: only three relations pay the fidelity price.
    #[test]
    fn an_imported_close_on_an_ordinary_relation_is_honoured() {
        let (_tmp, _g, source) = temp_store();
        let (_m, lisbon, _porto) = seed(&source);
        source
            .close_fact_interval(
                lisbon,
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
        let f = target.get_fact(lisbon).unwrap().unwrap();
        assert!(f.valid_until.is_some(), "Bruno really did move out");
        assert_eq!(f.close_authority.as_deref(), Some("caller"));
    }

    /// [V5] `resolution` is an audit tag, never authority. A file whose close
    /// event claims `resolution: "caller"` over `authority: "heuristic"`
    /// lands the tag and keeps the machine authority — on import and again
    /// on every rebuild — because nothing on the replay path maps the tag to
    /// an authority.
    #[test]
    fn a_forged_caller_resolution_does_not_promote_a_heuristic_close() {
        let (_tmp, _g, source) = temp_store();
        let (_m, _porto, lisbon) = seed(&source);
        let mut doc = source.export_v3(None).unwrap();
        for e in doc.events.iter_mut() {
            if e.kind == "fact.interval_closed" {
                e.payload["authority"] = serde_json::json!("heuristic");
                e.payload["resolution"] = serde_json::json!("caller");
            }
        }

        let target = extra_store(&_tmp, "target");
        target.import_v3(&doc).unwrap();
        target.rebuild_projections(None, false).unwrap();
        let f = target.get_fact(lisbon).unwrap().unwrap();
        assert!(f.valid_until.is_some(), "an ordinary close still lands");
        assert_eq!(f.close_authority.as_deref(), Some("heuristic"));
        assert_eq!(f.resolution.as_deref(), Some("caller"));
    }

    /// [V5] The "would this move anything?" exemption compares `resolution`
    /// too. A store holds its own closed allergy; an imported copy of that
    /// close with a fresh event id moves nothing and is not counted, but the
    /// same copy re-tagged with another judge would rewrite the safety fact's
    /// audit trail, so the gate refuses it.
    #[test]
    fn an_imported_close_cannot_retag_a_closed_safety_fact() {
        let (_tmp, _g, store) = temp_store();
        let (_m, allergy) = seed_allergy(&store);
        store
            .close_fact_interval(
                allergy,
                chrono::Utc::now(),
                "corrected",
                "caller",
                None,
                Some("caller"),
            )
            .unwrap();
        let close = store
            .export_v3(None)
            .unwrap()
            .events
            .into_iter()
            .find(|e| e.kind == "fact.interval_closed")
            .unwrap();
        let replay = |id: &str, resolution: &str| {
            let mut e = close.clone();
            e.id = id.into();
            e.payload["resolution"] = serde_json::json!(resolution);
            let mut doc = store.export_v3(None).unwrap();
            doc.events = vec![e];
            store.import_v3(&doc).unwrap().close_authority_rejected
        };

        assert_eq!(replay("aaaaaaaa-bbbb-4ccc-8ddd-000000000001", "caller"), 0);
        assert_eq!(
            replay("aaaaaaaa-bbbb-4ccc-8ddd-000000000002", "semantic_llm"),
            1
        );
        let f = store.get_fact(allergy).unwrap().unwrap();
        assert_eq!(f.resolution.as_deref(), Some("caller"));
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

    /// [G8] The accepted cost, pinned so nobody re-derives the exemption from
    /// first principles and reopens the hole.
    ///
    /// An extracted cessation is born closed, so importing it OPEN does invent
    /// a claim: "Ana stopped metformin" restores as a metformin prescription
    /// that reads active. The exemption existed to prevent exactly that — and
    /// `reason: "stopped"` turned out to be one more string a forger types, so
    /// the exemption was a door with the key taped to it.
    ///
    /// What is left is noise, not danger: the reopened cessation sits beside
    /// the real claim as a duplicate, already queued for adjudication, and
    /// the count below is the store telling the caller to go look.
    #[test]
    fn an_extracted_cessation_reopens_on_import_and_is_counted() {
        let (_tmp, _g, source) = temp_store();
        let (_m, stopped) = seed_extracted_cessation(&source);
        let doc = source.export_v3(None).unwrap();

        let target = extra_store(&_tmp, "target");
        let report = target.import_v3(&doc).unwrap();
        assert_eq!(
            report.close_authority_rejected, 1,
            "the caller has to be told the store did less than the file asked"
        );
        let f = target.get_fact(stopped).unwrap().unwrap();
        assert!(engram_core::interval::is_open_fact(&f));
        assert_eq!(f.valid_until, None);
        assert_eq!(f.close_reason, None);
        assert_eq!(f.close_authority, None);
    }

    /// The second gate's blocker, kept because it is still the cheapest
    /// statement of the rule: a forger could close ANY live safety fact by
    /// setting `valid_until` to that fact's own `valid_from`, and every guard
    /// that read the interval's shape saw a cessation. Nothing reads the shape
    /// now, so the attack has nothing left to imitate.
    #[test]
    fn a_forged_point_close_event_cannot_retire_a_live_safety_fact() {
        let (_tmp, _g, source) = temp_store();
        let (_m, allergy) = seed_allergy(&source);
        let mut doc = source.export_v3(None).unwrap();
        // The row section is left honest — the allergy is OPEN in it — so
        // this test cannot pass on the row gate.
        let from = doc
            .facts
            .iter()
            .find(|f| f.predicate == "allergic_to")
            .and_then(|f| f.valid_from.clone())
            .expect("the allergy has a start");
        doc.events = vec![ExportEvent {
            seq: 1,
            id: "11111111-2222-4333-8444-555555555555".into(),
            ts: "2026-08-02T04:00:00Z".into(),
            actor: "forged-import".into(),
            kind: "fact.interval_closed".into(),
            subject_id: Some(allergy.to_string()),
            kb: Some("personal".into()),
            diary: Some("default".into()),
            payload: serde_json::json!({
                "fact_id": allergy.to_string(),
                "valid_until": from,
                "reason": "superseded",
                "authority": "heuristic",
                "sets_superseded_by": false,
            }),
        }];
        doc.counts.events = 1;

        let target = extra_store(&_tmp, "target");
        let report = target.import_v3(&doc).unwrap();
        assert_eq!(report.close_authority_rejected, 1);
        let f = target.get_fact(allergy).unwrap().unwrap();
        assert!(
            engram_core::interval::is_open_fact(&f),
            "she is still allergic to penicillin"
        );
        assert_eq!(f.valid_until, None);
        assert_eq!(f.close_authority, None);
        assert_eq!(f.close_reason, None);
    }

    /// The row half of the same forgery. `superseded` says a claim that was
    /// STANDING got overruled, which is precisely the case where re-opening
    /// is the cautious answer — the shape it wears does not change that.
    #[test]
    fn a_forged_point_close_row_that_is_not_a_cessation_is_dropped() {
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
        row.valid_until = row.valid_from.clone();
        row.close_reason = Some("superseded".to_string());
        row.close_authority = Some("heuristic".to_string());

        let target = extra_store(&_tmp, "target");
        let report = target.import_v3(&doc).unwrap();
        assert_eq!(report.close_authority_rejected, 1);
        let f = target.get_fact(allergy).unwrap().unwrap();
        assert!(engram_core::interval::is_open_fact(&f));
        assert_eq!(f.valid_until, None);
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

    // -----------------------------------------------------------------
    // [G9] Events remember where they came from. Without this, refusing a
    // close at import time is theatre: the refused event stays in the journal
    // and the next `admin rebuild` applies it as the store's own history.
    // -----------------------------------------------------------------

    /// The third gate's blocker. Keep the honest open row and its authentic
    /// `fact.observed`, append ONE attacker-controlled close, import (which
    /// refuses it), then rebuild. The same bytes must be refused a second
    /// time.
    #[test]
    fn a_refused_imported_close_is_refused_again_by_admin_rebuild() {
        let (_tmp, _g, source) = temp_store();
        let (_m, allergy) = seed_allergy(&source);
        let mut doc = source.export_v3(None).unwrap();
        let from = doc
            .facts
            .iter()
            .find(|f| f.predicate == "allergic_to")
            .and_then(|f| f.valid_from.clone())
            .expect("the allergy has a start");
        doc.events.push(ExportEvent {
            seq: doc.events.len() as i64 + 1,
            id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into(),
            ts: "2026-08-02T06:00:00Z".into(),
            actor: "forged-import".into(),
            kind: "fact.interval_closed".into(),
            subject_id: Some(allergy.to_string()),
            kb: Some("personal".into()),
            diary: Some("default".into()),
            payload: serde_json::json!({
                "fact_id": allergy.to_string(),
                "valid_until": from,
                "reason": "superseded",
                "authority": "heuristic",
                "sets_superseded_by": false,
            }),
        });
        doc.counts.events = doc.events.len();

        let target = extra_store(&_tmp, "target");
        let report = target.import_v3(&doc).unwrap();
        assert_eq!(report.close_authority_rejected, 1);
        assert!(engram_core::interval::is_open_fact(
            &target.get_fact(allergy).unwrap().unwrap()
        ));

        // The event IS in the journal — the store records what it was handed —
        // and it is stamped `imported`, so the replay reaches the same verdict.
        let rebuilt = target.rebuild_projections(None, false).unwrap();
        assert_eq!(
            rebuilt.close_authority_rejected, 1,
            "a rebuild that stayed silent here would look exactly like one \
             that had honoured the forgery"
        );
        let f = target.get_fact(allergy).unwrap().unwrap();
        assert!(
            engram_core::interval::is_open_fact(&f),
            "she is still allergic to penicillin, after the rebuild too"
        );
        assert_eq!(f.close_authority, None);
    }

    /// The legitimate path, which G9 must not break: a store's OWN cessation,
    /// written through the gated write path, survives `admin rebuild` closed.
    /// If this fails, the gate is no longer distinguishing origin — it is just
    /// refusing everything.
    #[test]
    fn a_stores_own_cessation_survives_admin_rebuild_closed() {
        let (_tmp, _g, store) = temp_store();
        let (_m, stopped) = seed_extracted_cessation(&store);
        let before = store.get_fact(stopped).unwrap().unwrap();
        assert!(!engram_core::interval::is_open_fact(&before));

        let report = store.rebuild_projections(None, false).unwrap();
        assert_eq!(
            report.close_authority_rejected, 0,
            "nothing here came from a file"
        );
        let f = store.get_fact(stopped).unwrap().unwrap();
        assert!(
            !engram_core::interval::is_open_fact(&f),
            "a rebuild replays this store's history, it does not re-litigate it"
        );
        assert_eq!(f.close_reason.as_deref(), Some("stopped"));
        assert_eq!(f.close_authority.as_deref(), Some("heuristic"));
        assert_eq!(f.valid_until, before.valid_until);
    }

    /// An export carries no origin field, so a file cannot claim to be the
    /// store's own history. This is the property that makes G8 stick.
    #[test]
    fn an_export_file_cannot_declare_an_event_trusted() {
        let (_tmp, _g, source) = temp_store();
        seed_allergy(&source);
        let doc = source.export_v3(None).unwrap();
        let json = serde_json::to_value(&doc).unwrap();
        for e in json["events"].as_array().expect("events travel") {
            assert!(
                e.get("origin").is_none(),
                "an origin the file can set is an origin the forger sets: {e}"
            );
        }

        let target = extra_store(&_tmp, "target");
        target.import_v3(&doc).unwrap();
        let imported: i64 = target
            .conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE origin = 'own'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(imported, 0, "every imported event is stamped `imported`");
    }

    /// The row/event ordering trap the gate documented: rows land before
    /// events and both used `INSERT OR IGNORE`, so a forged row silently won
    /// every column and the authentic observation beside it became a no-op.
    /// The journal is authoritative now, so the forged row never lands at all.
    #[test]
    fn a_forged_fact_row_cannot_shadow_its_own_authentic_event() {
        let (_tmp, _g, source) = temp_store();
        let (_m, allergy) = seed_allergy(&source);
        let mut doc = source.export_v3(None).unwrap();
        // Edit ONLY the redundant row, leaving the honest open `fact.observed`
        // event untouched — and forge the OBJECT as well as the close, so the
        // test fails if the row wins on any column rather than just this one.
        let row = doc
            .facts
            .iter_mut()
            .find(|f| f.predicate == "allergic_to")
            .expect("the export carries the allergy");
        row.valid_until = row.valid_from.clone();
        row.close_reason = Some("stopped".to_string());
        row.close_authority = Some("heuristic".to_string());
        row.object = "aspirin".to_string();
        row.object_norm = "aspirin".to_string();

        let target = extra_store(&_tmp, "target");
        target.import_v3(&doc).unwrap();
        let f = target.get_fact(allergy).unwrap().unwrap();
        assert!(engram_core::interval::is_open_fact(&f));
        assert_eq!(
            f.object, "penicillin",
            "the journal said penicillin; the row section does not get a vote"
        );

        // And the store agrees with its own journal, which is the quieter half
        // of the same bug: an import whose projections a rebuild would change.
        let before = facts_digest(&target);
        target.rebuild_projections(None, false).unwrap();
        assert_eq!(facts_digest(&target), before);
    }

    /// A fact row the journal does NOT describe still lands — a truncated
    /// journal is not a reason to drop data — and still meets the [G8] gate.
    #[test]
    fn a_fact_row_with_no_event_still_imports_and_is_still_gated() {
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
        row.valid_until = Some("2099-01-01T00:00:00Z".to_string());
        row.close_reason = Some("stopped".to_string());
        row.close_authority = Some("caller".to_string());

        let target = extra_store(&_tmp, "target");
        let report = target.import_v3(&doc).unwrap();
        assert_eq!(report.facts, 1, "the orphan row is not dropped");
        assert_eq!(report.close_authority_rejected, 1);
        assert!(engram_core::interval::is_open_fact(
            &target.get_fact(allergy).unwrap().unwrap()
        ));
    }

    /// [G8] The v2 path. A forged row used to be exempted, then
    /// `synthesize_genesis` turned it into journal history the store believed
    /// it had witnessed. Rows land first, so the gate runs before genesis sees
    /// anything — and no close event is synthesized for a fact held open.
    #[test]
    fn a_forged_v2_cessation_row_synthesizes_no_close_event() {
        let (_tmp, _g, source) = temp_store();
        let (_m, allergy) = seed_allergy(&source);
        let mut facts = source.export_facts(None).unwrap();
        let row = facts
            .iter_mut()
            .find(|f| f.predicate == "allergic_to")
            .unwrap();
        row.valid_until = row.valid_from.clone();
        row.close_reason = Some("stopped".to_string());
        row.close_authority = Some("heuristic".to_string());

        let target = extra_store(&_tmp, "target");
        let report = target
            .import_v2_with_genesis(
                &source.export_kbs(None).unwrap(),
                &source.export_documents(None).unwrap(),
                &source.export_memories(None).unwrap(),
                &source.export_chunks(None).unwrap(),
                &facts,
                "import:v2",
            )
            .unwrap();
        assert_eq!(report.close_authority_rejected, 1);

        let synthesized: i64 = target
            .conn
            .query_row(
                "SELECT COUNT(*) FROM events
                  WHERE kind = 'fact.interval_closed'
                    AND json_extract(payload, '$.fact_id') = ?1",
                rusqlite::params![allergy.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            synthesized, 0,
            "genesis reads the rows the gate already opened"
        );
        assert!(engram_core::interval::is_open_fact(
            &target.get_fact(allergy).unwrap().unwrap()
        ));

        target.rebuild_projections(None, false).unwrap();
        assert!(
            engram_core::interval::is_open_fact(&target.get_fact(allergy).unwrap().unwrap()),
            "and the rebuild cannot resurrect a close nobody journaled"
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

#[cfg(test)]
mod import_close_rule_tests {
    use super::*;

    /// [G8] The rule takes one argument, and these are the tests that keep it
    /// that way. Three gate passes each answered a forgery by consulting one
    /// more field of the file; the fix was to consult none of them.
    #[test]
    fn every_safety_relation_lands_open_whatever_the_file_says() {
        for relation in ["takes_medication", "allergic_to", "diagnosed_with"] {
            assert!(
                import_close_is_refused(relation),
                "`{relation}` is a safety slot, so an imported close never lands"
            );
        }
    }

    /// The three fields that used to be evidence are not arguments any more,
    /// which is the point — this test exists to make deleting them from the
    /// signature a deliberate act rather than an accident someone reverts.
    #[test]
    fn an_ordinary_relation_still_imports_closed() {
        for relation in ["lives_in", "works_at", "owns"] {
            assert!(
                !import_close_is_refused(relation),
                "`{relation}` is not a safety slot; Bruno really did move out \
                 of Lisbon"
            );
        }
    }

    /// The registry decides what a safety slot is, and it canonicalises
    /// aliases first, so a file cannot dodge the rule by spelling the relation
    /// the other way.
    #[test]
    fn an_alias_of_a_safety_relation_is_still_a_safety_relation() {
        let canonical = engram_core::relations::canonical_relation("takes_medication");
        assert!(engram_core::relations::is_safety_slot(&canonical));
        assert!(import_close_is_refused("Takes_Medication"));
    }
}

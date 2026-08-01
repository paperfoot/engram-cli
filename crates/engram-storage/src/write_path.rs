//! Journaled writes over the fact and derivation projections.
//!
//! Everything here obeys one rule (D7): **storage appends every event for the
//! projection it owns.** No CLI lane appends `memory.*`, `fact.*`,
//! `derivation.*` or `decision.*` itself — it calls one of these methods and
//! reads back what it did. Two lanes both appending for one operation is not a
//! style problem: it writes the event twice, with two different payloads, and
//! `admin rebuild` then replays the close twice.
//!
//! These live beside `sqlite.rs` rather than inside it because `sqlite.rs` is
//! already 6.6k lines; they are `impl SqliteStore` blocks either way.

use crate::error::StorageError;
use crate::journal::{cli_actor, payload, EventDraft};
use crate::sqlite::{fact_columns, SqliteStore};
use chrono::{DateTime, Utc};
use engram_core::events::EventKind;
use engram_core::interval::{CloseAuthority, CloseReason};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

/// What `soft_delete_memory` actually did. A bare `bool` could not say which
/// intervals it closed, and `forget`'s response has to name them.
#[derive(Debug, Clone, Default)]
pub struct RetractOutcome {
    pub retracted: bool,
    pub closed: Vec<Uuid>,
}

/// One row of the derivation ledger — extraction as a memoized pure function
/// of (content, extractor version).
#[derive(Debug, Clone)]
pub struct DerivationRow {
    pub content_hash: String,
    pub extractor_id: String,
    pub memory_id: Uuid,
    pub derived_at: DateTime<Utc>,
    pub model: Option<String>,
    pub fact_count: i64,
    pub output_fact_ids: Vec<Uuid>,
    pub event_id: Uuid,
}

/// The `resolution` audit tag a caller already passes, mapped to the authority
/// vocabulary the safety audit reads. `CloseAuthority::as_str()` is the only
/// source of these strings, so the column has exactly one vocabulary.
pub(crate) fn close_authority_for(resolution: &str) -> &'static str {
    match resolution {
        "caller" => CloseAuthority::Caller {
            actor: String::new(),
        }
        .as_str(),
        "replayed_caller" => CloseAuthority::ReplayedCaller {
            decided_at: DateTime::UNIX_EPOCH,
        }
        .as_str(),
        "explicit_end" => CloseAuthority::ExplicitEnd.as_str(),
        "successor" | "slot_lww" => CloseAuthority::SuccessorInterval.as_str(),
        // exact_match, duplicate, semantic_llm, recency_*, triage_*, compile_*
        // and anything unrecognised: a machine decided, and the audit must be
        // able to see that.
        _ => CloseAuthority::Heuristic { rule: "" }.as_str(),
    }
}

impl SqliteStore {
    /// Close one interval and append its `fact.interval_closed`.
    ///
    /// `superseded_by`/`superseded_at` are written whenever a SUCCESSOR FACT
    /// closed the interval — i.e. whenever `closed_by` names a fact — and left
    /// NULL when nothing succeeded it (`stopped`, `retracted_source`, an
    /// explicit end date). The trigger is the presence of a successor, NOT
    /// `reason == "superseded"`: a caller correction closes with `corrected`
    /// and still has a successor, and keying the compat write off the reason
    /// would drop those rows out of `list_recent_conflicts` (which JOINs on
    /// `superseded_by`) and out of every unswept legacy reader.
    ///
    /// It does NOT consult `may_close`. That gate belongs to the planner, one
    /// layer up: a storage method that silently refuses a write makes the plan
    /// and the store disagree, which is the failure `plan_write` exists to
    /// prevent. Storage records what it is told, and records who told it.
    pub fn close_fact_interval(
        &self,
        fact_id: Uuid,
        at: DateTime<Utc>,
        reason: &str,
        authority: &str,
        closed_by: Option<Uuid>,
        resolution: Option<&str>,
    ) -> Result<bool, StorageError> {
        self.with_tx(|s| {
            let existing: Option<(String, String, Option<String>)> = s
                .conn
                .query_row(
                    "SELECT kb, diary, valid_from FROM facts WHERE id = ?1
                      AND valid_until IS NULL AND superseded_by IS NULL",
                    params![fact_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((kb, diary, valid_from)) = existing else {
                // An interval closes once. An already-closed one is not an
                // error — the caller races, or asks twice — but it is not a
                // second close either.
                return Ok(false);
            };
            // The same clamp `engram_core::interval::close_at` applies, and the
            // same one the migration applies in SQL: a close can never precede
            // its own valid_from, because a negative-length interval is
            // nonsense to every consumer that reads it.
            let clamped = match valid_from
                .as_deref()
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
            {
                Some(from) => at.max(from.with_timezone(&Utc)),
                None => at,
            };
            let sets_superseded_by = closed_by.is_some();
            let n = s.conn.execute(
                "UPDATE facts
                    SET valid_until = ?1, close_reason = ?2, close_authority = ?3,
                        closed_by = ?4,
                        superseded_by = CASE WHEN ?5 THEN ?4 ELSE superseded_by END,
                        superseded_at = CASE WHEN ?5 THEN ?1 ELSE superseded_at END,
                        resolution = COALESCE(?6, resolution)
                  WHERE id = ?7 AND valid_until IS NULL AND superseded_by IS NULL",
                params![
                    clamped.to_rfc3339(),
                    reason,
                    authority,
                    closed_by.map(|u| u.to_string()),
                    sets_superseded_by,
                    resolution,
                    fact_id.to_string(),
                ],
            )?;
            if n == 0 {
                return Ok(false);
            }
            s.append_event(
                &EventDraft::new(
                    EventKind::FactIntervalClosed,
                    cli_actor(&diary),
                    payload::fact_interval_closed(
                        fact_id,
                        clamped,
                        closed_by,
                        reason,
                        authority,
                        sets_superseded_by,
                        sets_superseded_by.then(|| clamped.to_rfc3339()).as_deref(),
                        false,
                    ),
                )
                .scope(Some(&kb), Some(&diary))
                .subject(fact_id),
            )?;
            Ok(true)
        })
    }

    /// Close every OPEN interval derived from `memory_id`, one
    /// `fact.interval_closed` each, in the caller's transaction; returns the
    /// ids closed.
    ///
    /// Does NOT touch `fact_extraction_log`: `edit` needs to close a memory's
    /// facts and leave the marker alone (so the re-extraction it just made
    /// necessary stays pending), and welding the two together is what made
    /// `reextraction: "replayed"` impossible to report honestly. Must NOT
    /// filter on `memories.deleted = 0` — `forget` calls this AFTER the soft
    /// delete, inside the same transaction.
    pub fn close_facts_for_memory(
        &self,
        memory_id: Uuid,
        at: DateTime<Utc>,
        reason: CloseReason,
        authority: &CloseAuthority,
        _actor: &str,
    ) -> Result<Vec<Uuid>, StorageError> {
        self.with_tx(|s| {
            let ids: Vec<Uuid> = {
                let mut stmt = s.conn.prepare(
                    "SELECT id FROM facts
                      WHERE source_memory_id = ?1
                        AND valid_until IS NULL AND superseded_by IS NULL",
                )?;
                let rows =
                    stmt.query_map(params![memory_id.to_string()], |r| r.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .filter_map(|v| Uuid::parse_str(v).ok())
                    .collect()
            };
            let mut closed = Vec::with_capacity(ids.len());
            for id in ids {
                if s.close_fact_interval(id, at, reason.as_str(), authority.as_str(), None, None)? {
                    closed.push(id);
                }
            }
            Ok(closed)
        })
    }

    /// Separate from the close, so `edit` can close a memory's facts WITHOUT
    /// clearing the marker. Returns whether a marker was there to clear.
    pub fn clear_extraction_marker(&self, memory_id: Uuid) -> Result<bool, StorageError> {
        let n = self.conn.execute(
            "DELETE FROM fact_extraction_log WHERE memory_id = ?1",
            params![memory_id.to_string()],
        )?;
        Ok(n > 0)
    }

    /// The old delete body, kept for `hard_delete_memory` ONLY — ingest
    /// rollback for a memory that never became visible. Every other caller
    /// wants `close_facts_for_memory`; the rename exists precisely so a
    /// rollback cannot silently turn into a pile of closed intervals belonging
    /// to a memory that no longer exists.
    pub fn purge_facts_for_memory(&self, memory_id: Uuid) -> Result<usize, StorageError> {
        let id = memory_id.to_string();
        let n = self
            .conn
            .execute("DELETE FROM facts WHERE source_memory_id = ?1", params![id])?;
        self.conn.execute(
            "DELETE FROM fact_extraction_log WHERE memory_id = ?1",
            params![id],
        )?;
        Ok(n)
    }

    /// `(kb, diary, deleted)` for a memory, INCLUDING a deleted one.
    /// `memory_scope` filters deleted rows out, which is why `forget` and
    /// `edit` could only say "no memory found (already deleted?)" — a guess
    /// where the store knew the answer.
    pub fn memory_scope_including_deleted(
        &self,
        id: Uuid,
    ) -> Result<Option<(String, String, bool)>, StorageError> {
        self.conn
            .query_row(
                "SELECT kb, diary, deleted FROM memories WHERE id = ?1",
                params![id.to_string()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Upsert the content-addressed derivation row, write the legacy
    /// memory-keyed marker beside it, and append `derivation.ran` — one
    /// transaction.
    ///
    /// Two markers coexist on purpose for now: `fact_extraction_log` still
    /// drives the backfill work queue, while `derivations` is what makes
    /// re-derivation memoized. Collapsing them is a read/daemon surface change.
    pub fn record_derivation(
        &self,
        memory_id: Uuid,
        content_hash: &str,
        extractor_id: &str,
        model: Option<&str>,
        output_fact_ids: &[Uuid],
        actor: &str,
    ) -> Result<(), StorageError> {
        self.with_tx(|s| {
            let scope: Option<(String, String)> = s
                .conn
                .query_row(
                    "SELECT kb, diary FROM memories WHERE id = ?1",
                    params![memory_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (kb, diary) = scope.unwrap_or_else(|| ("default".into(), "default".into()));
            let (_, event_id) = s.append_event(
                &EventDraft::new(
                    EventKind::DerivationRan,
                    actor,
                    payload::derivation_ran(
                        memory_id,
                        content_hash,
                        extractor_id,
                        model,
                        output_fact_ids,
                        false,
                    ),
                )
                .scope(Some(&kb), Some(&diary))
                .subject(memory_id),
            )?;
            let ids_json = serde_json::to_string(
                &output_fact_ids
                    .iter()
                    .map(|u| u.to_string())
                    .collect::<Vec<_>>(),
            )?;
            let now = Utc::now().to_rfc3339();
            s.conn.execute(
                "INSERT INTO derivations(content_hash, extractor_id, memory_id, derived_at,
                        model, fact_count, output_fact_ids, event_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(content_hash, extractor_id) DO UPDATE SET
                     memory_id = excluded.memory_id,
                     derived_at = excluded.derived_at,
                     model = excluded.model,
                     fact_count = excluded.fact_count,
                     output_fact_ids = excluded.output_fact_ids,
                     event_id = excluded.event_id",
                params![
                    content_hash,
                    extractor_id,
                    memory_id.to_string(),
                    now,
                    model,
                    output_fact_ids.len() as i64,
                    ids_json,
                    event_id.to_string(),
                ],
            )?;
            s.record_fact_extraction(memory_id, model, output_fact_ids.len() as i64, "remember")?;
            Ok(())
        })
    }

    /// "Has this exact content already been derived by this exact extractor
    /// version?" Content-addressed, so an edit that restores the previous text
    /// is a cache HIT and re-extraction is skipped.
    pub fn derivation_for(
        &self,
        content_hash: &str,
        extractor_id: &str,
    ) -> Result<Option<DerivationRow>, StorageError> {
        self.conn
            .query_row(
                "SELECT content_hash, extractor_id, memory_id, derived_at, model,
                        fact_count, output_fact_ids, event_id
                   FROM derivations WHERE content_hash = ?1 AND extractor_id = ?2",
                params![content_hash, extractor_id],
                parse_derivation_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn derivations_for_memory(
        &self,
        memory_id: Uuid,
    ) -> Result<Vec<DerivationRow>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT content_hash, extractor_id, memory_id, derived_at, model,
                    fact_count, output_fact_ids, event_id
               FROM derivations WHERE memory_id = ?1 ORDER BY derived_at DESC",
        )?;
        let rows = stmt.query_map(params![memory_id.to_string()], parse_derivation_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Facts on one relation for one subject. `include_closed = false` returns
    /// only intervals that still assert something.
    pub fn facts_for_relation(
        &self,
        kb: Option<&str>,
        diary: Option<&str>,
        subject_norm: &str,
        relation: &str,
        include_closed: bool,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        let open = if include_closed {
            ""
        } else {
            " AND valid_until IS NULL AND superseded_by IS NULL"
        };
        let sql = format!(
            "SELECT {} FROM facts
              WHERE subject_norm = ?1 AND predicate = ?2
                AND (?3 IS NULL OR kb = ?3) AND (?4 IS NULL OR diary = ?4){open}
              ORDER BY created_at DESC",
            fact_columns("")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![subject_norm, relation, kb, diary],
            crate::sqlite::parse_fact_row_pub,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Open intervals for a subject, optionally narrowed to one relation.
    /// `None` for `kb`/`diary` means every one of them — refusing that is
    /// legal and costs an N×M fan-out in `state`; refusing it *silently* is
    /// what produces `WHERE kb = '*'`.
    pub fn open_intervals(
        &self,
        kb: Option<&str>,
        diary: Option<&str>,
        subject_norm: &str,
        relation: Option<&str>,
        include_closed: bool,
    ) -> Result<Vec<engram_core::types::Fact>, StorageError> {
        let open = if include_closed {
            ""
        } else {
            " AND valid_until IS NULL AND superseded_by IS NULL"
        };
        let sql = format!(
            "SELECT {} FROM facts
              WHERE subject_norm = ?1
                AND (?2 IS NULL OR predicate = ?2)
                AND (?3 IS NULL OR kb = ?3) AND (?4 IS NULL OR diary = ?4){open}
              ORDER BY predicate ASC, created_at DESC",
            fact_columns("")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![subject_norm, relation, kb, diary],
            crate::sqlite::parse_fact_row_pub,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }
}

fn parse_derivation_row(r: &rusqlite::Row) -> rusqlite::Result<DerivationRow> {
    let ids_json: String = r.get(6)?;
    let ids: Vec<String> = serde_json::from_str(&ids_json).unwrap_or_default();
    let derived_at: String = r.get(3)?;
    let event_id: String = r.get(7)?;
    let memory_id: String = r.get(2)?;
    Ok(DerivationRow {
        content_hash: r.get(0)?,
        extractor_id: r.get(1)?,
        memory_id: Uuid::parse_str(&memory_id).unwrap_or(Uuid::nil()),
        derived_at: DateTime::parse_from_rfc3339(&derived_at)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(DateTime::UNIX_EPOCH),
        model: r.get(4)?,
        fact_count: r.get(5)?,
        output_fact_ids: ids.iter().filter_map(|s| Uuid::parse_str(s).ok()).collect(),
        event_id: Uuid::parse_str(&event_id).unwrap_or(Uuid::nil()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_store;
    use engram_core::types::{Fact, FactType, Memory, MemorySource, SourceKind};

    fn memory(store: &SqliteStore, content: &str) -> Uuid {
        let m = Memory {
            id: Uuid::new_v4(),
            content: content.to_string(),
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
            tags: vec![],
        };
        store.insert_memory_with_kb(&m, "personal").unwrap();
        m.id
    }

    fn fact(store: &SqliteStore, memory_id: Uuid, predicate: &str, object: &str) -> Uuid {
        let f = Fact {
            id: Uuid::new_v4(),
            source_memory_id: memory_id,
            subject: "Ana".into(),
            subject_norm: "ana".into(),
            predicate: predicate.into(),
            object: object.into(),
            object_norm: object.to_lowercase(),
            confidence: 0.9,
            created_at: Utc::now(),
            superseded_by: None,
            superseded_at: None,
            diary: "default".into(),
            fact_type: FactType::Assertion,
            valid_from: Some(Utc::now()),
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
        f.id
    }

    fn kinds_for(store: &SqliteStore, id: Uuid) -> Vec<String> {
        store
            .events_for_subject(id)
            .unwrap()
            .iter()
            .map(|e| e.kind.as_str().to_string())
            .collect()
    }

    #[test]
    fn insert_fact_appends_fact_observed_with_its_scope() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana takes metformin.");
        let f = fact(&store, m, "takes_medication", "metformin");
        let events = store.events_for_subject(f).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::FactObserved);
        assert_eq!(events[0].kb.as_deref(), Some("personal"));
        assert_eq!(events[0].diary.as_deref(), Some("default"));
        assert_eq!(events[0].actor, "cli:default");
        assert_eq!(
            events[0].payload["fact_type"], "assertion",
            "without fact_type a rebuilt profile_slot comes back an assertion"
        );
    }

    #[test]
    fn close_fact_interval_sets_superseded_by_only_when_superseded() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana lived in Lisbon, then Porto.");
        let stopped = fact(&store, m, "takes_medication", "metformin");
        let old = fact(&store, m, "lives_in", "Lisbon");
        let new = fact(&store, m, "lives_in", "Porto");

        // No successor: valid_until moves, superseded_by stays NULL. The
        // legacy "active fact" predicate would still call this current, which
        // is exactly why the sweep exists.
        assert!(store
            .close_fact_interval(stopped, Utc::now(), "stopped", "explicit_end", None, None)
            .unwrap());
        let row = store.get_fact(stopped).unwrap().unwrap();
        assert!(row.valid_until.is_some());
        assert_eq!(row.superseded_by, None);
        assert_eq!(row.close_reason.as_deref(), Some("stopped"));

        // A successor: the compat pair is written, because every unswept
        // legacy reader and `list_recent_conflicts` still JOIN on it.
        assert!(store
            .close_fact_interval(
                old,
                Utc::now(),
                "corrected",
                "caller",
                Some(new),
                Some("caller")
            )
            .unwrap());
        let row = store.get_fact(old).unwrap().unwrap();
        assert_eq!(row.superseded_by, Some(new));
        assert!(row.superseded_at.is_some());
        assert_eq!(row.close_reason.as_deref(), Some("corrected"));
        assert_eq!(row.close_authority.as_deref(), Some("caller"));

        let ev = store.events_for_subject(old).unwrap();
        let closed = ev.last().unwrap();
        assert_eq!(closed.kind, EventKind::FactIntervalClosed);
        assert!(
            closed.payload_bool("sets_superseded_by"),
            "the compat write must be rebuildable from the event alone"
        );
    }

    #[test]
    fn supersede_fact_with_caller_is_not_recorded_as_heuristic() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana moved.");
        let old = fact(&store, m, "lives_in", "Lisbon");
        let new = fact(&store, m, "lives_in", "Porto");
        assert!(store.supersede_fact_with(old, new, "caller", None).unwrap());
        let row = store.get_fact(old).unwrap().unwrap();
        assert_eq!(
            row.close_authority.as_deref(),
            Some("caller"),
            "a person decided this; the safety audit must not read it as a machine"
        );
        assert_eq!(row.resolution.as_deref(), Some("caller"));

        let old2 = fact(&store, m, "works_at", "Acme");
        let new2 = fact(&store, m, "works_at", "Globex");
        assert!(store
            .supersede_fact_with(old2, new2, "recency_gap", None)
            .unwrap());
        assert_eq!(
            store
                .get_fact(old2)
                .unwrap()
                .unwrap()
                .close_authority
                .as_deref(),
            Some("heuristic")
        );
    }

    #[test]
    fn an_already_closed_interval_cannot_be_superseded_again() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana stopped metformin.");
        let f = fact(&store, m, "takes_medication", "metformin");
        let successor = fact(&store, m, "takes_medication", "empagliflozin");
        assert!(store
            .close_fact_interval(f, Utc::now(), "stopped", "explicit_end", None, None)
            .unwrap());
        assert!(
            !store
                .supersede_fact_with(f, successor, "slot_lww", None)
                .unwrap(),
            "an interval closes once"
        );
        assert_eq!(
            store.get_fact(f).unwrap().unwrap().close_reason.as_deref(),
            Some("stopped"),
            "and the second close must not overwrite the first one's reason"
        );
    }

    #[test]
    fn soft_delete_closes_intervals_instead_of_deleting_facts() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana takes metformin.");
        let f = fact(&store, m, "takes_medication", "metformin");
        let outcome = store.soft_delete_memory(m).unwrap().expect("memory exists");
        assert!(outcome.retracted);
        assert_eq!(outcome.closed, vec![f]);

        let row = store.get_fact(f).unwrap().expect("the fact must survive");
        assert_eq!(row.close_reason.as_deref(), Some("retracted_source"));
        assert!(row.valid_until.is_some());
        assert!(kinds_for(&store, m).contains(&"memory.retracted".to_string()));

        // Second forget of the same memory: nothing to retract, and it is not
        // a "not found" either — the caller can tell the two apart.
        let again = store.soft_delete_memory(m).unwrap().expect("still exists");
        assert!(!again.retracted);
        assert!(store.soft_delete_memory(Uuid::new_v4()).unwrap().is_none());
    }

    #[test]
    fn closing_a_memorys_facts_keeps_its_adjudication_rows() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana lived in Lisbon, then Porto.");
        let old = fact(&store, m, "lives_in", "Lisbon");
        let new = fact(&store, m, "lives_in", "Porto");
        let conflict = Uuid::new_v4();
        store
            .with_tx(|s| {
                s.conn.execute(
                    "INSERT INTO fact_conflict_queue
                        (id, new_fact_id, old_fact_id, diary, kb, detector, status, created_at)
                     VALUES (?1, ?2, ?3, 'default', 'personal', 'slot', 'open', ?4)",
                    rusqlite::params![
                        conflict.to_string(),
                        new.to_string(),
                        old.to_string(),
                        Utc::now().to_rfc3339()
                    ],
                )?;
                Ok(())
            })
            .unwrap();

        // The v0.3 path deleted the facts here, and the FK cascade took the
        // question with them. 14.5k rows died this way.
        store.delete_facts_for_memory(m).unwrap();
        assert!(store.get_fact_conflict(conflict).unwrap().is_some());
        assert!(store.get_fact(old).unwrap().is_some());
        assert!(store.get_fact(new).unwrap().is_some());
    }

    #[test]
    fn a_stopped_interval_is_not_active_anywhere() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana stopped metformin.");
        let f = fact(&store, m, "takes_medication", "metformin");
        assert_eq!(store.count_facts().unwrap(), 1);
        store
            .close_fact_interval(f, Utc::now(), "stopped", "explicit_end", None, None)
            .unwrap();
        assert_eq!(store.count_facts().unwrap(), 0);
        assert!(store
            .get_active_facts("ana", "takes_medication", "default")
            .unwrap()
            .is_empty());
        assert!(store
            .get_active_facts_in_kb("ana", "takes_medication", "default", "personal")
            .unwrap()
            .is_empty());
        assert!(store.list_facts(None, false, 50).unwrap().is_empty());
        assert!(store
            .list_facts_scoped(None, Some("personal"), false, 50, 0)
            .unwrap()
            .is_empty());
        assert!(store
            .list_facts_by_subject("ana", None, false)
            .unwrap()
            .is_empty());
        assert!(store
            .open_intervals(None, None, "ana", None, false)
            .unwrap()
            .is_empty());
        // …but it is still there when history is asked for.
        assert_eq!(store.list_facts(None, true, 50).unwrap().len(), 1);
    }

    #[test]
    fn derivation_is_memoized_by_content_and_extractor() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana takes metformin.");
        let f = fact(&store, m, "takes_medication", "metformin");
        let hash = engram_core::keys::content_hash("Ana takes metformin.");
        store
            .record_derivation(
                m,
                &hash,
                crate::journal::EXTRACTOR_FACT_LLM,
                Some("stub-chat"),
                &[f],
                "cli:default",
            )
            .unwrap();

        let hit = store
            .derivation_for(&hash, crate::journal::EXTRACTOR_FACT_LLM)
            .unwrap()
            .expect("same content, same extractor: a hit");
        assert_eq!(hit.output_fact_ids, vec![f]);
        assert_eq!(hit.memory_id, m);

        // Edited content misses…
        let edited = engram_core::keys::content_hash("Ana takes empagliflozin.");
        assert!(store
            .derivation_for(&edited, crate::journal::EXTRACTOR_FACT_LLM)
            .unwrap()
            .is_none());
        // …and reverting the edit hits again, without paying the LLM twice.
        assert!(store
            .derivation_for(&hash, crate::journal::EXTRACTOR_FACT_LLM)
            .unwrap()
            .is_some());
        // A different extractor version is a different question.
        assert!(store
            .derivation_for(&hash, crate::journal::EXTRACTOR_COMPILE)
            .unwrap()
            .is_none());
        assert!(kinds_for(&store, m).contains(&"derivation.ran".to_string()));
        // The legacy marker is written too, so the backfill queue keeps working.
        assert!(store.clear_extraction_marker(m).unwrap());
    }

    #[test]
    fn an_edit_appends_revised_and_an_importance_bump_appends_meta_changed() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana takes metformin.");
        assert!(store
            .update_memory(m, Some("Ana takes empagliflozin."), None)
            .unwrap());
        assert!(store.update_memory(m, None, Some(9)).unwrap());
        let kinds = kinds_for(&store, m);
        assert_eq!(
            kinds,
            vec!["memory.written", "memory.revised", "memory.meta_changed"],
            "the distinction is what tells a rebuild to clear the extraction marker"
        );
        assert!(!store
            .update_memory(Uuid::new_v4(), Some("x"), None)
            .unwrap());
    }

    #[test]
    fn repair_closes_the_intervals_the_soft_delete_rewrite_preserved() {
        let (_tmp, _g, store) = temp_store();
        let m = memory(&store, "Ana takes metformin.");
        let f = fact(&store, m, "takes_medication", "metformin");
        // A memory deleted the old way: row flagged, facts left open.
        store
            .with_tx(|s| {
                s.conn.execute(
                    "UPDATE memories SET deleted = 1, deleted_at = ?2 WHERE id = ?1",
                    rusqlite::params![m.to_string(), Utc::now().to_rfc3339()],
                )?;
                Ok(())
            })
            .unwrap();
        let report = store.repair_integrity(false).unwrap();
        assert_eq!(report.facts_from_deleted_memories_closed, 1);
        assert!(
            store.get_fact(f).unwrap().is_some(),
            "the next `engram repair` must not undo the soft-delete fix"
        );
        assert_eq!(
            store.get_fact(f).unwrap().unwrap().close_reason.as_deref(),
            Some("retracted_source")
        );
    }
}

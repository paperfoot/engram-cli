//! The decision ledger: what a person decided, keyed so it can be replayed.
//!
//! v0.3 answered a contradiction by mutating two fact rows and setting a queue
//! row to `resolved`. Nothing recorded WHO decided, or what question they were
//! answering, so the next re-derivation of the same pair asked again — and the
//! FK cascade that erased the queue erased the only trace either way. A
//! decision here is a row keyed by `(kb, semantic_key)`, which is what lets a
//! later write find a caller's prior ruling and apply it instead of re-asking.

use crate::error::StorageError;
use crate::journal::{payload, EventDraft};
use crate::sqlite::SqliteStore;
use chrono::{DateTime, Utc};
use engram_core::events::EventKind;
use engram_core::keys::semantic_key_decode;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

/// FROZEN (interfaces §3.4): `engram-graph` implements `DecisionLedger for
/// SqliteStore` against this shape, so a field rename here is a cross-crate
/// break.
#[derive(Debug, Clone)]
pub struct DecisionRow {
    pub id: Uuid,
    pub kb: String,
    pub diary: String,
    pub decision_key: String,
    pub subject_norm: String,
    pub relation: String,
    pub pair_hash: String,
    pub verdict: String,
    /// The `object_norm` of the value this verdict RETIRED [G3]. The semantic
    /// key names the QUESTION and sorts its object pair, so it cannot say which
    /// side lost; without this column a replay onto a re-derived pair falls
    /// back to pair ORDER, and a reversed derivation retires the value the
    /// caller kept. `None` on a `keep_both`/`void` (nothing lost), and on a
    /// pre-v12 row the migration could not orient because its conflict row had
    /// already been deleted — for which `replay` queues rather than guesses.
    pub loser_object_norm: Option<String>,
    pub authority: String,
    pub actor: String,
    pub note: Option<String>,
    pub conflict_id: Option<Uuid>,
    pub event_id: Uuid,
    pub key_version: String,
    pub decided_at: DateTime<Utc>,
}

/// What a queue insert actually did. A bare `bool` conflated "this exact pair
/// is already queued" with "a different pair asking the same question is
/// already queued", and the applier's `deduplicated: true` response is only
/// honest for the second one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    DuplicatePair { existing: Uuid },
    DuplicateKey { existing: Uuid },
}

const DECISION_COLUMNS: &str = "id, kb, diary, decision_key, subject_norm, relation, pair_hash,
     verdict, authority, actor, note, conflict_id, event_id, key_version, decided_at,
     loser_object_norm";

fn parse_decision_row(r: &rusqlite::Row) -> rusqlite::Result<DecisionRow> {
    let id: String = r.get(0)?;
    let conflict_id: Option<String> = r.get(11)?;
    let event_id: String = r.get(12)?;
    let decided_at: String = r.get(14)?;
    Ok(DecisionRow {
        id: Uuid::parse_str(&id).unwrap_or(Uuid::nil()),
        kb: r.get(1)?,
        diary: r.get(2)?,
        decision_key: r.get(3)?,
        subject_norm: r.get(4)?,
        relation: r.get(5)?,
        pair_hash: r.get(6)?,
        verdict: r.get(7)?,
        authority: r.get(8)?,
        actor: r.get(9)?,
        note: r.get(10)?,
        conflict_id: conflict_id.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
        event_id: Uuid::parse_str(&event_id).unwrap_or(Uuid::nil()),
        key_version: r.get(13)?,
        decided_at: DateTime::parse_from_rfc3339(&decided_at)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(DateTime::UNIX_EPOCH),
        // Last on purpose: appended by the v12 ALTER, so it sits after
        // `decided_at` in `PRAGMA table_info` order and in this SELECT.
        loser_object_norm: r.get(15)?,
    })
}

impl SqliteStore {
    /// v11 → v12. One added column, `fact_decisions.loser_object_norm`, and a
    /// backfill of every existing decision whose conflict row is still there
    /// [G10].
    ///
    /// Leaving those rows `NULL` and recovering the orientation at read time
    /// looked equivalent, and is not: `recover_loser` reads the queue row, the
    /// queue row hangs off the facts by foreign key, and a `kb delete` cascades
    /// it away while the decision survives. An orientation that lives only in a
    /// destructible row is an orientation the store can be made to forget —
    /// after which "close the old one" lands on whichever value the next
    /// re-derivation happens to emit second. Writing it down at migration time,
    /// while the evidence still exists, is what makes an upgraded v11 store
    /// keep the caller's ruling.
    ///
    /// The conflict row names `old_fact_id` and `new_fact_id`; the verdict says
    /// which of them lost. `keep_both` and `void` retired nothing and are left
    /// alone. A decision whose conflict row (or losing fact) is already gone
    /// stays `NULL` — the migration never invents an orientation it cannot read,
    /// and those pairs now QUEUE on their next derivation instead of replaying
    /// positionally.
    ///
    /// This no longer skips the D25 backup on "additive-only" grounds — it does
    /// write rows. It needs no rollback copy for a narrower reason: the only
    /// column it writes is the one it just created, every write is guarded by
    /// `IS NULL`, and so there is no prior value for a bad backfill to destroy.
    pub(crate) fn migrate_v12(&self) -> Result<(), StorageError> {
        self.add_column_if_missing("fact_decisions", "loser_object_norm", "TEXT")?;
        self.backfill_decision_orientation()?;
        Ok(())
    }

    /// Write down which value each unoriented directional decision retired,
    /// read from the conflict row it answered. Returns the number of rows it
    /// could orient.
    ///
    /// Idempotent and per-row: `IS NULL` means it never overwrites a recorded
    /// orientation, and a row whose evidence is gone is skipped rather than
    /// guessed at. Separated from [`SqliteStore::migrate_v12`] because the
    /// migration is not the only place that loses this column — `admin
    /// rebuild` deletes `fact_decisions` and replays `decision.recorded`, and
    /// an event written before schema v12 carries no `loser_object_norm`, so a
    /// rebuilt store drops back to NULL. That is safe in direction (those pairs
    /// queue rather than reversing [G10]) but it discards work the store had
    /// already done, and this is the call that would put it back.
    pub(crate) fn backfill_decision_orientation(&self) -> Result<usize, StorageError> {
        let n = self.conn.execute(
            "UPDATE fact_decisions
                SET loser_object_norm = (
                    SELECT f.object_norm
                      FROM fact_conflict_queue q
                      JOIN facts f
                        ON f.id = CASE fact_decisions.verdict
                                    WHEN 'close_old' THEN q.old_fact_id
                                    ELSE q.new_fact_id
                                  END
                     WHERE q.id = fact_decisions.conflict_id)
              WHERE loser_object_norm IS NULL
                AND conflict_id IS NOT NULL
                AND verdict IN ('close_old', 'reject_new')",
            [],
        )?;
        Ok(n)
    }

    /// Append `decision.recorded` and insert the projection row, in one
    /// transaction. The event id is internal; the decision id comes back.
    ///
    /// `loser_object_norm` is the value this verdict retires [G3] — the
    /// `object_norm` of the fact that closes, `None` when nothing closes. It
    /// goes into the event as well as the row, so `admin rebuild` restores an
    /// orientation instead of dropping back to pair order.
    ///
    /// A malformed key is `MalformedRow`, never three silently blank columns:
    /// a decision that cannot be decomposed cannot be looked up either, and a
    /// blank `pair_hash` would match the next blank one.
    #[allow(clippy::too_many_arguments)]
    pub fn record_decision(
        &self,
        kb: &str,
        diary: &str,
        key_encoded: &str,
        verdict: &str,
        loser_object_norm: Option<&str>,
        authority: &str,
        actor: &str,
        note: Option<&str>,
        conflict_id: Option<Uuid>,
        key_version: &str,
    ) -> Result<Uuid, StorageError> {
        let (subject_norm, relation, pair_hash) =
            semantic_key_decode(key_encoded).ok_or_else(|| {
                StorageError::MalformedRow(format!(
                    "decision key `{key_encoded}` does not decode into subject/relation/pair"
                ))
            })?;
        self.with_tx(|s| {
            let id = Uuid::new_v4();
            let decided_at = Utc::now();
            let (_, event_id) = s.append_event(
                &EventDraft::new(
                    EventKind::DecisionRecorded,
                    actor,
                    payload::decision_recorded(
                        id,
                        key_encoded,
                        key_version,
                        verdict,
                        loser_object_norm,
                        authority,
                        conflict_id,
                        note,
                        actor,
                        kb,
                        diary,
                        &decided_at.to_rfc3339(),
                        None,
                        false,
                    ),
                )
                .scope(Some(kb), Some(diary))
                .subject(conflict_id.unwrap_or(id)),
            )?;
            s.conn.execute(
                "INSERT INTO fact_decisions(id, kb, diary, decision_key, subject_norm, relation,
                        pair_hash, verdict, authority, actor, note, conflict_id, event_id,
                        key_version, decided_at, loser_object_norm)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    id.to_string(),
                    kb,
                    diary,
                    key_encoded,
                    subject_norm,
                    relation,
                    pair_hash,
                    verdict,
                    authority,
                    actor,
                    note,
                    conflict_id.map(|u| u.to_string()),
                    event_id.to_string(),
                    key_version,
                    decided_at.to_rfc3339(),
                    loser_object_norm,
                ],
            )?;
            Ok(id)
        })
    }

    /// The binding decision for a key, with the precedence IN the query so no
    /// caller can implement a different one.
    ///
    /// `(authority = 'caller') DESC` first is the whole rule: the newest
    /// CALLER row beats a newer machine row, `void` is never selected, and
    /// `id DESC` makes ties deterministic so the byte-identical rebuild
    /// property is not decided by row order. Scoped by `kb`, deliberately not
    /// by `diary`.
    ///
    /// A failed lookup PROPAGATES. Mapping an error to `Ok(None)` would read
    /// "the database was busy" as "nobody has decided this", which re-asks a
    /// settled safety question and can re-close an interval the caller
    /// reinstated.
    pub fn lookup_decision(
        &self,
        kb: &str,
        key_encoded: &str,
    ) -> Result<Option<DecisionRow>, StorageError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {DECISION_COLUMNS}
                       FROM fact_decisions
                      WHERE kb = ?1 AND decision_key = ?2 AND verdict <> 'void'
                      ORDER BY (authority = 'caller') DESC, decided_at DESC, id DESC
                      LIMIT 1"
                ),
                params![kb, key_encoded],
                parse_decision_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// The full history for a key, newest first, INCLUDING voided rows — a
    /// void is the record that a question was merged away, and hiding it makes
    /// the ledger look like it lost rows.
    pub fn decisions_for_key(
        &self,
        kb: &str,
        key_encoded: &str,
    ) -> Result<Vec<DecisionRow>, StorageError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM fact_decisions
              WHERE kb = ?1 AND decision_key = ?2
              ORDER BY decided_at DESC, id DESC"
        ))?;
        let rows = stmt.query_map(params![kb, key_encoded], parse_decision_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Claim the queue row and record the decision in ONE transaction, and be
    /// the SOLE lost-claim signal.
    ///
    /// The UPDATE is conditional (`WHERE id=? AND status='open'`); zero rows
    /// updated means somebody else won the claim, and `Ok(None)` lets the
    /// caller report `conflict_already_resolved` instead of reporting a
    /// success it did not perform. The old `facts resolve` path was
    /// non-transactional and ignored the claim boolean, so two racing resolves
    /// superseded both facts and both reported success.
    ///
    /// `loser_object_norm` [G3] is the caller's: the queue row names two facts,
    /// and only the caller knows which of them its `verdict` retires.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_conflict_with_decision(
        &self,
        conflict_id: Uuid,
        kb: &str,
        diary: &str,
        key_encoded: &str,
        verdict: &str,
        loser_object_norm: Option<&str>,
        authority: &str,
        actor: &str,
        note: Option<&str>,
        key_version: &str,
    ) -> Result<Option<Uuid>, StorageError> {
        self.with_tx(|s| {
            let claimed = s.conn.execute(
                "UPDATE fact_conflict_queue
                    SET status = 'resolved', resolved_at = ?1, resolved_by = ?2,
                        note = COALESCE(?3, note)
                  WHERE id = ?4 AND status = 'open'",
                params![
                    Utc::now().to_rfc3339(),
                    actor,
                    note,
                    conflict_id.to_string()
                ],
            )?;
            if claimed == 0 {
                return Ok(None);
            }
            let id = s.record_decision(
                kb,
                diary,
                key_encoded,
                verdict,
                loser_object_norm,
                authority,
                actor,
                note,
                Some(conflict_id),
                key_version,
            )?;
            Ok(Some(id))
        })
    }

    /// The append helper for an applied replay [IN-17]. fact-semantics' applier
    /// calls it once per replay, in the same transaction as the effect.
    /// Without it the obligation is prose.
    #[allow(clippy::too_many_arguments)]
    pub fn record_replay(
        &self,
        kb: &str,
        diary: &str,
        decision_id: Uuid,
        new_fact_id: Uuid,
        old_fact_id: Option<Uuid>,
        applied_verdict: &str,
        actor: &str,
    ) -> Result<(), StorageError> {
        self.with_tx(|s| {
            s.append_event(
                &EventDraft::new(
                    EventKind::DecisionReplayed,
                    actor,
                    payload::decision_replayed(
                        decision_id,
                        new_fact_id,
                        old_fact_id,
                        applied_verdict,
                    ),
                )
                .scope(Some(kb), Some(diary))
                .subject(new_fact_id),
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_store;
    use engram_core::keys::{object_pair_hash, semantic_key_encode, SEMANTIC_KEY_VERSION};

    fn key(a: &str, b: &str) -> String {
        semantic_key_encode("ana", "lives_in", &object_pair_hash(a, b))
    }

    #[test]
    fn newest_caller_decision_beats_a_newer_machine_decision() {
        let (_tmp, _g, store) = temp_store();
        let k = key("lisbon", "porto");
        store
            .record_decision(
                "personal",
                "default",
                &k,
                "close_old",
                Some("lisbon"),
                "caller",
                "cli:default",
                Some("user confirmed the move"),
                None,
                SEMANTIC_KEY_VERSION,
            )
            .unwrap();
        store
            .record_decision(
                "personal",
                "default",
                &k,
                "keep_both",
                None,
                "machine",
                "triage",
                None,
                None,
                SEMANTIC_KEY_VERSION,
            )
            .unwrap();
        let binding = store.lookup_decision("personal", &k).unwrap().unwrap();
        assert_eq!(
            binding.verdict, "close_old",
            "a later sweep must not overrule the person who answered"
        );
        assert_eq!(binding.authority, "caller");
        assert_eq!(store.decisions_for_key("personal", &k).unwrap().len(), 2);
        // Scoped by kb: another KB's ruling is not this KB's.
        assert!(store.lookup_decision("work", &k).unwrap().is_none());
    }

    #[test]
    fn void_decisions_are_never_selected() {
        let (_tmp, _g, store) = temp_store();
        let k = key("lisbon", "porto");
        store
            .record_decision(
                "personal",
                "default",
                &k,
                "void",
                None,
                "machine",
                "migration",
                None,
                None,
                SEMANTIC_KEY_VERSION,
            )
            .unwrap();
        assert!(store.lookup_decision("personal", &k).unwrap().is_none());
        assert_eq!(
            store.decisions_for_key("personal", &k).unwrap().len(),
            1,
            "…but the history still shows it, or the ledger looks like it lost rows"
        );
    }

    #[test]
    fn a_failed_decision_lookup_propagates() {
        let (_tmp, _g, store) = temp_store();
        store
            .with_tx(|s| {
                s.conn.execute("DROP TABLE fact_decisions", [])?;
                Ok(())
            })
            .unwrap();
        let err = store.lookup_decision("personal", &key("a", "b"));
        assert!(
            err.is_err(),
            "reading a failure as `nobody decided` re-asks a settled safety question"
        );
    }

    #[test]
    fn a_malformed_key_is_refused_rather_than_stored_blank() {
        let (_tmp, _g, store) = temp_store();
        let err = store
            .record_decision(
                "personal",
                "default",
                "not-a-key",
                "close_old",
                Some("lisbon"),
                "caller",
                "cli:default",
                None,
                None,
                SEMANTIC_KEY_VERSION,
            )
            .unwrap_err();
        assert!(matches!(err, StorageError::MalformedRow(_)), "{err:?}");
    }

    /// A minimal (memory, fact, fact, queue row) fixture. The keys are
    /// constructed here rather than through the write path, because wave 2's
    /// write path deliberately passes an empty key until fact-semantics lands.
    fn seeded(store: &SqliteStore) -> (Uuid, Uuid, Uuid, String) {
        use engram_core::types::{Fact, FactType, Memory, MemorySource, SourceKind};
        let m = Memory {
            id: Uuid::new_v4(),
            content: "Ana moved.".into(),
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
        let mk = |object: &str| Fact {
            id: Uuid::new_v4(),
            source_memory_id: m.id,
            subject: "Ana".into(),
            subject_norm: "ana".into(),
            predicate: "lives_in".into(),
            object: object.into(),
            object_norm: object.to_lowercase(),
            confidence: 0.9,
            created_at: Utc::now(),
            superseded_by: None,
            superseded_at: None,
            diary: "default".into(),
            fact_type: FactType::ProfileSlot,
            valid_from: Some(Utc::now()),
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
        (m.id, new.id, old.id, key("lisbon", "porto"))
    }

    fn another_fact(store: &SqliteStore, memory_id: Uuid, object: &str) -> Uuid {
        use engram_core::types::{Fact, FactType, SourceKind};
        let f = Fact {
            id: Uuid::new_v4(),
            source_memory_id: memory_id,
            subject: "Ana".into(),
            subject_norm: "ana".into(),
            predicate: "lives_in".into(),
            object: object.into(),
            object_norm: object.to_lowercase(),
            confidence: 0.9,
            created_at: Utc::now(),
            superseded_by: None,
            superseded_at: None,
            diary: "default".into(),
            fact_type: FactType::ProfileSlot,
            valid_from: Some(Utc::now()),
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
        f.id
    }

    #[test]
    fn queue_insert_distinguishes_duplicate_pair_from_duplicate_key() {
        let (_tmp, _g, store) = temp_store();
        let (m, new, old, k) = seeded(&store);
        let first = Uuid::new_v4();
        assert_eq!(
            store
                .insert_fact_conflict(
                    first,
                    new,
                    old,
                    "default",
                    "personal",
                    "slot",
                    None,
                    "supersede-old",
                    &k,
                    "porto",
                    "relations@1"
                )
                .unwrap(),
            InsertOutcome::Inserted
        );
        // Same ordered pair.
        assert_eq!(
            store
                .insert_fact_conflict(
                    Uuid::new_v4(),
                    new,
                    old,
                    "default",
                    "personal",
                    "slot",
                    None,
                    "supersede-old",
                    &k,
                    "porto",
                    "relations@1"
                )
                .unwrap(),
            InsertOutcome::DuplicatePair { existing: first }
        );
        // A DIFFERENT pair asking the same question — this is the one the
        // applier may honestly report as `deduplicated`.
        let other_new = another_fact(&store, m, "Braga");
        assert_eq!(
            store
                .insert_fact_conflict(
                    Uuid::new_v4(),
                    other_new,
                    old,
                    "default",
                    "personal",
                    "slot",
                    None,
                    "supersede-old",
                    &k,
                    "porto",
                    "relations@1"
                )
                .unwrap(),
            InsertOutcome::DuplicateKey { existing: first }
        );
        // An empty key is stored as SQL NULL, so the wave-2 stopgap cannot
        // collide every row under the partial unique index.
        let keyless = Uuid::new_v4();
        assert_eq!(
            store
                .insert_fact_conflict(
                    keyless,
                    other_new,
                    new,
                    "default",
                    "personal",
                    "slot",
                    None,
                    "supersede-old",
                    "",
                    "",
                    "relations@1"
                )
                .unwrap(),
            InsertOutcome::Inserted
        );
        assert!(store
            .get_fact_conflict(keyless)
            .unwrap()
            .unwrap()
            .decision_key
            .is_none());
        assert_eq!(
            store
                .events_for_subject(first)
                .unwrap()
                .first()
                .map(|e| e.kind.as_str()),
            Some("conflict.opened")
        );
    }

    #[test]
    fn a_lost_conflict_claim_returns_none_not_success() {
        let (_tmp, _g, store) = temp_store();
        let (_m, new, old, k) = seeded(&store);
        let qid = Uuid::new_v4();
        store
            .insert_fact_conflict(
                qid,
                new,
                old,
                "default",
                "personal",
                "slot",
                None,
                "supersede-old",
                &k,
                "porto",
                "relations@1",
            )
            .unwrap();
        let won = store
            .resolve_conflict_with_decision(
                qid,
                "personal",
                "default",
                &k,
                "close_old",
                Some("lisbon"),
                "caller",
                "cli:default",
                Some("user confirmed"),
                SEMANTIC_KEY_VERSION,
            )
            .unwrap();
        assert!(won.is_some());
        // The racing second resolve must NOT report a success it did not
        // perform: two racing resolves used to supersede both facts and both
        // print "resolved".
        let lost = store
            .resolve_conflict_with_decision(
                qid,
                "personal",
                "default",
                &k,
                "reject_new",
                Some("porto"),
                "caller",
                "cli:other",
                None,
                SEMANTIC_KEY_VERSION,
            )
            .unwrap();
        assert_eq!(lost, None);
        assert_eq!(
            store.decisions_for_key("personal", &k).unwrap().len(),
            1,
            "a lost claim records nothing"
        );
        assert_eq!(
            store
                .lookup_decision("personal", &k)
                .unwrap()
                .unwrap()
                .verdict,
            "close_old"
        );
    }

    #[test]
    fn an_orphan_sweep_records_a_void_decision_per_row_it_closes() {
        let (_tmp, _g, store) = temp_store();
        let (_m, new, old, k) = seeded(&store);
        let qid = Uuid::new_v4();
        store
            .insert_fact_conflict(
                qid,
                new,
                old,
                "default",
                "personal",
                "slot",
                None,
                "supersede-old",
                &k,
                "porto",
                "relations@1",
            )
            .unwrap();
        // Orphans exist in the wild for exactly one reason: the row outlived
        // its facts because foreign keys were off when they went. Reproduce
        // that, rather than letting the cascade take the queue row too — which
        // is the defect, not the fixture.
        store.conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        store
            .conn
            .execute("DELETE FROM facts WHERE id = ?1", params![new.to_string()])
            .unwrap();
        store.conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let n = store
            .resolve_orphan_fact_conflicts(None, None, "repair")
            .unwrap();
        assert_eq!(n, 1);
        let history = store.decisions_for_key("personal", &k).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].verdict, "void");
        assert_eq!(history[0].conflict_id, Some(qid));
        assert!(
            store.lookup_decision("personal", &k).unwrap().is_none(),
            "a void must never be replayed onto a later pair"
        );
    }

    /// Invariant 1, swept: EVERY path that answers a queued question has to
    /// answer it in the journal too, or `admin rebuild` replays the
    /// `conflict.opened` and the question comes back from the dead with the
    /// caller's ruling gone. This is the same defect as the extraction
    /// marker's (blocker 6) — a projection write with no event — and the
    /// unwelded `resolve_fact_conflict` that used to sit beside this path
    /// could not pass it, which is why it no longer exists.
    #[test]
    fn a_resolved_conflict_stays_resolved_through_a_rebuild() {
        let (_tmp, _g, store) = temp_store();
        let (_m, new, old, k) = seeded(&store);
        let qid = Uuid::new_v4();
        store
            .insert_fact_conflict(
                qid,
                new,
                old,
                "default",
                "personal",
                "slot",
                None,
                "supersede-old",
                &k,
                "porto",
                "relations@1",
            )
            .unwrap();
        store
            .resolve_conflict_with_decision(
                qid,
                "personal",
                "default",
                &k,
                "close_old",
                Some("lisbon"),
                "caller",
                "cli:default",
                Some("user confirmed"),
                SEMANTIC_KEY_VERSION,
            )
            .unwrap()
            .expect("the row was open");

        store.rebuild_projections(None, false).unwrap();
        let row = store.get_fact_conflict(qid).unwrap().expect("still queued");
        assert_eq!(row.status, "resolved");
        assert_eq!(row.note.as_deref(), Some("user confirmed"));
        assert_eq!(
            store
                .lookup_decision("personal", &k)
                .unwrap()
                .unwrap()
                .verdict,
            "close_old"
        );
    }

    /// The shape a v0.4-rc binary left behind: `fact_decisions` with no
    /// orientation column at all. Nothing else about the table moved between
    /// v11 and v12, so dropping the column and rewinding the stamp reproduces
    /// that store — and the next `SqliteStore::open` then runs the REAL
    /// migration over it rather than a test-only imitation.
    fn demote_to_v11(store: &SqliteStore) {
        store
            .conn
            .execute_batch(
                "ALTER TABLE fact_decisions DROP COLUMN loser_object_norm;
                 UPDATE schema_version SET version = 11;",
            )
            .unwrap();
    }

    fn queued(store: &SqliteStore, new: Uuid, old: Uuid, k: &str, new_object: &str) -> Uuid {
        let qid = Uuid::new_v4();
        store
            .insert_fact_conflict(
                qid,
                new,
                old,
                "default",
                "personal",
                "slot",
                None,
                "supersede-old",
                k,
                new_object,
                "relations@1",
            )
            .unwrap();
        qid
    }

    /// G10's storage half. A decision taken on a v11 store carries no
    /// orientation, and the read-time recovery that stood in for one reads the
    /// queue row — which hangs off the facts by foreign key, so a `kb delete`
    /// takes it and leaves the ruling to be re-applied by pair order. The
    /// migration writes the orientation down while the evidence is still there.
    #[test]
    fn the_v12_migration_orients_a_v11_decision_from_its_conflict_row() {
        let (tmp, _g, store) = temp_store();
        let (_m, new, old, k) = seeded(&store);
        let qid = queued(&store, new, old, &k, "porto");
        store
            .resolve_conflict_with_decision(
                qid,
                "personal",
                "default",
                &k,
                "close_old",
                Some("lisbon"),
                "caller",
                "cli:default",
                None,
                SEMANTIC_KEY_VERSION,
            )
            .unwrap()
            .expect("the row was open");
        demote_to_v11(&store);
        drop(store);

        let store = SqliteStore::open(tmp.path().join("db.sqlite")).unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            crate::sqlite::CURRENT_VERSION
        );
        assert_eq!(
            store
                .lookup_decision("personal", &k)
                .unwrap()
                .unwrap()
                .loser_object_norm
                .as_deref(),
            Some("lisbon"),
            "the conflict row named the loser; the migration had to read it"
        );
        assert_eq!(
            store.backfill_decision_orientation().unwrap(),
            0,
            "the sweep is idempotent: a second pass has nothing left to orient"
        );

        // The cascade that used to erase the orientation. `kb delete` removes
        // the facts, the queue row goes with them by foreign key, and the
        // decision survives — now with its side still written down.
        store
            .conn
            .execute("DELETE FROM facts WHERE kb = 'personal'", [])
            .unwrap();
        assert!(
            store.get_fact_conflict(qid).unwrap().is_none(),
            "the queue row must actually cascade, or this proves nothing"
        );
        assert_eq!(
            store
                .lookup_decision("personal", &k)
                .unwrap()
                .unwrap()
                .loser_object_norm
                .as_deref(),
            Some("lisbon"),
            "an orientation that only lives in a destructible row is deletable"
        );
    }

    /// …and it never invents one. A verdict that retired nothing, and a
    /// directional verdict whose evidence is already gone, both stay NULL —
    /// which `replay` reads as "queue this", not as "no loser".
    #[test]
    fn the_v12_migration_leaves_what_it_cannot_read_unoriented() {
        let (tmp, _g, store) = temp_store();
        let (m, new, old, k) = seeded(&store);
        let braga = another_fact(&store, m, "Braga");
        let faro = another_fact(&store, m, "Faro");

        // The control: directional, evidence intact. It must come back
        // oriented, or the two NULLs below prove nothing about the sweep.
        let q_live = queued(&store, new, old, &k, "porto");
        store
            .resolve_conflict_with_decision(
                q_live,
                "personal",
                "default",
                &k,
                "close_old",
                Some("lisbon"),
                "caller",
                "cli:default",
                None,
                SEMANTIC_KEY_VERSION,
            )
            .unwrap()
            .expect("open");

        // Directional, but its evidence is destroyed before the migration runs.
        let k_gone = key("braga", "porto");
        let q_gone = queued(&store, braga, new, &k_gone, "braga");
        store
            .resolve_conflict_with_decision(
                q_gone,
                "personal",
                "default",
                &k_gone,
                "close_old",
                Some("porto"),
                "caller",
                "cli:default",
                None,
                SEMANTIC_KEY_VERSION,
            )
            .unwrap()
            .expect("open");

        // Non-directional: keep-both retired nothing, so there is nothing to
        // backfill and NULL is the honest answer rather than a gap.
        let k_keep = key("faro", "lisbon");
        let q_keep = queued(&store, faro, old, &k_keep, "faro");
        store
            .resolve_conflict_with_decision(
                q_keep,
                "personal",
                "default",
                &k_keep,
                "keep_both",
                None,
                "caller",
                "cli:default",
                None,
                SEMANTIC_KEY_VERSION,
            )
            .unwrap()
            .expect("open");

        store
            .conn
            .execute(
                "DELETE FROM facts WHERE id = ?1",
                params![braga.to_string()],
            )
            .unwrap();
        assert!(store.get_fact_conflict(q_gone).unwrap().is_none());
        demote_to_v11(&store);
        drop(store);

        let store = SqliteStore::open(tmp.path().join("db.sqlite")).unwrap();
        assert!(
            store
                .lookup_decision("personal", &k_gone)
                .unwrap()
                .unwrap()
                .loser_object_norm
                .is_none(),
            "no conflict row, no orientation — guessing is what G10 forbids"
        );
        assert!(store
            .lookup_decision("personal", &k_keep)
            .unwrap()
            .unwrap()
            .loser_object_norm
            .is_none());
        // The one it CAN read is still read: the sweep is per row, not
        // all-or-nothing.
        assert_eq!(
            store
                .lookup_decision("personal", &k)
                .unwrap()
                .unwrap()
                .loser_object_norm
                .as_deref(),
            Some("lisbon")
        );
    }
}

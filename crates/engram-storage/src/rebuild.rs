//! `engram admin rebuild` — drop the projections and replay them.
//!
//! Invariant 3, and the architectural rollback for a corrupted projection. Its
//! scope is narrower than the one-line statement, and the command PRINTS the
//! narrowing on every run: a rebuild that implies it restored things it did not
//! is the class of dishonest health check 0.3.1 spent a release removing.
//!
//! | table | rebuilt | why |
//! |---|---|---|
//! | `facts` (minus `embedding`/`embed_model`) | yes | embeddings are a recomputable cache, and a 768-float blob has no business in a JSON payload |
//! | `fact_conflict_queue`, `fact_decisions` | yes | from `conflict.opened` + `decision.recorded` |
//! | `derivations`, `fact_extraction_log` | yes | from `derivation.ran`, minus the markers `memory.revised` cleared |
//! | `lessons` | yes | from `lesson.recorded|confirmed|retracted` (D24) |
//! | `memories`, `chunks` | **no** | access telemetry and chunk embeddings are deliberately not journaled |
//! | `claims`, `entities`, `relations`, `wiki_pages`, `source_spans` | **no** | compiler caches; `engram compile` regenerates them |
//!
//! Three rules that are cheap to state and expensive to rediscover:
//!
//! 1. **Replay writes raw SQL, never the journaling methods.** `apply` must not
//!    call `insert_fact`/`close_fact_interval`/`record_decision`: those append
//!    events, and a rebuild that appends is a rebuild that grows its own input.
//! 2. **`facts.kb` comes from the event's `kb` column**, never from a subquery
//!    over `memories`. Re-running that subquery at replay time silently
//!    re-homes every fact whose source memory later moved between KBs.
//! 3. **An orphaned queue row is PRESERVED, not deleted and not inserted.**
//!    The queue's two FKs to `facts` are `ON DELETE CASCADE`, so a plain
//!    INSERT of a row whose facts are gone aborts the whole rebuild — its
//!    `conflict.opened` therefore cannot replay. Deleting the row first and
//!    then declining to recreate it destroys migrated history the journal
//!    never held; so the delete skips those rows and the report names both
//!    numbers: `conflicts_skipped_orphan` events unreplayable,
//!    `conflicts_preserved_orphan` rows left in place. This is the one place
//!    a rebuild is knowingly not a pure function of the journal, and it says
//!    so on every run.

use crate::error::StorageError;
use crate::journal::EventRow;
use crate::sqlite::SqliteStore;
use engram_core::events::EventKind;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RebuildReport {
    pub dry_run: bool,
    pub kb: Option<String>,
    pub events_replayed: usize,
    pub events_skipped_deleted_kb: usize,
    pub facts: i64,
    pub conflicts: i64,
    pub decisions: i64,
    pub derivations: i64,
    pub extraction_markers: i64,
    pub lessons: i64,
    pub conflicts_skipped_orphan: usize,
    pub conflicts_preserved_orphan: usize,
    pub embeddings_restored: usize,
    pub skipped: Vec<SkippedTable>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SkippedTable {
    pub table: &'static str,
    pub reason: &'static str,
}

/// Named on every run, with reasons, because invariant 3's one-line statement
/// promises more than any journal can deliver.
pub fn skipped_tables() -> Vec<SkippedTable> {
    vec![
        SkippedTable {
            table: "memories",
            reason: "access_count, last_accessed and stability are deliberately not journaled; \
                     memory.* events are provenance for them, not their source",
        },
        SkippedTable {
            table: "chunks",
            reason: "chunk embeddings are a recomputable cache; `engram reindex` owns them",
        },
        SkippedTable {
            table: "facts.embedding",
            reason: "a 768-float blob has no business in a JSON event payload; preserved across \
                     the rebuild rather than replayed",
        },
        SkippedTable {
            table: "fact_conflict_queue.orphans",
            reason: "a question whose facts are gone cannot be replayed; preserved in place \
                     rather than deleted, and counted in conflicts_preserved_orphan",
        },
        SkippedTable {
            table: "claims",
            reason: "compiler cache; `engram compile` regenerates it",
        },
        SkippedTable {
            table: "entities",
            reason: "compiler cache; `engram compile` regenerates it",
        },
        SkippedTable {
            table: "relations",
            reason: "compiler cache; `engram compile` regenerates it",
        },
        SkippedTable {
            table: "wiki_pages",
            reason: "compiler cache; `engram compile` regenerates it",
        },
        SkippedTable {
            table: "source_spans",
            reason: "compiler cache; `engram compile` regenerates it",
        },
    ]
}

/// Where a replayed event came from, and therefore how far it is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// This store's own journal. Whatever it says, this store did — and that
    /// INCLUDES the v9 history the migration synthesized, where a relation
    /// the registry has since promoted to `safety` was closed by the old
    /// slot-LWW heuristic. Re-gating those on every rebuild would reopen
    /// thousands of correctly-retired prescriptions, which is a rebuild that
    /// edits history rather than replaying it.
    OwnJournal,
    /// An event out of an export file — bytes a caller can edit with `jq`.
    /// The [G2] gate applies, exactly as it does to the row sections.
    Imported,
}

/// The close a fact row is ALREADY carrying, read back before a replayed
/// `fact.interval_closed` writes over it.
struct CloseColumns {
    valid_until: Option<String>,
    reason: Option<String>,
    authority: Option<String>,
    closed_by: Option<String>,
    superseded_by: Option<String>,
    superseded_at: Option<String>,
}

impl CloseColumns {
    /// Would replaying this close change the row?
    ///
    /// [G2] The gate refuses closes; a refusal it COUNTS is a refusal it will
    /// eventually show the caller, so it must not count one where there was
    /// nothing to refuse. That case is ordinary, not exotic: a legitimate
    /// extracted cessation is born closed, so the file carries the same close
    /// twice — once in the fact row, where the cessation exemption vets it,
    /// and once in the `fact.interval_closed` event that recorded it. The
    /// event arrives at a row that already says exactly what the event says.
    /// Blocking it changes nothing, and reporting a rejected close on a
    /// healthy round trip would be the store crying wolf.
    fn would_move(&self, e: &EventRow, until: &str) -> Result<bool, StorageError> {
        let closed_by = e.payload_opt_str("closed_by");
        let superseded_at = e
            .payload_opt_str("superseded_at")
            .unwrap_or_else(|| until.to_string());
        Ok(self.valid_until.as_deref() != Some(until)
            || self.reason.as_deref() != Some(e.payload_str("reason")?.as_str())
            || self.authority.as_deref() != Some(e.payload_str("authority")?.as_str())
            || self.closed_by != closed_by
            || (e.payload_bool("sets_superseded_by")
                && (self.superseded_by != closed_by
                    || self.superseded_at.as_deref() != Some(superseded_at.as_str()))))
    }
}

/// Replay one event onto the projections. Raw SQL only — see rule 1.
pub fn apply(store: &SqliteStore, e: &EventRow) -> Result<(), StorageError> {
    apply_from(store, e, Origin::OwnJournal).map(|_| ())
}

/// [G2] Replay one event, returning the number of closes the safety gate
/// refused (0 or 1). `Origin::OwnJournal` never refuses anything, so
/// `admin rebuild` is unchanged.
pub fn apply_from(
    store: &SqliteStore,
    e: &EventRow,
    origin: Origin,
) -> Result<usize, StorageError> {
    let mut refused = 0usize;
    match e.kind {
        EventKind::FactObserved => {
            let id = e.payload_uuid("fact_id")?;
            let mut valid_until = e.payload_opt_str("valid_until");
            if origin == Origin::Imported {
                let valid_from = e.payload_opt_str("valid_from");
                if crate::export_v3::interval_is_inverted(
                    valid_from.as_deref(),
                    valid_until.as_deref(),
                ) {
                    return Err(crate::export_v3::inverted_interval_error(
                        &id.to_string(),
                        valid_from.as_deref().unwrap_or(""),
                        valid_until.as_deref().unwrap_or(""),
                    ));
                }
                // An observation carries no authority at all, so an imported
                // one that arrives already CLOSED on a safety relation is a
                // close nobody signed. It carries no `close_reason` either,
                // which is why the cessation exemption cannot fire here: the
                // payload has no way to say "this ENDED" as against "this was
                // overruled", and the exemption is only for the first. The
                // ask is read anyway rather than hard-coded to `None`, so a
                // journal that later learns to carry it needs no edit here.
                //
                // Nothing legitimate is lost. This branch is `INSERT OR
                // IGNORE`, the file's fact ROW for the same id landed before
                // any event replayed, and that row is where a cessation
                // proves itself. A file that reaches this line with a closed
                // safety observation is one whose rows were removed and whose
                // journal is the only thing still claiming the interval
                // ended.
                if valid_until.is_some()
                    && !crate::export_v3::cessation_close_is_exempt(
                        e.payload_opt_str("close_reason").as_deref(),
                        valid_from.as_deref(),
                        valid_until.as_deref(),
                    )
                    && crate::export_v3::close_needs_person(&e.payload_str("relation")?, None)
                {
                    valid_until = None;
                    refused += 1;
                }
            }
            store.conn.execute(
                "INSERT OR IGNORE INTO facts(id, source_memory_id, subject, subject_norm,
                        predicate, object, object_norm, confidence, created_at,
                        superseded_by, superseded_at, diary, fact_type, valid_from,
                        valid_until, resolution, source_kind, confirmations,
                        closed_by, close_reason, close_authority, kb)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL, ?10, ?11, ?12,
                         ?13, ?14, ?15, ?16, NULL, NULL, NULL, ?17)",
                params![
                    id.to_string(),
                    e.payload_str("source_memory_id")?,
                    e.payload_str("subject")?,
                    e.payload_str("subject_norm")?,
                    e.payload_str("relation")?,
                    e.payload_str("object")?,
                    e.payload_str("object_norm")?,
                    e.payload
                        .get("confidence")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0),
                    e.payload_str("created_at")?,
                    // The diary and fact_type come from the payload, not from
                    // a default: without them every rebuilt fact returns as an
                    // `assertion` in the default diary.
                    e.payload_opt_str("diary")
                        .or_else(|| e.diary.clone())
                        .unwrap_or_else(|| "default".to_string()),
                    e.payload_opt_str("fact_type")
                        .unwrap_or_else(|| "assertion".to_string()),
                    e.payload_opt_str("valid_from"),
                    valid_until,
                    e.payload_opt_str("resolution"),
                    e.payload_opt_str("source_kind")
                        .unwrap_or_else(|| "agent_extracted".to_string()),
                    // [G5] The count the observation was made WITH. A live
                    // insert says 0 and the `fact.confirmed` events below add
                    // to it; a migrated row says what the v9 store had
                    // already counted, because nothing replays those. Events
                    // written before this field defaulted to zero, which is
                    // what they meant.
                    e.payload
                        .get("confirmations")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    // Rule 2.
                    e.kb.clone().unwrap_or_else(|| "default".to_string()),
                ],
            )?;
        }
        EventKind::FactIntervalClosed => {
            let id = e.payload_uuid("fact_id")?;
            let sets = e.payload_bool("sets_superseded_by");
            if origin == Origin::Imported {
                // The relation is not in the close payload — it is in the row
                // the earlier `fact.observed` (or the file's row section)
                // already landed. The row's own close columns come back with
                // it, for the "would this move anything?" question below.
                let target: Option<(String, Option<String>, CloseColumns)> = store
                    .conn
                    .query_row(
                        "SELECT predicate, valid_from, valid_until, close_reason,
                                close_authority, closed_by, superseded_by, superseded_at
                           FROM facts WHERE id = ?1",
                        params![id.to_string()],
                        |r| {
                            Ok((
                                r.get(0)?,
                                r.get(1)?,
                                CloseColumns {
                                    valid_until: r.get(2)?,
                                    reason: r.get(3)?,
                                    authority: r.get(4)?,
                                    closed_by: r.get(5)?,
                                    superseded_by: r.get(6)?,
                                    superseded_at: r.get(7)?,
                                },
                            ))
                        },
                    )
                    .optional()?;
                if let Some((predicate, valid_from, current)) = target {
                    let until = e.payload_str("valid_until")?;
                    if crate::export_v3::interval_is_inverted(valid_from.as_deref(), Some(&until)) {
                        return Err(crate::export_v3::inverted_interval_error(
                            &id.to_string(),
                            valid_from.as_deref().unwrap_or(""),
                            &until,
                        ));
                    }
                    // [G2] NO cessation exemption on this event, whatever
                    // shape or reason it claims. `fact.interval_closed` closes
                    // an interval that already exists, so it can never be the
                    // born-closed cessation the exemption is for — and it is
                    // exactly the forgery the exemption was letting through:
                    // set `valid_until` to a live allergy's own `valid_from`
                    // and a shape test reads the attack as a point.
                    if crate::export_v3::close_needs_person(
                        &predicate,
                        Some(e.payload_str("authority")?).as_deref(),
                    ) && current.would_move(e, &until)?
                    {
                        // The event is kept — the journal records what the
                        // file claimed — but the projection does not move.
                        return Ok(1);
                    }
                }
            }
            store.conn.execute(
                "UPDATE facts
                    SET valid_until = ?1, close_reason = ?2, close_authority = ?3,
                        closed_by = ?4,
                        superseded_by = CASE WHEN ?5 THEN ?4 ELSE superseded_by END,
                        superseded_at = CASE WHEN ?5 THEN ?6 ELSE superseded_at END
                  WHERE id = ?7",
                params![
                    e.payload_str("valid_until")?,
                    e.payload_str("reason")?,
                    e.payload_str("authority")?,
                    e.payload_opt_str("closed_by"),
                    sets,
                    // Pre-`superseded_at` payloads fall back to event time,
                    // which is what a live close writes anyway.
                    e.payload_opt_str("superseded_at")
                        .unwrap_or(e.payload_str("valid_until")?),
                    id.to_string(),
                ],
            )?;
        }
        EventKind::FactConfirmed => {
            let id = e.payload_uuid("fact_id")?;
            store.conn.execute(
                "UPDATE facts SET confirmations = confirmations + 1 WHERE id = ?1",
                params![id.to_string()],
            )?;
        }
        EventKind::DerivationRan => {
            let memory_id = e.payload_str("memory_id")?;
            let ids = e
                .payload
                .get("output_fact_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.len() as i64)
                .unwrap_or(0);
            let ids_json = e
                .payload
                .get("output_fact_ids")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());
            store.conn.execute(
                "INSERT OR REPLACE INTO derivations(content_hash, extractor_id, memory_id,
                        derived_at, model, fact_count, output_fact_ids, event_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    e.payload_str("content_hash")?,
                    e.payload_str("deriver")?,
                    memory_id,
                    e.ts.to_rfc3339(),
                    e.payload_opt_str("model"),
                    ids,
                    ids_json,
                    e.id.to_string(),
                ],
            )?;
            store.conn.execute(
                "INSERT OR REPLACE INTO fact_extraction_log(memory_id, extracted_at, model,
                        fact_count, source)
                 VALUES (?1, ?2, ?3, ?4, 'remember')",
                params![
                    memory_id,
                    e.ts.to_rfc3339(),
                    e.payload_opt_str("model"),
                    ids
                ],
            )?;
        }
        // D6, option 2: an edit clears the extraction marker, and the clear
        // has to be a function of the journal or a rebuild un-pends every
        // edited memory — the re-extraction time bomb, reintroduced by the
        // rebuild that was supposed to make everything reconstructible.
        EventKind::MemoryRevised => {
            store.conn.execute(
                "DELETE FROM fact_extraction_log WHERE memory_id = ?1",
                params![e.payload_str("memory_id")?],
            )?;
        }
        EventKind::ConflictOpened => {
            let id = e.payload_uuid("conflict_id")?;
            let new_fact = e.payload_str("new_fact_id")?;
            let old_fact = e.payload_str("old_fact_id")?;
            let both_exist: i64 = store.conn.query_row(
                "SELECT (EXISTS(SELECT 1 FROM facts WHERE id = ?1))
                      * (EXISTS(SELECT 1 FROM facts WHERE id = ?2))",
                params![new_fact, old_fact],
                |r| r.get(0),
            )?;
            if both_exist == 0 {
                // Rule 3.
                return Err(StorageError::NotFound(format!("orphan conflict {id}")));
            }
            store.conn.execute(
                "INSERT OR IGNORE INTO fact_conflict_queue(id, new_fact_id, old_fact_id, diary,
                        kb, detector, score, suggested, note, status, created_at,
                        decision_key, member_key, registry_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'open', ?10, ?11, ?12, ?13)",
                params![
                    id.to_string(),
                    new_fact,
                    old_fact,
                    e.diary.clone().unwrap_or_else(|| "default".to_string()),
                    e.kb.clone().unwrap_or_else(|| "default".to_string()),
                    e.payload_str("detector")?,
                    e.payload.get("score").and_then(|v| v.as_f64()),
                    e.payload_opt_str("suggested"),
                    e.payload_opt_str("note"),
                    e.payload_opt_str("created_at")
                        .unwrap_or_else(|| e.ts.to_rfc3339()),
                    e.payload_opt_str("decision_key"),
                    e.payload_opt_str("member_key"),
                    e.payload_opt_str("registry_version"),
                ],
            )?;
        }
        EventKind::DecisionRecorded => {
            let id = e.payload_uuid("decision_id")?;
            let key = e.payload_str("semantic_key")?;
            let (subject_norm, relation, pair_hash) = engram_core::keys::semantic_key_decode(&key)
                .ok_or_else(|| {
                    StorageError::MalformedRow(format!(
                        "event {} carries an undecodable semantic_key",
                        e.id
                    ))
                })?;
            let conflict_id = e.payload_opt_str("conflict_id");
            store.conn.execute(
                "INSERT OR IGNORE INTO fact_decisions(id, kb, diary, decision_key, subject_norm,
                        relation, pair_hash, verdict, authority, actor, note, conflict_id,
                        event_id, key_version, decided_at, loser_object_norm)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    id.to_string(),
                    e.payload_opt_str("kb")
                        .or_else(|| e.kb.clone())
                        .unwrap_or_else(|| "default".to_string()),
                    e.payload_opt_str("diary")
                        .or_else(|| e.diary.clone())
                        .unwrap_or_else(|| "default".to_string()),
                    key,
                    subject_norm,
                    relation,
                    pair_hash,
                    e.payload_str("verdict")?,
                    e.payload_str("authority")?,
                    e.payload_opt_str("actor")
                        .unwrap_or_else(|| e.actor.clone()),
                    e.payload_opt_str("note"),
                    conflict_id,
                    e.id.to_string(),
                    e.payload_opt_str("key_version")
                        .unwrap_or_else(|| engram_core::keys::SEMANTIC_KEY_VERSION.to_string()),
                    e.payload_opt_str("decided_at")
                        .unwrap_or_else(|| e.ts.to_rfc3339()),
                    // Absent on every pre-v12 event, and absent is the honest
                    // answer: the rebuild restores NOT RECORDED rather than
                    // inventing a side [G3].
                    e.payload_opt_str("loser_object_norm"),
                ],
            )?;
            if let Some(cid) = e.payload_opt_str("conflict_id") {
                // `collapsed_into` is what makes `superseded_question`
                // rebuildable rather than inferred from note text.
                let status = if e.payload_opt_str("collapsed_into").is_some() {
                    crate::migrate_v10::STATUS_SUPERSEDED_QUESTION
                } else {
                    "resolved"
                };
                store.conn.execute(
                    "UPDATE fact_conflict_queue
                        SET status = ?1, resolved_at = ?2, resolved_by = ?3,
                            note = COALESCE(?4, note)
                      WHERE id = ?5",
                    params![
                        status,
                        e.payload_opt_str("decided_at")
                            .unwrap_or_else(|| e.ts.to_rfc3339()),
                        e.payload_opt_str("actor")
                            .unwrap_or_else(|| e.actor.clone()),
                        e.payload_opt_str("note"),
                        cid,
                    ],
                )?;
            }
        }
        // Procedural memory (D24). Replay SETS `occurrences` from the payload
        // rather than incrementing a counter: the payload carries the
        // post-write count, so a lesson learned five times rebuilds as five
        // whether the replay sees one event or five.
        EventKind::LessonRecorded => {
            let id = e.payload_uuid("lesson_id")?;
            store.conn.execute(
                "INSERT INTO lessons(id, kb, diary, trigger, lesson, lesson_norm, status,
                        source_kind, confidence, occurrences, evidence_json, memory_id, rule,
                        created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                 ON CONFLICT(id) DO UPDATE SET
                        occurrences = excluded.occurrences,
                        evidence_json = excluded.evidence_json,
                        memory_id = COALESCE(excluded.memory_id, lessons.memory_id),
                        updated_at = excluded.updated_at",
                params![
                    id.to_string(),
                    e.payload_opt_str("kb")
                        .or_else(|| e.kb.clone())
                        .unwrap_or_else(|| "default".to_string()),
                    e.payload_opt_str("diary")
                        .or_else(|| e.diary.clone())
                        .unwrap_or_else(|| "lessons".to_string()),
                    e.payload_str("trigger")?,
                    e.payload_str("lesson")?,
                    e.payload_str("lesson_norm")?,
                    e.payload_str("status")?,
                    e.payload_opt_str("source_kind")
                        .unwrap_or_else(|| "agent_extracted".to_string()),
                    e.payload
                        .get("confidence")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.5),
                    e.payload
                        .get("occurrences")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(1),
                    e.payload
                        .get("evidence")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "[]".to_string()),
                    e.payload_opt_str("memory_id"),
                    e.payload_opt_str("rule"),
                    // `created_at` comes from the payload, never from `now()`:
                    // a rebuilt lesson dated at rebuild time is a lesson whose
                    // history was quietly rewritten.
                    e.payload_opt_str("created_at")
                        .unwrap_or_else(|| e.ts.to_rfc3339()),
                    e.payload_opt_str("updated_at")
                        .unwrap_or_else(|| e.ts.to_rfc3339()),
                ],
            )?;
        }
        EventKind::LessonConfirmed | EventKind::LessonRetracted => {
            let id = e.payload_uuid("lesson_id")?;
            let status = if e.kind == EventKind::LessonConfirmed {
                crate::lessons::STATUS_CONFIRMED
            } else {
                crate::lessons::STATUS_RETIRED
            };
            store.conn.execute(
                "UPDATE lessons
                    SET status = ?1,
                        memory_id = COALESCE(?2, memory_id),
                        updated_at = ?3
                  WHERE id = ?4",
                params![
                    status,
                    e.payload_opt_str("memory_id"),
                    e.payload_opt_str("updated_at")
                        .unwrap_or_else(|| e.ts.to_rfc3339()),
                    id.to_string(),
                ],
            )?;
        }
        // No projection effect: an audit record of an applied replay, and the
        // provenance of memories/compile/jobs, which this rebuild does not own.
        EventKind::DecisionReplayed
        | EventKind::MemoryWritten
        | EventKind::MemoryRetracted
        | EventKind::MemoryMetaChanged
        | EventKind::CompileRan
        | EventKind::JobSubmitted
        | EventKind::JobCompleted
        | EventKind::JobFailed => {}
    }
    Ok(refused)
}

impl SqliteStore {
    /// Drop and replay the fact and adjudication projections.
    ///
    /// `--dry-run` runs the identical code path inside a transaction it then
    /// rolls back, rather than a second implementation against shadow tables:
    /// a dry run that does not exercise the real replay is a dry run that can
    /// disagree with the wet one.
    pub fn rebuild_projections(
        &self,
        kb: Option<&str>,
        dry_run: bool,
    ) -> Result<RebuildReport, StorageError> {
        // A deleted KB's events are still in the journal — `delete_kb` deletes
        // rows, not history — so replaying them would resurrect data the user
        // deleted.
        if let Some(name) = kb {
            let known: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM knowledge_bases WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )?;
            if known == 0 {
                return Err(StorageError::NotFound(format!(
                    "no knowledge base named `{name}`; a deleted KB's events stay in the journal \
                     and replaying them would resurrect data you deleted"
                )));
            }
        }

        self.begin_immediate()?;
        let outcome = self.rebuild_inner(kb, dry_run);
        match (&outcome, dry_run) {
            (Ok(_), false) => self.commit()?,
            _ => self.rollback()?,
        }
        outcome
    }

    fn rebuild_inner(
        &self,
        kb: Option<&str>,
        dry_run: bool,
    ) -> Result<RebuildReport, StorageError> {
        let mut report = RebuildReport {
            dry_run,
            kb: kb.map(|s| s.to_string()),
            skipped: skipped_tables(),
            ..Default::default()
        };

        // Fact embeddings are read out first and re-applied after the replay,
        // so a rebuild does not silently cost the store its dense fact search.
        let embeddings: Vec<(String, Vec<u8>, Option<String>)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, embedding, embed_model FROM facts
                  WHERE embedding IS NOT NULL AND (?1 IS NULL OR kb = ?1)",
            )?;
            let rows = stmt.query_map(params![kb], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        // The QUEUE goes first on purpose: deleting facts first cascades queue
        // rows away, and under `--kb` that cascade reaches rows in OTHER KBs
        // whose pair straddles the boundary — a scoped rebuild that quietly
        // deletes another KB's questions is the same class of defect this lane
        // exists to fix.
        //
        // Rule 3 [G4]: an ORPHAN — a row whose facts are already gone — is
        // not deleted, because the replay cannot put it back. The v0.3 write
        // path deleted facts and the FK cascade took ~14.5k questions with
        // them; the migration rescued what was left, and a rebuild that drops
        // the survivors finishes the job the bug started. Preserve in place,
        // count, and say so.
        report.conflicts_preserved_orphan = self.conn.query_row(
            "SELECT COUNT(*) FROM fact_conflict_queue q
              WHERE (?1 IS NULL OR q.kb = ?1)
                AND (NOT EXISTS (SELECT 1 FROM facts f WHERE f.id = q.new_fact_id)
                     OR NOT EXISTS (SELECT 1 FROM facts f WHERE f.id = q.old_fact_id))",
            params![kb],
            |r| r.get::<_, i64>(0),
        )? as usize;
        self.conn.execute(
            "DELETE FROM fact_conflict_queue
              WHERE (?1 IS NULL OR kb = ?1)
                AND EXISTS (SELECT 1 FROM facts f WHERE f.id = new_fact_id)
                AND EXISTS (SELECT 1 FROM facts f WHERE f.id = old_fact_id)",
            params![kb],
        )?;
        self.conn.execute(
            "DELETE FROM fact_decisions WHERE (?1 IS NULL OR kb = ?1)",
            params![kb],
        )?;
        self.conn.execute(
            "DELETE FROM derivations WHERE memory_id IN
                 (SELECT id FROM memories WHERE (?1 IS NULL OR kb = ?1))",
            params![kb],
        )?;
        self.conn.execute(
            "DELETE FROM fact_extraction_log WHERE memory_id IN
                 (SELECT id FROM memories WHERE (?1 IS NULL OR kb = ?1))",
            params![kb],
        )?;
        self.conn.execute(
            "DELETE FROM facts WHERE (?1 IS NULL OR kb = ?1)",
            params![kb],
        )?;
        // Lessons are a journalled projection like any other (D24): dropping
        // and replaying them is what keeps invariant 3 true on a store where
        // anyone ran `engram lessons add`.
        self.conn.execute(
            "DELETE FROM lessons WHERE (?1 IS NULL OR kb = ?1)",
            params![kb],
        )?;

        // The open-key unique index constrains the FINAL state, not every
        // intermediate one: a question that was later collapsed is `open` at
        // the moment its `conflict.opened` replays, alongside the survivor it
        // was merged into. Leaving the index up makes `INSERT OR IGNORE`
        // silently drop the loser, and the row vanishes from the rebuild.
        // Recreating it afterwards is also the check that the replayed state
        // actually satisfies it.
        self.conn
            .execute_batch("DROP INDEX IF EXISTS idx_fcq_open_key")?;

        let live_kbs: std::collections::HashSet<String> = {
            let mut stmt = self.conn.prepare("SELECT name FROM knowledge_bases")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?.into_iter().collect()
        };

        for event in self.events_slice(0, kb, None)? {
            // An unscoped rebuild replays only events whose KB still exists.
            if let Some(event_kb) = &event.kb {
                if !live_kbs.contains(event_kb) {
                    report.events_skipped_deleted_kb += 1;
                    continue;
                }
            }
            match apply(self, &event) {
                Ok(()) => report.events_replayed += 1,
                Err(StorageError::NotFound(_)) if event.kind == EventKind::ConflictOpened => {
                    report.conflicts_skipped_orphan += 1;
                }
                Err(e) => return Err(e),
            }
        }

        for (id, blob, model) in &embeddings {
            report.embeddings_restored += self.conn.execute(
                "UPDATE facts SET embedding = ?1, embed_model = ?2 WHERE id = ?3",
                params![blob, model, id],
            )?;
        }

        self.conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_fcq_open_key
                 ON fact_conflict_queue(kb, decision_key)
                 WHERE status='open' AND decision_key IS NOT NULL",
        )?;

        report.facts = self.count_rows("facts", kb)?;
        report.conflicts = self.count_rows("fact_conflict_queue", kb)?;
        report.decisions = self.count_rows("fact_decisions", kb)?;
        report.derivations = self.count_rows("derivations", None)?;
        report.extraction_markers = self.count_rows("fact_extraction_log", None)?;
        report.lessons = self.count_rows("lessons", kb)?;
        Ok(report)
    }

    fn count_rows(&self, table: &str, kb: Option<&str>) -> Result<i64, StorageError> {
        let sql = match kb {
            Some(_) => format!("SELECT COUNT(*) FROM {table} WHERE kb = ?1"),
            None => format!("SELECT COUNT(*) FROM {table}"),
        };
        Ok(match kb {
            Some(k) => self.conn.query_row(&sql, params![k], |r| r.get(0))?,
            None => self.conn.query_row(&sql, [], |r| r.get(0))?,
        })
    }
}

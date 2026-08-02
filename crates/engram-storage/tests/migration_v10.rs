//! The v9→v10 migration, exercised against a real schema-v9 store.
//!
//! `fixtures/v9_store.sql` was captured from the CLI before `CURRENT_VERSION`
//! moved to 10; the tree can no longer produce one. Every test here loads it
//! into a TempDir file and lets `SqliteStore::open` run the real migration —
//! never `:memory:`, because `VACUUM INTO` needs a directory, and never the
//! user's store, because `ENGRAM_STORE` is pinned at the TempDir for the
//! test's lifetime.

use std::path::PathBuf;
use tempfile::TempDir;

/// The captured v9 dump. `include_str!` so a missing fixture is a compile
/// error rather than a test that silently passes on an empty store.
const V9_DUMP: &str = include_str!("fixtures/v9_store.sql");

/// A TempDir holding a freshly restored v9 store, NOT yet opened by
/// `SqliteStore` (opening is what migrates it, and several tests need to look
/// at the store before that happens).
pub fn restore_v9() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let db = tmp.path().join("db.sqlite");
    let conn = rusqlite::Connection::open(&db).expect("create fixture db");
    conn.execute_batch(V9_DUMP).expect("load v9 fixture");
    drop(conn);
    (tmp, db)
}

fn version_of(db: &PathBuf) -> i32 {
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .expect("schema_version row")
}

fn count(db: &PathBuf, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row(sql, [], |r| r.get(0)).expect(sql)
}

#[test]
fn the_fixture_is_a_v9_store_carrying_every_case_the_migration_must_handle() {
    let (_tmp, db) = restore_v9();
    assert_eq!(version_of(&db), 9, "fixture must still be schema v9");

    // The ordinary corpus: two KBs, so `admin rebuild --kb` has something to
    // leave alone.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM memories"), 8);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM facts"), 11);
    assert_eq!(count(&db, "SELECT COUNT(DISTINCT kb) FROM facts"), 2);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_extraction_log"),
        1,
        "the derivations backfill needs a marker to backfill from"
    );

    // Three caller verdicts, so the migration's verdict inference has all
    // three outcomes to infer.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_conflict_queue WHERE status='resolved' AND resolved_by='caller'"
        ),
        3
    );

    // The clamp case: valid_from POST-DATES superseded_at.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM facts WHERE valid_from > superseded_at AND superseded_by IS NOT NULL"
        ),
        1,
        "without this row the MAX() clamp is untested code"
    );

    // A closed interval with no successor — step 2's third UPDATE.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM facts WHERE valid_until IS NOT NULL AND superseded_by IS NULL"
        ),
        1
    );

    // Two OPEN rows about the same subject/relation/object pair: the
    // duplicate-key collapse, without which the unique index aborts.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_conflict_queue q
               JOIN facts n ON n.id = q.new_fact_id
               JOIN facts o ON o.id = q.old_fact_id
              WHERE q.status='open' AND n.object_norm='braga' AND o.object_norm='porto'"
        ),
        2
    );

    // Orphans: rows whose facts are gone. They cannot be seeded through the
    // CLI at all — the FK cascade takes the queue row with the facts.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_conflict_queue q
              WHERE NOT EXISTS (SELECT 1 FROM facts f WHERE f.id = q.new_fact_id)"
        ),
        2
    );

    // A fact re-asserted seven times. Every other fixture fact has zero
    // confirmations, and a digest that only ever compares 0 to 0 cannot
    // notice a rebuild that resets the count — which is exactly what the
    // release gate found.
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM facts WHERE confirmations = 7"),
        1,
        "without a nonzero count the confirmations column is untested"
    );
}

// ---------------------------------------------------------------------------
// The migration itself. `SqliteStore::open` on a restored v9 file runs it.
// ---------------------------------------------------------------------------

use engram_storage::sqlite::SqliteStore;

/// Open the restored fixture, which migrates it. `ENGRAM_STORE` is pinned at
/// the same TempDir for the call's lifetime so nothing can reach the real
/// store even through a helper that ignores the explicit path.
fn migrate(db: &PathBuf) -> SqliteStore {
    let previous = std::env::var_os("ENGRAM_STORE");
    std::env::set_var("ENGRAM_STORE", db.parent().unwrap());
    let store = SqliteStore::open(db).expect("migrate v9 -> v10");
    match previous {
        Some(v) => std::env::set_var("ENGRAM_STORE", v),
        None => std::env::remove_var("ENGRAM_STORE"),
    }
    store
}

fn one(db: &PathBuf, sql: &str) -> String {
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
        .map(|v| v.unwrap_or_default())
        .unwrap_or_default()
}

fn backup_names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("pre-v10-backup"))
        .collect();
    v.sort();
    v
}

fn digest(db: &PathBuf) -> Vec<(String, i64)> {
    [
        "events",
        "facts",
        "fact_decisions",
        "fact_conflict_queue",
        "derivations",
    ]
    .iter()
    .map(|t| {
        (
            t.to_string(),
            count(db, &format!("SELECT COUNT(*) FROM {t}")),
        )
    })
    .collect()
}

#[test]
fn a_v9_store_migrates_to_v10_with_every_object() {
    let (_tmp, db) = restore_v9();
    let store = migrate(&db);
    assert_eq!(
        store.schema_version().unwrap(),
        engram_storage::sqlite::CURRENT_VERSION,
        "a migrated store lands on the version this binary ships, not on a literal"
    );

    for sql in [
        "SELECT COUNT(*) FROM events",
        "SELECT COUNT(*) FROM fact_decisions",
        "SELECT COUNT(*) FROM derivations",
        "SELECT COUNT(*) FROM migration_log",
    ] {
        assert!(count(&db, sql) >= 0, "{sql} failed");
    }
    for col in ["closed_by", "close_reason", "close_authority"] {
        assert_eq!(
            count(
                &db,
                &format!("SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name = '{col}'")
            ),
            1,
            "facts.{col} missing"
        );
    }
    for col in ["decision_key", "member_key", "registry_version"] {
        assert_eq!(
            count(
                &db,
                &format!(
                "SELECT COUNT(*) FROM pragma_table_info('fact_conflict_queue') WHERE name = '{col}'"
            )
            ),
            1,
            "fact_conflict_queue.{col} missing"
        );
    }
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM pragma_table_info('fact_decisions')
              WHERE name = 'loser_object_norm'"
        ),
        1,
        "the v12 column must exist however the store reached the current version"
    );
    for idx in [
        "idx_facts_open_interval",
        "idx_facts_relation",
        "idx_facts_scope_open",
        "idx_fcq_open_key",
        "idx_fact_decisions_key",
    ] {
        assert_eq!(
            count(
                &db,
                &format!("SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='{idx}'")
            ),
            1,
            "index {idx} missing"
        );
    }
    // Every fact and every queue row is represented in the journal, or a
    // rebuild starts from an incomplete log.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM events WHERE kind='fact.observed'"
        ),
        count(&db, "SELECT COUNT(*) FROM facts")
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM events WHERE kind='conflict.opened'"
        ),
        count(&db, "SELECT COUNT(*) FROM fact_conflict_queue")
    );
}

#[test]
fn the_backup_is_written_before_the_transaction_and_still_reads_as_v9() {
    let (tmp, db) = restore_v9();
    let _store = migrate(&db);
    let backups = backup_names(tmp.path());
    assert_eq!(backups.len(), 1, "exactly one backup, got {backups:?}");
    let stamp = backups[0].rsplit('.').next().unwrap();
    assert_eq!(stamp.len(), 14, "timestamped filename: {}", backups[0]);
    assert!(stamp.chars().all(|c| c.is_ascii_digit()));
    let restored = tmp.path().join(&backups[0]);
    assert_eq!(
        version_of(&restored),
        9,
        "the backup must predate the migration — it is the rollback"
    );
}

#[test]
fn no_backup_for_memory_stores_fresh_stores_or_second_opens() {
    let store = SqliteStore::open_in_memory().unwrap();
    assert_eq!(
        store.schema_version().unwrap(),
        engram_storage::sqlite::CURRENT_VERSION,
        "a migrated store lands on the version this binary ships, not on a literal"
    );

    let tmp = TempDir::new().unwrap();
    let fresh = tmp.path().join("db.sqlite");
    let _ = migrate(&fresh);
    assert!(
        backup_names(tmp.path()).is_empty(),
        "a fresh store has no history to back up"
    );

    let (tmp2, db) = restore_v9();
    let _ = migrate(&db);
    let before = backup_names(tmp2.path());
    assert_eq!(before.len(), 1);
    let _ = migrate(&db);
    assert_eq!(
        backup_names(tmp2.path()),
        before,
        "an up-to-date store must not pay for a copy on every command"
    );
}

#[test]
fn the_clamp_prevents_negative_length_intervals() {
    let (_tmp, db) = restore_v9();
    let _store = migrate(&db);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (from, until): (String, String) = conn
        .query_row(
            "SELECT valid_from, valid_until FROM facts
              WHERE id='cccccccc-0000-4000-8000-000000000001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        until, from,
        "a future-dated valid_from must clamp its close, not invert the interval"
    );
}

#[test]
fn resolution_maps_to_close_reason_and_authority() {
    let (_tmp, db) = restore_v9();
    let _store = migrate(&db);
    // A caller correction is `corrected`/`caller`. The safety audit reads
    // `close_authority='heuristic'` as "a machine retired this", so a blanket
    // heuristic would make it report the exact opposite of the truth.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM facts
              WHERE resolution='caller'
                AND (close_reason<>'corrected' OR close_authority<>'caller')"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM facts
              WHERE superseded_by IS NOT NULL AND COALESCE(resolution,'') <> 'caller'
                AND (close_reason<>'superseded' OR close_authority<>'heuristic')"
        ),
        0
    );
    // A closed interval with no successor: `stopped`/`explicit_end`, never
    // NULL — a NULL reason is an unreplayable event.
    assert_eq!(
        one(
            &db,
            "SELECT close_reason || '/' || close_authority FROM facts
              WHERE id='cccccccc-0000-4000-8000-000000000002'"
        ),
        "stopped/explicit_end"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM facts WHERE valid_until IS NOT NULL AND close_reason IS NULL"
        ),
        0
    );
}

#[test]
fn resolved_queue_rows_become_decisions_with_inferred_verdicts() {
    let (_tmp, db) = restore_v9();
    let _store = migrate(&db);
    for verdict in ["close_old", "reject_new", "keep_both"] {
        assert!(
            count(
                &db,
                &format!("SELECT COUNT(*) FROM fact_decisions WHERE verdict='{verdict}'")
            ) >= 1,
            "no decision inferred for verdict {verdict}"
        );
    }
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_decisions WHERE authority='caller'"
        ),
        3,
        "resolved_by='caller' must migrate to authority='caller', not 'machine'"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_decisions d
              WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.id = d.event_id)"
        ),
        0,
        "every decision row must name the event that recorded it"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_decisions WHERE pair_hash=''"
        ),
        0
    );
    // G3. The v9 supersession pointers already say WHICH value lost, so the
    // migrated decisions arrive oriented — a legacy ruling should not depend
    // forever on a queue row that a later `kb delete` can cascade away.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_decisions d
              WHERE d.verdict IN ('close_old','reject_new')
                AND d.loser_object_norm IS NULL"
        ),
        0,
        "a directional decision with no recorded loser replays on pair order"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_decisions d
               JOIN fact_conflict_queue q ON q.id = d.conflict_id
               JOIN facts f ON f.id = CASE d.verdict WHEN 'close_old'
                                      THEN q.old_fact_id ELSE q.new_fact_id END
              WHERE d.verdict IN ('close_old','reject_new')
                AND d.loser_object_norm <> f.object_norm"
        ),
        0,
        "the recorded loser must be the value the verdict retired"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_decisions
              WHERE verdict IN ('keep_both','void') AND loser_object_norm IS NOT NULL"
        ),
        0,
        "a verdict that retires nothing must record nothing"
    );
}

#[test]
fn duplicate_open_keys_collapse_before_the_unique_index() {
    let (_tmp, db) = restore_v9();
    let _store = migrate(&db);
    // The later duplicate loses; the question as FIRST asked survives, because
    // its id may already be in an agent's notes or a saved resolve_command.
    // Asserted by created_at ordering rather than by id, so the property is
    // pinned even if the fixture is regenerated with different uuids.
    assert_eq!(
        one(
            &db,
            "SELECT status FROM fact_conflict_queue
              WHERE decision_key = (SELECT decision_key FROM fact_conflict_queue
                                     WHERE id='dddddddd-0000-4000-8000-000000000001')
              ORDER BY created_at ASC, id ASC LIMIT 1"
        ),
        "open",
        "the first asking of a question must survive its own duplicates"
    );
    assert_eq!(
        one(
            &db,
            "SELECT status FROM fact_conflict_queue
              WHERE decision_key = (SELECT decision_key FROM fact_conflict_queue
                                     WHERE id='dddddddd-0000-4000-8000-000000000001')
              ORDER BY created_at DESC, id DESC LIMIT 1"
        ),
        "superseded_question",
        "and it is never `resolved` — nobody answered it"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_decisions d
              JOIN fact_conflict_queue q ON q.id = d.conflict_id
              WHERE d.verdict='void' AND q.status='superseded_question'"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM events
              WHERE kind='decision.recorded' AND payload LIKE '%collapsed_into%'"
        ),
        1,
        "the merge must be rebuildable, not inferred from note text"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM (SELECT kb, decision_key FROM fact_conflict_queue
              WHERE status='open' AND decision_key IS NOT NULL
              GROUP BY kb, decision_key HAVING COUNT(*) > 1)"
        ),
        0,
        "the unique index could not have been created otherwise"
    );
}

#[test]
fn orphaned_queue_rows_stay_open_with_a_null_key() {
    let (_tmp, db) = restore_v9();
    let _store = migrate(&db);
    assert_eq!(
        one(
            &db,
            "SELECT status FROM fact_conflict_queue WHERE id='dddddddd-0000-4000-8000-000000000002'"
        ),
        "open",
        "an orphan is unanswerable, not answered"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_conflict_queue
              WHERE id='dddddddd-0000-4000-8000-000000000002' AND decision_key IS NULL"
        ),
        1,
        "inventing a key would create a decision that replays onto an unrelated pair"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_decisions
              WHERE conflict_id='dddddddd-0000-4000-8000-000000000003'"
        ),
        0
    );
    let summary = one(
        &db,
        "SELECT summary_json FROM migration_log WHERE version=10",
    );
    let report: serde_json::Value = serde_json::from_str(&summary).unwrap();
    assert_eq!(report["queue_rows_skipped_orphan"], 1);
}

#[test]
fn open_queue_rows_get_conflict_opened_events() {
    let (_tmp, db) = restore_v9();
    let _store = migrate(&db);
    // Without these a rebuild cannot restore the open queue, and invariant 3
    // fails on any store with a backlog.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_conflict_queue q
              WHERE q.status='open'
                AND NOT EXISTS (SELECT 1 FROM events e
                                 WHERE e.kind='conflict.opened' AND e.subject_id = q.id)"
        ),
        0
    );
}

#[test]
fn migrating_twice_changes_nothing() {
    let (_tmp, db) = restore_v9();
    let store = migrate(&db);
    let before = digest(&db);
    drop(store);
    // Force the whole step to run again: the version stamp is what makes a
    // second open a no-op, so idempotency is only proved by removing it.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE schema_version SET version = 9", [])
            .unwrap();
    }
    let _store = migrate(&db);
    assert_eq!(
        digest(&db),
        before,
        "a re-run must append zero events and zero rows"
    );
}

// ---------------------------------------------------------------------------
// Invariant 3: `admin rebuild` reproduces the fact and adjudication
// projections from the journal alone. Proved against the migrated fixture,
// because a store whose journal is entirely synthesized is the hardest input
// the property has.
// ---------------------------------------------------------------------------

/// A digest of everything the rebuild claims to own, EXCLUDING the columns it
/// honestly says it does not (`embedding`, `embed_model`).
fn projection_digest(db: &PathBuf) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).expect("open");
    let mut out = Vec::new();
    for sql in [
        "SELECT id || '|' || subject_norm || '|' || predicate || '|' || object_norm
              || '|' || COALESCE(valid_from,'') || '|' || COALESCE(valid_until,'')
              || '|' || COALESCE(close_reason,'') || '|' || COALESCE(close_authority,'')
              || '|' || COALESCE(closed_by,'') || '|' || COALESCE(superseded_by,'')
              || '|' || COALESCE(superseded_at,'') || '|' || kb || '|' || diary
              || '|' || fact_type || '|' || confirmations
           FROM facts ORDER BY id",
        // EVERY queue row, orphans included. The old digest filtered rows
        // whose facts are absent and so could not see the rebuild deleting
        // them — the exclusion was the reason blocker 4 shipped.
        "SELECT id || '|' || new_fact_id || '|' || old_fact_id || '|' || status
              || '|' || kb || '|' || diary || '|' || detector
              || '|' || COALESCE(suggested,'') || '|' || COALESCE(decision_key,'')
              || '|' || COALESCE(member_key,'') || '|' || COALESCE(registry_version,'')
              || '|' || COALESCE(resolved_at,'') || '|' || COALESCE(resolved_by,'')
           FROM fact_conflict_queue q
          ORDER BY id",
        // `loser_object_norm` is in the digest because it is the one column a
        // rebuild cannot re-derive: the queue row it was taken from may be
        // gone by then, so the event has to carry it or the orientation is
        // lost on the next `admin rebuild` [G3].
        "SELECT id || '|' || kb || '|' || decision_key || '|' || verdict
              || '|' || authority || '|' || subject_norm || '|' || relation
              || '|' || pair_hash || '|' || COALESCE(conflict_id,'')
              || '|' || COALESCE(loser_object_norm,'')
           FROM fact_decisions ORDER BY id",
        "SELECT content_hash || '|' || extractor_id || '|' || memory_id || '|' || fact_count
           FROM derivations ORDER BY content_hash, extractor_id",
        "SELECT memory_id FROM fact_extraction_log ORDER BY memory_id",
    ] {
        let mut stmt = conn.prepare(sql).expect(sql);
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        out.extend(rows);
        out.push("--".to_string());
    }
    out
}

#[test]
fn rebuild_reproduces_the_projection_from_the_journal_alone() {
    let (_tmp, db) = restore_v9();
    let store = migrate(&db);
    let before = projection_digest(&db);
    let report = store.rebuild_projections(None, false).unwrap();
    assert!(report.events_replayed > 0);
    assert_eq!(
        projection_digest(&db),
        before,
        "the projection must come back identical, or the journal is not the source of truth"
    );
    // The orphaned queue rows are the ONE place the JOURNAL cannot reproduce
    // the projection, so the rebuild leaves those rows where they are and
    // says so: two events unreplayable, two rows preserved.
    assert_eq!(report.conflicts_skipped_orphan, 2);
    assert_eq!(report.conflicts_preserved_orphan, 2);
    assert!(report
        .skipped
        .iter()
        .any(|s| s.table == "memories" && !s.reason.is_empty()));
    assert!(report.skipped.iter().any(|s| s.table == "chunks"));
    assert!(report.skipped.iter().any(|s| s.table == "claims"));
    assert!(report.skipped.iter().any(|s| s.table == "entities"));
}

/// Blocker 4 / ruling G4. The fixture's two orphans are the shape a real
/// store holds ~15k of: questions whose facts were deleted by the v0.3 write
/// path, migrated with their history intact. A rebuild that deletes the queue
/// wholesale and then cannot recreate them destroys the only surviving record
/// that they were ever asked.
#[test]
fn rebuild_preserves_orphan_conflict_rows_it_cannot_recreate() {
    let (_tmp, db) = restore_v9();
    let store = migrate(&db);
    const ORPHANS: [&str; 2] = [
        "dddddddd-0000-4000-8000-000000000002", // open
        "dddddddd-0000-4000-8000-000000000003", // resolved
    ];
    let queue_before = count(&db, "SELECT COUNT(*) FROM fact_conflict_queue");
    for id in ORPHANS {
        assert_eq!(
            count(
                &db,
                &format!("SELECT COUNT(*) FROM fact_conflict_queue WHERE id = '{id}'")
            ),
            1,
            "the migration must carry {id} over"
        );
    }

    let report = store.rebuild_projections(None, false).unwrap();
    assert_eq!(report.conflicts_preserved_orphan, 2);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_conflict_queue"),
        queue_before,
        "a rebuild must not shrink the queue by the rows it cannot rebuild"
    );
    for id in ORPHANS {
        assert_eq!(
            count(
                &db,
                &format!("SELECT COUNT(*) FROM fact_conflict_queue WHERE id = '{id}'")
            ),
            1,
            "rebuild deleted orphan {id} and could not put it back"
        );
    }
    assert_eq!(
        one(
            &db,
            "SELECT status FROM fact_conflict_queue
              WHERE id = 'dddddddd-0000-4000-8000-000000000003'"
        ),
        "resolved",
        "a preserved orphan keeps its answer, not just its row"
    );
    // …and a second rebuild is still a no-op: preservation must not double.
    let again = store.rebuild_projections(None, false).unwrap();
    assert_eq!(again.conflicts_preserved_orphan, 2);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_conflict_queue"),
        queue_before
    );
}

/// Blocker 5 / ruling G5. `confirmations` is a trust signal earned over
/// months of re-assertion; a rebuild that silently returns it to zero has
/// downgraded the store's own confidence in what it knows.
#[test]
fn rebuild_preserves_historical_confirmation_counts() {
    let (_tmp, db) = restore_v9();
    let store = migrate(&db);
    const RE_ASSERTED: &str = "53684213-b628-41a0-9fee-372ec4a20bb0";
    let counted = |db: &PathBuf| {
        count(
            db,
            &format!("SELECT confirmations FROM facts WHERE id = '{RE_ASSERTED}'"),
        )
    };
    assert_eq!(counted(&db), 7, "the migration must carry the count over");

    store.rebuild_projections(None, false).unwrap();
    assert_eq!(
        counted(&db),
        7,
        "the journal has to carry the count, or a rebuild resets it to zero"
    );
}

#[test]
fn a_dry_run_writes_nothing() {
    let (_tmp, db) = restore_v9();
    let store = migrate(&db);
    let before = projection_digest(&db);
    let events_before = count(&db, "SELECT COUNT(*) FROM events");
    let report = store.rebuild_projections(None, true).unwrap();
    assert!(report.dry_run);
    assert_eq!(projection_digest(&db), before);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM events"),
        events_before,
        "a rebuild that appends is a rebuild that grows its own input"
    );
}

#[test]
fn rebuild_kb_touches_only_that_kb() {
    let (_tmp, db) = restore_v9();
    let store = migrate(&db);
    let work_before: Vec<String> = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare("SELECT id FROM facts WHERE kb='work' ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert!(!work_before.is_empty());
    let report = store.rebuild_projections(Some("personal"), false).unwrap();
    assert_eq!(report.kb.as_deref(), Some("personal"));
    let work_after: Vec<String> = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare("SELECT id FROM facts WHERE kb='work' ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(work_after, work_before, "another KB must be untouched");
    // …including its questions, which the FK cascade would have taken if the
    // facts had been deleted before the queue.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_conflict_queue WHERE kb='work'"
        ),
        1
    );
}

#[test]
fn rebuilding_a_deleted_kb_is_refused_rather_than_resurrecting_it() {
    let (_tmp, db) = restore_v9();
    let store = migrate(&db);
    let err = store.rebuild_projections(Some("gone"), false).unwrap_err();
    assert!(
        err.to_string().contains("resurrect"),
        "the refusal must say why: {err}"
    );
}

#[test]
fn an_unknown_event_kind_is_an_error_not_a_silent_skip() {
    let (_tmp, db) = restore_v9();
    let store = migrate(&db);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO events(id, ts, actor, kind, payload)
             VALUES ('22222222-2222-4222-8222-222222222222',
                     '2026-01-01T00:00:00+00:00', 'cli:default', 'memory.exploded', '{}')",
            [],
        )
        .unwrap();
    }
    let err = store.rebuild_projections(None, false).unwrap_err();
    assert!(
        matches!(err, engram_storage::StorageError::MalformedRow(_)),
        "a skipped event is a projection that silently diverges from its journal: {err:?}"
    );
}

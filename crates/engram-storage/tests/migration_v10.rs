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
}

//! Schema v9 → v10: the store gains a journal, and every projection gains
//! the columns that make it rebuildable from one.
//!
//! The hard part is not the DDL. It is that a v9 store already holds the
//! answers to ~14.5k adjudication questions in a shape (`fact_conflict_queue`
//! rows plus `facts.superseded_by`) that no replay can read, and those answers
//! are the only record of what a person decided. This migration turns them
//! into decisions keyed the same way a wave-3 write keys them, so a caller's
//! past ruling replays instead of being asked again.
//!
//! Everything here is idempotent by construction: genesis event ids are
//! `uuid5` of their source row, every projection insert is `INSERT OR IGNORE`
//! on its primary key, every backfill `UPDATE` is `COALESCE`-guarded or
//! filtered to rows that still need it, and the duplicate-key collapse is a
//! no-op on a second run because its losers are no longer `status='open'`.

use crate::error::StorageError;
use crate::journal::{payload, EventDraft, ACTOR_MIGRATION_V10};
use crate::sqlite::SqliteStore;
use engram_core::events::EventKind;
use engram_core::keys::{
    content_hash, object_pair_hash, semantic_key_encode, SEMANTIC_KEY_VERSION,
};
use engram_core::relations::{canonical_relation, member_identity, REGISTRY_VERSION};
use rusqlite::params;
use std::path::PathBuf;
use uuid::Uuid;

/// Namespace for deterministic genesis event ids. Fixed forever: a re-run
/// must mint the SAME id or `INSERT OR IGNORE` stops being idempotent and a
/// second migration doubles the journal.
const GENESIS_NAMESPACE: Uuid = Uuid::from_u128(0x7b2e4c10_9d55_5a41_8f03_6c19ab2d7e84);

/// Queue status for a question that was never answered, only merged into an
/// identical one. Not `resolved` — nobody decided it — and invisible to every
/// `status='open'` listing.
pub const STATUS_SUPERSEDED_QUESTION: &str = "superseded_question";

/// `fact_decisions.loser_object_norm` is here as well as in `migrate_v12`'s
/// ALTER [G3]: a store that first meets this table at v10 gets the column
/// immediately and v12 is a no-op on it; a store whose table already exists
/// gets it at v12. Both land on the same shape, which is what
/// `add_column_if_missing` is for.
///
/// No SQL comments in this string, ever. SQLite keeps the CREATE text verbatim
/// in `sqlite_master` and rewrites it on `ALTER TABLE`; a `--` line comment
/// survives that rewrite badly enough to leave the statement unparseable, so a
/// note written here disables the next migration to touch the table.
pub const V10_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS fact_decisions (
    id            TEXT PRIMARY KEY,
    kb            TEXT NOT NULL DEFAULT 'default',
    diary         TEXT NOT NULL DEFAULT 'default',
    decision_key  TEXT NOT NULL,
    subject_norm  TEXT NOT NULL,
    relation      TEXT NOT NULL,
    pair_hash     TEXT NOT NULL,
    verdict       TEXT NOT NULL,
    authority     TEXT NOT NULL,
    actor         TEXT NOT NULL,
    note          TEXT,
    conflict_id   TEXT,
    event_id      TEXT NOT NULL,
    key_version   TEXT NOT NULL,
    decided_at    TEXT NOT NULL,
    loser_object_norm TEXT
);
CREATE INDEX IF NOT EXISTS idx_fact_decisions_key
    ON fact_decisions(kb, decision_key, authority, decided_at DESC);

CREATE TABLE IF NOT EXISTS derivations (
    content_hash    TEXT NOT NULL,
    extractor_id    TEXT NOT NULL,
    memory_id       TEXT NOT NULL,
    derived_at      TEXT NOT NULL,
    model           TEXT,
    fact_count      INTEGER NOT NULL DEFAULT 0,
    output_fact_ids TEXT NOT NULL DEFAULT '[]',
    event_id        TEXT NOT NULL,
    PRIMARY KEY (content_hash, extractor_id)
);
CREATE INDEX IF NOT EXISTS idx_derivations_memory ON derivations(memory_id);

CREATE TABLE IF NOT EXISTS migration_log (
    version      INTEGER PRIMARY KEY,
    ran_at       TEXT NOT NULL,
    summary_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_facts_open_interval
    ON facts(kb, diary, subject_norm, predicate)
    WHERE valid_until IS NULL AND superseded_by IS NULL;
CREATE INDEX IF NOT EXISTS idx_facts_relation
    ON facts(kb, diary, subject_norm, predicate);
CREATE INDEX IF NOT EXISTS idx_facts_scope_open
    ON facts(kb, diary, valid_until, superseded_by);
"#;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MigrationV10Report {
    pub backup_path: Option<PathBuf>,
    pub facts_backfilled: usize,
    pub intervals_closed: usize,
    pub genesis_events: usize,
    pub decisions_recorded: usize,
    pub derivations_backfilled: usize,
    pub open_conflicts_keyed: usize,
    pub open_conflicts_collapsed: usize,
    pub queue_rows_skipped_orphan: usize,
    pub elapsed_ms: u128,
}

/// Deterministic id for a synthesized event.
fn genesis_id(kind: EventKind, subject: &str, discriminator: &str) -> Uuid {
    let key = format!("{}\u{1f}{subject}\u{1f}{discriminator}", kind.as_str());
    Uuid::new_v5(&GENESIS_NAMESPACE, key.as_bytes())
}

fn ts_of(raw: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
}

/// One v9 memory row, in the shape the genesis generator needs.
struct MemorySeed {
    id: String,
    kb: String,
    diary: String,
    importance: i64,
    content: String,
    event_time: Option<String>,
    created_at: String,
}

/// One orphaned queue row: real, keyless, and still entitled to its event.
struct OrphanSeed {
    id: String,
    kb: String,
    diary: String,
    detector: String,
    score: Option<f32>,
    suggested: Option<String>,
    note: Option<String>,
    created_at: String,
    new_fact_id: String,
    old_fact_id: String,
}

/// One v9 fact row, in the shape the genesis generator needs.
struct FactSeed {
    id: String,
    kb: String,
    diary: String,
    created_at: String,
    valid_until: Option<String>,
    superseded_by: Option<String>,
    superseded_at: Option<String>,
    close_reason: Option<String>,
    close_authority: Option<String>,
}

impl SqliteStore {
    /// The whole v9→v10 step, inside the caller's migration transaction.
    pub(crate) fn migrate_v10(
        &self,
        backup_path: Option<PathBuf>,
    ) -> Result<MigrationV10Report, StorageError> {
        let started = std::time::Instant::now();
        let mut report = MigrationV10Report {
            backup_path,
            ..Default::default()
        };

        self.v10_ddl()?;
        let (backfilled, closed) = self.v10_interval_backfill()?;
        report.facts_backfilled = backfilled;
        report.intervals_closed = closed;

        // [G9] The migration's genesis IS this store's own past, rewritten
        // into events. Stamping it `Imported` would re-gate every close the
        // store made before v10 on its next rebuild.
        let genesis = self.synthesize_genesis(ACTOR_MIGRATION_V10, crate::journal::Origin::Own)?;
        report.genesis_events = genesis.events;
        report.derivations_backfilled = genesis.derivations;

        let resolved = self.v10_resolved_queue_rows()?;
        report.decisions_recorded += resolved.decisions;
        report.queue_rows_skipped_orphan += resolved.skipped_orphan;
        report.genesis_events += resolved.events;

        let open = self.v10_open_queue_rows()?;
        report.open_conflicts_keyed = open.keyed;
        report.genesis_events += open.events;

        let collapsed = self.v10_collapse_duplicate_keys()?;
        report.open_conflicts_collapsed = collapsed.collapsed;
        report.decisions_recorded += collapsed.collapsed;
        report.genesis_events += collapsed.events;

        // ONLY NOW. Creating this before the collapse is a guaranteed abort on
        // any store that has ever queued the same pair twice — which is every
        // store with a backlog.
        self.conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_fcq_open_key
                 ON fact_conflict_queue(kb, decision_key)
                 WHERE status='open' AND decision_key IS NOT NULL;",
        )?;

        report.elapsed_ms = started.elapsed().as_millis();
        self.conn.execute(
            "INSERT OR REPLACE INTO migration_log(version, ran_at, summary_json)
             VALUES (10, ?1, ?2)",
            params![
                chrono::Utc::now().to_rfc3339(),
                serde_json::to_string(&report)?
            ],
        )?;
        tracing::info!(target: "engram::migration", version = 10, summary = ?report, "v9 -> v10");
        Ok(report)
    }

    fn v10_ddl(&self) -> Result<(), StorageError> {
        self.add_column_if_missing("facts", "closed_by", "TEXT")?;
        self.add_column_if_missing("facts", "close_reason", "TEXT")?;
        self.add_column_if_missing("facts", "close_authority", "TEXT")?;
        self.add_column_if_missing("fact_conflict_queue", "decision_key", "TEXT")?;
        self.add_column_if_missing("fact_conflict_queue", "member_key", "TEXT")?;
        self.add_column_if_missing("fact_conflict_queue", "registry_version", "TEXT")?;
        self.conn
            .execute_batch(crate::journal::JOURNAL_SCHEMA_SQL)?;
        self.conn.execute_batch(V10_SCHEMA_SQL)?;
        Ok(())
    }

    /// Step 2. The `MAX` is `engram_core::interval::close_at`'s clamp written
    /// in SQL, and it is not optional: a fact whose stated `valid_from`
    /// post-dates its write time (a future-dated `lives_in`, an import) would
    /// otherwise migrate to `valid_until < valid_from` — a negative-length
    /// interval that `overlaps` and every downstream consumer read as
    /// nonsense.
    fn v10_interval_backfill(&self) -> Result<(usize, usize), StorageError> {
        let backfilled = self.conn.execute(
            "UPDATE facts SET valid_from = COALESCE(valid_from, created_at)
              WHERE valid_from IS NULL",
            [],
        )?;
        let closed = self.conn.execute(
            "UPDATE facts
                SET valid_until = MAX(valid_from, COALESCE(valid_until, superseded_at, created_at)),
                    closed_by   = COALESCE(closed_by, superseded_by),
                    close_reason = COALESCE(close_reason,
                        CASE WHEN resolution = 'caller' THEN 'corrected' ELSE 'superseded' END),
                    close_authority = COALESCE(close_authority,
                        CASE WHEN resolution = 'caller' THEN 'caller' ELSE 'heuristic' END)
              WHERE superseded_by IS NOT NULL",
            [],
        )?;
        // A closed interval with NO successor. No live v9 write path makes
        // one, but `import_fact_row` restores whatever a v2 export carries, so
        // a store built from an import can hold them — and step 3 would
        // otherwise synthesize a `fact.interval_closed` whose reason and
        // authority are NULL, which is an unreplayable event.
        let stopped = self.conn.execute(
            "UPDATE facts SET close_reason = 'stopped', close_authority = 'explicit_end'
              WHERE valid_until IS NOT NULL AND superseded_by IS NULL AND close_reason IS NULL",
            [],
        )?;
        Ok((backfilled, closed + stopped))
    }

    fn fact_seeds(&self) -> Result<Vec<FactSeed>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kb, diary, created_at, valid_until, superseded_by, superseded_at,
                    close_reason, close_authority
               FROM facts ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(FactSeed {
                id: r.get(0)?,
                kb: r.get(1)?,
                diary: r.get(2)?,
                created_at: r.get(3)?,
                valid_until: r.get(4)?,
                superseded_by: r.get(5)?,
                superseded_at: r.get(6)?,
                close_reason: r.get(7)?,
                close_authority: r.get(8)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }
}

pub(crate) struct GenesisCounts {
    pub events: usize,
    pub derivations: usize,
}

impl SqliteStore {
    /// Step 3, and the SAME generator `import_v2_with_genesis` uses, so a v2
    /// import and a v9 migration produce identical journals for identical
    /// rows. Order matters: it is the replay order, and `admin rebuild`
    /// depends on it.
    ///
    /// [G9] The two callers differ in exactly one thing, and it is the only
    /// thing they SHOULD differ in: a migration synthesizes this store's own
    /// past (`Origin::Own`), a v2 import synthesizes a past it was handed in a
    /// file (`Origin::Imported`). Same events, same ids, different trust.
    pub(crate) fn synthesize_genesis(
        &self,
        actor: &str,
        origin: crate::journal::Origin,
    ) -> Result<GenesisCounts, StorageError> {
        let mut events = 0usize;

        // 1. memory.written, oldest first.
        let memories: Vec<MemorySeed> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, kb, diary, importance, content, event_time, created_at
                   FROM memories ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(MemorySeed {
                    id: r.get(0)?,
                    kb: r.get(1)?,
                    diary: r.get(2)?,
                    importance: r.get(3)?,
                    content: r.get(4)?,
                    event_time: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut hashes: std::collections::HashMap<String, String> =
            std::collections::HashMap::with_capacity(memories.len());
        for m in &memories {
            let hash = content_hash(&m.content);
            let draft = EventDraft::new(
                EventKind::MemoryWritten,
                actor,
                payload::memory_written(
                    &hash,
                    m.importance,
                    &[],
                    m.event_time.as_deref(),
                    "genesis",
                    true,
                ),
            )
            .scope(Some(&m.kb), Some(&m.diary))
            .origin(origin);
            let draft = match Uuid::parse_str(&m.id) {
                Ok(u) => draft.subject(u),
                Err(_) => draft,
            };
            hashes.insert(m.id.clone(), hash);
            if self
                .append_event_with_identity(
                    genesis_id(EventKind::MemoryWritten, &m.id, ""),
                    ts_of(&m.created_at),
                    &draft,
                )?
                .is_some()
            {
                events += 1;
            }
        }

        // 2. derivation.ran per extraction marker, plus its content-keyed
        //    projection row — the memoization table the marker never was.
        let markers: Vec<(String, String, Option<String>, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT memory_id, extracted_at, model, fact_count
                   FROM fact_extraction_log ORDER BY extracted_at, memory_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut derivations = 0usize;
        for (memory_id, extracted_at, model, _fact_count) in &markers {
            let Some(hash) = hashes.get(memory_id) else {
                continue; // a marker whose memory is gone keys nothing
            };
            let fact_ids: Vec<Uuid> = {
                let mut stmt = self.conn.prepare(
                    "SELECT id FROM facts WHERE source_memory_id = ?1 ORDER BY created_at, id",
                )?;
                let rows = stmt.query_map(params![memory_id], |r| r.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .filter_map(|s| Uuid::parse_str(s).ok())
                    .collect()
            };
            let (kb, diary) = self.scope_of_memory(memory_id)?;
            let event_id = genesis_id(EventKind::DerivationRan, memory_id, hash);
            let draft = EventDraft::new(
                EventKind::DerivationRan,
                actor,
                payload::derivation_ran(
                    Uuid::parse_str(memory_id).unwrap_or(Uuid::nil()),
                    hash,
                    crate::journal::EXTRACTOR_FACT_LLM,
                    model.as_deref(),
                    &fact_ids,
                    true,
                ),
            )
            .scope(Some(&kb), Some(&diary))
            .origin(origin);
            let draft = match Uuid::parse_str(memory_id) {
                Ok(u) => draft.subject(u),
                Err(_) => draft,
            };
            if self
                .append_event_with_identity(event_id, ts_of(extracted_at), &draft)?
                .is_some()
            {
                events += 1;
            }
            let ids_json =
                serde_json::to_string(&fact_ids.iter().map(|u| u.to_string()).collect::<Vec<_>>())?;
            let n = self.conn.execute(
                "INSERT OR IGNORE INTO derivations(content_hash, extractor_id, memory_id,
                        derived_at, model, fact_count, output_fact_ids, event_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    hash,
                    crate::journal::EXTRACTOR_FACT_LLM,
                    memory_id,
                    extracted_at,
                    model,
                    fact_ids.len() as i64,
                    ids_json,
                    event_id.to_string(),
                ],
            )?;
            derivations += n;
        }

        // 3. fact.observed, then 4. fact.interval_closed for every closed one.
        let seeds = self.fact_seeds()?;
        for seed in &seeds {
            let Ok(fid) = Uuid::parse_str(&seed.id) else {
                continue;
            };
            let Some(fact) = self.get_fact(fid)? else {
                continue;
            };
            let draft = EventDraft::new(
                EventKind::FactObserved,
                actor,
                payload::fact_observed(&fact, true),
            )
            .scope(Some(&seed.kb), Some(&seed.diary))
            .origin(origin)
            .subject(fid);
            if self
                .append_event_with_identity(
                    genesis_id(EventKind::FactObserved, &seed.id, ""),
                    ts_of(&seed.created_at),
                    &draft,
                )?
                .is_some()
            {
                events += 1;
            }
        }
        for seed in &seeds {
            let Some(until) = seed.valid_until.as_deref() else {
                continue;
            };
            let Ok(fid) = Uuid::parse_str(&seed.id) else {
                continue;
            };
            let closed_by = seed
                .superseded_by
                .as_deref()
                .and_then(|s| Uuid::parse_str(s).ok());
            let draft = EventDraft::new(
                EventKind::FactIntervalClosed,
                actor,
                payload::fact_interval_closed(
                    fid,
                    ts_of(until),
                    closed_by,
                    seed.close_reason.as_deref().unwrap_or("superseded"),
                    seed.close_authority.as_deref().unwrap_or("heuristic"),
                    seed.superseded_by.is_some(),
                    seed.superseded_at.as_deref(),
                    true,
                ),
            )
            .scope(Some(&seed.kb), Some(&seed.diary))
            .origin(origin)
            .subject(fid);
            if self
                .append_event_with_identity(
                    genesis_id(EventKind::FactIntervalClosed, &seed.id, ""),
                    ts_of(until),
                    &draft,
                )?
                .is_some()
            {
                events += 1;
            }
        }

        Ok(GenesisCounts {
            events,
            derivations,
        })
    }

    fn scope_of_memory(&self, memory_id: &str) -> Result<(String, String), StorageError> {
        Ok(self
            .conn
            .query_row(
                "SELECT kb, diary FROM memories WHERE id = ?1",
                params![memory_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or_else(|_| ("default".to_string(), "default".to_string())))
    }
}

/// One queue row joined to both its facts.
struct QueueSeed {
    id: String,
    kb: String,
    diary: String,
    detector: String,
    score: Option<f32>,
    suggested: Option<String>,
    note: Option<String>,
    created_at: String,
    resolved_at: Option<String>,
    resolved_by: Option<String>,
    new_id: String,
    new_subject_norm: String,
    new_predicate: String,
    new_object_norm: String,
    new_superseded_by: Option<String>,
    old_id: String,
    old_object_norm: String,
    old_superseded_by: Option<String>,
}

const QUEUE_JOIN: &str = "SELECT q.id, q.kb, q.diary, q.detector, q.score, q.suggested, q.note,
        q.created_at, q.resolved_at, q.resolved_by,
        nf.id, nf.subject_norm, nf.predicate, nf.object_norm, nf.superseded_by,
        of.id, of.object_norm, of.superseded_by
   FROM fact_conflict_queue q
   JOIN facts nf ON nf.id = q.new_fact_id
   JOIN facts of ON of.id = q.old_fact_id";

fn parse_queue_seed(r: &rusqlite::Row) -> rusqlite::Result<QueueSeed> {
    Ok(QueueSeed {
        id: r.get(0)?,
        kb: r.get(1)?,
        diary: r.get(2)?,
        detector: r.get(3)?,
        score: r.get(4)?,
        suggested: r.get(5)?,
        note: r.get(6)?,
        created_at: r.get(7)?,
        resolved_at: r.get(8)?,
        resolved_by: r.get(9)?,
        new_id: r.get(10)?,
        new_subject_norm: r.get(11)?,
        new_predicate: r.get(12)?,
        new_object_norm: r.get(13)?,
        new_superseded_by: r.get(14)?,
        old_id: r.get(15)?,
        old_object_norm: r.get(16)?,
        old_superseded_by: r.get(17)?,
    })
}

impl QueueSeed {
    fn relation(&self) -> String {
        canonical_relation(&self.new_predicate)
    }

    fn decision_key(&self) -> String {
        semantic_key_encode(
            &self.new_subject_norm,
            &self.relation(),
            &object_pair_hash(&self.new_object_norm, &self.old_object_norm),
        )
    }

    fn member_key(&self) -> String {
        member_identity(&self.new_predicate, &self.new_object_norm)
    }

    /// What the v9 rows say the caller (or the sweep) actually decided.
    fn verdict(&self) -> &'static str {
        if self.new_superseded_by.as_deref() == Some(self.old_id.as_str()) {
            "reject_new"
        } else if self.old_superseded_by.as_deref() == Some(self.new_id.as_str()) {
            "close_old"
        } else {
            "keep_both"
        }
    }

    fn authority(&self) -> &'static str {
        match self.resolved_by.as_deref() {
            Some("caller") => "caller",
            _ => "machine",
        }
    }

    /// WHICH value the v9 row retired [G3]. The supersession pointers say it
    /// outright, so the ~14.5k migrated decisions arrive oriented instead of
    /// depending forever on a queue row that a `kb delete` can cascade away.
    fn loser_object_norm(&self) -> Option<&str> {
        match self.verdict() {
            "reject_new" => Some(&self.new_object_norm),
            "close_old" => Some(&self.old_object_norm),
            _ => None,
        }
    }
}

pub(crate) struct ResolvedCounts {
    pub decisions: usize,
    pub skipped_orphan: usize,
    pub events: usize,
}

pub(crate) struct OpenCounts {
    pub keyed: usize,
    pub events: usize,
}

pub(crate) struct CollapseCounts {
    pub collapsed: usize,
    pub events: usize,
}

impl SqliteStore {
    /// Step 4 — ~14.5k answered questions become decisions that replay.
    fn v10_resolved_queue_rows(&self) -> Result<ResolvedCounts, StorageError> {
        let seeds: Vec<QueueSeed> = {
            // `superseded_question` is excluded deliberately: it is a status
            // THIS migration writes, so treating it as "answered" would make a
            // second run mint a decision for a question nobody answered.
            let sql = format!(
                "{QUEUE_JOIN} WHERE q.status NOT IN ('open', '{STATUS_SUPERSEDED_QUESTION}')
                  ORDER BY q.created_at, q.id"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map([], parse_queue_seed)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        // Rows whose facts are gone never reach the join. Count them so the
        // summary is honest: there is nothing left to key them by, and
        // inventing a key would create a decision that replays onto an
        // unrelated pair.
        let skipped_orphan: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM fact_conflict_queue q
                  WHERE q.status NOT IN ('open', '{STATUS_SUPERSEDED_QUESTION}')
                    AND (NOT EXISTS (SELECT 1 FROM facts f WHERE f.id = q.new_fact_id)
                         OR NOT EXISTS (SELECT 1 FROM facts f WHERE f.id = q.old_fact_id))"
            ),
            [],
            |r| r.get(0),
        )?;

        let mut decisions = 0usize;
        let mut events = 0usize;
        for seed in &seeds {
            let key = seed.decision_key();
            let member = seed.member_key();
            events += self.append_conflict_opened(seed, Some(&key), Some(&member))?;
            let decided_at = seed
                .resolved_at
                .clone()
                .unwrap_or_else(|| seed.created_at.clone());
            let decision_id = Uuid::new_v5(&GENESIS_NAMESPACE, seed.id.as_bytes());
            let event_id = genesis_id(EventKind::DecisionRecorded, &seed.id, seed.verdict());
            let conflict_uuid = Uuid::parse_str(&seed.id).ok();
            let draft = EventDraft::new(
                EventKind::DecisionRecorded,
                ACTOR_MIGRATION_V10,
                payload::decision_recorded(
                    decision_id,
                    &key,
                    SEMANTIC_KEY_VERSION,
                    seed.verdict(),
                    seed.loser_object_norm(),
                    seed.authority(),
                    conflict_uuid,
                    seed.note.as_deref(),
                    seed.resolved_by.as_deref().unwrap_or("migration"),
                    &seed.kb,
                    &seed.diary,
                    &decided_at,
                    None,
                    true,
                ),
            )
            .scope(Some(&seed.kb), Some(&seed.diary));
            let draft = match conflict_uuid {
                Some(u) => draft.subject(u),
                None => draft,
            };
            if self
                .append_event_with_identity(event_id, ts_of(&decided_at), &draft)?
                .is_some()
            {
                events += 1;
            }
            decisions += self.insert_decision_row(
                decision_id,
                &seed.kb,
                &seed.diary,
                &key,
                &seed.new_subject_norm,
                &seed.relation(),
                seed.verdict(),
                seed.loser_object_norm(),
                seed.authority(),
                seed.resolved_by.as_deref().unwrap_or("migration"),
                seed.note.as_deref(),
                conflict_uuid,
                event_id,
                &decided_at,
            )?;
            self.backfill_queue_keys(&seed.id, &key, &member)?;
        }
        Ok(ResolvedCounts {
            decisions,
            skipped_orphan: skipped_orphan as usize,
            events,
        })
    }

    /// Step 5.1 — every OPEN row gets its key and a `conflict.opened` event.
    /// Without the events a rebuild cannot restore the open queue, and
    /// invariant 3 fails on any store with a backlog. Rows whose facts are
    /// gone keep `decision_key = NULL`; the unique index's
    /// `AND decision_key IS NOT NULL` clause is exactly what lets them stay
    /// open and visible instead of being invented or dropped.
    fn v10_open_queue_rows(&self) -> Result<OpenCounts, StorageError> {
        let seeds: Vec<QueueSeed> = {
            let sql = format!("{QUEUE_JOIN} WHERE q.status = 'open' ORDER BY q.created_at, q.id");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map([], parse_queue_seed)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut keyed = 0usize;
        let mut events = 0usize;
        for seed in &seeds {
            let key = seed.decision_key();
            let member = seed.member_key();
            events += self.append_conflict_opened(seed, Some(&key), Some(&member))?;
            keyed += self.backfill_queue_keys(&seed.id, &key, &member)?;
        }
        // Orphaned open rows still deserve their event: the row is real and a
        // rebuild that drops it makes the backlog shrink by itself.
        let orphans: Vec<OrphanSeed> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, kb, diary, detector, score, suggested, note, created_at,
                        new_fact_id, old_fact_id
                   FROM fact_conflict_queue q
                  WHERE NOT EXISTS (SELECT 1 FROM facts f WHERE f.id = q.new_fact_id)
                     OR NOT EXISTS (SELECT 1 FROM facts f WHERE f.id = q.old_fact_id)
                  ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(OrphanSeed {
                    id: r.get(0)?,
                    kb: r.get(1)?,
                    diary: r.get(2)?,
                    detector: r.get(3)?,
                    score: r.get(4)?,
                    suggested: r.get(5)?,
                    note: r.get(6)?,
                    created_at: r.get(7)?,
                    new_fact_id: r.get(8)?,
                    old_fact_id: r.get(9)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for o in &orphans {
            let draft = EventDraft::new(
                EventKind::ConflictOpened,
                ACTOR_MIGRATION_V10,
                payload::conflict_opened(
                    Uuid::parse_str(&o.id).unwrap_or(Uuid::nil()),
                    Uuid::parse_str(&o.new_fact_id).unwrap_or(Uuid::nil()),
                    Uuid::parse_str(&o.old_fact_id).unwrap_or(Uuid::nil()),
                    &o.detector,
                    o.score,
                    o.suggested.as_deref(),
                    o.note.as_deref(),
                    None,
                    None,
                    None,
                    &o.created_at,
                    true,
                ),
            )
            .scope(Some(&o.kb), Some(&o.diary));
            let draft = match Uuid::parse_str(&o.id) {
                Ok(u) => draft.subject(u),
                Err(_) => draft,
            };
            if self
                .append_event_with_identity(
                    genesis_id(EventKind::ConflictOpened, &o.id, ""),
                    ts_of(&o.created_at),
                    &draft,
                )?
                .is_some()
            {
                events += 1;
            }
        }
        Ok(OpenCounts { keyed, events })
    }

    /// Step 5.2 — the step whose absence aborts the migration. A partial
    /// UNIQUE index does not merge duplicates, it rejects them, and duplicate
    /// semantic keys among the open rows are not a corner case: they ARE the
    /// storm (the same pair re-queued on every re-derivation).
    fn v10_collapse_duplicate_keys(&self) -> Result<CollapseCounts, StorageError> {
        let rows: Vec<(String, String, String, String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT kb, diary, decision_key, id, created_at FROM fact_conflict_queue
                  WHERE status='open' AND decision_key IS NOT NULL
                  ORDER BY kb, decision_key, created_at ASC, id ASC",
            )?;
            let r = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            r.collect::<Result<Vec<_>, _>>()?
        };
        let mut collapsed = 0usize;
        let mut events = 0usize;
        let now = chrono::Utc::now().to_rfc3339();
        let mut survivor: Option<(String, String, String)> = None; // (kb, key, id)
        for (kb, diary, key, id, _created_at) in &rows {
            match &survivor {
                Some((skb, skey, sid)) if skb == kb && skey == key => {
                    // The survivor is the question as FIRST asked: its id may
                    // already be in an agent's notes or a saved
                    // resolve_command. Everything later is the same question.
                    self.conn.execute(
                        "UPDATE fact_conflict_queue
                            SET status = ?1, resolved_at = ?2, resolved_by = 'migration',
                                note = COALESCE(note, 'auto: merged into an identical open question')
                          WHERE id = ?3 AND status = 'open'",
                        params![STATUS_SUPERSEDED_QUESTION, now, id],
                    )?;
                    let decision_id =
                        Uuid::new_v5(&GENESIS_NAMESPACE, format!("collapse\u{1f}{id}").as_bytes());
                    let event_id = genesis_id(EventKind::DecisionRecorded, id, "void");
                    let loser = Uuid::parse_str(id).ok();
                    let into = Uuid::parse_str(sid).ok();
                    let draft = EventDraft::new(
                        EventKind::DecisionRecorded,
                        ACTOR_MIGRATION_V10,
                        payload::decision_recorded(
                            decision_id,
                            key,
                            SEMANTIC_KEY_VERSION,
                            "void",
                            // A collapse retires a QUESTION, not a value.
                            None,
                            "machine",
                            loser,
                            Some("auto: merged into an identical open question"),
                            "migration",
                            kb,
                            diary,
                            &now,
                            into,
                            true,
                        ),
                    )
                    .scope(Some(kb), Some(diary));
                    let draft = match loser {
                        Some(u) => draft.subject(u),
                        None => draft,
                    };
                    if self
                        .append_event_with_identity(event_id, ts_of(&now), &draft)?
                        .is_some()
                    {
                        events += 1;
                    }
                    let (subject_norm, relation, _pair) =
                        engram_core::keys::semantic_key_decode(key).ok_or_else(|| {
                            StorageError::MalformedRow(format!("decision key `{key}` is malformed"))
                        })?;
                    self.insert_decision_row(
                        decision_id,
                        kb,
                        diary,
                        key,
                        &subject_norm,
                        &relation,
                        "void",
                        None,
                        "machine",
                        "migration",
                        Some("auto: merged into an identical open question"),
                        loser,
                        event_id,
                        &now,
                    )?;
                    collapsed += 1;
                }
                _ => survivor = Some((kb.clone(), key.clone(), id.clone())),
            }
        }
        Ok(CollapseCounts { collapsed, events })
    }

    fn append_conflict_opened(
        &self,
        seed: &QueueSeed,
        decision_key: Option<&str>,
        member_key: Option<&str>,
    ) -> Result<usize, StorageError> {
        let conflict_uuid = Uuid::parse_str(&seed.id).unwrap_or(Uuid::nil());
        let draft = EventDraft::new(
            EventKind::ConflictOpened,
            ACTOR_MIGRATION_V10,
            payload::conflict_opened(
                conflict_uuid,
                Uuid::parse_str(&seed.new_id).unwrap_or(Uuid::nil()),
                Uuid::parse_str(&seed.old_id).unwrap_or(Uuid::nil()),
                &seed.detector,
                seed.score,
                seed.suggested.as_deref(),
                seed.note.as_deref(),
                decision_key,
                member_key,
                Some(REGISTRY_VERSION),
                &seed.created_at,
                true,
            ),
        )
        .scope(Some(&seed.kb), Some(&seed.diary))
        .subject(conflict_uuid);
        Ok(self
            .append_event_with_identity(
                genesis_id(EventKind::ConflictOpened, &seed.id, ""),
                ts_of(&seed.created_at),
                &draft,
            )?
            .map(|_| 1)
            .unwrap_or(0))
    }

    fn backfill_queue_keys(
        &self,
        id: &str,
        key: &str,
        member: &str,
    ) -> Result<usize, StorageError> {
        Ok(self.conn.execute(
            "UPDATE fact_conflict_queue
                SET decision_key = COALESCE(decision_key, ?1),
                    member_key = COALESCE(member_key, ?2),
                    registry_version = COALESCE(registry_version, ?3)
              WHERE id = ?4 AND decision_key IS NULL",
            params![key, member, REGISTRY_VERSION, id],
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_decision_row(
        &self,
        id: Uuid,
        kb: &str,
        diary: &str,
        key: &str,
        subject_norm: &str,
        relation: &str,
        verdict: &str,
        loser_object_norm: Option<&str>,
        authority: &str,
        actor: &str,
        note: Option<&str>,
        conflict_id: Option<Uuid>,
        event_id: Uuid,
        decided_at: &str,
    ) -> Result<usize, StorageError> {
        let pair_hash = engram_core::keys::semantic_key_decode(key)
            .map(|(_, _, p)| p)
            .ok_or_else(|| {
                StorageError::MalformedRow(format!("decision key `{key}` is malformed"))
            })?;
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO fact_decisions(id, kb, diary, decision_key, subject_norm,
                    relation, pair_hash, verdict, authority, actor, note, conflict_id,
                    event_id, key_version, decided_at, loser_object_norm)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                id.to_string(),
                kb,
                diary,
                key,
                subject_norm,
                relation,
                pair_hash,
                verdict,
                authority,
                actor,
                note,
                conflict_id.map(|u| u.to_string()),
                event_id.to_string(),
                SEMANTIC_KEY_VERSION,
                decided_at,
                loser_object_norm,
            ],
        )?)
    }
}

//! Background jobs, and the transaction primitives the single-writer `Core`
//! needs.
//!
//! Two things live here for one reason: `SqliteStore::conn` is `pub(crate)`,
//! so an `impl SqliteStore` block has to be inside this crate, and daemon-jobs
//! owns exactly one file in it (D27's scoped exception). Everything below is
//! the daemon's half of storage — the jobs table and the transaction shapes
//! `engram-cli`'s writer thread runs closures inside — and nothing else in
//! `engram-storage` calls it.
//!
//! The jobs table is `compile_jobs`, extended, not a new table. That is
//! deliberate: `MEMORY_CONSTRAINS_DERIVED` proves a deleted memory has been
//! compiled out by looking for a `completed` row in `compile_jobs` newer than
//! the deletion, and a parallel table would fork that predicate into two
//! answers. Which is also why [`STATUS_COMPLETED`] keeps one meaning —
//! **the derived layer was fully rebuilt** — and a paused or cancelled compile
//! must never be written as completed.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

use crate::error::StorageError;
use crate::sqlite::SqliteStore;

// ---------------------------------------------------------------------------
// Status vocabulary (interfaces §6, per D26)
// ---------------------------------------------------------------------------

/// Submitted, unclaimed. Nothing is running it yet.
pub const STATUS_QUEUED: &str = "queued";
/// Claimed by a worker holding a live lease.
pub const STATUS_RUNNING: &str = "running";
/// **The derived layer was fully rebuilt.** `MEMORY_CONSTRAINS_DERIVED` reads
/// this value as proof a deleted memory was compiled out; writing it for a
/// compile that stopped early tells the store a lie it cannot detect.
pub const STATUS_COMPLETED: &str = "completed";
/// Stopped at a checkpoint with a resumable cursor: budget, cancellation or
/// shutdown. Not a failure, and emphatically not a completion.
pub const STATUS_PAUSED: &str = "paused";
/// Terminal, not resumable.
pub const STATUS_CANCELLED: &str = "cancelled";
/// The runner returned an error.
pub const STATUS_FAILED: &str = "failed";
/// Superseded by a newer job for the same KB, or orphaned by a crash with no
/// cursor to resume from.
pub const STATUS_ABANDONED: &str = "abandoned";

/// Statuses that mean "this job is still someone's problem".
pub const LIVE_STATUSES: [&str; 2] = [STATUS_QUEUED, STATUS_RUNNING];

/// A status nothing will move again.
pub fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        STATUS_COMPLETED | STATUS_CANCELLED | STATUS_FAILED | STATUS_ABANDONED
    )
}

// ---------------------------------------------------------------------------
// Schema (D21 — one home; whoever does not own it calls `ensure_jobs_schema`)
// ---------------------------------------------------------------------------

/// The columns wave 4 adds to `compile_jobs`. Every one has a DEFAULT, so an
/// older binary — which lists its columns explicitly on INSERT — still reads
/// and writes this table unchanged, and no schema-version bump is needed.
const JOBS_SCHEMA_COLUMNS: &[(&str, &str)] = &[
    ("kind", "TEXT NOT NULL DEFAULT 'compile'"),
    ("params_json", "TEXT NOT NULL DEFAULT '{}'"),
    ("cursor_json", "TEXT NOT NULL DEFAULT '{}'"),
    ("progress_done", "INTEGER NOT NULL DEFAULT 0"),
    ("progress_total", "INTEGER"),
    ("actor", "TEXT NOT NULL DEFAULT 'cli'"),
    ("lease_owner", "TEXT"),
    ("lease_expires_at", "TEXT"),
    ("attempts", "INTEGER NOT NULL DEFAULT 0"),
    ("cancel_requested", "INTEGER NOT NULL DEFAULT 0"),
    ("started_at", "TEXT"),
    ("finished_at", "TEXT"),
];

/// The index half, applied after the duplicate collapse. `idx_jobs_live` is a
/// partial unique index, so "at most one live job per (kind, kb)" is enforced
/// by SQLite rather than by a read-then-write window two processes can both
/// walk through.
pub const JOBS_SCHEMA_SQL: &str = "
CREATE INDEX IF NOT EXISTS idx_jobs_status_created ON compile_jobs(status, created_at);
CREATE INDEX IF NOT EXISTS idx_jobs_kind_kb        ON compile_jobs(kind, kb, created_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_live
    ON compile_jobs(kind, kb) WHERE status IN ('queued', 'running');
";

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JobRow {
    pub id: Uuid,
    pub kind: String,
    pub kb: String,
    pub status: String,
    /// Legacy column, kept: `evidence` for every compile.
    pub mode: String,
    pub params: Value,
    pub cursor: Value,
    pub progress_done: i64,
    pub progress_total: Option<i64>,
    /// Parsed from the legacy `metadata_json`; `jobs list` still calls it
    /// `metadata` on the wire.
    pub stats: Value,
    pub message: Option<String>,
    pub actor: String,
    pub attempts: i64,
    pub cancel_requested: bool,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl JobRow {
    /// The legacy `CompileJobRecord` shape, field for field, plus the wave-4
    /// additions. `jobs list` and `GET /v1/jobs` serialize this, so a caller
    /// written against 0.3.x keeps parsing.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "id": self.id.to_string(),
            "kb": self.kb,
            "status": self.status,
            "mode": self.mode,
            "created_at": self.created_at.to_rfc3339(),
            "updated_at": self.updated_at.to_rfc3339(),
            "message": self.message,
            "metadata": self.stats,
            // additive
            "handle": format!("job:{}", self.id),
            "kind": self.kind,
            "params": self.params,
            "cursor": self.cursor,
            "progress_done": self.progress_done,
            "progress_total": self.progress_total,
            "actor": self.actor,
            "attempts": self.attempts,
            "cancel_requested": self.cancel_requested,
            "started_at": self.started_at.map(|t| t.to_rfc3339()),
            "finished_at": self.finished_at.map(|t| t.to_rfc3339()),
            "resumable": self.status == STATUS_PAUSED
                || (self.status == STATUS_FAILED && self.cursor != Value::Null),
        })
    }
}

pub struct NewJob<'a> {
    pub kind: &'a str,
    pub kb: &'a str,
    pub params: Value,
    pub actor: &'a str,
}

#[derive(Debug)]
pub enum SubmitOutcome {
    Submitted(Uuid),
    /// A live job for this `(kind, kb)` already exists. Submit is idempotent,
    /// not an error: two agents asking for the same compile want one compile.
    AlreadyLive(Box<JobRow>),
}

#[derive(Debug, Default, Clone)]
pub struct JobFilter {
    pub kind: Option<String>,
    pub kb: Option<String>,
    pub status: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct JobHeartbeat {
    pub cancel_requested: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CompilePressure {
    /// Memories written since the newest COMPLETED compile of this KB.
    pub pending: i64,
    pub oldest_pending_at: Option<DateTime<Utc>>,
    pub last_compile_at: Option<DateTime<Utc>>,
}

const JOB_COLUMNS: &str = "id, kind, kb, status, mode, params_json, cursor_json, progress_done, \
     progress_total, metadata_json, message, actor, attempts, cancel_requested, lease_owner, \
     lease_expires_at, created_at, updated_at, started_at, finished_at";

// ---------------------------------------------------------------------------
// Transaction primitives for the single-writer core
// ---------------------------------------------------------------------------

impl SqliteStore {
    /// `with_tx`, generalised over the closure's error type.
    ///
    /// `Core::write` runs closures that return `CliError`, and `with_tx` is
    /// hard-wired to `StorageError`. Rather than making every write closure
    /// launder its errors through a storage variant — which is how a bad-input
    /// refusal ends up reported as a database failure — the transaction shape
    /// is written once and generic.
    ///
    /// Unwind-safe, which `with_tx` is not: a closure that panics leaves
    /// `is_autocommit() == false` on the connection forever, and every later
    /// write silently joins a transaction nothing will ever commit. Here the
    /// panic rolls back first and then resumes.
    pub fn with_tx_res<T, E>(&self, f: impl FnOnce(&Self) -> Result<T, E>) -> Result<T, E>
    where
        E: From<StorageError>,
    {
        if !self.conn.is_autocommit() {
            return f(self);
        }
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| E::from(crate::error::tx_error(e)))?;
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match out {
            Ok(Ok(v)) => {
                self.conn
                    .execute_batch("COMMIT")
                    .map_err(|e| E::from(StorageError::from(e)))?;
                Ok(v)
            }
            Ok(Err(e)) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
            Err(panic) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// Whether this connection currently holds an open transaction. The
    /// nested-write guard reads it: `Core::write` from inside another
    /// connection's open transaction is a guaranteed lock wait, not a bug that
    /// shows up as a slow request.
    pub fn in_transaction(&self) -> bool {
        !self.conn.is_autocommit()
    }

    /// Issue a `ROLLBACK` if one is owed. The writer thread calls it after
    /// catching a panic so the next task does not join a doomed transaction.
    pub fn rollback_if_open(&self) {
        if !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }

    /// Whether move 3's content-addressed derivation ledger can memoize LLM
    /// extraction for this store. `false` until move 3 lands, which is what
    /// makes `jobs resume` refuse to silently re-spend a caller's money.
    pub fn derivation_cache_available(&self) -> bool {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='derivations'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false)
            && self
                .conn
                .query_row("SELECT COUNT(*) FROM derivations", [], |r| {
                    r.get::<_, i64>(0)
                })
                .map(|n| n > 0)
                .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// The jobs API
// ---------------------------------------------------------------------------

impl SqliteStore {
    /// Idempotent, additive, downgrade-safe. Runs at store-open time rather
    /// than on the first write, because `engram jobs list` is a read-only
    /// command that selects the new columns and must not be the thing that
    /// discovers they were never added.
    pub fn ensure_jobs_schema(&self) -> Result<(), StorageError> {
        let existing: Vec<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(compile_jobs)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if existing.is_empty() {
            // No `compile_jobs` at all: a store this old has not been migrated
            // yet, and migrate() owns creating it. Nothing to extend.
            return Ok(());
        }
        let missing: Vec<&(&str, &str)> = JOBS_SCHEMA_COLUMNS
            .iter()
            .filter(|(name, _)| !existing.iter().any(|e| e == name))
            .collect();
        let index_present: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_jobs_live'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if missing.is_empty() && index_present {
            return Ok(());
        }
        self.with_tx(|s| {
            for (name, decl) in &missing {
                s.conn.execute_batch(&format!(
                    "ALTER TABLE compile_jobs ADD COLUMN {name} {decl}"
                ))?;
            }
            // Collapse duplicate live rows BEFORE the unique index, exactly as
            // interfaces §2.1 step 5 requires for `idx_fcq_open_key`. The
            // legacy `create_compile_job` abandons-then-inserts in two
            // unguarded statements, so a store written by 0.3.x can hold two
            // `running` rows for one KB — and a bare CREATE UNIQUE INDEX
            // against that store errors inside schema setup and bricks every
            // command. Survivor: the oldest, ties broken by id.
            s.conn.execute(
                "UPDATE compile_jobs
                    SET status = 'abandoned',
                        message = COALESCE(message,
                            'superseded: duplicate live job collapsed at wave-4 schema setup'),
                        updated_at = ?1
                  WHERE status IN ('queued', 'running')
                    AND id NOT IN (
                        -- SQLite's documented bare-column rule: with a single
                        -- MIN() in the select list, `id` comes from the row
                        -- that matched it. Survivor = oldest, ties by id.
                        SELECT id FROM (SELECT id, MIN(created_at || id)
                                          FROM compile_jobs
                                         WHERE status IN ('queued','running')
                                         GROUP BY kind, kb))",
                params![Utc::now().to_rfc3339()],
            )?;
            s.conn.execute_batch(JOBS_SCHEMA_SQL)?;
            Ok(())
        })
    }

    /// INSERT, with the unique-index violation translated into
    /// [`SubmitOutcome::AlreadyLive`] rather than surfaced as a SQL error.
    /// There is no read-then-write window: the index decides.
    pub fn submit_job(&self, new: &NewJob<'_>) -> Result<SubmitOutcome, StorageError> {
        self.ensure_kb(new.kb, None)?;
        let id = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        let inserted = self.conn.execute(
            "INSERT INTO compile_jobs(id, kb, status, mode, created_at, updated_at,
                                      metadata_json, kind, params_json, cursor_json, actor)
             VALUES (?1, ?2, 'queued', 'evidence', ?3, ?3, '{}', ?4, ?5, '{}', ?6)",
            params![
                id.to_string(),
                new.kb,
                now,
                new.kind,
                serde_json::to_string(&new.params)?,
                new.actor
            ],
        );
        match inserted {
            Ok(_) => Ok(SubmitOutcome::Submitted(id)),
            Err(e) if is_unique_violation(&e) => {
                let live = self.live_job(new.kind, new.kb)?.ok_or_else(|| {
                    StorageError::Busy(format!(
                        "another {} job for kb '{}' is live",
                        new.kind, new.kb
                    ))
                })?;
                Ok(SubmitOutcome::AlreadyLive(Box::new(live)))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn live_job(&self, kind: &str, kb: &str) -> Result<Option<JobRow>, StorageError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {JOB_COLUMNS} FROM compile_jobs
                      WHERE kind = ?1 AND kb = ?2 AND status IN ('queued','running')
                      ORDER BY created_at LIMIT 1"
                ),
                params![kind, kb],
                parse_job_row,
            )
            .optional()?
            .transpose()
    }

    /// Exactly-one-row claim. `false` means somebody else holds a live lease —
    /// a worker that reads `false` and runs anyway is the double-write this
    /// method exists to make impossible.
    pub fn claim_job(&self, id: Uuid, owner: &str, lease: Duration) -> Result<bool, StorageError> {
        let now = Utc::now();
        let expires = now + chrono_duration(lease);
        let changed = self.conn.execute(
            "UPDATE compile_jobs
                SET status = 'running', lease_owner = ?1, lease_expires_at = ?2,
                    attempts = attempts + 1, started_at = COALESCE(started_at, ?3),
                    updated_at = ?3
              WHERE id = ?4
                AND (status = 'queued'
                     OR (status = 'running'
                         AND (lease_expires_at IS NULL OR lease_expires_at < ?3)))",
            params![
                owner,
                expires.to_rfc3339(),
                now.to_rfc3339(),
                id.to_string()
            ],
        )?;
        Ok(changed == 1)
    }

    /// The oldest queued job, or a running job whose lease expired (crash
    /// recovery), restricted to the kinds this process can actually run.
    pub fn next_runnable_job(&self, kinds: &[&str]) -> Result<Option<JobRow>, StorageError> {
        if kinds.is_empty() {
            return Ok(None);
        }
        let placeholders = kinds
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM compile_jobs
              WHERE kind IN ({placeholders})
                AND (status = 'queued'
                     OR (status = 'running'
                         AND (lease_expires_at IS NULL OR lease_expires_at < ?1)))
              ORDER BY created_at LIMIT 1"
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(Utc::now().to_rfc3339())];
        for k in kinds {
            binds.push(Box::new((*k).to_string()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
        self.conn
            .query_row(&sql, refs.as_slice(), parse_job_row)
            .optional()?
            .transpose()
    }

    /// Extend the lease and report whether cancellation was requested.
    /// `NotFound` means the lease was stolen; a worker that sees it must stop
    /// rather than keep writing on behalf of a job it no longer owns.
    pub fn heartbeat_job(
        &self,
        id: Uuid,
        owner: &str,
        lease: Duration,
    ) -> Result<JobHeartbeat, StorageError> {
        let now = Utc::now();
        let expires = now + chrono_duration(lease);
        let changed = self.conn.execute(
            "UPDATE compile_jobs SET lease_expires_at = ?1, updated_at = ?2
              WHERE id = ?3 AND lease_owner = ?4 AND status = 'running'",
            params![
                expires.to_rfc3339(),
                now.to_rfc3339(),
                id.to_string(),
                owner
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(format!(
                "job {id} is no longer leased to {owner}"
            )));
        }
        let cancel: i64 = self.conn.query_row(
            "SELECT cancel_requested FROM compile_jobs WHERE id = ?1",
            params![id.to_string()],
            |r| r.get(0),
        )?;
        Ok(JobHeartbeat {
            cancel_requested: cancel != 0,
        })
    }

    /// Persist a resume point. Guarded on `lease_owner`, so a zombie worker
    /// cannot rewind a job another process now owns.
    pub fn checkpoint_job(
        &self,
        id: Uuid,
        owner: &str,
        cursor: &Value,
        done: i64,
        total: Option<i64>,
    ) -> Result<bool, StorageError> {
        let changed = self.conn.execute(
            "UPDATE compile_jobs
                SET cursor_json = ?1, progress_done = ?2, progress_total = ?3, updated_at = ?4
              WHERE id = ?5 AND lease_owner = ?6 AND status = 'running'",
            params![
                serde_json::to_string(cursor)?,
                done,
                total,
                Utc::now().to_rfc3339(),
                id.to_string(),
                owner
            ],
        )?;
        Ok(changed == 1)
    }

    /// Terminal (or paused) transition, guarded on the lease. Append the
    /// matching `job.completed` / `job.failed` event in the SAME transaction
    /// as this call — the caller does, because only the caller knows the
    /// stats.
    pub fn finish_job(
        &self,
        id: Uuid,
        owner: &str,
        status: &str,
        message: Option<&str>,
        stats: &Value,
    ) -> Result<bool, StorageError> {
        let now = Utc::now().to_rfc3339();
        let finished = if is_terminal(status) {
            Some(now.clone())
        } else {
            None
        };
        let changed = self.conn.execute(
            "UPDATE compile_jobs
                SET status = ?1, message = ?2, metadata_json = ?3, updated_at = ?4,
                    finished_at = ?5, lease_owner = NULL, lease_expires_at = NULL
              WHERE id = ?6 AND status = 'running' AND lease_owner = ?7",
            params![
                status,
                message,
                serde_json::to_string(stats)?,
                now,
                finished,
                id.to_string(),
                owner
            ],
        )?;
        Ok(changed == 1)
    }

    /// Abandon every live job for `(kind, kb)`.
    ///
    /// The legacy `create_compile_job` clears only `running` rows, so a
    /// `queued` row left by `jobs submit --detach` would make the very next
    /// synchronous `engram compile` of that KB fail on `idx_jobs_live` with a
    /// raw `UNIQUE constraint failed`. The synchronous path calls this first,
    /// inside the same transaction as its insert, so the abandon/insert pair
    /// is atomic and two concurrent compiles cannot leave two live rows.
    pub fn abandon_live_jobs(
        &self,
        kind: &str,
        kb: &str,
        message: &str,
    ) -> Result<usize, StorageError> {
        let n = self.conn.execute(
            "UPDATE compile_jobs SET status = 'abandoned', message = ?1, updated_at = ?2,
                                     finished_at = ?2, lease_owner = NULL, lease_expires_at = NULL
              WHERE kind = ?3 AND kb = ?4 AND status IN ('queued','running')",
            params![message, Utc::now().to_rfc3339(), kind, kb],
        )?;
        Ok(n)
    }

    /// Test seam: run one statement against this connection.
    ///
    /// It exists to CONSTRUCT the shapes an older binary could leave behind —
    /// two live compile rows for one KB, a store without the wave-4 index —
    /// which by definition no supported code path can produce. Nothing in the
    /// product calls it.
    #[doc(hidden)]
    pub fn conn_free_for_test(&self, sql: &str) -> Result<(), StorageError> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }

    /// Test seam: point both sides of a conflict row at ONE fact.
    ///
    /// A 0.3.x store can legitimately hold such a row, and the old resolve
    /// would happily supersede that fact by itself. There is no supported way
    /// to construct it through the write path, which is exactly why the guard
    /// needs a way to be tested.
    #[doc(hidden)]
    pub fn point_conflict_at_one_fact_for_test(
        &self,
        conflict_id: Uuid,
        fact_id: Uuid,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE fact_conflict_queue SET new_fact_id = ?1, old_fact_id = ?1 WHERE id = ?2",
            params![fact_id.to_string(), conflict_id.to_string()],
        )?;
        Ok(())
    }

    pub fn request_cancel(&self, id: Uuid) -> Result<bool, StorageError> {
        let changed = self.conn.execute(
            "UPDATE compile_jobs SET cancel_requested = 1, updated_at = ?1
              WHERE id = ?2 AND status IN ('queued','running')",
            params![Utc::now().to_rfc3339(), id.to_string()],
        )?;
        if changed == 1 {
            // A queued job nobody claimed has no worker to notice the flag, so
            // cancel it here rather than leaving a row that says "cancel
            // requested" forever.
            self.conn.execute(
                "UPDATE compile_jobs
                    SET status = 'cancelled', message = 'cancelled before it started',
                        finished_at = ?1, updated_at = ?1
                  WHERE id = ?2 AND status = 'queued'",
                params![Utc::now().to_rfc3339(), id.to_string()],
            )?;
        }
        Ok(changed == 1)
    }

    /// `paused` / `failed` / `cancelled` → `queued`, cursor preserved, lease
    /// cleared. Refuses for `running` and `completed`: a completed job has
    /// nothing left to do and a running one already has an owner.
    pub fn requeue_job(&self, id: Uuid) -> Result<bool, StorageError> {
        let changed = self.conn.execute(
            "UPDATE compile_jobs
                SET status = 'queued', lease_owner = NULL, lease_expires_at = NULL,
                    cancel_requested = 0, finished_at = NULL, updated_at = ?1,
                    message = NULL
              WHERE id = ?2 AND status IN ('paused','failed','cancelled')",
            params![Utc::now().to_rfc3339(), id.to_string()],
        )?;
        Ok(changed == 1)
    }

    pub fn get_job(&self, id: Uuid) -> Result<Option<JobRow>, StorageError> {
        self.conn
            .query_row(
                &format!("SELECT {JOB_COLUMNS} FROM compile_jobs WHERE id = ?1"),
                params![id.to_string()],
                parse_job_row,
            )
            .optional()?
            .transpose()
    }

    /// `(rows, total_matching)` — the envelope carries `returned` and `total`
    /// separately (IN-23), so a caller reading `count` never mistakes a page
    /// for a backlog.
    pub fn list_jobs(&self, f: &JobFilter) -> Result<(Vec<JobRow>, i64), StorageError> {
        let mut where_sql = String::from(" WHERE 1=1");
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(kind) = &f.kind {
            binds.push(Box::new(kind.clone()));
            where_sql.push_str(&format!(" AND kind = ?{}", binds.len()));
        }
        if let Some(kb) = &f.kb {
            binds.push(Box::new(kb.clone()));
            where_sql.push_str(&format!(" AND kb = ?{}", binds.len()));
        }
        if let Some(status) = &f.status {
            binds.push(Box::new(status.clone()));
            where_sql.push_str(&format!(" AND status = ?{}", binds.len()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM compile_jobs{where_sql}"),
            refs.as_slice(),
            |r| r.get(0),
        )?;
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM compile_jobs{where_sql}
              ORDER BY created_at DESC LIMIT {} OFFSET {}",
            f.limit.max(1),
            f.offset
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(refs.as_slice(), parse_job_row)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok((rows, total))
    }

    /// Sleep-time pressure, derived entirely from existing rows: no counter
    /// table, nothing to drift.
    ///
    /// The `kind = 'compile'` filter is load-bearing. Without it a completed
    /// `backfill` job would reset compile pressure to zero and the KB would
    /// never be compiled again.
    pub fn compile_pressure(&self, kb: &str) -> Result<CompilePressure, StorageError> {
        let last: Option<String> = self
            .conn
            .query_row(
                "SELECT MAX(created_at) FROM compile_jobs
                  WHERE kb = ?1 AND kind = 'compile' AND status = 'completed'",
                params![kb],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let since = last.clone().unwrap_or_else(|| "0000".to_string());
        let (pending, oldest): (i64, Option<String>) = self.conn.query_row(
            "SELECT COUNT(*), MIN(created_at) FROM memories
              WHERE kb = ?1 AND deleted = 0 AND created_at > ?2",
            params![kb, since],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(CompilePressure {
            pending,
            oldest_pending_at: oldest.as_deref().and_then(parse_ts),
            last_compile_at: last.as_deref().and_then(parse_ts),
        })
    }

    /// The newest write anywhere in a KB, journal first and projection second.
    ///
    /// The idle gate must be STORE-derived: a daemon watching only its own
    /// writer metrics considers the store idle while an agent hammers `engram
    /// remember` from another process, and compiles into the middle of an
    /// ingest.
    pub fn newest_write_ts(&self, kb: &str) -> Result<Option<DateTime<Utc>>, StorageError> {
        let from_events: Option<String> = self
            .conn
            .query_row(
                "SELECT MAX(ts) FROM events WHERE kb = ?1",
                params![kb],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let from_memories: Option<String> = self
            .conn
            .query_row(
                "SELECT MAX(created_at) FROM memories WHERE kb = ?1",
                params![kb],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok([from_events, from_memories]
            .into_iter()
            .flatten()
            .filter_map(|s| parse_ts(&s))
            .max())
    }

    /// How many auto-submitted compiles this KB has had since `since`. The
    /// daily cap reads it, so a misconfigured trigger cannot compile in a loop.
    pub fn auto_compiles_since(&self, kb: &str, since: DateTime<Utc>) -> Result<i64, StorageError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM compile_jobs
                  WHERE kb = ?1 AND kind = 'compile' AND created_at > ?2
                    AND actor LIKE 'daemon:sleep%'",
                params![kb, since.to_rfc3339()],
                |r| r.get(0),
            )
            .map_err(StorageError::from)
    }

    /// Crash recovery at daemon start: a `running` row whose lease expired is
    /// either resumable (it has a cursor) or dead (it does not), and each says
    /// so in `message` rather than sitting at `running` forever.
    pub fn reconcile_orphan_jobs(&self, grace: Duration) -> Result<usize, StorageError> {
        let cutoff = (Utc::now() - chrono_duration(grace)).to_rfc3339();
        let now = Utc::now().to_rfc3339();
        self.with_tx(|s| {
            let paused = s.conn.execute(
                "UPDATE compile_jobs
                    SET status = 'paused', lease_owner = NULL, lease_expires_at = NULL,
                        updated_at = ?1,
                        message = 'worker vanished; resumable from its last checkpoint'
                  WHERE status = 'running'
                    AND (lease_expires_at IS NULL OR lease_expires_at < ?2)
                    AND cursor_json NOT IN ('{}', '', 'null')",
                params![now, cutoff],
            )?;
            let failed = s.conn.execute(
                "UPDATE compile_jobs
                    SET status = 'failed', lease_owner = NULL, lease_expires_at = NULL,
                        updated_at = ?1, finished_at = ?1,
                        message = 'worker vanished before its first checkpoint; nothing to resume'
                  WHERE status = 'running'
                    AND (lease_expires_at IS NULL OR lease_expires_at < ?2)",
                params![now, cutoff],
            )?;
            Ok(paused + failed)
        })
    }
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ffi::ErrorCode::ConstraintViolation
    ) && e.to_string().to_ascii_lowercase().contains("unique")
}

fn chrono_duration(d: Duration) -> ChronoDuration {
    ChronoDuration::from_std(d).unwrap_or_else(|_| ChronoDuration::seconds(60))
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Row → `JobRow`. The outer `rusqlite::Result` is the SQL layer; the inner
/// `Result` is the parse, because a malformed `params_json` is a corrupt store
/// and not a query failure.
#[allow(clippy::type_complexity)]
fn parse_job_row(r: &rusqlite::Row) -> rusqlite::Result<Result<JobRow, StorageError>> {
    let id: String = r.get(0)?;
    let created_at: String = r.get(16)?;
    let updated_at: String = r.get(17)?;
    let params_json: String = r.get(5)?;
    let cursor_json: String = r.get(6)?;
    let metadata_json: String = r.get(9)?;
    let lease_expires_at: Option<String> = r.get(15)?;
    let started_at: Option<String> = r.get(18)?;
    let finished_at: Option<String> = r.get(19)?;
    let cancel: i64 = r.get(13)?;
    Ok((|| -> Result<JobRow, StorageError> {
        Ok(JobRow {
            id: Uuid::parse_str(&id)
                .map_err(|e| StorageError::MalformedRow(format!("job id {id}: {e}")))?,
            kind: r.get::<_, String>(1).unwrap_or_default(),
            kb: r.get::<_, String>(2).unwrap_or_default(),
            status: r.get::<_, String>(3).unwrap_or_default(),
            mode: r.get::<_, String>(4).unwrap_or_default(),
            params: serde_json::from_str(&params_json).unwrap_or(Value::Null),
            cursor: serde_json::from_str(&cursor_json).unwrap_or(Value::Null),
            progress_done: r.get(7).unwrap_or(0),
            progress_total: r.get(8).unwrap_or(None),
            stats: serde_json::from_str(&metadata_json).unwrap_or(Value::Null),
            message: r.get(10).unwrap_or(None),
            actor: r.get::<_, String>(11).unwrap_or_default(),
            attempts: r.get(12).unwrap_or(0),
            cancel_requested: cancel != 0,
            lease_owner: r.get(14).unwrap_or(None),
            lease_expires_at: lease_expires_at.as_deref().and_then(parse_ts),
            created_at: parse_ts(&created_at).unwrap_or_else(Utc::now),
            updated_at: parse_ts(&updated_at).unwrap_or_else(Utc::now),
            started_at: started_at.as_deref().and_then(parse_ts),
            finished_at: finished_at.as_deref().and_then(parse_ts),
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, SqliteStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = SqliteStore::open(dir.path().join("db.sqlite")).expect("open");
        s.ensure_jobs_schema().expect("schema");
        (dir, s)
    }

    #[test]
    fn ensure_jobs_schema_is_idempotent() {
        let (_d, s) = store();
        s.ensure_jobs_schema().expect("second run");
        s.ensure_jobs_schema().expect("third run");
    }

    #[test]
    fn submit_is_idempotent_per_kind_and_kb() {
        let (_d, s) = store();
        let first = s
            .submit_job(&NewJob {
                kind: "compile",
                kb: "default",
                params: json!({}),
                actor: "cli",
            })
            .expect("submit");
        let id = match first {
            SubmitOutcome::Submitted(id) => id,
            other => panic!("expected a fresh submit, got {other:?}"),
        };
        let second = s
            .submit_job(&NewJob {
                kind: "compile",
                kb: "default",
                params: json!({}),
                actor: "cli",
            })
            .expect("submit");
        match second {
            SubmitOutcome::AlreadyLive(row) => assert_eq!(row.id, id),
            other => panic!("a second live submit must not create a row: {other:?}"),
        }
        let (rows, total) = s
            .list_jobs(&JobFilter {
                limit: 50,
                ..Default::default()
            })
            .expect("list");
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn a_claim_is_exactly_one_winner() {
        let (_d, s) = store();
        let SubmitOutcome::Submitted(id) = s
            .submit_job(&NewJob {
                kind: "compile",
                kb: "default",
                params: json!({}),
                actor: "cli",
            })
            .expect("submit")
        else {
            panic!("fresh submit");
        };
        assert!(s
            .claim_job(id, "a", Duration::from_secs(60))
            .expect("claim"));
        assert!(
            !s.claim_job(id, "b", Duration::from_secs(60))
                .expect("claim"),
            "a live lease must refuse a second claimer"
        );
        // The stale owner cannot rewind a job it no longer owns.
        assert!(!s
            .checkpoint_job(id, "b", &json!({"n": 1}), 1, Some(2))
            .expect("checkpoint"));
        assert!(s
            .checkpoint_job(id, "a", &json!({"n": 1}), 1, Some(2))
            .expect("checkpoint"));
    }

    #[test]
    fn compile_pressure_is_null_safe_on_a_kb_that_never_compiled() {
        let (_d, s) = store();
        s.ensure_kb("default", None).expect("kb");
        let p = s.compile_pressure("default").expect("pressure");
        assert_eq!(p.pending, 0);
        assert!(p.last_compile_at.is_none());
    }

    #[test]
    fn an_orphan_with_a_cursor_pauses_and_one_without_fails() {
        let (_d, s) = store();
        for (kb, cursor) in [("with", json!({"next": 3})), ("without", json!({}))] {
            s.ensure_kb(kb, None).expect("kb");
            let SubmitOutcome::Submitted(id) = s
                .submit_job(&NewJob {
                    kind: "compile",
                    kb,
                    params: json!({}),
                    actor: "cli",
                })
                .expect("submit")
            else {
                panic!("fresh submit");
            };
            assert!(s
                .claim_job(id, "dead", Duration::from_secs(0))
                .expect("claim"));
            s.conn
                .execute(
                    "UPDATE compile_jobs SET cursor_json = ?1 WHERE id = ?2",
                    params![serde_json::to_string(&cursor).unwrap(), id.to_string()],
                )
                .expect("cursor");
        }
        let moved = s
            .reconcile_orphan_jobs(Duration::from_secs(0))
            .expect("reconcile");
        assert_eq!(moved, 2);
        let (rows, _) = s
            .list_jobs(&JobFilter {
                limit: 50,
                ..Default::default()
            })
            .expect("list");
        let by_kb: std::collections::HashMap<_, _> = rows
            .iter()
            .map(|r| (r.kb.clone(), r.status.clone()))
            .collect();
        assert_eq!(by_kb["with"], STATUS_PAUSED);
        assert_eq!(by_kb["without"], STATUS_FAILED);
    }
}
